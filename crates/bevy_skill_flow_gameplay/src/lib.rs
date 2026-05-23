//! Gameplay primitives that demonstrate how to bind game semantics to
//! `bevy_skill_flow`.

use bevy_skill_flow::SettlementMode;
use bevy_skill_flow::{
    CastModel, SkillAction, SkillActionOutput, SkillArgs, SkillContext, SkillDef, SkillError,
    SkillNode, SkillRegistry, SkillResult, SkillValue,
};
use indexmap::IndexMap;

#[derive(Clone, Debug, Default)]
pub struct TraceAction;

impl SkillAction for TraceAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let label = trace_arg(args, "label")
            .or_else(|| trace_arg(args, "id"))
            .unwrap_or_else(|| "trace".to_owned());
        let mut payload = SkillArgs::new();
        payload.insert("label".to_owned(), SkillValue::String(label));
        out.emit_intent(ctx, "trace", payload)
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

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let prefab = trace_arg(args, "prefab").unwrap_or_else(|| "projectile".to_owned());
        let mut payload = args.clone();
        payload.insert("prefab".to_owned(), SkillValue::String(prefab));
        out.emit_intent(ctx, "spawn_projectile", payload)
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

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let amount_value = number_arg(args, "amount").unwrap_or(0.0);
        let amount = trace_arg(args, "amount").unwrap_or_else(|| "?".to_owned());
        let mut payload = args.clone();
        payload
            .entry("amount".to_owned())
            .or_insert_with(|| SkillValue::String(amount));
        out.emit_intent(ctx, "damage", payload)?;
        if let Some(target) = ctx.current_target {
            out.emit_damage_request(ctx, target, amount_value, settlement_mode(args)?)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ApplyBuffAction;

impl SkillAction for ApplyBuffAction {
    fn validate(&self, args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        if !args.contains_key("buff") {
            return Err(SkillError::InvalidSkill(
                "apply_buff".to_owned(),
                "expected `buff`".to_owned(),
            ));
        }
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let buff = trace_arg(args, "buff").unwrap_or_else(|| "buff".to_owned());
        let duration_seconds = number_arg(args, "duration");
        let mut payload = args.clone();
        payload
            .entry("buff".to_owned())
            .or_insert_with(|| SkillValue::String(buff.clone()));
        out.emit_intent(ctx, "apply_buff", payload)?;
        if let Some(target) = ctx.current_target {
            out.emit_apply_buff_request(
                ctx,
                target,
                buff,
                duration_seconds,
                node_arg(args, "on_add")?,
                node_arg(args, "on_remove")?,
            );
        }
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

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let id = trace_arg(args, "id").unwrap_or_else(|| "spell".to_owned());
        let bonus = trace_arg(args, "damage_bonus").unwrap_or_else(|| "0".to_owned());
        let mut payload = args.clone();
        payload
            .entry("id".to_owned())
            .or_insert_with(|| SkillValue::String(id));
        payload
            .entry("damage_bonus".to_owned())
            .or_insert_with(|| SkillValue::String(bonus));
        out.emit_intent(ctx, "spell", payload)
    }
}

#[derive(Clone, Debug, Default)]
pub struct WandDeckCastModel;

impl CastModel for WandDeckCastModel {
    fn compile(
        &self,
        skill: &SkillDef,
        _registry: &SkillRegistry,
    ) -> Result<SkillNode, SkillError> {
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
        Ok(SkillNode::Sequence(actions))
    }
}

pub fn register_gameplay_primitives(registry: &mut SkillRegistry) -> &mut SkillRegistry {
    registry
        .register_skill_action("trace", TraceAction)
        .register_skill_action("spawn_projectile", SpawnProjectileAction)
        .register_skill_action("damage", DamageAction)
        .register_skill_action("apply_buff", ApplyBuffAction)
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

fn trace_arg(args: &SkillArgs, key: &str) -> Option<String> {
    match args.get(key) {
        Some(SkillValue::String(value)) => Some(value.clone()),
        Some(SkillValue::Number(value)) => Some(value.to_string()),
        Some(SkillValue::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn number_arg(args: &SkillArgs, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(SkillValue::Number(value)) => Some(*value),
        Some(SkillValue::String(value)) => value.parse().ok(),
        _ => None,
    }
}

fn settlement_mode(args: &SkillArgs) -> Result<SettlementMode, SkillError> {
    match trace_arg(args, "mode")
        .unwrap_or_else(|| "request".to_owned())
        .as_str()
    {
        "sync" | "Sync" => Ok(SettlementMode::Sync),
        "request" | "Request" => Ok(SettlementMode::Request),
        "await" | "Await" => Ok(SettlementMode::Await),
        other => Err(SkillError::InvalidSkill(
            "damage".to_owned(),
            format!("unknown damage settlement mode `{other}`"),
        )),
    }
}

fn node_arg(args: &SkillArgs, key: &str) -> Result<Option<SkillNode>, SkillError> {
    match args.get(key) {
        Some(value) => erased_node_from_value(value).map(Some),
        None => Ok(None),
    }
}

fn erased_node_from_value(value: &SkillValue) -> Result<SkillNode, SkillError> {
    match value {
        SkillValue::Node(node) => Ok((**node).clone()),
        SkillValue::List(values) => erased_node_from_list(values),
        other => Err(SkillError::Runtime(format!(
            "expected hook skill node, got `{other:?}`"
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
            "could not decode erased hook node `{other:?}`"
        ))),
    }
}
