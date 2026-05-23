//! Gameplay primitives that demonstrate how to bind game semantics to
//! `bevy_skill_flow`.

use bevy::prelude::{
    App, Entity, FixedUpdate, IntoScheduleConfigs, Message, MessageReader, MessageWriter, Plugin,
};
use bevy_skill_ecs::{
    SkillAction, SkillActionInput, SkillActionOutput, SkillActionRegistry, SkillContext,
    SkillParams as SkillArgs, SkillResult, SkillValue,
};
use bevy_skill_flow::{
    CastModel, SkillArgs as DslArgs, SkillDef, SkillError, SkillNode, SkillRegistry,
    SkillValue as DslValue,
};
use bevy_skill_flow::{
    SettlementMode, SkillEffectRequest, SkillEffectResolved, SkillRuntimeSet, SkillRuntimeSignal,
};
use indexmap::IndexMap;

pub type DamageRequest = SkillEffectRequest;
pub type DamageResolved = SkillEffectResolved;
pub type ApplyBuffRequest = SkillEffectRequest;

#[derive(Message, Clone, Debug)]
pub struct ProjectileHit {
    pub execution_id: u64,
    pub projectile: Entity,
    pub target: Option<Entity>,
    pub position: Option<[f32; 3]>,
}

#[derive(Debug, Default)]
pub struct SkillGameplayPlugin;

impl Plugin for SkillGameplayPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<ProjectileHit>().add_systems(
            FixedUpdate,
            projectile_hit_signal_bridge
                .in_set(SkillRuntimeSet::Trigger)
                .before(bevy_skill_flow::resume_skill_signals),
        );
    }
}

pub fn projectile_hit_signal_bridge(
    mut hits: MessageReader<ProjectileHit>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
) {
    for hit in hits.read() {
        let mut payload = SkillArgs::new();
        payload.insert(
            "projectile".to_owned(),
            SkillValue::String(format!("{:?}", hit.projectile)),
        );
        if let Some(position) = hit.position {
            payload.insert(
                "position".to_owned(),
                SkillValue::List(
                    position
                        .into_iter()
                        .map(|value| SkillValue::Number(value as f64))
                        .collect(),
                ),
            );
        }
        for name in ["hit", "projectile_hit"] {
            signals.write(SkillRuntimeSignal {
                name: name.to_owned(),
                payload: payload.clone(),
                execution_id: Some(hit.execution_id),
                skill_entity: None,
                caster: None,
                target: hit.target,
            });
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TraceAction;

impl SkillAction for TraceAction {
    fn validate(
        &self,
        _args: &SkillArgs,
        _registry: &SkillActionRegistry,
    ) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let args = &input.args;
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
    fn validate(
        &self,
        args: &SkillArgs,
        _registry: &SkillActionRegistry,
    ) -> Result<(), SkillError> {
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
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let args = &input.args;
        let prefab = trace_arg(args, "prefab").unwrap_or_else(|| "projectile".to_owned());
        let mut payload = args.clone();
        payload.insert("prefab".to_owned(), SkillValue::String(prefab));
        out.emit_intent(ctx, "spawn_projectile", payload)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DamageAction;

impl SkillAction for DamageAction {
    fn validate(
        &self,
        args: &SkillArgs,
        _registry: &SkillActionRegistry,
    ) -> Result<(), SkillError> {
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
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let args = &input.args;
        let amount_value = number_arg(args, "amount").unwrap_or(0.0);
        let amount = trace_arg(args, "amount").unwrap_or_else(|| "?".to_owned());
        let mut payload = args.clone();
        payload
            .entry("amount".to_owned())
            .or_insert_with(|| SkillValue::String(amount));
        out.emit_intent(ctx, "damage", payload)?;
        if let Some(target) = ctx.current_target {
            let mode = settlement_mode(args)?;
            let mut effect_payload = SkillArgs::new();
            effect_payload.insert("amount".to_owned(), SkillValue::Number(amount_value));
            out.emit_effect_request(ctx, "damage", Some(target), effect_payload, mode)?;
            if mode == SettlementMode::Sync {
                out.set_var("last_damage_amount", SkillValue::Number(amount_value));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct ApplyBuffAction;

impl SkillAction for ApplyBuffAction {
    fn validate(
        &self,
        args: &SkillArgs,
        _registry: &SkillActionRegistry,
    ) -> Result<(), SkillError> {
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
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let args = &input.args;
        let buff = trace_arg(args, "buff").unwrap_or_else(|| "buff".to_owned());
        let duration_seconds = number_arg(args, "duration");
        let mut payload = args.clone();
        payload
            .entry("buff".to_owned())
            .or_insert_with(|| SkillValue::String(buff.clone()));
        out.emit_intent(ctx, "apply_buff", payload)?;
        if let Some(target) = ctx.current_target {
            let mut effect_payload = SkillArgs::new();
            effect_payload.insert("buff".to_owned(), SkillValue::String(buff));
            if let Some(duration) = duration_seconds {
                effect_payload.insert("duration".to_owned(), SkillValue::Number(duration));
            }
            out.emit_timed_effect(
                ctx,
                "apply_buff",
                Some(target),
                effect_payload,
                duration_seconds,
                input.payload("on_add"),
                input.payload("on_remove"),
            )?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct SpellAction;

impl SkillAction for SpellAction {
    fn validate(
        &self,
        args: &SkillArgs,
        _registry: &SkillActionRegistry,
    ) -> Result<(), SkillError> {
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
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let args = &input.args;
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
                    if let Some(DslValue::Number(amount)) = args.get("amount") {
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

pub fn register_gameplay_primitives(
    registry: &mut SkillRegistry,
    actions: &mut SkillActionRegistry,
) {
    actions
        .register_skill_action("trace", TraceAction)
        .register_skill_action("spawn_projectile", SpawnProjectileAction)
        .register_skill_action("damage", DamageAction)
        .register_skill_action("apply_buff", ApplyBuffAction)
        .register_skill_action("spell", SpellAction);
    registry.register_cast_model("wand_deck", WandDeckCastModel);
}

fn spell_action(id: &str, args: &DslArgs, damage_bonus: f64) -> SkillNode {
    let mut action_args = IndexMap::new();
    action_args.insert("id".to_owned(), DslValue::String(id.to_owned()));
    action_args.insert("damage_bonus".to_owned(), DslValue::Number(damage_bonus));
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
