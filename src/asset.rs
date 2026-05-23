use crate::compile::compile_skill;
use crate::dsl::{SkillDef, SkillId};
use crate::registry::{SkillError, SkillRegistry};
use bevy::prelude::{Message, Res, ResMut, Resource};
pub use bevy_skill_ecs::SkillLibrary;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillAssetDocument {
    Single(SkillDef),
    Many(Vec<SkillDef>),
    Named { skills: Vec<SkillDef> },
}

pub fn parse_skill_def(source: &str) -> Result<SkillDef, SkillError> {
    ron::from_str(source).map_err(|err| SkillError::Ron(err.to_string()))
}

pub fn parse_skill_document(source: &str) -> Result<Vec<SkillDef>, SkillError> {
    if let Ok(skills) = ron::from_str::<Vec<SkillDef>>(source) {
        return Ok(skills);
    }
    #[derive(Deserialize)]
    struct NamedSkills {
        skills: Vec<SkillDef>,
    }
    if let Ok(named) = ron::from_str::<NamedSkills>(source) {
        return Ok(named.skills);
    }
    parse_skill_def(source).map(|skill| vec![skill])
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillAssetReloaded {
    pub key: String,
    pub skills: Vec<SkillId>,
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillAssetReloadFailed {
    pub key: String,
    pub error: SkillError,
}

#[derive(Resource, Default, Clone, Debug)]
pub struct SkillAssetSources {
    sources: IndexMap<String, SkillAssetSource>,
}

#[derive(Clone, Debug)]
pub struct SkillAssetSource {
    pub source: String,
    pub version: u64,
    pub applied_version: u64,
    pub last_skills: Vec<SkillId>,
    pub last_error: Option<SkillError>,
}

impl SkillAssetSources {
    pub fn set_source(&mut self, key: impl Into<String>, source: impl Into<String>) {
        let key = key.into();
        let source = source.into();
        match self.sources.get_mut(&key) {
            Some(entry) => {
                entry.source = source;
                entry.version = entry.version.saturating_add(1);
            }
            None => {
                self.sources.insert(
                    key,
                    SkillAssetSource {
                        source,
                        version: 1,
                        applied_version: 0,
                        last_skills: Vec::new(),
                        last_error: None,
                    },
                );
            }
        }
    }

    pub fn get(&self, key: &str) -> Option<&SkillAssetSource> {
        self.sources.get(key)
    }

    pub fn dirty_keys(&self) -> impl Iterator<Item = &String> {
        self.sources
            .iter()
            .filter(|(_, source)| source.version != source.applied_version)
            .map(|(key, _)| key)
    }
}

pub fn compile_dirty_skill_assets(
    mut sources: ResMut<SkillAssetSources>,
    registry: Res<SkillRegistry>,
    mut library: ResMut<SkillLibrary>,
    mut reloaded: bevy::prelude::MessageWriter<SkillAssetReloaded>,
    mut failed: bevy::prelude::MessageWriter<SkillAssetReloadFailed>,
) {
    let dirty = sources.dirty_keys().cloned().collect::<Vec<_>>();
    for key in dirty {
        let Some(snapshot) = sources.get(&key).cloned() else {
            continue;
        };
        let result = compile_asset_source(&snapshot.source, &registry, &mut library);
        let Some(entry) = sources.sources.get_mut(&key) else {
            continue;
        };
        match result {
            Ok(skills) => {
                for old in &entry.last_skills {
                    if !skills.contains(old) {
                        library.remove(old);
                    }
                }
                entry.last_skills = skills.clone();
                entry.last_error = None;
                entry.applied_version = snapshot.version;
                reloaded.write(SkillAssetReloaded { key, skills });
            }
            Err(err) => {
                entry.last_error = Some(err.clone());
                entry.applied_version = snapshot.version;
                failed.write(SkillAssetReloadFailed { key, error: err });
            }
        }
    }
}

fn compile_asset_source(
    source: &str,
    registry: &SkillRegistry,
    library: &mut SkillLibrary,
) -> Result<Vec<SkillId>, SkillError> {
    let defs = parse_skill_document(source)?;
    let mut updated = Vec::with_capacity(defs.len());
    for def in defs {
        let id = def.id.clone();
        match compile_skill(&def, registry) {
            Ok(compiled) => {
                library.insert_compiled(compiled);
                updated.push(id);
            }
            Err(err) => {
                library.mark_invalid(id.clone(), err.to_string());
                return Err(err);
            }
        }
    }
    Ok(updated)
}
