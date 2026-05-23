use crate::dsl::{
    SkillArgs, SkillCompiled, SkillContext, SkillDef, SkillNode, SkillSpecialValue, SkillValue,
};
use crate::expr::{eval_skill_expr, resolve_value};
use crate::registry::{CastModel, SkillError, SkillModifier, SkillRegistry};
use bevy_skill_ecs::{
    SkillGraph, SkillGraphNodeKind, SkillNodeId, SkillSpecialValue as EcsSkillSpecialValue,
    SkillValue as EcsSkillValue,
};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct DirectCastModel;

impl CastModel for DirectCastModel {
    fn compile(
        &self,
        skill: &SkillDef,
        _registry: &SkillRegistry,
    ) -> Result<SkillNode, SkillError> {
        Ok(skill.body.clone())
    }
}

pub fn compile_skill(
    skill: &SkillDef,
    registry: &SkillRegistry,
) -> Result<SkillCompiled, SkillError> {
    let cast_model = registry
        .cast_model(&skill.cast_model)
        .ok_or_else(|| SkillError::UnknownCastModel(skill.cast_model.clone()))?;
    let root = cast_model.compile(skill, registry)?;
    let tags = skill.tags.iter().cloned().collect::<IndexSet<_>>();
    let mut params = skill.params.clone();
    let mut modifier_ctx = compile_context(&skill.id, &tags, &params);
    for modifier_id in &skill.modifiers {
        let modifier = registry
            .modifier(modifier_id)
            .ok_or_else(|| SkillError::UnknownModifier(modifier_id.clone()))?;
        if modifier.applies(&tags) {
            modifier.apply(&mut params, &modifier_ctx)?;
            modifier_ctx.stats = params.clone();
        }
    }
    validate_node(&root, registry)?;
    let graph = graph_from_parts(&skill.id, &tags, &params, &root)?;
    Ok(SkillCompiled {
        id: skill.id.clone(),
        tags,
        cast_model: skill.cast_model.clone(),
        requirements: skill.requirements.clone(),
        graph,
    })
}

pub fn compile_skill_graph(
    skill: &SkillDef,
    registry: &SkillRegistry,
) -> Result<SkillGraph, SkillError> {
    compile_skill(skill, registry).map(|compiled| compiled.graph)
}

fn graph_from_parts(
    id: &crate::dsl::SkillId,
    tags: &IndexSet<String>,
    params: &SkillArgs,
    root: &SkillNode,
) -> Result<SkillGraph, SkillError> {
    let mut graph = SkillGraph::new(bevy_skill_ecs::SkillId::new(id.0.clone()));
    graph.tags = tags.iter().cloned().collect();
    graph.params = params
        .iter()
        .map(|(key, value)| Ok((key.clone(), to_ecs_value(value)?)))
        .collect::<Result<_, SkillError>>()?;
    let root_id = append_graph_node(root, &mut graph)?;
    graph.set_root(root_id);
    graph
        .validate()
        .map_err(|err| SkillError::Runtime(err.to_string()))?;
    Ok(graph)
}

fn compile_context(
    id: &crate::dsl::SkillId,
    tags: &IndexSet<String>,
    params: &SkillArgs,
) -> SkillContext {
    SkillContext {
        skill_entity: None,
        caster: None,
        skill_id: id.clone(),
        current_target: None,
        source_event: None,
        vars: IndexMap::new(),
        stats: params.clone(),
        tags: tags.clone(),
        rng_seed: 0,
        execution_id: 0,
        step_budget: bevy_skill_ecs::SkillRuntimeConfig::default().step_budget,
        step_budget_remaining: bevy_skill_ecs::SkillRuntimeConfig::default().step_budget,
    }
}

fn append_graph_node(node: &SkillNode, graph: &mut SkillGraph) -> Result<SkillNodeId, SkillError> {
    match node {
        SkillNode::Sequence(nodes) => {
            let id = graph.add_node(SkillGraphNodeKind::Sequence);
            append_children(graph, id, "items", nodes)?;
            Ok(id)
        }
        SkillNode::Parallel(nodes) => {
            let id = graph.add_node(SkillGraphNodeKind::Parallel);
            append_children(graph, id, "branches", nodes)?;
            Ok(id)
        }
        SkillNode::Delay(seconds, node) => {
            let id = graph.add_node(SkillGraphNodeKind::Delay {
                seconds: bevy_skill_ecs::SkillExpr(seconds.0.clone()),
            });
            let child = append_graph_node(node, graph)?;
            graph
                .push_child(id, "then", child)
                .map_err(|err| SkillError::Runtime(err.to_string()))?;
            Ok(id)
        }
        SkillNode::Repeat {
            times,
            duration,
            interval,
            node,
        } => {
            let id = graph.add_node(SkillGraphNodeKind::Repeat {
                times: times
                    .as_ref()
                    .map(|expr| bevy_skill_ecs::SkillExpr(expr.0.clone())),
                duration: duration
                    .as_ref()
                    .map(|expr| bevy_skill_ecs::SkillExpr(expr.0.clone())),
                interval: interval
                    .as_ref()
                    .map(|expr| bevy_skill_ecs::SkillExpr(expr.0.clone())),
            });
            let child = append_graph_node(node, graph)?;
            graph
                .push_child(id, "body", child)
                .map_err(|err| SkillError::Runtime(err.to_string()))?;
            Ok(id)
        }
        SkillNode::If {
            condition,
            then_node,
            else_node,
        } => {
            let id = graph.add_node(SkillGraphNodeKind::If {
                condition: bevy_skill_ecs::SkillExpr(condition.0.clone()),
            });
            let then_id = append_graph_node(then_node, graph)?;
            graph
                .push_child(id, "then", then_id)
                .map_err(|err| SkillError::Runtime(err.to_string()))?;
            if let Some(else_node) = else_node {
                let else_id = append_graph_node(else_node, graph)?;
                graph
                    .push_child(id, "else", else_id)
                    .map_err(|err| SkillError::Runtime(err.to_string()))?;
            }
            Ok(id)
        }
        SkillNode::Let(name, value, node) => {
            let id = graph.add_node(SkillGraphNodeKind::SetSkillVar {
                name: name.clone(),
                value: to_ecs_value(value)?,
            });
            let child = append_graph_node(node, graph)?;
            graph
                .push_child(id, "then", child)
                .map_err(|err| SkillError::Runtime(err.to_string()))?;
            Ok(id)
        }
        SkillNode::On(name, node) => {
            let id = graph.add_node(SkillGraphNodeKind::WaitEvent {
                event: name.clone(),
            });
            let child = append_graph_node(node, graph)?;
            graph
                .push_payload(id, name.clone(), child)
                .map_err(|err| SkillError::Runtime(err.to_string()))?;
            Ok(id)
        }
        SkillNode::Emit(name, payload) => Ok(graph.add_node(SkillGraphNodeKind::EmitSkillEvent {
            event: name.clone(),
            payload: payload
                .iter()
                .map(|(key, value)| Ok((key.clone(), to_ecs_value(value)?)))
                .collect::<Result<_, SkillError>>()?,
        })),
        SkillNode::Action(id, args) => {
            let mut action_args = bevy_skill_ecs::SkillParams::new();
            let graph_id = graph.add_node(SkillGraphNodeKind::Extension {
                constructor: id.clone(),
                args: bevy_skill_ecs::SkillParams::new(),
            });
            for (key, value) in args {
                if let Some(payload) = payload_node_from_value(key, value)? {
                    let payload_id = append_graph_node(&payload, graph)?;
                    graph
                        .push_payload(graph_id, key.clone(), payload_id)
                        .map_err(|err| SkillError::Runtime(err.to_string()))?;
                } else {
                    action_args.insert(key.clone(), to_ecs_value(value)?);
                }
            }
            graph.node_mut(graph_id).expect("fresh node").kind = SkillGraphNodeKind::Extension {
                constructor: id.clone(),
                args: action_args,
            };
            Ok(graph_id)
        }
        SkillNode::Deck(_) | SkillNode::Spell(_, _) | SkillNode::Modifier(_, _) => Err(
            SkillError::Runtime("extension node remained after cast model compilation".to_owned()),
        ),
    }
}

fn append_children(
    graph: &mut SkillGraph,
    parent: SkillNodeId,
    slot: &str,
    nodes: &[SkillNode],
) -> Result<(), SkillError> {
    for node in nodes {
        let child = append_graph_node(node, graph)?;
        graph
            .push_child(parent, slot, child)
            .map_err(|err| SkillError::Runtime(err.to_string()))?;
    }
    Ok(())
}

fn payload_node_from_value<'a>(
    key: &str,
    value: &'a SkillValue,
) -> Result<Option<std::borrow::Cow<'a, SkillNode>>, SkillError> {
    if let SkillValue::Node(node) = value {
        return Ok(Some(std::borrow::Cow::Borrowed(node)));
    }
    if !(key == "payload" || key.starts_with("on_") || key.starts_with("payload_")) {
        return Ok(None);
    }
    erased_node_from_value(value)
        .map(std::borrow::Cow::Owned)
        .map(Some)
}

fn erased_node_from_value(value: &SkillValue) -> Result<SkillNode, SkillError> {
    match value {
        SkillValue::Node(node) => Ok((**node).clone()),
        SkillValue::List(values) => erased_node_from_list(values),
        other => Err(SkillError::Runtime(format!(
            "expected payload skill node, got `{other:?}`"
        ))),
    }
}

fn erased_node_from_list(values: &[SkillValue]) -> Result<SkillNode, SkillError> {
    match values {
        [SkillValue::String(action), SkillValue::Map(args)] => {
            Ok(SkillNode::Action(action.clone(), args.clone()))
        }
        [SkillValue::String(event), node] => Ok(SkillNode::On(
            event.clone(),
            Box::new(erased_node_from_value(node)?),
        )),
        [SkillValue::List(nodes)] => Ok(SkillNode::Sequence(
            nodes
                .iter()
                .map(erased_node_from_value)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        nodes
            if nodes
                .iter()
                .all(|value| matches!(value, SkillValue::List(_))) =>
        {
            Ok(SkillNode::Sequence(
                nodes
                    .iter()
                    .map(erased_node_from_value)
                    .collect::<Result<Vec<_>, _>>()?,
            ))
        }
        other => Err(SkillError::Runtime(format!(
            "could not decode erased payload node `{other:?}`"
        ))),
    }
}

fn to_ecs_value(value: &SkillValue) -> Result<EcsSkillValue, SkillError> {
    Ok(match value {
        SkillValue::Special(SkillSpecialValue::Expr(value)) => {
            EcsSkillValue::Special(EcsSkillSpecialValue::Expr(value.clone()))
        }
        SkillValue::Special(SkillSpecialValue::Ref(value)) => {
            EcsSkillValue::Special(EcsSkillSpecialValue::Ref(value.clone()))
        }
        SkillValue::Special(SkillSpecialValue::Tag(value)) => {
            EcsSkillValue::Special(EcsSkillSpecialValue::Tag(value.clone()))
        }
        SkillValue::Special(SkillSpecialValue::Stat(value)) => {
            EcsSkillValue::Special(EcsSkillSpecialValue::Stat(value.clone()))
        }
        SkillValue::Map(map) => EcsSkillValue::Map(
            map.iter()
                .map(|(key, value)| Ok((key.clone(), to_ecs_value(value)?)))
                .collect::<Result<_, SkillError>>()?,
        ),
        SkillValue::List(list) => EcsSkillValue::List(
            list.iter()
                .map(to_ecs_value)
                .collect::<Result<Vec<_>, SkillError>>()?,
        ),
        SkillValue::Number(value) => EcsSkillValue::Number(*value),
        SkillValue::Bool(value) => EcsSkillValue::Bool(*value),
        SkillValue::String(value) => EcsSkillValue::String(value.clone()),
        SkillValue::Node(_) => {
            return Err(SkillError::Runtime(
                "nested SkillNode values must be compiled as payload slots".to_owned(),
            ));
        }
        SkillValue::Null => EcsSkillValue::Null,
    })
}

pub fn validate_node(node: &SkillNode, registry: &SkillRegistry) -> Result<(), SkillError> {
    match node {
        SkillNode::Sequence(nodes) | SkillNode::Parallel(nodes) => {
            for node in nodes {
                validate_node(node, registry)?;
            }
        }
        SkillNode::Delay(_, node) | SkillNode::Let(_, _, node) | SkillNode::On(_, node) => {
            validate_node(node, registry)?
        }
        SkillNode::Repeat { node, .. } => validate_node(node, registry)?,
        SkillNode::If {
            then_node,
            else_node,
            ..
        } => {
            validate_node(then_node, registry)?;
            if let Some(else_node) = else_node {
                validate_node(else_node, registry)?;
            }
        }
        SkillNode::Emit(_, args) => validate_values(args.values())?,
        SkillNode::Action(id, args) => {
            validate_values(args.values())?;
            let action = registry
                .action(id)
                .ok_or_else(|| SkillError::UnknownAction(id.clone()))?;
            action.validate(args, registry)?;
        }
        SkillNode::Deck(_) | SkillNode::Spell(_, _) | SkillNode::Modifier(_, _) => {
            return Err(SkillError::Runtime(
                "extension node remained after cast model compilation".to_owned(),
            ));
        }
    }
    Ok(())
}

fn validate_values<'a>(values: impl Iterator<Item = &'a SkillValue>) -> Result<(), SkillError> {
    for value in values {
        match value {
            SkillValue::Map(map) => validate_values(map.values())?,
            SkillValue::List(list) => validate_values(list.iter())?,
            SkillValue::Node(_) => {}
            _ => {}
        }
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename = "ModifierDef")]
pub struct ModifierDef {
    pub id: String,
    #[serde(default)]
    pub applies_to_tags: Vec<String>,
    #[serde(default)]
    pub ops: Vec<StatOp>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum StatOp {
    Add(String, SkillValue),
    Mul(String, SkillValue),
    Set(String, SkillValue),
}

#[derive(Clone, Debug)]
pub struct StatModifier {
    applies_to_tags: IndexSet<String>,
    ops: Vec<StatOp>,
}

impl StatModifier {
    pub fn new(applies_to_tags: impl IntoIterator<Item = String>, ops: Vec<StatOp>) -> Self {
        Self {
            applies_to_tags: applies_to_tags.into_iter().collect(),
            ops,
        }
    }
}

impl From<ModifierDef> for StatModifier {
    fn from(value: ModifierDef) -> Self {
        Self::new(value.applies_to_tags, value.ops)
    }
}

impl SkillModifier for StatModifier {
    fn applies(&self, tags: &IndexSet<String>) -> bool {
        self.applies_to_tags.is_empty()
            || self
                .applies_to_tags
                .iter()
                .any(|tag| tags.contains(tag.as_str()))
    }

    fn apply(&self, params: &mut SkillArgs, ctx: &SkillContext) -> Result<(), SkillError> {
        for op in &self.ops {
            match op {
                StatOp::Add(name, value) => {
                    let current = as_number(params.get(name)).unwrap_or(0.0);
                    let delta = value_as_number(value, ctx)?;
                    params.insert(name.clone(), SkillValue::Number(current + delta));
                }
                StatOp::Mul(name, value) => {
                    let current = as_number(params.get(name)).unwrap_or(1.0);
                    let factor = value_as_number(value, ctx)?;
                    params.insert(name.clone(), SkillValue::Number(current * factor));
                }
                StatOp::Set(name, value) => {
                    let resolved = resolve_value(value, ctx)?;
                    params.insert(name.clone(), resolved);
                }
            }
        }
        Ok(())
    }
}

fn value_as_number(value: &SkillValue, ctx: &SkillContext) -> Result<f64, SkillError> {
    let resolved = match value {
        SkillValue::Special(SkillSpecialValue::Expr(expr)) => {
            eval_skill_expr(&crate::dsl::SkillExpr(expr.clone()), ctx)?
        }
        other => other.clone(),
    };
    as_number(Some(&resolved)).ok_or_else(|| SkillError::Runtime("expected number".to_owned()))
}

fn as_number(value: Option<&SkillValue>) -> Option<f64> {
    match value {
        Some(SkillValue::Number(value)) => Some(*value),
        _ => None,
    }
}

pub fn stats_from_numbers(
    values: impl IntoIterator<Item = (String, f64)>,
) -> IndexMap<String, SkillValue> {
    values
        .into_iter()
        .map(|(key, value)| (key, SkillValue::Number(value)))
        .collect()
}
