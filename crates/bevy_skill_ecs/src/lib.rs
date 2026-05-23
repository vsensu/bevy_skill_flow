//! ECS-facing skill runtime protocol and graph model.
//!
//! This crate intentionally contains no game combat semantics. Concrete
//! gameplay effects are represented by extension nodes/components supplied by a
//! host game or a higher-level DSL.

use bevy::prelude::{
    App, Commands, Component, Entity, IntoScheduleConfigs, Message, Plugin, Reflect, Resource,
    SystemSet, World,
};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod expr;
mod runtime;

pub use expr::*;
pub use runtime::*;

pub type SkillParams = IndexMap<String, SkillValue>;
pub type SkillTags = IndexSet<String>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Reflect, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillId(pub String);

impl SkillId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl From<&str> for SkillId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

impl std::fmt::Display for SkillId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SkillGraphHandle(pub String);

impl From<SkillId> for SkillGraphHandle {
    fn from(value: SkillId) -> Self {
        Self(value.0)
    }
}

#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    Eq,
    PartialEq,
    Ord,
    PartialOrd,
    Hash,
    Reflect,
    Serialize,
    Deserialize,
)]
#[serde(transparent)]
pub struct SkillNodeId(pub u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillValue {
    Special(SkillSpecialValue),
    Map(IndexMap<String, SkillValue>),
    List(Vec<SkillValue>),
    Number(f64),
    Bool(bool),
    String(String),
    Null,
}

impl Default for SkillValue {
    fn default() -> Self {
        Self::Null
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkillSpecialValue {
    Expr(String),
    Ref(String),
    Tag(String),
    Stat(String),
}

#[derive(Clone, Debug, PartialEq, Reflect)]
pub struct SkillExpr(pub String);

impl SkillExpr {
    pub fn new(expr: impl Into<String>) -> Self {
        Self(expr.into())
    }
}

impl Serialize for SkillExpr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_newtype_variant("SkillExpr", 0, "Expr", &self.0)
    }
}

impl<'de> Deserialize<'de> for SkillExpr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match SkillValue::deserialize(deserializer)? {
            SkillValue::Special(SkillSpecialValue::Expr(expr)) | SkillValue::String(expr) => {
                Ok(Self(expr))
            }
            SkillValue::List(values) if values.len() == 1 => match values.into_iter().next() {
                Some(SkillValue::String(expr)) => Ok(Self(expr)),
                other => Err(serde::de::Error::custom(format!(
                    "expected Expr(\"...\"), got `{other:?}`"
                ))),
            },
            other => Err(serde::de::Error::custom(format!(
                "expected a string expression or Expr(\"...\"), got `{other:?}`"
            ))),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillGraph {
    pub id: SkillId,
    #[serde(default)]
    pub tags: SkillTags,
    #[serde(default)]
    pub params: SkillParams,
    #[serde(default)]
    pub requirements: Vec<SkillRequirement>,
    pub root: Option<SkillNodeId>,
    #[serde(default)]
    pub nodes: Vec<SkillGraphNode>,
}

impl SkillGraph {
    pub fn new(id: SkillId) -> Self {
        Self {
            id,
            tags: SkillTags::new(),
            params: SkillParams::new(),
            requirements: Vec::new(),
            root: None,
            nodes: Vec::new(),
        }
    }

    pub fn add_node(&mut self, kind: SkillGraphNodeKind) -> SkillNodeId {
        let id = SkillNodeId(self.nodes.len() as u32);
        self.nodes.push(SkillGraphNode::new(id, kind));
        id
    }

    pub fn node(&self, id: SkillNodeId) -> Option<&SkillGraphNode> {
        self.nodes.get(id.0 as usize).filter(|node| node.id == id)
    }

    pub fn node_mut(&mut self, id: SkillNodeId) -> Option<&mut SkillGraphNode> {
        self.nodes
            .get_mut(id.0 as usize)
            .filter(|node| node.id == id)
    }

    pub fn set_root(&mut self, id: SkillNodeId) {
        self.root = Some(id);
    }

    pub fn push_child(
        &mut self,
        parent: SkillNodeId,
        slot: impl Into<String>,
        child: SkillNodeId,
    ) -> Result<(), SkillGraphError> {
        self.ensure_node(parent)?;
        self.ensure_node(child)?;
        self.node_mut(parent)
            .expect("validated parent")
            .children
            .entry(slot.into())
            .or_default()
            .push(child);
        Ok(())
    }

    pub fn push_payload(
        &mut self,
        parent: SkillNodeId,
        slot: impl Into<String>,
        child: SkillNodeId,
    ) -> Result<(), SkillGraphError> {
        self.ensure_node(parent)?;
        self.ensure_node(child)?;
        self.node_mut(parent)
            .expect("validated parent")
            .payloads
            .entry(slot.into())
            .or_default()
            .push(child);
        Ok(())
    }

    pub fn validate(&self) -> Result<(), SkillGraphError> {
        let root = self.root.ok_or(SkillGraphError::MissingRoot)?;
        self.ensure_node(root)?;
        for node in &self.nodes {
            for child in node.children.values().flatten() {
                self.ensure_node(*child)?;
            }
            for payload in node.payloads.values().flatten() {
                self.ensure_node(*payload)?;
            }
        }
        let mut visiting = IndexSet::new();
        let mut visited = IndexSet::new();
        self.visit_acyclic(root, &mut visiting, &mut visited)
    }

    fn ensure_node(&self, id: SkillNodeId) -> Result<(), SkillGraphError> {
        match self.node(id) {
            Some(_) => Ok(()),
            None => Err(SkillGraphError::MissingNode(id)),
        }
    }

    fn visit_acyclic(
        &self,
        id: SkillNodeId,
        visiting: &mut IndexSet<SkillNodeId>,
        visited: &mut IndexSet<SkillNodeId>,
    ) -> Result<(), SkillGraphError> {
        if visited.contains(&id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(SkillGraphError::Cycle { at: id });
        }
        let node = self.node(id).ok_or(SkillGraphError::MissingNode(id))?;
        for next in node
            .children
            .values()
            .chain(node.payloads.values())
            .flatten()
        {
            self.visit_acyclic(*next, visiting, visited)?;
        }
        visiting.shift_remove(&id);
        visited.insert(id);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkillRequirement {
    Cost { resource: String, amount: SkillExpr },
    Cooldown { seconds: SkillExpr },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillCompiled {
    pub id: SkillId,
    pub tags: SkillTags,
    pub cast_model: String,
    pub graph: SkillGraph,
}

#[derive(Resource, Default, Clone, Debug)]
pub struct SkillLibrary {
    compiled: IndexMap<SkillId, Entity>,
    pending: IndexMap<SkillId, SkillCompiled>,
    orphaned: Vec<Entity>,
    invalid: IndexMap<SkillId, String>,
}

impl SkillLibrary {
    pub fn get(&self, id: &SkillId) -> Option<Entity> {
        self.get_entity(id)
            .or_else(|| self.pending.contains_key(id).then_some(Entity::PLACEHOLDER))
    }

    pub fn get_entity(&self, id: &SkillId) -> Option<Entity> {
        self.compiled.get(id).copied()
    }

    pub fn invalid(&self, id: &SkillId) -> Option<&String> {
        self.invalid.get(id)
    }

    pub fn insert_entity(&mut self, id: SkillId, entity: Entity) -> Option<Entity> {
        self.invalid.shift_remove(&id);
        self.pending.shift_remove(&id);
        self.compiled.insert(id, entity)
    }

    pub fn insert_compiled(&mut self, skill: SkillCompiled) {
        self.invalid.shift_remove(&skill.id);
        self.pending.insert(skill.id.clone(), skill);
    }

    pub fn remove(&mut self, id: &SkillId) -> Option<Entity> {
        let entity = self.detach_entity(id);
        if let Some(entity) = entity {
            self.orphaned.push(entity);
        }
        entity
    }

    fn detach_entity(&mut self, id: &SkillId) -> Option<Entity> {
        self.invalid.shift_remove(id);
        self.pending.shift_remove(id);
        self.compiled.shift_remove(id)
    }

    pub fn compiled_ids(&self) -> impl Iterator<Item = &SkillId> {
        self.compiled.keys().chain(
            self.pending
                .keys()
                .filter(|id| !self.compiled.contains_key(*id)),
        )
    }

    pub fn compiled_len(&self) -> usize {
        self.compiled.len()
            + self
                .pending
                .keys()
                .filter(|id| !self.compiled.contains_key(*id))
                .count()
    }

    pub fn mark_invalid(&mut self, id: SkillId, err: impl Into<String>) {
        if let Some(entity) = self.compiled.shift_remove(&id) {
            self.orphaned.push(entity);
        }
        self.pending.shift_remove(&id);
        self.invalid.insert(id, err.into());
    }

    fn take_pending(&mut self) -> Vec<SkillCompiled> {
        self.pending.drain(..).map(|(_, skill)| skill).collect()
    }

    fn take_orphaned(&mut self) -> Vec<Entity> {
        std::mem::take(&mut self.orphaned)
    }
}

pub fn materialize_pending_compiled_skills(
    mut commands: Commands,
    mut library: bevy::prelude::ResMut<SkillLibrary>,
) {
    for entity in library.take_orphaned() {
        commands.entity(entity).despawn();
    }
    for compiled in library.take_pending() {
        replace_compiled_skill(&mut commands, &mut library, compiled);
    }
}

pub fn spawn_compiled_skill(commands: &mut Commands, compiled: SkillCompiled) -> Entity {
    let skill_entity = commands
        .spawn((
            CompiledSkill {
                id: compiled.id.clone(),
                cast_model: compiled.cast_model.clone(),
            },
            CompiledSkillTags(compiled.tags.clone()),
            CompiledSkillParams(compiled.graph.params.clone()),
            SkillRequirements(compiled.graph.requirements.clone()),
        ))
        .id();
    spawn_compiled_skill_nodes(commands, skill_entity, &compiled.graph);
    skill_entity
}

pub fn replace_compiled_skill(
    commands: &mut Commands,
    library: &mut SkillLibrary,
    compiled: SkillCompiled,
) -> Entity {
    if let Some(old) = library.detach_entity(&compiled.id) {
        commands.entity(old).despawn();
    }
    let id = compiled.id.clone();
    let skill_entity = spawn_compiled_skill(commands, compiled);
    library.insert_entity(id, skill_entity);
    skill_entity
}

pub fn despawn_compiled_skill(
    commands: &mut Commands,
    library: &mut SkillLibrary,
    id: &SkillId,
) -> Option<Entity> {
    let entity = library.detach_entity(id)?;
    commands.entity(entity).despawn();
    Some(entity)
}

pub fn mark_invalid_skill(
    commands: &mut Commands,
    library: &mut SkillLibrary,
    id: SkillId,
    err: impl Into<String>,
) {
    if let Some(old) = library.detach_entity(&id) {
        commands.entity(old).despawn();
    }
    library.mark_invalid(id, err);
}

pub fn spawn_compiled_skill_world(world: &mut World, compiled: SkillCompiled) -> Entity {
    let skill_entity = world
        .spawn((
            CompiledSkill {
                id: compiled.id.clone(),
                cast_model: compiled.cast_model.clone(),
            },
            CompiledSkillTags(compiled.tags.clone()),
            CompiledSkillParams(compiled.graph.params.clone()),
            SkillRequirements(compiled.graph.requirements.clone()),
        ))
        .id();
    spawn_compiled_skill_nodes_world(world, skill_entity, &compiled.graph);
    skill_entity
}

pub fn replace_compiled_skill_world(
    world: &mut World,
    library: &mut SkillLibrary,
    compiled: SkillCompiled,
) -> Entity {
    if let Some(old) = library.detach_entity(&compiled.id)
        && let Ok(entity) = world.get_entity_mut(old)
    {
        entity.despawn();
    }
    let id = compiled.id.clone();
    let skill_entity = spawn_compiled_skill_world(world, compiled);
    library.insert_entity(id, skill_entity);
    skill_entity
}

pub fn despawn_compiled_skill_world(
    world: &mut World,
    library: &mut SkillLibrary,
    id: &SkillId,
) -> Option<Entity> {
    let entity = library.detach_entity(id)?;
    if let Ok(entity_mut) = world.get_entity_mut(entity) {
        entity_mut.despawn();
    }
    Some(entity)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillGraphNode {
    pub id: SkillNodeId,
    pub kind: SkillGraphNodeKind,
    #[serde(default)]
    pub children: IndexMap<String, Vec<SkillNodeId>>,
    #[serde(default)]
    pub payloads: IndexMap<String, Vec<SkillNodeId>>,
}

impl SkillGraphNode {
    pub fn new(id: SkillNodeId, kind: SkillGraphNodeKind) -> Self {
        Self {
            id,
            kind,
            children: IndexMap::new(),
            payloads: IndexMap::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkillGraphNodeKind {
    Sequence,
    Parallel,
    Delay {
        seconds: SkillExpr,
    },
    Repeat {
        times: Option<SkillExpr>,
        duration: Option<SkillExpr>,
        interval: Option<SkillExpr>,
    },
    If {
        condition: SkillExpr,
    },
    WaitEvent {
        event: String,
    },
    EmitSkillEvent {
        event: String,
        payload: SkillParams,
    },
    SetSkillVar {
        name: String,
        value: SkillValue,
    },
    WithSkillContext,
    Extension {
        constructor: String,
        args: SkillParams,
    },
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum SkillGraphError {
    #[error("skill graph has no root node")]
    MissingRoot,
    #[error("skill graph references missing node `{0:?}`")]
    MissingNode(SkillNodeId),
    #[error("skill graph contains a cycle at node `{at:?}`")]
    Cycle { at: SkillNodeId },
}

fn spawn_compiled_skill_nodes(commands: &mut Commands, skill_entity: Entity, graph: &SkillGraph) {
    let mut entities = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let entity = commands
            .spawn((SkillGraphNodeRef { id: node.id }, SkillNodeOf(skill_entity)))
            .id();
        insert_node_kind_commands(commands, entity, &node.kind);
        entities.push((node.id, entity));
    }

    insert_graph_relationships_commands(commands, skill_entity, graph, &entities);
}

fn spawn_compiled_skill_nodes_world(world: &mut World, skill_entity: Entity, graph: &SkillGraph) {
    let mut entities = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let entity = world
            .spawn((SkillGraphNodeRef { id: node.id }, SkillNodeOf(skill_entity)))
            .id();
        insert_node_kind_world(world, entity, &node.kind);
        entities.push((node.id, entity));
    }

    insert_graph_relationships_world(world, skill_entity, graph, &entities);
}

fn insert_node_kind_commands(commands: &mut Commands, entity: Entity, kind: &SkillGraphNodeKind) {
    match kind {
        SkillGraphNodeKind::Sequence => {
            commands.entity(entity).insert(Sequence);
        }
        SkillGraphNodeKind::Parallel => {
            commands.entity(entity).insert(Parallel);
        }
        SkillGraphNodeKind::Delay { seconds } => {
            commands.entity(entity).insert(Delay {
                seconds: seconds.clone(),
            });
        }
        SkillGraphNodeKind::Repeat {
            times,
            duration,
            interval,
        } => {
            commands.entity(entity).insert(Repeat {
                times: times.clone(),
                duration: duration.clone(),
                interval: interval.clone(),
            });
        }
        SkillGraphNodeKind::If { condition } => {
            commands.entity(entity).insert(If {
                condition: condition.clone(),
            });
        }
        SkillGraphNodeKind::WaitEvent { event } => {
            commands.entity(entity).insert(WaitEvent {
                event: event.clone(),
            });
        }
        SkillGraphNodeKind::EmitSkillEvent { event, payload } => {
            commands.entity(entity).insert(EmitSkillEvent {
                event: event.clone(),
                payload: payload.clone(),
            });
        }
        SkillGraphNodeKind::SetSkillVar { name, value } => {
            commands.entity(entity).insert(SetSkillVar {
                name: name.clone(),
                value: value.clone(),
            });
        }
        SkillGraphNodeKind::WithSkillContext => {
            commands.entity(entity).insert(WithSkillContext);
        }
        SkillGraphNodeKind::Extension { constructor, args } => {
            commands.entity(entity).insert(SkillExtension {
                constructor: constructor.clone(),
                args: args.clone(),
            });
        }
    }
}

fn insert_node_kind_world(world: &mut World, entity: Entity, kind: &SkillGraphNodeKind) {
    match kind {
        SkillGraphNodeKind::Sequence => {
            world.entity_mut(entity).insert(Sequence);
        }
        SkillGraphNodeKind::Parallel => {
            world.entity_mut(entity).insert(Parallel);
        }
        SkillGraphNodeKind::Delay { seconds } => {
            world.entity_mut(entity).insert(Delay {
                seconds: seconds.clone(),
            });
        }
        SkillGraphNodeKind::Repeat {
            times,
            duration,
            interval,
        } => {
            world.entity_mut(entity).insert(Repeat {
                times: times.clone(),
                duration: duration.clone(),
                interval: interval.clone(),
            });
        }
        SkillGraphNodeKind::If { condition } => {
            world.entity_mut(entity).insert(If {
                condition: condition.clone(),
            });
        }
        SkillGraphNodeKind::WaitEvent { event } => {
            world.entity_mut(entity).insert(WaitEvent {
                event: event.clone(),
            });
        }
        SkillGraphNodeKind::EmitSkillEvent { event, payload } => {
            world.entity_mut(entity).insert(EmitSkillEvent {
                event: event.clone(),
                payload: payload.clone(),
            });
        }
        SkillGraphNodeKind::SetSkillVar { name, value } => {
            world.entity_mut(entity).insert(SetSkillVar {
                name: name.clone(),
                value: value.clone(),
            });
        }
        SkillGraphNodeKind::WithSkillContext => {
            world.entity_mut(entity).insert(WithSkillContext);
        }
        SkillGraphNodeKind::Extension { constructor, args } => {
            world.entity_mut(entity).insert(SkillExtension {
                constructor: constructor.clone(),
                args: args.clone(),
            });
        }
    }
}

fn insert_graph_relationships_commands(
    commands: &mut Commands,
    skill_entity: Entity,
    graph: &SkillGraph,
    entities: &[(SkillNodeId, Entity)],
) {
    let entity_for = |id: SkillNodeId| {
        entities
            .iter()
            .find_map(|(node_id, entity)| (*node_id == id).then_some(*entity))
    };

    if let Some(root) = graph.root.and_then(entity_for) {
        commands.entity(root).insert(SkillRootOf {
            graph: skill_entity,
        });
    }

    for node in &graph.nodes {
        let Some(parent) = entity_for(node.id) else {
            continue;
        };
        for (slot, children) in &node.children {
            for (order, child) in children.iter().enumerate() {
                if let Some(child_entity) = entity_for(*child) {
                    commands.entity(child_entity).insert(SkillChildOf {
                        parent,
                        slot: slot.clone(),
                        order: order as u32,
                    });
                }
            }
        }
        for (slot, payloads) in &node.payloads {
            for (order, payload) in payloads.iter().enumerate() {
                if let Some(payload_entity) = entity_for(*payload) {
                    commands.entity(payload_entity).insert(SkillPayloadOf {
                        parent,
                        slot: slot.clone(),
                        order: order as u32,
                    });
                }
            }
        }
    }
}

fn insert_graph_relationships_world(
    world: &mut World,
    skill_entity: Entity,
    graph: &SkillGraph,
    entities: &[(SkillNodeId, Entity)],
) {
    let entity_for = |id: SkillNodeId| {
        entities
            .iter()
            .find_map(|(node_id, entity)| (*node_id == id).then_some(*entity))
    };

    if let Some(root) = graph.root.and_then(entity_for) {
        world.entity_mut(root).insert(SkillRootOf {
            graph: skill_entity,
        });
    }

    for node in &graph.nodes {
        let Some(parent) = entity_for(node.id) else {
            continue;
        };
        for (slot, children) in &node.children {
            for (order, child) in children.iter().enumerate() {
                if let Some(child_entity) = entity_for(*child) {
                    world.entity_mut(child_entity).insert(SkillChildOf {
                        parent,
                        slot: slot.clone(),
                        order: order as u32,
                    });
                }
            }
        }
        for (slot, payloads) in &node.payloads {
            for (order, payload) in payloads.iter().enumerate() {
                if let Some(payload_entity) = entity_for(*payload) {
                    world.entity_mut(payload_entity).insert(SkillPayloadOf {
                        parent,
                        slot: slot.clone(),
                        order: order as u32,
                    });
                }
            }
        }
    }
}

#[derive(Component, Clone, Debug, Default, Reflect, PartialEq)]
pub struct Sequence;

#[derive(Component, Clone, Debug, Default, Reflect, PartialEq)]
pub struct Parallel;

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct Delay {
    pub seconds: SkillExpr,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct Repeat {
    pub times: Option<SkillExpr>,
    pub duration: Option<SkillExpr>,
    pub interval: Option<SkillExpr>,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct If {
    pub condition: SkillExpr,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct WaitEvent {
    pub event: String,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct EmitSkillEvent {
    pub event: String,
    #[reflect(ignore)]
    pub payload: SkillParams,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SetSkillVar {
    pub name: String,
    #[reflect(ignore)]
    pub value: SkillValue,
}

#[derive(Component, Clone, Debug, Default, Reflect, PartialEq)]
pub struct WithSkillContext;

#[derive(Component, Clone, Debug, PartialEq)]
pub struct CompiledSkill {
    pub id: SkillId,
    pub cast_model: String,
}

#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct CompiledSkillTags(pub SkillTags);

#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct CompiledSkillParams(pub SkillParams);

#[derive(Component, Clone, Debug, Default, PartialEq)]
pub struct SkillRequirements(pub Vec<SkillRequirement>);

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillGraphNodeRef {
    pub id: SkillNodeId,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship(relationship_target = SkillNodesOf)]
pub struct SkillNodeOf(#[relationship] pub Entity);

#[derive(Component, Clone, Debug, Default, Reflect, PartialEq)]
#[relationship_target(relationship = SkillNodeOf, linked_spawn)]
pub struct SkillNodesOf(Vec<Entity>);

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship(relationship_target = SkillRoots)]
pub struct SkillRootOf {
    #[relationship]
    pub graph: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship_target(relationship = SkillRootOf)]
pub struct SkillRoots(Vec<Entity>);

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship(relationship_target = SkillChildren)]
pub struct SkillChildOf {
    #[relationship]
    pub parent: Entity,
    pub slot: String,
    pub order: u32,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship_target(relationship = SkillChildOf)]
pub struct SkillChildren(Vec<Entity>);

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship(relationship_target = SkillPayloads)]
pub struct SkillPayloadOf {
    #[relationship]
    pub parent: Entity,
    pub slot: String,
    pub order: u32,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship_target(relationship = SkillPayloadOf)]
pub struct SkillPayloads(Vec<Entity>);

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillExtension {
    pub constructor: String,
    #[reflect(ignore)]
    pub args: SkillParams,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship(relationship_target = SkillExecutions)]
pub struct ExecutionOfSkill {
    #[relationship]
    pub skill: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
#[relationship_target(relationship = ExecutionOfSkill, linked_spawn)]
pub struct SkillExecutions(Vec<Entity>);

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct ExecutionOwnedBy {
    pub owner: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillObjectFromExecution {
    pub execution: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillAffectsTarget {
    pub target: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillExecution {
    pub id: u64,
    pub skill: SkillId,
    pub state: ExecutionState,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct ExecutionCursor {
    pub queue: Vec<PendingSkillNode>,
    pub step_budget_remaining: u32,
}

#[derive(Clone, Debug, Reflect, PartialEq)]
pub struct ExecutionContext {
    pub execution_id: u64,
    pub caster: Option<Entity>,
    pub target: Option<Entity>,
}

#[derive(Clone, Debug, Reflect, PartialEq)]
pub enum ExecutionState {
    Running,
    Waiting,
    Finished,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug, Reflect, PartialEq)]
pub struct PendingSkillNode {
    pub node: SkillNodeId,
}

#[derive(Message, Clone, Debug)]
pub struct SkillCastRequest {
    pub skill: SkillId,
    pub caster: Entity,
    pub target: Option<Entity>,
}

#[derive(Message, Clone, Debug)]
pub struct SkillCastAccepted {
    pub skill: SkillId,
    pub execution_id: u64,
    pub caster: Entity,
    pub target: Option<Entity>,
}

#[derive(Message, Clone, Debug)]
pub struct SkillCastRejected {
    pub skill: SkillId,
    pub caster: Entity,
    pub target: Option<Entity>,
    pub message: String,
}

#[derive(Message, Clone, Debug)]
pub struct SkillExecutionFinished {
    pub skill: SkillId,
    pub execution_id: u64,
    pub caster: Entity,
    pub target: Option<Entity>,
}

#[derive(Message, Clone, Debug)]
pub struct SkillEffectRequest {
    pub request_id: u64,
    pub execution_id: u64,
    pub source: Option<Entity>,
    pub target: Option<Entity>,
    pub kind: String,
    pub payload: SkillParams,
    pub mode: SettlementMode,
}

#[derive(Message, Clone, Debug)]
pub struct SkillEffectResolved {
    pub request_id: u64,
    pub execution_id: u64,
    pub source: Option<Entity>,
    pub target: Option<Entity>,
    pub kind: String,
    pub payload: SkillParams,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SettlementMode {
    Sync,
    Request,
    Await,
}

#[derive(Debug, Error, Clone, PartialEq)]
pub enum SkillExecutionError {
    #[error("step budget exhausted after {steps} steps")]
    StepBudgetExhausted { steps: u32 },
    #[error("runtime error: {0}")]
    Runtime(String),
}

#[derive(SystemSet, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SkillRuntimeSet {
    Asset,
    Request,
    Validate,
    Execute,
    Effect,
    Message,
    Trigger,
    Cleanup,
}

#[derive(Resource, Clone, Debug)]
pub struct SkillRuntimeConfig {
    pub step_budget: u32,
}

impl Default for SkillRuntimeConfig {
    fn default() -> Self {
        Self { step_budget: 1024 }
    }
}

#[derive(Debug, Default)]
pub struct SkillEcsPlugin;

impl Plugin for SkillEcsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SkillRuntimeConfig>()
            .init_resource::<SkillLibrary>()
            .init_resource::<SkillActionRegistry>()
            .init_resource::<SkillRuntimeCounters>()
            .init_resource::<SkillResourcePools>()
            .init_resource::<SkillCooldowns>()
            .add_message::<SkillCastRequest>()
            .add_message::<SkillCastAccepted>()
            .add_message::<SkillCastRejected>()
            .add_message::<SkillExecutionFinished>()
            .add_message::<SkillExecutionFailed>()
            .add_message::<SkillRuntimeSignal>()
            .add_message::<SkillIntent>()
            .add_message::<SkillEffectRequest>()
            .add_message::<SkillEffectResolved>()
            .add_observer(skill_observer_trigger_bridge)
            .configure_sets(
                bevy::prelude::FixedUpdate,
                (
                    SkillRuntimeSet::Asset,
                    SkillRuntimeSet::Request,
                    SkillRuntimeSet::Validate,
                    SkillRuntimeSet::Execute,
                    SkillRuntimeSet::Effect,
                    SkillRuntimeSet::Message,
                    SkillRuntimeSet::Trigger,
                    SkillRuntimeSet::Cleanup,
                )
                    .chain(),
            )
            .add_systems(
                bevy::prelude::FixedUpdate,
                (
                    materialize_pending_compiled_skills.in_set(SkillRuntimeSet::Asset),
                    tick_skill_cooldowns.in_set(SkillRuntimeSet::Request),
                    (handle_skill_cast_requests, tick_skill_delays)
                        .chain()
                        .in_set(SkillRuntimeSet::Execute),
                    resume_effect_resolved.in_set(SkillRuntimeSet::Message),
                    resume_skill_signals.in_set(SkillRuntimeSet::Trigger),
                    tick_skill_effects.in_set(SkillRuntimeSet::Cleanup),
                ),
            );
    }
}
