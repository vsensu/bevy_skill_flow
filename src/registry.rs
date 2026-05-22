use crate::dsl::{SkillArgs, SkillCompiled, SkillContext, SkillDef, SkillPlan};
use bevy::prelude::{Resource, World};
use indexmap::IndexMap;
use std::sync::Arc;
use thiserror::Error;

pub type SkillResult = Result<(), SkillError>;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum SkillError {
    #[error("RON parse failed: {0}")]
    Ron(String),
    #[error("skill `{0}` is invalid: {1}")]
    InvalidSkill(String, String),
    #[error("action `{0}` is not registered")]
    UnknownAction(String),
    #[error("modifier `{0}` is not registered")]
    UnknownModifier(String),
    #[error("cast model `{0}` is not registered")]
    UnknownCastModel(String),
    #[error("expression `{expr}` failed: {message}")]
    Expr { expr: String, message: String },
    #[error("runtime error: {0}")]
    Runtime(String),
}

pub trait SkillAction: Send + Sync + 'static {
    fn validate(&self, args: &SkillArgs, registry: &SkillRegistry) -> Result<(), SkillError>;
    fn execute(&self, world: &mut World, ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult;
}

pub trait SkillModifier: Send + Sync + 'static {
    fn applies(&self, skill: &SkillCompiled) -> bool;
    fn apply(&self, plan: &mut SkillPlan, ctx: &SkillContext) -> Result<(), SkillError>;
}

pub trait CastModel: Send + Sync + 'static {
    fn compile(&self, skill: &SkillDef, registry: &SkillRegistry) -> Result<SkillPlan, SkillError>;
}

#[derive(Resource, Clone)]
pub struct SkillRegistry {
    actions: IndexMap<String, Arc<dyn SkillAction>>,
    modifiers: IndexMap<String, Arc<dyn SkillModifier>>,
    cast_models: IndexMap<String, Arc<dyn CastModel>>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::with_core()
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            actions: IndexMap::new(),
            modifiers: IndexMap::new(),
            cast_models: IndexMap::new(),
        }
    }

    pub fn with_core() -> Self {
        let mut registry = Self::new();
        registry.register_cast_model("direct", crate::compile::DirectCastModel);
        registry
    }

    pub fn register_skill_action<A>(&mut self, id: impl Into<String>, action: A) -> &mut Self
    where
        A: SkillAction,
    {
        self.actions.insert(id.into(), Arc::new(action));
        self
    }

    pub fn register_skill_modifier<M>(&mut self, id: impl Into<String>, modifier: M) -> &mut Self
    where
        M: SkillModifier,
    {
        self.modifiers.insert(id.into(), Arc::new(modifier));
        self
    }

    pub fn register_cast_model<C>(&mut self, id: impl Into<String>, cast_model: C) -> &mut Self
    where
        C: CastModel,
    {
        self.cast_models.insert(id.into(), Arc::new(cast_model));
        self
    }

    pub fn action(&self, id: &str) -> Option<Arc<dyn SkillAction>> {
        self.actions.get(id).cloned()
    }

    pub fn modifier(&self, id: &str) -> Option<Arc<dyn SkillModifier>> {
        self.modifiers.get(id).cloned()
    }

    pub fn cast_model(&self, id: &str) -> Option<Arc<dyn CastModel>> {
        self.cast_models.get(id).cloned()
    }

    pub fn has_action(&self, id: &str) -> bool {
        self.actions.contains_key(id)
    }

    pub fn has_modifier(&self, id: &str) -> bool {
        self.modifiers.contains_key(id)
    }

    pub fn has_cast_model(&self, id: &str) -> bool {
        self.cast_models.contains_key(id)
    }
}
