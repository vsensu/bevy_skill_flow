use crate::asset::{SkillLibrary, parse_skill_def};
use crate::compile::compile_skill;
use crate::dsl::{SkillCompiled, SkillDef, SkillId};
use crate::registry::{SkillError, SkillRegistry};
use bevy::prelude::{App, Plugin, Resource};
use indexmap::IndexMap;
use ron::ser::PrettyConfig;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Resource)]
pub struct SkillEditorConfig {
    pub skills_dir: PathBuf,
    pub autosave_on_compile_success: bool,
}

impl Default for SkillEditorConfig {
    fn default() -> Self {
        Self {
            skills_dir: PathBuf::from("assets/skills"),
            autosave_on_compile_success: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SkillEditorPlugin {
    config: SkillEditorConfig,
}

impl SkillEditorPlugin {
    pub fn new(config: SkillEditorConfig) -> Self {
        Self { config }
    }
}

impl Plugin for SkillEditorPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(self.config.clone())
            .init_resource::<SkillEditorState>();
    }
}

#[derive(Clone, Debug, Default, Resource)]
pub struct SkillEditorState {
    pub files: Vec<SkillFileEntry>,
    pub current_file: Option<PathBuf>,
    pub source: String,
    pub dirty: bool,
    pub diagnostics: EditorDiagnostics,
    pub current_def: Option<SkillDef>,
    pub preview_skill_id: Option<SkillId>,
    pub compiled_cache: IndexMap<SkillId, SkillCompiled>,
    pub last_io_error: Option<String>,
}

impl SkillEditorState {
    pub fn load_dir(
        &mut self,
        config: &SkillEditorConfig,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) -> io::Result<()> {
        fs::create_dir_all(&config.skills_dir)?;
        self.files = scan_skill_files(&config.skills_dir)?;
        self.compiled_cache.clear();
        self.diagnostics = EditorDiagnostics::default();

        for file in &mut self.files {
            let source = fs::read_to_string(&file.path)?;
            let result = validate_source(&source, registry);
            file.skill_id = result.skill_id.clone();
            file.parse_error = result.parse_error.clone();
            file.compile_error = result.compile_error.clone();
            if let Some(compiled) = result.compiled {
                library.insert_compiled(compiled.clone());
                self.compiled_cache.insert(compiled.id.clone(), compiled);
            } else if let (Some(id), Some(err)) = (result.skill_id, result.compile_error) {
                library.mark_invalid(id, err);
            }
        }

        if self.current_file.is_none()
            || !self
                .files
                .iter()
                .any(|entry| Some(&entry.path) == self.current_file.as_ref())
        {
            self.current_file = self.files.first().map(|entry| entry.path.clone());
        }

        if let Some(path) = self.current_file.clone() {
            self.open_file(path, registry, library)?;
        } else {
            self.source.clear();
            self.dirty = false;
            self.current_def = None;
            self.preview_skill_id = None;
            self.diagnostics = EditorDiagnostics::default();
        }

        Ok(())
    }

    pub fn open_file(
        &mut self,
        path: PathBuf,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) -> io::Result<()> {
        let source = fs::read_to_string(&path)?;
        self.current_file = Some(path);
        self.source = source;
        self.dirty = false;
        self.preview_skill_id = None;
        self.revalidate(registry, library);
        Ok(())
    }

    pub fn edit_source(
        &mut self,
        source: String,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) {
        if self.source == source {
            return;
        }
        self.source = source;
        self.dirty = true;
        self.revalidate(registry, library);
    }

    pub fn edit_def(
        &mut self,
        def: SkillDef,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) -> Result<(), SkillEditorError> {
        let source = serialize_skill_def(&def)?;
        self.edit_source(source, registry, library);
        Ok(())
    }

    pub fn revalidate(&mut self, registry: &SkillRegistry, library: &mut SkillLibrary) {
        let previous_id = self.current_def.as_ref().map(|def| def.id.clone());
        let result = validate_source(&self.source, registry);
        self.diagnostics.parse_error = result.parse_error;
        self.diagnostics.compile_error = result.compile_error.clone();
        self.diagnostics.skill_id = result.skill_id.clone();
        self.current_def = result.def;

        if let (Some(id), Some(new_id)) = (previous_id, self.diagnostics.skill_id.as_ref())
            && &id != new_id
        {
            library.remove(&id);
            self.compiled_cache.shift_remove(&id);
            if self.preview_skill_id.as_ref() == Some(&id) {
                self.preview_skill_id = None;
            }
        }

        if let Some(compiled) = result.compiled {
            library.insert_compiled(compiled.clone());
            self.preview_skill_id = Some(compiled.id.clone());
            self.compiled_cache.insert(compiled.id.clone(), compiled);
        } else if let (Some(id), Some(err)) =
            (self.diagnostics.skill_id.clone(), result.compile_error)
            && !self.compiled_cache.contains_key(&id)
        {
            if self.preview_skill_id.as_ref() == Some(&id) {
                self.preview_skill_id = None;
            }
            library.mark_invalid(id, err);
        }
    }

    pub fn save_current(&mut self, registry: &SkillRegistry) -> Result<PathBuf, SkillEditorError> {
        parse_skill_def(&self.source)?;
        let Some(path) = self.current_file.clone() else {
            return Err(SkillEditorError::NoCurrentFile);
        };
        fs::write(&path, &self.source)?;
        self.dirty = false;
        self.diagnostics = validate_source(&self.source, registry).into_diagnostics();
        self.update_current_file_diagnostics();
        Ok(path)
    }

    pub fn save_current_with_library(
        &mut self,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) -> Result<PathBuf, SkillEditorError> {
        parse_skill_def(&self.source)?;
        let Some(path) = self.current_file.clone() else {
            return Err(SkillEditorError::NoCurrentFile);
        };
        fs::write(&path, &self.source)?;
        self.dirty = false;
        self.revalidate(registry, library);
        self.update_current_file_diagnostics();
        Ok(path)
    }

    pub fn create_new(
        &mut self,
        config: &SkillEditorConfig,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) -> Result<PathBuf, SkillEditorError> {
        fs::create_dir_all(&config.skills_dir)?;
        let path = next_new_skill_path(&config.skills_dir);
        self.current_file = Some(path.clone());
        self.source = DEFAULT_SKILL_TEMPLATE.to_owned();
        self.dirty = true;
        self.revalidate(registry, library);
        self.files.push(SkillFileEntry::from_path(path.clone()));
        Ok(path)
    }

    pub fn delete_current(
        &mut self,
        config: &SkillEditorConfig,
        registry: &SkillRegistry,
        library: &mut SkillLibrary,
    ) -> Result<Option<PathBuf>, SkillEditorError> {
        let Some(path) = self.current_file.clone() else {
            return Ok(None);
        };
        if let Some(id) = self.current_def.as_ref().map(|def| def.id.clone()) {
            library.remove(&id);
            self.compiled_cache.shift_remove(&id);
            if self.preview_skill_id.as_ref() == Some(&id) {
                self.preview_skill_id = None;
            }
        }
        if path.exists() {
            fs::remove_file(&path)?;
        }
        self.load_dir(config, registry, library)?;
        Ok(Some(path))
    }

    pub fn selected_skill_id(&self) -> Option<SkillId> {
        self.current_def.as_ref().map(|def| def.id.clone())
    }

    pub fn selected_compiled(&self) -> Option<&SkillCompiled> {
        self.preview_skill_id
            .as_ref()
            .and_then(|id| self.compiled_cache.get(id))
            .or_else(|| {
                self.selected_skill_id()
                    .and_then(|id| self.compiled_cache.get(&id))
            })
    }

    pub fn selected_preview_skill_id(&self) -> Option<SkillId> {
        self.selected_compiled().map(|compiled| compiled.id.clone())
    }

    pub fn preview_is_stale(&self) -> bool {
        self.selected_compiled().is_some()
            && (self.diagnostics.parse_error.is_some() || self.diagnostics.compile_error.is_some())
    }

    fn update_current_file_diagnostics(&mut self) {
        let Some(path) = self.current_file.as_ref() else {
            return;
        };
        if let Some(file) = self.files.iter_mut().find(|file| &file.path == path) {
            file.skill_id = self.diagnostics.skill_id.clone();
            file.parse_error = self.diagnostics.parse_error.clone();
            file.compile_error = self.diagnostics.compile_error.clone();
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SkillFileEntry {
    pub path: PathBuf,
    pub skill_id: Option<SkillId>,
    pub parse_error: Option<String>,
    pub compile_error: Option<SkillError>,
}

impl SkillFileEntry {
    fn from_path(path: PathBuf) -> Self {
        Self {
            path,
            skill_id: None,
            parse_error: None,
            compile_error: None,
        }
    }

    pub fn label(&self) -> String {
        self.path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or("<unknown>")
            .to_owned()
    }
}

#[derive(Clone, Debug, Default)]
pub struct EditorDiagnostics {
    pub skill_id: Option<SkillId>,
    pub parse_error: Option<String>,
    pub compile_error: Option<SkillError>,
}

#[derive(Clone, Debug)]
struct ValidationResult {
    skill_id: Option<SkillId>,
    def: Option<SkillDef>,
    compiled: Option<SkillCompiled>,
    parse_error: Option<String>,
    compile_error: Option<SkillError>,
}

impl ValidationResult {
    fn into_diagnostics(self) -> EditorDiagnostics {
        EditorDiagnostics {
            skill_id: self.skill_id,
            parse_error: self.parse_error,
            compile_error: self.compile_error,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SkillEditorError {
    #[error("{0}")]
    Skill(#[from] SkillError),
    #[error("{0}")]
    RonSerialize(#[from] ron::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("no skill file is selected")]
    NoCurrentFile,
}

pub fn scan_skill_files(skills_dir: &Path) -> io::Result<Vec<SkillFileEntry>> {
    let mut files = Vec::new();
    if !skills_dir.exists() {
        return Ok(files);
    }
    for entry in fs::read_dir(skills_dir)? {
        let path = entry?.path();
        if path.is_file() && is_skill_file(&path) {
            files.push(SkillFileEntry::from_path(path));
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

pub fn is_skill_file(path: &Path) -> bool {
    path.file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.ends_with(".skill.ron"))
}

pub fn next_new_skill_path(skills_dir: &Path) -> PathBuf {
    let base = skills_dir.join("new_skill.skill.ron");
    if !base.exists() {
        return base;
    }
    for index in 2.. {
        let candidate = skills_dir.join(format!("new_skill_{index}.skill.ron"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search should always find a free skill path")
}

fn validate_source(source: &str, registry: &SkillRegistry) -> ValidationResult {
    match parse_skill_def(source) {
        Ok(def) => {
            let skill_id = Some(def.id.clone());
            match compile_skill(&def, registry) {
                Ok(compiled) => ValidationResult {
                    skill_id,
                    def: Some(def),
                    compiled: Some(compiled),
                    parse_error: None,
                    compile_error: None,
                },
                Err(err) => ValidationResult {
                    skill_id,
                    def: Some(def),
                    compiled: None,
                    parse_error: None,
                    compile_error: Some(err),
                },
            }
        }
        Err(err) => ValidationResult {
            skill_id: None,
            def: None,
            compiled: None,
            parse_error: Some(err.to_string()),
            compile_error: None,
        },
    }
}

pub fn serialize_skill_def(def: &SkillDef) -> Result<String, ron::Error> {
    let mut source = ron::ser::to_string_pretty(
        def,
        PrettyConfig::default()
            .struct_names(true)
            .new_line("\n".to_owned())
            .indentor("  ".to_owned()),
    )?;
    source.push('\n');
    Ok(source)
}

pub const DEFAULT_SKILL_TEMPLATE: &str = r#"Skill(
  id: "new_skill",
  tags: ["spell"],
  params: {
    "damage": 18.0,
  },
  body: Sequence([
    Emit("skill_cast", { "label": "New Skill" }),
    Action("spawn_projectile", {
      "kind": "bolt",
      "count": 1.0,
      "damage": Expr("stat.damage"),
      "speed": 560.0,
      "radius": 12.0,
    }),
  ]),
)
"#;
