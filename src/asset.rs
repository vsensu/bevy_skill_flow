use crate::compile::compile_skill;
use crate::dsl::{SkillCompiled, SkillDef, SkillId};
use crate::registry::{SkillError, SkillRegistry};
use bevy::prelude::Resource;
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

#[derive(Resource, Default, Clone, Debug)]
pub struct SkillLibrary {
    compiled: IndexMap<SkillId, SkillCompiled>,
    invalid: IndexMap<SkillId, SkillError>,
}

impl SkillLibrary {
    pub fn get(&self, id: &SkillId) -> Option<&SkillCompiled> {
        self.compiled.get(id)
    }

    pub fn invalid(&self, id: &SkillId) -> Option<&SkillError> {
        self.invalid.get(id)
    }

    pub fn insert_compiled(&mut self, skill: SkillCompiled) {
        self.invalid.shift_remove(&skill.id);
        self.compiled.insert(skill.id.clone(), skill);
    }

    pub fn mark_invalid(&mut self, id: SkillId, err: SkillError) {
        self.compiled.shift_remove(&id);
        self.invalid.insert(id, err);
    }

    pub fn replace_from_ron(
        &mut self,
        source: &str,
        registry: &SkillRegistry,
    ) -> Result<Vec<SkillId>, SkillError> {
        let defs = parse_skill_document(source)?;
        let mut updated = Vec::with_capacity(defs.len());
        for def in defs {
            let id = def.id.clone();
            match compile_skill(&def, registry) {
                Ok(compiled) => {
                    self.insert_compiled(compiled);
                    updated.push(id);
                }
                Err(err) => {
                    self.mark_invalid(id.clone(), err.clone());
                    return Err(err);
                }
            }
        }
        Ok(updated)
    }
}
