use crate::compile::SkillLowerContext;
use crate::dsl::{SkillArgs, SkillCompileContext, SkillDef, SkillNode, SkillTypedArgs};
use bevy::prelude::Resource;
pub use bevy_skill_ecs::SkillError;
use indexmap::{IndexMap, IndexSet};
use serde::de::DeserializeOwned;
use std::sync::Arc;

pub trait SkillModifier: Send + Sync + 'static {
    fn applies(&self, tags: &IndexSet<String>) -> bool;
    fn apply(&self, params: &mut SkillArgs, ctx: &SkillCompileContext) -> Result<(), SkillError>;
}

pub trait CastModel: Send + Sync + 'static {
    fn compile(&self, skill: &SkillDef, registry: &SkillRegistry) -> Result<SkillNode, SkillError>;
}

pub trait SkillDslNode: Send + Sync + 'static {
    const NAME: &'static str;
    type Args: DeserializeOwned;

    fn lower(args: Self::Args, ctx: &mut SkillLowerContext<'_>) -> Result<SkillNode, SkillError>;
}

pub trait ErasedSkillDslNode: Send + Sync + 'static {
    fn lower(
        &self,
        args: &SkillTypedArgs,
        ctx: &mut SkillLowerContext<'_>,
    ) -> Result<SkillNode, SkillError>;
}

#[derive(Clone, Debug)]
struct SkillDslNodeWrapper<T>(std::marker::PhantomData<T>);

impl<T> Default for SkillDslNodeWrapper<T> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<T> ErasedSkillDslNode for SkillDslNodeWrapper<T>
where
    T: SkillDslNode,
{
    fn lower(
        &self,
        args: &SkillTypedArgs,
        ctx: &mut SkillLowerContext<'_>,
    ) -> Result<SkillNode, SkillError> {
        let args = args.deserialize_args::<T::Args>().map_err(|err| {
            SkillError::InvalidSkill(T::NAME.to_owned(), format!("typed DSL args invalid: {err}"))
        })?;
        T::lower(args, ctx)
    }
}

#[derive(Resource, Clone)]
pub struct SkillRegistry {
    modifiers: IndexMap<String, Arc<dyn SkillModifier>>,
    cast_models: IndexMap<String, Arc<dyn CastModel>>,
    dsl_nodes: IndexMap<String, Arc<dyn ErasedSkillDslNode>>,
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::with_core()
    }
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            modifiers: IndexMap::new(),
            cast_models: IndexMap::new(),
            dsl_nodes: IndexMap::new(),
        }
    }

    pub fn with_core() -> Self {
        let mut registry = Self::new();
        registry.register_cast_model("direct", crate::compile::DirectCastModel);
        registry
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

    pub fn register_dsl_node<T>(&mut self) -> &mut Self
    where
        T: SkillDslNode,
    {
        self.dsl_nodes.insert(
            T::NAME.to_owned(),
            Arc::new(SkillDslNodeWrapper::<T>::default()),
        );
        self
    }

    pub fn modifier(&self, id: &str) -> Option<Arc<dyn SkillModifier>> {
        self.modifiers.get(id).cloned()
    }

    pub fn cast_model(&self, id: &str) -> Option<Arc<dyn CastModel>> {
        self.cast_models.get(id).cloned()
    }

    pub fn dsl_node(&self, name: &str) -> Option<Arc<dyn ErasedSkillDslNode>> {
        self.dsl_nodes.get(name).cloned()
    }

    pub fn has_modifier(&self, id: &str) -> bool {
        self.modifiers.contains_key(id)
    }

    pub fn has_cast_model(&self, id: &str) -> bool {
        self.cast_models.contains_key(id)
    }

    pub fn has_dsl_node(&self, name: &str) -> bool {
        self.dsl_nodes.contains_key(name)
    }
}
