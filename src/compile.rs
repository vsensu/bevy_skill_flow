use crate::dsl::{
    SkillCompiled, SkillContext, SkillDef, SkillNode, SkillPlan, SkillSpecialValue, SkillValue,
};
use crate::expr::{eval_skill_expr, resolve_value};
use crate::registry::{CastModel, SkillError, SkillModifier, SkillRegistry};
use indexmap::{IndexMap, IndexSet};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default)]
pub struct DirectCastModel;

impl CastModel for DirectCastModel {
    fn compile(
        &self,
        skill: &SkillDef,
        _registry: &SkillRegistry,
    ) -> Result<SkillPlan, SkillError> {
        Ok(SkillPlan::new(skill.body.clone(), skill.params.clone()))
    }
}

pub fn compile_skill(
    skill: &SkillDef,
    registry: &SkillRegistry,
) -> Result<SkillCompiled, SkillError> {
    let cast_model = registry
        .cast_model(&skill.cast_model)
        .ok_or_else(|| SkillError::UnknownCastModel(skill.cast_model.clone()))?;
    let mut plan = cast_model.compile(skill, registry)?;
    let tags = skill.tags.iter().cloned().collect::<IndexSet<_>>();
    let mut compiled = SkillCompiled {
        id: skill.id.clone(),
        tags,
        cast_model: skill.cast_model.clone(),
        plan,
    };
    let modifier_ctx = SkillContext::new(&compiled, None, 0);
    for modifier_id in &skill.modifiers {
        let modifier = registry
            .modifier(modifier_id)
            .ok_or_else(|| SkillError::UnknownModifier(modifier_id.clone()))?;
        if modifier.applies(&compiled) {
            plan = compiled.plan.clone();
            modifier.apply(&mut plan, &modifier_ctx)?;
            compiled.plan = plan;
        }
    }
    validate_node(&compiled.plan.root, registry)?;
    Ok(compiled)
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
    fn applies(&self, skill: &SkillCompiled) -> bool {
        self.applies_to_tags.is_empty()
            || self
                .applies_to_tags
                .iter()
                .any(|tag| skill.tags.contains(tag.as_str()))
    }

    fn apply(&self, plan: &mut SkillPlan, ctx: &SkillContext) -> Result<(), SkillError> {
        for op in &self.ops {
            match op {
                StatOp::Add(name, value) => {
                    let current = as_number(plan.stats.get(name)).unwrap_or(0.0);
                    let delta = value_as_number(value, ctx)?;
                    plan.stats
                        .insert(name.clone(), SkillValue::Number(current + delta));
                }
                StatOp::Mul(name, value) => {
                    let current = as_number(plan.stats.get(name)).unwrap_or(1.0);
                    let factor = value_as_number(value, ctx)?;
                    plan.stats
                        .insert(name.clone(), SkillValue::Number(current * factor));
                }
                StatOp::Set(name, value) => {
                    let resolved = resolve_value(value, ctx)?;
                    plan.stats.insert(name.clone(), resolved);
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
