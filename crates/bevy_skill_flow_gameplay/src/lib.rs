//! Gameplay primitives that demonstrate how to bind game semantics to
//! `bevy_skill_flow`.

use bevy::prelude::World;
use bevy_skill_flow::{
    CastModel, SkillAction, SkillArgs, SkillContext, SkillDef, SkillError, SkillNode, SkillPlan,
    SkillRegistry, SkillResult, SkillValue, trace_arg,
};
use indexmap::IndexMap;

#[derive(Clone, Debug, Default)]
pub struct TraceAction;

impl SkillAction for TraceAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, _world: &mut World, ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let label = trace_arg(args, "label")
            .or_else(|| trace_arg(args, "id"))
            .unwrap_or_else(|| "trace".to_owned());
        ctx.trace.push(label);
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpawnProjectileAction;

impl SkillAction for SpawnProjectileAction {
    fn validate(&self, args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        if !args.contains_key("payload") && !args.contains_key("prefab") {
            return Err(SkillError::InvalidSkill(
                "spawn_projectile".to_owned(),
                "expected `prefab` or `payload`".to_owned(),
            ));
        }
        Ok(())
    }

    fn execute(&self, _world: &mut World, ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let prefab = trace_arg(args, "prefab").unwrap_or_else(|| "projectile".to_owned());
        ctx.trace.push(format!("spawn_projectile:{prefab}"));
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct DamageAction;

impl SkillAction for DamageAction {
    fn validate(&self, args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        if !args.contains_key("amount") {
            return Err(SkillError::InvalidSkill(
                "damage".to_owned(),
                "expected `amount`".to_owned(),
            ));
        }
        Ok(())
    }

    fn execute(&self, _world: &mut World, ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let amount = trace_arg(args, "amount").unwrap_or_else(|| "?".to_owned());
        ctx.trace.push(format!("damage:{amount}"));
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpellAction;

impl SkillAction for SpellAction {
    fn validate(&self, args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        if !args.contains_key("id") {
            return Err(SkillError::InvalidSkill(
                "spell".to_owned(),
                "expected `id`".to_owned(),
            ));
        }
        Ok(())
    }

    fn execute(&self, _world: &mut World, ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let id = trace_arg(args, "id").unwrap_or_else(|| "spell".to_owned());
        let bonus = trace_arg(args, "damage_bonus").unwrap_or_else(|| "0".to_owned());
        ctx.trace.push(format!("spell:{id}:bonus={bonus}"));
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct WandDeckCastModel;

impl CastModel for WandDeckCastModel {
    fn compile(
        &self,
        skill: &SkillDef,
        _registry: &SkillRegistry,
    ) -> Result<SkillPlan, SkillError> {
        let cards = match &skill.body {
            SkillNode::Deck(cards) => cards,
            _ => {
                return Err(SkillError::InvalidSkill(
                    skill.id.0.clone(),
                    "wand_deck cast model expects a Deck([...]) body".to_owned(),
                ));
            }
        };
        let mut actions = Vec::new();
        let mut damage_bonus = 0.0;
        let mut double_cast = false;
        let mut index = 0;
        while index < cards.len() {
            match &cards[index] {
                SkillNode::Modifier(id, args) if id == "damage_plus" => {
                    if let Some(SkillValue::Number(amount)) = args.get("amount") {
                        damage_bonus += amount;
                    }
                    index += 1;
                }
                SkillNode::Modifier(id, _) if id == "double_cast" => {
                    double_cast = true;
                    index += 1;
                }
                SkillNode::Spell(id, args) => {
                    actions.push(spell_action(id, args, damage_bonus));
                    if double_cast {
                        if let Some(SkillNode::Spell(next_id, next_args)) = cards.get(index + 1) {
                            actions.push(spell_action(next_id, next_args, damage_bonus));
                            index += 1;
                        }
                        double_cast = false;
                    }
                    index += 1;
                }
                other => {
                    return Err(SkillError::InvalidSkill(
                        skill.id.0.clone(),
                        format!("unsupported wand deck card `{other:?}`"),
                    ));
                }
            }
        }
        Ok(SkillPlan::new(
            SkillNode::Sequence(actions),
            skill.params.clone(),
        ))
    }
}

pub fn register_gameplay_primitives(registry: &mut SkillRegistry) -> &mut SkillRegistry {
    registry
        .register_skill_action("trace", TraceAction)
        .register_skill_action("spawn_projectile", SpawnProjectileAction)
        .register_skill_action("damage", DamageAction)
        .register_skill_action("spell", SpellAction)
        .register_cast_model("wand_deck", WandDeckCastModel)
}

fn spell_action(id: &str, args: &SkillArgs, damage_bonus: f64) -> SkillNode {
    let mut action_args = IndexMap::new();
    action_args.insert("id".to_owned(), SkillValue::String(id.to_owned()));
    action_args.insert("damage_bonus".to_owned(), SkillValue::Number(damage_bonus));
    for (key, value) in args {
        action_args.insert(key.clone(), value.clone());
    }
    SkillNode::Action("spell".to_owned(), action_args)
}
