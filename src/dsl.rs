use bevy::prelude::{Entity, Message};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub type SkillArgs = IndexMap<String, SkillValue>;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename = "Skill")]
pub struct SkillDef {
    pub id: SkillId,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default = "default_cast_model")]
    pub cast_model: String,
    #[serde(default)]
    pub requirements: Vec<SkillRequirement>,
    #[serde(default)]
    pub params: SkillArgs,
    #[serde(default)]
    pub modifiers: Vec<String>,
    pub body: SkillNode,
}

fn default_cast_model() -> String {
    "direct".to_owned()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkillRequirement {
    Cost { resource: String, amount: SkillExpr },
    Cooldown { seconds: SkillExpr },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkillNode {
    Sequence(Vec<SkillNode>),
    Parallel(Vec<SkillNode>),
    Delay(SkillExpr, Box<SkillNode>),
    Repeat {
        #[serde(default)]
        times: Option<SkillExpr>,
        #[serde(default)]
        duration: Option<SkillExpr>,
        #[serde(default)]
        interval: Option<SkillExpr>,
        node: Box<SkillNode>,
    },
    If {
        condition: SkillExpr,
        then_node: Box<SkillNode>,
        #[serde(default)]
        else_node: Option<Box<SkillNode>>,
    },
    Let(String, SkillValue, Box<SkillNode>),
    On(String, Box<SkillNode>),
    Emit(String, SkillArgs),
    Action(String, SkillArgs),

    /// Extension syntax consumed by registered cast models. The default runner
    /// rejects these if they remain after compilation.
    Deck(Vec<SkillNode>),
    Spell(String, SkillArgs),
    Modifier(String, SkillArgs),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillValue {
    Special(SkillSpecialValue),
    Map(IndexMap<String, SkillValue>),
    List(Vec<SkillValue>),
    Number(f64),
    Bool(bool),
    String(String),
    Node(Box<SkillNode>),
    Null,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SkillSpecialValue {
    Expr(String),
    Ref(String),
    Tag(String),
    Stat(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillExpr(pub String);

impl SkillExpr {
    pub fn new(expr: impl Into<String>) -> Self {
        Self(expr.into())
    }
}

impl Serialize for SkillExpr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_newtype_variant("SkillExpr", 0, "Expr", &self.0)
    }
}

impl<'de> Deserialize<'de> for SkillExpr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
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

#[derive(Clone, Debug, PartialEq)]
pub struct SkillPlan {
    pub root: SkillNode,
    pub stats: IndexMap<String, SkillValue>,
}

impl SkillPlan {
    pub fn new(root: SkillNode, stats: IndexMap<String, SkillValue>) -> Self {
        Self { root, stats }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillCompiled {
    pub id: SkillId,
    pub tags: IndexSet<String>,
    pub cast_model: String,
    pub plan: SkillPlan,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillContext {
    pub skill_entity: Option<Entity>,
    pub caster: Option<Entity>,
    pub skill_id: SkillId,
    pub current_target: Option<Entity>,
    pub source_event: Option<SkillRuntimeSignal>,
    pub vars: IndexMap<String, SkillValue>,
    pub stats: IndexMap<String, SkillValue>,
    pub tags: IndexSet<String>,
    pub rng_seed: u64,
    pub execution_id: u64,
}

impl SkillContext {
    pub fn new(compiled: &SkillCompiled, caster: Option<Entity>, execution_id: u64) -> Self {
        Self {
            skill_entity: None,
            caster,
            skill_id: compiled.id.clone(),
            current_target: None,
            source_event: None,
            vars: IndexMap::new(),
            stats: compiled.plan.stats.clone(),
            tags: compiled.tags.clone(),
            rng_seed: execution_id,
            execution_id,
        }
    }
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillRuntimeSignal {
    pub name: String,
    pub payload: SkillArgs,
    pub skill_entity: Option<Entity>,
    pub caster: Option<Entity>,
    pub target: Option<Entity>,
}

impl SkillRuntimeSignal {
    pub fn new(name: impl Into<String>, payload: SkillArgs) -> Self {
        Self {
            name: name.into(),
            payload,
            skill_entity: None,
            caster: None,
            target: None,
        }
    }
}

#[derive(Message, Clone, Debug)]
pub struct SkillCastRequest {
    pub skill: SkillId,
    pub caster: Entity,
    pub target: Option<Entity>,
}

#[derive(Message, Clone, Debug)]
pub struct SkillCastStarted {
    pub skill: SkillId,
    pub execution_id: u64,
    pub skill_entity: Entity,
    pub caster: Entity,
    pub target: Option<Entity>,
}

#[derive(Message, Clone, Debug)]
pub struct SkillCastFinished {
    pub skill: SkillId,
    pub execution_id: u64,
    pub skill_entity: Entity,
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
pub struct SkillExecutionFailed {
    pub skill: Option<SkillId>,
    pub execution_id: Option<u64>,
    pub skill_entity: Option<Entity>,
    pub message: String,
}

pub type SkillExecutionError = SkillExecutionFailed;

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillIntent {
    pub kind: String,
    pub skill_entity: Entity,
    pub caster: Entity,
    pub target: Option<Entity>,
    pub payload: SkillArgs,
}
