//! ECS-facing skill runtime protocol and graph model.
//!
//! This crate intentionally contains no game combat semantics. Concrete
//! effects such as damage, projectiles, buffs, or wand cards are represented by
//! extension nodes/components supplied by a host game or a higher-level DSL.

use bevy::prelude::{
    App, Component, Entity, IntoScheduleConfigs, Message, Plugin, Reflect, Resource, SystemSet,
};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Clone, Debug, PartialEq, Reflect, Serialize, Deserialize)]
pub struct SkillExpr(pub String);

impl SkillExpr {
    pub fn new(expr: impl Into<String>) -> Self {
        Self(expr.into())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkillGraph {
    pub id: SkillId,
    #[serde(default)]
    pub tags: SkillTags,
    #[serde(default)]
    pub params: SkillParams,
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
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SetSkillVar {
    pub name: String,
    #[reflect(ignore)]
    pub value: SkillValue,
}

#[derive(Component, Clone, Debug, Default, Reflect, PartialEq)]
pub struct WithSkillContext;

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillRootOf {
    pub graph: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillChildOf {
    pub parent: Entity,
    pub slot: String,
    pub order: u32,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct SkillPayloadOf {
    pub parent: Entity,
    pub slot: String,
    pub order: u32,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct ExecutionOfSkill {
    pub skill: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct ExecutionOwnedBy {
    pub owner: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct ProjectileFromExecution {
    pub execution: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct BuffFromExecution {
    pub execution: Entity,
}

#[derive(Component, Clone, Debug, Reflect, PartialEq)]
pub struct AuraAffectsTarget {
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
pub struct DamageRequest {
    pub request_id: u64,
    pub execution_id: u64,
    pub source: Option<Entity>,
    pub target: Entity,
    pub amount: f64,
    pub mode: SettlementMode,
}

#[derive(Message, Clone, Debug)]
pub struct DamageResolved {
    pub request_id: u64,
    pub execution_id: u64,
    pub source: Option<Entity>,
    pub target: Entity,
    pub amount: f64,
}

#[derive(Message, Clone, Debug)]
pub struct ApplyBuffRequest {
    pub execution_id: u64,
    pub target: Entity,
    pub buff: String,
    pub duration_seconds: Option<f64>,
}

#[derive(Message, Clone, Debug)]
pub struct ProjectileHit {
    pub execution_id: u64,
    pub projectile: Entity,
    pub target: Option<Entity>,
    pub position: Option<[f32; 3]>,
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
            .add_message::<SkillCastRequest>()
            .add_message::<SkillCastAccepted>()
            .add_message::<SkillCastRejected>()
            .add_message::<SkillExecutionFinished>()
            .add_message::<DamageRequest>()
            .add_message::<DamageResolved>()
            .add_message::<ApplyBuffRequest>()
            .add_message::<ProjectileHit>()
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
            );
    }
}
