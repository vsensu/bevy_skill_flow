use indexmap::{IndexMap, IndexSet};
use ron::value::RawValue;
use serde::de::{EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

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

#[derive(Clone, Debug, PartialEq, Serialize)]
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

    /// Registered typed DSL node captured from an otherwise unknown RON
    /// variant. Compilation lowers this to executable core nodes.
    Typed {
        name: String,
        args: SkillTypedArgs,
    },
}

#[derive(Clone, Debug, Serialize)]
pub enum SkillTypedArgs {
    Unit,
    Struct(IndexMap<String, Box<RawValue>>),
}

impl PartialEq for SkillTypedArgs {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Unit, Self::Unit) => true,
            (Self::Struct(left), Self::Struct(right)) => {
                left.len() == right.len()
                    && left.iter().all(|(key, value)| {
                        right
                            .get(key)
                            .is_some_and(|right_value| value.to_string() == right_value.to_string())
                    })
            }
            _ => false,
        }
    }
}

impl SkillTypedArgs {
    pub fn deserialize_args<T>(&self) -> Result<T, String>
    where
        T: serde::de::DeserializeOwned,
    {
        match self {
            Self::Unit => ron::from_str("()").map_err(|err| err.to_string()),
            Self::Struct(fields) => {
                let mut source = String::from("(");
                for (index, (key, value)) in fields.iter().enumerate() {
                    if index > 0 {
                        source.push_str(", ");
                    }
                    source.push_str(key);
                    source.push_str(": ");
                    source.push_str(&value.to_string());
                }
                source.push(')');
                ron::from_str(&source).map_err(|err| err.to_string())
            }
        }
    }
}

impl<'de> Deserialize<'de> for SkillNode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_enum("SkillNode", &[], SkillNodeVisitor)
    }
}

struct SkillNodeVisitor;

impl<'de> Visitor<'de> for SkillNodeVisitor {
    type Value = SkillNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a skill DSL node")
    }

    fn visit_enum<A>(self, data: A) -> Result<Self::Value, A::Error>
    where
        A: EnumAccess<'de>,
    {
        let (VariantName(name), variant) = data.variant::<VariantName>()?;
        match name.as_str() {
            "Sequence" => Ok(SkillNode::Sequence(variant.newtype_variant()?)),
            "Parallel" => Ok(SkillNode::Parallel(variant.newtype_variant()?)),
            "Delay" => {
                let (seconds, node) = variant.tuple_variant(2, Tuple2Visitor::default())?;
                Ok(SkillNode::Delay(seconds, node))
            }
            "Repeat" => {
                variant.struct_variant(&["times", "duration", "interval", "node"], RepeatVisitor)
            }
            "If" => variant.struct_variant(&["condition", "then_node", "else_node"], IfVisitor),
            "Let" => {
                let (name, value, node) = variant.tuple_variant(3, Tuple3Visitor::default())?;
                Ok(SkillNode::Let(name, value, node))
            }
            "On" => {
                let (event, node) = variant.tuple_variant(2, Tuple2Visitor::default())?;
                Ok(SkillNode::On(event, node))
            }
            "Emit" => {
                let (event, args) = variant.tuple_variant(2, Tuple2Visitor::default())?;
                Ok(SkillNode::Emit(event, args))
            }
            "Action" => {
                let (action, args) = variant.tuple_variant(2, Tuple2Visitor::default())?;
                Ok(SkillNode::Action(action, args))
            }
            "Deck" => Ok(SkillNode::Deck(variant.newtype_variant()?)),
            "Spell" => {
                let (spell, args) = variant.tuple_variant(2, Tuple2Visitor::default())?;
                Ok(SkillNode::Spell(spell, args))
            }
            "Modifier" => {
                let (modifier, args) = variant.tuple_variant(2, Tuple2Visitor::default())?;
                Ok(SkillNode::Modifier(modifier, args))
            }
            _ => {
                let args = variant.struct_variant(&[], SkillTypedArgsVisitor)?;
                Ok(SkillNode::Typed { name, args })
            }
        }
    }
}

struct VariantName(String);

impl<'de> Deserialize<'de> for VariantName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_identifier(VariantNameVisitor)
    }
}

struct VariantNameVisitor;

impl<'de> Visitor<'de> for VariantNameVisitor {
    type Value = VariantName;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a variant identifier")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(VariantName(value.to_owned()))
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(VariantName(value.to_owned()))
    }
}

struct Tuple2Visitor<T, U>(std::marker::PhantomData<(T, U)>);

impl<T, U> Default for Tuple2Visitor<T, U> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<'de, T, U> Visitor<'de> for Tuple2Visitor<T, U>
where
    T: Deserialize<'de>,
    U: Deserialize<'de>,
{
    type Value = (T, U);

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a two-item tuple variant")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let first = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
        let second = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
        Ok((first, second))
    }
}

struct Tuple3Visitor<T, U, V>(std::marker::PhantomData<(T, U, V)>);

impl<T, U, V> Default for Tuple3Visitor<T, U, V> {
    fn default() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<'de, T, U, V> Visitor<'de> for Tuple3Visitor<T, U, V>
where
    T: Deserialize<'de>,
    U: Deserialize<'de>,
    V: Deserialize<'de>,
{
    type Value = (T, U, V);

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a three-item tuple variant")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let first = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
        let second = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;
        let third = seq
            .next_element()?
            .ok_or_else(|| serde::de::Error::invalid_length(2, &self))?;
        Ok((first, second, third))
    }
}

struct RepeatVisitor;

impl<'de> Visitor<'de> for RepeatVisitor {
    type Value = SkillNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Repeat fields")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut times = None;
        let mut duration = None;
        let mut interval = None;
        let mut node = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "times" => times = Some(map.next_value()?),
                "duration" => duration = Some(map.next_value()?),
                "interval" => interval = Some(map.next_value()?),
                "node" => node = Some(map.next_value()?),
                _ => {
                    let _ = map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(SkillNode::Repeat {
            times: times.unwrap_or(None),
            duration: duration.unwrap_or(None),
            interval: interval.unwrap_or(None),
            node: node.ok_or_else(|| serde::de::Error::missing_field("node"))?,
        })
    }
}

struct IfVisitor;

impl<'de> Visitor<'de> for IfVisitor {
    type Value = SkillNode;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("If fields")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut condition = None;
        let mut then_node = None;
        let mut else_node = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "condition" => condition = Some(map.next_value()?),
                "then_node" => then_node = Some(map.next_value()?),
                "else_node" => else_node = Some(map.next_value()?),
                _ => {
                    let _ = map.next_value::<serde::de::IgnoredAny>()?;
                }
            }
        }
        Ok(SkillNode::If {
            condition: condition.ok_or_else(|| serde::de::Error::missing_field("condition"))?,
            then_node: then_node.ok_or_else(|| serde::de::Error::missing_field("then_node"))?,
            else_node: else_node.unwrap_or(None),
        })
    }
}

struct SkillTypedArgsVisitor;

impl<'de> Visitor<'de> for SkillTypedArgsVisitor {
    type Value = SkillTypedArgs;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("typed DSL node fields")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(SkillTypedArgs::Unit)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut fields = IndexMap::new();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value::<Box<RawValue>>()?;
            fields.insert(key, value);
        }
        Ok(SkillTypedArgs::Struct(fields))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SkillValue {
    Special(SkillSpecialValue),
    Node(Box<SkillNode>),
    Map(IndexMap<String, SkillValue>),
    List(Vec<SkillValue>),
    Number(f64),
    Bool(bool),
    String(String),
    Null,
}

impl<'de> Deserialize<'de> for SkillValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let source = raw.to_string();
        let trimmed = source.trim();

        if let Ok(value) = ron::from_str::<TaggedSkillValue>(trimmed) {
            return Ok(value.into());
        }
        if let Ok(value) = ron::from_str::<SkillSpecialValue>(trimmed) {
            return Ok(Self::Special(value));
        }
        if let Ok(value) = ron::from_str::<SkillNode>(trimmed) {
            return Ok(Self::Node(Box::new(value)));
        }
        if let Ok(value) = ron::from_str::<IndexMap<String, SkillValue>>(trimmed) {
            return Ok(Self::Map(value));
        }
        if let Ok(value) = ron::from_str::<Vec<SkillValue>>(trimmed) {
            return Ok(Self::List(value));
        }
        if let Ok(value) = ron::from_str::<f64>(trimmed) {
            return Ok(Self::Number(value));
        }
        if let Ok(value) = ron::from_str::<bool>(trimmed) {
            return Ok(Self::Bool(value));
        }
        if let Ok(value) = ron::from_str::<String>(trimmed) {
            return Ok(Self::String(value));
        }
        if matches!(trimmed, "()" | "None" | "null") {
            return Ok(Self::Null);
        }

        Err(serde::de::Error::custom(format!(
            "expected skill value, got `{trimmed}`"
        )))
    }
}

#[derive(Deserialize)]
enum TaggedSkillValue {
    Special(SkillSpecialValue),
    Node(Box<SkillNode>),
    Map(IndexMap<String, SkillValue>),
    List(Vec<SkillValue>),
    Number(f64),
    Bool(bool),
    String(String),
    Null,
}

impl From<TaggedSkillValue> for SkillValue {
    fn from(value: TaggedSkillValue) -> Self {
        match value {
            TaggedSkillValue::Special(value) => Self::Special(value),
            TaggedSkillValue::Node(value) => Self::Node(value),
            TaggedSkillValue::Map(value) => Self::Map(value),
            TaggedSkillValue::List(value) => Self::List(value),
            TaggedSkillValue::Number(value) => Self::Number(value),
            TaggedSkillValue::Bool(value) => Self::Bool(value),
            TaggedSkillValue::String(value) => Self::String(value),
            TaggedSkillValue::Null => Self::Null,
        }
    }
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
