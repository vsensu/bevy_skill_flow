use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub type SkillArgs = IndexMap<String, SkillValue>;
pub type SkillCompiled = bevy_skill_ecs::SkillCompiled;
pub type SkillId = bevy_skill_ecs::SkillId;
pub type SkillRequirement = bevy_skill_ecs::SkillRequirement;

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
pub struct SkillCompileContext {
    pub skill_id: SkillId,
    pub vars: IndexMap<String, SkillValue>,
    pub stats: IndexMap<String, SkillValue>,
    pub tags: IndexSet<String>,
    pub rng_seed: u64,
    pub execution_id: u64,
    pub step_budget: u32,
    pub step_budget_remaining: u32,
}

impl SkillCompileContext {
    pub fn new(compiled: &SkillCompiled) -> Self {
        Self {
            skill_id: compiled.id.clone(),
            vars: IndexMap::new(),
            stats: stats_from_graph(&compiled.graph),
            tags: compiled.tags.iter().cloned().collect(),
            rng_seed: 0,
            execution_id: 0,
            step_budget: bevy_skill_ecs::SkillRuntimeConfig::default().step_budget,
            step_budget_remaining: bevy_skill_ecs::SkillRuntimeConfig::default().step_budget,
        }
    }
}

fn stats_from_graph(graph: &bevy_skill_ecs::SkillGraph) -> IndexMap<String, SkillValue> {
    graph
        .params
        .iter()
        .map(|(key, value)| (key.clone(), skill_value_from_ecs(value)))
        .collect()
}

fn skill_value_from_ecs(value: &bevy_skill_ecs::SkillValue) -> SkillValue {
    match value {
        bevy_skill_ecs::SkillValue::Special(special) => SkillValue::Special(match special {
            bevy_skill_ecs::SkillSpecialValue::Expr(expr) => SkillSpecialValue::Expr(expr.clone()),
            bevy_skill_ecs::SkillSpecialValue::Ref(reference) => {
                SkillSpecialValue::Ref(reference.clone())
            }
            bevy_skill_ecs::SkillSpecialValue::Tag(tag) => SkillSpecialValue::Tag(tag.clone()),
            bevy_skill_ecs::SkillSpecialValue::Stat(stat) => SkillSpecialValue::Stat(stat.clone()),
        }),
        bevy_skill_ecs::SkillValue::Map(values) => SkillValue::Map(
            values
                .iter()
                .map(|(key, value)| (key.clone(), skill_value_from_ecs(value)))
                .collect(),
        ),
        bevy_skill_ecs::SkillValue::List(values) => {
            SkillValue::List(values.iter().map(skill_value_from_ecs).collect())
        }
        bevy_skill_ecs::SkillValue::Number(value) => SkillValue::Number(*value),
        bevy_skill_ecs::SkillValue::Bool(value) => SkillValue::Bool(*value),
        bevy_skill_ecs::SkillValue::String(value) => SkillValue::String(value.clone()),
        bevy_skill_ecs::SkillValue::Null => SkillValue::Null,
    }
}
