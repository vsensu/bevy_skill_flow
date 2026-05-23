use crate::expr::{eval_skill_expr, resolve_args};
#[cfg(feature = "full_runtime_entities")]
use crate::{ExecutionOfSkill, SkillChildOf, SkillPayloadOf, SkillRootOf};
use crate::{
    SettlementMode, SkillCastAccepted, SkillCastRejected, SkillCastRequest, SkillCompiled,
    SkillEffectRequest, SkillEffectResolved, SkillExecutionFinished, SkillExpr, SkillGraph,
    SkillGraphNodeKind, SkillId, SkillLibrary, SkillNodeId, SkillParams as SkillArgs,
    SkillRequirement, SkillRuntimeConfig, SkillValue,
};
use bevy::prelude::{
    Commands, Component, Entity, Event, Message, MessageReader, MessageWriter, On, Query, Res,
    ResMut, Resource, Time, Timer, TimerMode,
};
use indexmap::{IndexMap, IndexSet};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

pub type SkillResult = Result<(), SkillError>;

#[derive(Debug, Error, Clone, PartialEq)]
pub enum SkillError {
    #[error("RON parse failed: {0}")]
    Ron(String),
    #[error("skill `{0}` is invalid: {1}")]
    InvalidSkill(String, String),
    #[error("action `{0}` is not registered")]
    UnknownAction(String),
    #[error("modifier `{0}` is not registered")]
    UnknownModifier(String),
    #[error("cast model `{0}` is not registered")]
    UnknownCastModel(String),
    #[error("expression `{expr}` failed: {message}")]
    Expr { expr: String, message: String },
    #[error("runtime error: {0}")]
    Runtime(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillContext {
    pub skill_entity: Option<Entity>,
    pub caster: Option<Entity>,
    pub skill_id: SkillId,
    pub current_target: Option<Entity>,
    pub source_event: Option<SkillRuntimeSignal>,
    pub vars: SkillArgs,
    pub stats: SkillArgs,
    pub tags: IndexSet<String>,
    pub rng_seed: u64,
    pub execution_id: u64,
    pub step_budget: u32,
    pub step_budget_remaining: u32,
}

impl SkillContext {
    pub fn new(compiled: &SkillCompiled, caster: Option<Entity>, execution_id: u64) -> Self {
        Self {
            skill_entity: None,
            caster,
            skill_id: compiled.id.clone(),
            current_target: None,
            source_event: None,
            vars: SkillArgs::new(),
            stats: compiled.graph.params.clone(),
            tags: compiled.tags.clone(),
            rng_seed: execution_id,
            execution_id,
            step_budget: SkillRuntimeConfig::default().step_budget,
            step_budget_remaining: SkillRuntimeConfig::default().step_budget,
        }
    }
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillRuntimeSignal {
    pub name: String,
    pub payload: SkillArgs,
    pub execution_id: Option<u64>,
    pub skill_entity: Option<Entity>,
    pub caster: Option<Entity>,
    pub target: Option<Entity>,
}

#[derive(Event, Clone, Debug, PartialEq)]
pub struct SkillObserverTrigger {
    pub name: String,
    pub payload: SkillArgs,
    pub execution_id: Option<u64>,
    pub skill_entity: Option<Entity>,
    pub caster: Option<Entity>,
    pub target: Option<Entity>,
}

impl SkillObserverTrigger {
    pub fn new(name: impl Into<String>, payload: SkillArgs) -> Self {
        Self {
            name: name.into(),
            payload,
            execution_id: None,
            skill_entity: None,
            caster: None,
            target: None,
        }
    }
}

impl SkillRuntimeSignal {
    pub fn new(name: impl Into<String>, payload: SkillArgs) -> Self {
        Self {
            name: name.into(),
            payload,
            execution_id: None,
            skill_entity: None,
            caster: None,
            target: None,
        }
    }
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillExecutionFailed {
    pub skill: Option<SkillId>,
    pub execution_id: Option<u64>,
    pub skill_entity: Option<Entity>,
    pub message: String,
}

#[derive(Message, Clone, Debug, PartialEq)]
pub struct SkillIntent {
    pub kind: String,
    pub skill_entity: Entity,
    pub caster: Entity,
    pub target: Option<Entity>,
    pub payload: SkillArgs,
}

pub trait SkillAction: Send + Sync + 'static {
    fn validate(&self, args: &SkillArgs, registry: &SkillActionRegistry) -> Result<(), SkillError>;
    fn emit(
        &self,
        ctx: &SkillContext,
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult;
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkillRuntimeNode {
    Sequence(Vec<SkillRuntimeNode>),
    Parallel(Vec<SkillRuntimeNode>),
    Delay(SkillExpr, Box<SkillRuntimeNode>),
    Repeat {
        times: Option<SkillExpr>,
        duration: Option<SkillExpr>,
        interval: Option<SkillExpr>,
        node: Box<SkillRuntimeNode>,
    },
    If {
        condition: SkillExpr,
        then_node: Box<SkillRuntimeNode>,
        else_node: Option<Box<SkillRuntimeNode>>,
    },
    Let(String, SkillValue, Box<SkillRuntimeNode>),
    On(String, Box<SkillRuntimeNode>),
    Emit(String, SkillArgs),
    Action(String, SkillActionInput),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillActionInput {
    pub args: SkillArgs,
    pub payloads: IndexMap<String, SkillRuntimeNode>,
}

impl SkillActionInput {
    pub fn payload(&self, name: &str) -> Option<SkillRuntimeNode> {
        self.payloads.get(name).cloned()
    }
}

#[derive(Resource, Clone, Default)]
pub struct SkillActionRegistry {
    actions: IndexMap<String, Arc<dyn SkillAction>>,
}

impl SkillActionRegistry {
    pub fn new() -> Self {
        Self {
            actions: IndexMap::new(),
        }
    }

    pub fn register_skill_action<A>(&mut self, id: impl Into<String>, action: A) -> &mut Self
    where
        A: SkillAction,
    {
        self.actions.insert(id.into(), Arc::new(action));
        self
    }

    pub fn action(&self, id: &str) -> Option<Arc<dyn SkillAction>> {
        self.actions.get(id).cloned()
    }

    pub fn has_action(&self, id: &str) -> bool {
        self.actions.contains_key(id)
    }
}

#[derive(Resource, Default, Clone, Debug)]
pub struct SkillRuntimeCounters {
    next_execution_id: u64,
}

impl SkillRuntimeCounters {
    pub fn next_execution_id(&mut self) -> u64 {
        self.next_execution_id += 1;
        self.next_execution_id
    }
}

#[derive(Resource, Default, Clone, Debug)]
pub struct SkillResourcePools {
    values: HashMap<(Entity, String), f64>,
}

impl SkillResourcePools {
    pub fn get(&self, owner: Entity, resource: &str) -> f64 {
        self.values
            .get(&(owner, resource.to_owned()))
            .copied()
            .unwrap_or(0.0)
    }

    pub fn set(&mut self, owner: Entity, resource: impl Into<String>, value: f64) {
        self.values.insert((owner, resource.into()), value);
    }

    pub fn add(&mut self, owner: Entity, resource: impl Into<String>, amount: f64) {
        let resource = resource.into();
        let current = self.get(owner, &resource);
        self.set(owner, resource, current + amount);
    }

    fn spend(&mut self, owner: Entity, resource: &str, amount: f64) -> Result<(), String> {
        let current = self.get(owner, resource);
        if current + f64::EPSILON < amount {
            return Err(format!(
                "not enough `{resource}`: required {amount}, available {current}"
            ));
        }
        self.set(owner, resource.to_owned(), current - amount);
        Ok(())
    }
}

#[derive(Resource, Default, Clone, Debug)]
pub struct SkillCooldowns {
    remaining: HashMap<(Entity, SkillId), f32>,
}

impl SkillCooldowns {
    pub fn remaining(&self, owner: Entity, skill: &SkillId) -> f32 {
        self.remaining
            .get(&(owner, skill.clone()))
            .copied()
            .unwrap_or(0.0)
    }

    pub fn start(&mut self, owner: Entity, skill: SkillId, seconds: f32) {
        if seconds > 0.0 {
            self.remaining.insert((owner, skill), seconds);
        }
    }

    fn tick(&mut self, delta_seconds: f32) {
        self.remaining.retain(|_, remaining| {
            *remaining -= delta_seconds;
            *remaining > 0.0
        });
    }
}

#[derive(Component, Clone, Debug, PartialEq)]
pub struct ActiveSkill {
    pub skill_id: SkillId,
    pub execution_id: u64,
    pub caster: Entity,
    pub target: Option<Entity>,
}

#[derive(Component, Clone, Debug, PartialEq)]
pub struct SkillExecutionState {
    pub branches: Vec<SkillBranch>,
}

impl SkillExecutionState {
    pub fn new(branches: Vec<SkillBranch>) -> Self {
        Self { branches }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SkillBranch {
    pub wait: SkillWait,
    pub node: SkillRuntimeNode,
    pub ctx: SkillContext,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkillWait {
    Delay(Timer),
    Signal(String),
    EffectResolved { request_id: u64 },
}

#[derive(Default)]
pub struct SkillActionOutput {
    pub intents: Vec<SkillIntent>,
    pub effect_requests: Vec<SkillEffectRequest>,
    pub effect_resolved: Vec<SkillEffectResolved>,
    pub waits: Vec<SkillActionWait>,
    pub effects: Vec<SkillEffectSpawn>,
    pub vars: SkillArgs,
}

pub enum SkillActionWait {
    EffectResolved { request_id: u64 },
}

pub struct SkillEffectSpawn {
    pub request: SkillEffectRequest,
    pub duration_seconds: Option<f64>,
    pub on_add: Option<SkillRuntimeNode>,
    pub on_remove: Option<SkillRuntimeNode>,
    pub ctx: SkillContext,
}

impl SkillActionOutput {
    pub fn emit_intent(
        &mut self,
        ctx: &SkillContext,
        kind: impl Into<String>,
        payload: SkillArgs,
    ) -> Result<(), SkillError> {
        let skill_entity = ctx
            .skill_entity
            .ok_or_else(|| SkillError::Runtime("skill context is missing its entity".to_owned()))?;
        let caster = ctx
            .caster
            .ok_or_else(|| SkillError::Runtime("skill context is missing its caster".to_owned()))?;
        self.intents.push(SkillIntent {
            kind: kind.into(),
            skill_entity,
            caster,
            target: ctx.current_target,
            payload,
        });
        Ok(())
    }

    pub fn set_var(&mut self, name: impl Into<String>, value: SkillValue) {
        self.vars.insert(name.into(), value);
    }

    pub fn emit_effect_request(
        &mut self,
        ctx: &SkillContext,
        kind: impl Into<String>,
        target: Option<Entity>,
        payload: SkillArgs,
        mode: SettlementMode,
    ) -> Result<u64, SkillError> {
        let kind = kind.into();
        let request_id = next_protocol_request_id(ctx, self.effect_requests.len() as u64);
        let request = SkillEffectRequest {
            request_id,
            execution_id: ctx.execution_id,
            source: ctx.caster,
            target,
            kind: kind.clone(),
            payload: payload.clone(),
            mode,
        };
        self.effect_requests.push(request);
        match mode {
            SettlementMode::Sync => self.effect_resolved.push(SkillEffectResolved {
                request_id,
                execution_id: ctx.execution_id,
                source: ctx.caster,
                target,
                kind,
                payload,
            }),
            SettlementMode::Request => {}
            SettlementMode::Await => self
                .waits
                .push(SkillActionWait::EffectResolved { request_id }),
        }
        Ok(request_id)
    }

    pub fn emit_timed_effect(
        &mut self,
        ctx: &SkillContext,
        kind: impl Into<String>,
        target: Option<Entity>,
        payload: SkillArgs,
        duration_seconds: Option<f64>,
        on_add: Option<SkillRuntimeNode>,
        on_remove: Option<SkillRuntimeNode>,
    ) -> Result<u64, SkillError> {
        let request_id = next_protocol_request_id(ctx, self.effect_requests.len() as u64);
        let request = SkillEffectRequest {
            request_id,
            execution_id: ctx.execution_id,
            source: ctx.caster,
            target,
            kind: kind.into(),
            payload,
            mode: SettlementMode::Request,
        };
        self.effect_requests.push(request.clone());
        self.effects.push(SkillEffectSpawn {
            request,
            duration_seconds,
            on_add,
            on_remove,
            ctx: ctx.clone(),
        });
        Ok(request_id)
    }
}

#[derive(Default)]
struct ExecutionOutput {
    branches: Vec<SkillBranch>,
    intents: Vec<SkillIntent>,
    signals: Vec<SkillRuntimeSignal>,
    effect_requests: Vec<SkillEffectRequest>,
    effect_resolved: Vec<SkillEffectResolved>,
    effects: Vec<SkillEffectSpawn>,
}

#[derive(Component, Clone, Debug, PartialEq)]
pub struct ActiveSkillEffect {
    pub execution_id: u64,
    pub skill_id: SkillId,
    pub caster: Option<Entity>,
    pub target: Option<Entity>,
    pub kind: String,
    pub timer: Option<Timer>,
    pub on_remove: Option<SkillRuntimeNode>,
    pub ctx: SkillContext,
}

#[cfg(feature = "full_runtime_entities")]
#[derive(Component, Clone, Debug, PartialEq)]
pub struct SkillRuntimeDebugNode {
    pub execution_id: u64,
    pub graph_node: SkillNodeId,
    pub kind: String,
}

pub fn handle_skill_cast_requests(
    mut commands: Commands,
    mut requests: MessageReader<SkillCastRequest>,
    library: Res<SkillLibrary>,
    registry: Res<SkillActionRegistry>,
    config: Res<SkillRuntimeConfig>,
    mut resources: ResMut<SkillResourcePools>,
    mut cooldowns: ResMut<SkillCooldowns>,
    mut counters: ResMut<SkillRuntimeCounters>,
) {
    for request in requests.read() {
        let Some(compiled) = library.get(&request.skill) else {
            commands.write_message(SkillCastRejected {
                skill: request.skill.clone(),
                caster: request.caster,
                target: request.target,
                message: "compiled skill not found".to_owned(),
            });
            continue;
        };

        let mut requirement_ctx = SkillContext::new(compiled, Some(request.caster), 0);
        requirement_ctx.stats = graph_stats(&compiled.graph);
        requirement_ctx.current_target = request.target;
        if let Err(message) = validate_and_pay_requirements(
            compiled,
            request.caster,
            &requirement_ctx,
            &mut resources,
            &mut cooldowns,
        ) {
            commands.write_message(SkillCastRejected {
                skill: request.skill.clone(),
                caster: request.caster,
                target: request.target,
                message,
            });
            continue;
        }

        let execution_id = counters.next_execution_id();
        let skill_entity = commands.spawn_empty().id();
        let active = ActiveSkill {
            skill_id: compiled.id.clone(),
            execution_id,
            caster: request.caster,
            target: request.target,
        };
        commands.entity(skill_entity).insert(active.clone());
        #[cfg(feature = "full_runtime_entities")]
        materialize_runtime_entities(&mut commands, skill_entity, execution_id, &compiled.graph);
        commands.write_message(SkillCastAccepted {
            skill: compiled.id.clone(),
            execution_id,
            caster: request.caster,
            target: request.target,
        });

        let root = match runtime_node_from_graph(&compiled.graph) {
            Ok(root) => root,
            Err(err) => {
                commands.entity(skill_entity).despawn();
                commands.write_message(SkillExecutionFailed {
                    skill: Some(compiled.id.clone()),
                    execution_id: Some(execution_id),
                    skill_entity: Some(skill_entity),
                    message: err.to_string(),
                });
                continue;
            }
        };
        let mut ctx = SkillContext::new(compiled, Some(request.caster), execution_id);
        ctx.stats = graph_stats(&compiled.graph);
        ctx.step_budget = config.step_budget;
        ctx.step_budget_remaining = config.step_budget;
        ctx.current_target = request.target;
        ctx.skill_entity = Some(skill_entity);
        match execute_node(&root, &mut ctx, &registry) {
            Ok(output) if output.branches.is_empty() => {
                let output = process_effect_spawns_commands(output, &registry, &mut commands);
                write_output_commands(output, &mut commands);
                commands.entity(skill_entity).despawn();
                commands.write_message(SkillExecutionFinished {
                    skill: compiled.id.clone(),
                    execution_id,
                    caster: request.caster,
                    target: request.target,
                });
            }
            Ok(output) => {
                let branches = output.branches.clone();
                let output = process_effect_spawns_commands(output, &registry, &mut commands);
                write_output_commands(output, &mut commands);
                commands
                    .entity(skill_entity)
                    .insert(SkillExecutionState::new(branches));
            }
            Err(err) => {
                commands.entity(skill_entity).despawn();
                commands.write_message(SkillExecutionFailed {
                    skill: Some(compiled.id.clone()),
                    execution_id: Some(execution_id),
                    skill_entity: Some(skill_entity),
                    message: err.to_string(),
                });
            }
        }
    }
}

pub fn tick_skill_cooldowns(time: Res<Time>, mut cooldowns: ResMut<SkillCooldowns>) {
    cooldowns.tick(time.delta().as_secs_f32());
}

pub fn skill_observer_trigger_bridge(trigger: On<SkillObserverTrigger>, mut commands: Commands) {
    commands.write_message(SkillRuntimeSignal {
        name: trigger.name.clone(),
        payload: trigger.payload.clone(),
        execution_id: trigger.execution_id,
        skill_entity: trigger.skill_entity,
        caster: trigger.caster,
        target: trigger.target,
    });
}

fn validate_and_pay_requirements(
    compiled: &SkillCompiled,
    caster: Entity,
    ctx: &SkillContext,
    resources: &mut SkillResourcePools,
    cooldowns: &mut SkillCooldowns,
) -> Result<(), String> {
    let mut costs = Vec::new();
    let mut cooldown_seconds = None;

    for requirement in &compiled.graph.requirements {
        match requirement {
            SkillRequirement::Cost { resource, amount } => {
                let amount = number(
                    &eval_skill_expr(&SkillExpr::new(amount.0.clone()), ctx)
                        .map_err(|err| err.to_string())?,
                )
                .map_err(|err| err.to_string())?;
                if amount > 0.0 {
                    costs.push((resource.clone(), amount));
                }
            }
            SkillRequirement::Cooldown { seconds } => {
                let seconds = number(
                    &eval_skill_expr(&SkillExpr::new(seconds.0.clone()), ctx)
                        .map_err(|err| err.to_string())?,
                )
                .map_err(|err| err.to_string())?;
                cooldown_seconds = Some(cooldown_seconds.unwrap_or(0.0_f64).max(seconds));
            }
        }
    }

    let remaining = cooldowns.remaining(caster, &compiled.id);
    if remaining > 0.0 {
        return Err(format!(
            "skill `{}` is on cooldown for {:.2}s",
            compiled.id, remaining
        ));
    }

    for (resource, amount) in &costs {
        let available = resources.get(caster, resource);
        if available + f64::EPSILON < *amount {
            return Err(format!(
                "not enough `{resource}`: required {amount}, available {available}"
            ));
        }
    }

    for (resource, amount) in costs {
        resources.spend(caster, &resource, amount)?;
    }
    if let Some(seconds) = cooldown_seconds {
        cooldowns.start(caster, compiled.id.clone(), seconds as f32);
    }
    Ok(())
}

pub fn tick_skill_delays(
    mut commands: Commands,
    time: Res<Time>,
    registry: Res<SkillActionRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
    mut effect_requests: MessageWriter<SkillEffectRequest>,
    mut effect_resolved: MessageWriter<SkillEffectResolved>,
) {
    for (entity, active, mut state) in &mut query {
        let mut next_branches = Vec::new();
        let mut failed_entity = false;
        for mut branch in state.branches.drain(..) {
            match &mut branch.wait {
                SkillWait::Delay(timer) => {
                    timer.tick(time.delta());
                    if timer.is_finished() {
                        match execute_node(&branch.node, &mut branch.ctx, &registry) {
                            Ok(output) => {
                                next_branches.extend(output.branches.clone());
                                let output = process_effect_spawns(
                                    output,
                                    &registry,
                                    &mut commands,
                                    &mut failed,
                                );
                                write_output(
                                    output,
                                    &mut intents,
                                    &mut signals,
                                    &mut effect_requests,
                                    &mut effect_resolved,
                                );
                            }
                            Err(err) => {
                                commands.entity(entity).despawn();
                                failed.write(SkillExecutionFailed {
                                    skill: Some(active.skill_id.clone()),
                                    execution_id: Some(active.execution_id),
                                    skill_entity: Some(entity),
                                    message: err.to_string(),
                                });
                                next_branches.clear();
                                failed_entity = true;
                                break;
                            }
                        }
                    } else {
                        next_branches.push(branch);
                    }
                }
                SkillWait::Signal(_) | SkillWait::EffectResolved { .. } => {
                    next_branches.push(branch)
                }
            }
        }
        if failed_entity {
            continue;
        }
        finish_or_store(
            entity,
            active,
            next_branches,
            &mut state,
            &mut commands,
            &mut finished,
        );
    }
}

pub fn resume_skill_signals(
    mut commands: Commands,
    mut signal_reader: MessageReader<SkillRuntimeSignal>,
    registry: Res<SkillActionRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut effect_requests: MessageWriter<SkillEffectRequest>,
) {
    let incoming = signal_reader.read().cloned().collect::<Vec<_>>();
    if incoming.is_empty() {
        return;
    }

    for (entity, active, mut state) in &mut query {
        let mut next_branches = Vec::new();
        let mut failed_entity = false;
        for mut branch in state.branches.drain(..) {
            let matching = match &branch.wait {
                SkillWait::Signal(name) => incoming.iter().find(|signal| {
                    signal.name == *name
                        && signal
                            .execution_id
                            .is_none_or(|execution_id| execution_id == branch.ctx.execution_id)
                        && signal
                            .skill_entity
                            .is_none_or(|entity| Some(entity) == branch.ctx.skill_entity)
                }),
                SkillWait::Delay(_) | SkillWait::EffectResolved { .. } => None,
            };

            if let Some(signal) = matching {
                branch.ctx.source_event = Some(signal.clone());
                branch.ctx.current_target = signal.target.or(branch.ctx.current_target);
                match execute_node(&branch.node, &mut branch.ctx, &registry) {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output =
                            process_effect_spawns(output, &registry, &mut commands, &mut failed);
                        write_output_deferred_signals(
                            output,
                            &mut intents,
                            &mut commands,
                            &mut effect_requests,
                        );
                    }
                    Err(err) => {
                        commands.entity(entity).despawn();
                        failed.write(SkillExecutionFailed {
                            skill: Some(active.skill_id.clone()),
                            execution_id: Some(active.execution_id),
                            skill_entity: Some(entity),
                            message: err.to_string(),
                        });
                        next_branches.clear();
                        failed_entity = true;
                        break;
                    }
                }
            } else {
                next_branches.push(branch);
            }
        }
        if failed_entity {
            continue;
        }
        finish_or_store(
            entity,
            active,
            next_branches,
            &mut state,
            &mut commands,
            &mut finished,
        );
    }
}

pub fn resume_effect_resolved(
    mut commands: Commands,
    mut resolved_reader: MessageReader<SkillEffectResolved>,
    registry: Res<SkillActionRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut effect_requests: MessageWriter<SkillEffectRequest>,
) {
    let incoming = resolved_reader.read().cloned().collect::<Vec<_>>();
    if incoming.is_empty() {
        return;
    }

    for (entity, active, mut state) in &mut query {
        let mut next_branches = Vec::new();
        let mut failed_entity = false;
        for mut branch in state.branches.drain(..) {
            let matching = match &branch.wait {
                SkillWait::EffectResolved { request_id } => incoming.iter().find(|resolved| {
                    resolved.execution_id == branch.ctx.execution_id
                        && resolved.request_id == *request_id
                }),
                SkillWait::Delay(_) | SkillWait::Signal(_) => None,
            };

            if let Some(resolved) = matching {
                seed_effect_result(&mut branch.ctx, resolved);
                match execute_node(&branch.node, &mut branch.ctx, &registry) {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output =
                            process_effect_spawns(output, &registry, &mut commands, &mut failed);
                        write_output_deferred_signals(
                            output,
                            &mut intents,
                            &mut commands,
                            &mut effect_requests,
                        );
                    }
                    Err(err) => {
                        commands.entity(entity).despawn();
                        failed.write(SkillExecutionFailed {
                            skill: Some(active.skill_id.clone()),
                            execution_id: Some(active.execution_id),
                            skill_entity: Some(entity),
                            message: err.to_string(),
                        });
                        next_branches.clear();
                        failed_entity = true;
                        break;
                    }
                }
            } else {
                next_branches.push(branch);
            }
        }
        if failed_entity {
            continue;
        }
        finish_or_store(
            entity,
            active,
            next_branches,
            &mut state,
            &mut commands,
            &mut finished,
        );
    }
}

pub fn tick_skill_effects(
    mut commands: Commands,
    time: Res<Time>,
    registry: Res<SkillActionRegistry>,
    mut query: Query<(Entity, &mut ActiveSkillEffect)>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
    mut effect_requests: MessageWriter<SkillEffectRequest>,
    mut effect_resolved: MessageWriter<SkillEffectResolved>,
) {
    for (entity, mut effect) in &mut query {
        let Some(timer) = &mut effect.timer else {
            continue;
        };
        timer.tick(time.delta());
        if !timer.is_finished() {
            continue;
        }

        if let Some(on_remove) = effect.on_remove.clone() {
            match execute_node(&on_remove, &mut effect.ctx, &registry) {
                Ok(output) => {
                    let output =
                        process_effect_spawns(output, &registry, &mut commands, &mut failed);
                    write_output(
                        output,
                        &mut intents,
                        &mut signals,
                        &mut effect_requests,
                        &mut effect_resolved,
                    );
                }
                Err(err) => {
                    failed.write(SkillExecutionFailed {
                        skill: Some(effect.skill_id.clone()),
                        execution_id: Some(effect.execution_id),
                        skill_entity: None,
                        message: err.to_string(),
                    });
                }
            }
        }
        commands.entity(entity).despawn();
    }
}

fn execute_node(
    node: &SkillRuntimeNode,
    ctx: &mut SkillContext,
    registry: &SkillActionRegistry,
) -> Result<ExecutionOutput, SkillError> {
    spend_step(ctx)?;
    match node {
        SkillRuntimeNode::Sequence(nodes) => execute_sequence(nodes, ctx, registry),
        SkillRuntimeNode::Parallel(nodes) => {
            let mut output = ExecutionOutput::default();
            for node in nodes {
                output.extend(execute_node(node, ctx, registry)?);
            }
            Ok(output)
        }
        SkillRuntimeNode::Delay(seconds, node) => {
            let seconds = number(&eval_skill_expr(seconds, ctx)?)?;
            Ok(ExecutionOutput {
                branches: vec![SkillBranch {
                    wait: SkillWait::Delay(Timer::from_seconds(seconds as f32, TimerMode::Once)),
                    node: (**node).clone(),
                    ctx: ctx.clone(),
                }],
                ..Default::default()
            })
        }
        SkillRuntimeNode::Repeat {
            times,
            interval,
            node,
            ..
        } => {
            let times = match times {
                Some(expr) => number(&eval_skill_expr(expr, ctx)?)? as usize,
                None => 1,
            };
            let mut sequence = Vec::new();
            for _ in 0..times {
                if let Some(interval) = interval {
                    sequence.push(SkillRuntimeNode::Delay(interval.clone(), node.clone()));
                } else {
                    sequence.push((**node).clone());
                }
            }
            execute_node(&SkillRuntimeNode::Sequence(sequence), ctx, registry)
        }
        SkillRuntimeNode::If {
            condition,
            then_node,
            else_node,
        } => {
            let branch = if truthy(&eval_skill_expr(condition, ctx)?) {
                Some(then_node.as_ref())
            } else {
                else_node.as_deref()
            };
            if let Some(branch) = branch {
                execute_node(branch, ctx, registry)
            } else {
                Ok(ExecutionOutput::default())
            }
        }
        SkillRuntimeNode::Let(name, value, node) => {
            let previous = ctx.vars.insert(name.clone(), value.clone());
            let result = execute_node(node, ctx, registry);
            match previous {
                Some(previous) => ctx.vars.insert(name.clone(), previous),
                None => ctx.vars.shift_remove(name),
            };
            result
        }
        SkillRuntimeNode::On(name, node) => Ok(ExecutionOutput {
            branches: vec![SkillBranch {
                wait: SkillWait::Signal(name.clone()),
                node: (**node).clone(),
                ctx: ctx.clone(),
            }],
            ..Default::default()
        }),
        SkillRuntimeNode::Emit(name, payload) => {
            let payload = resolve_args(payload, ctx)?;
            Ok(ExecutionOutput {
                signals: vec![SkillRuntimeSignal {
                    name: name.clone(),
                    payload,
                    execution_id: Some(ctx.execution_id),
                    skill_entity: ctx.skill_entity,
                    caster: ctx.caster,
                    target: ctx.current_target,
                }],
                ..Default::default()
            })
        }
        SkillRuntimeNode::Action(id, input) => {
            let action = registry
                .action(id)
                .ok_or_else(|| SkillError::UnknownAction(id.clone()))?;
            let input = SkillActionInput {
                args: resolve_args(&input.args, ctx)?,
                payloads: input.payloads.clone(),
            };
            let mut output = SkillActionOutput::default();
            action.emit(ctx, &input, &mut output)?;
            for (key, value) in output.vars {
                ctx.vars.insert(key, value);
            }
            let branches = output
                .waits
                .into_iter()
                .map(|wait| SkillBranch {
                    wait: match wait {
                        SkillActionWait::EffectResolved { request_id } => {
                            SkillWait::EffectResolved { request_id }
                        }
                    },
                    node: SkillRuntimeNode::Sequence(Vec::new()),
                    ctx: ctx.clone(),
                })
                .collect();
            Ok(ExecutionOutput {
                branches,
                intents: output.intents,
                effect_requests: output.effect_requests,
                effect_resolved: output.effect_resolved,
                effects: output.effects,
                ..Default::default()
            })
        }
    }
}

fn spend_step(ctx: &mut SkillContext) -> Result<(), SkillError> {
    if ctx.step_budget_remaining == 0 {
        return Err(SkillError::Runtime(
            crate::SkillExecutionError::StepBudgetExhausted {
                steps: ctx.step_budget,
            }
            .to_string(),
        ));
    }
    ctx.step_budget_remaining -= 1;
    Ok(())
}

fn execute_sequence(
    nodes: &[SkillRuntimeNode],
    ctx: &mut SkillContext,
    registry: &SkillActionRegistry,
) -> Result<ExecutionOutput, SkillError> {
    let mut output = ExecutionOutput::default();
    for (index, node) in nodes.iter().enumerate() {
        let mut child = execute_node(node, ctx, registry)?;
        if !child.branches.is_empty() {
            let rest = nodes[index + 1..].to_vec();
            if !rest.is_empty() {
                for branch in &mut child.branches {
                    let mut sequence = Vec::with_capacity(rest.len() + 1);
                    sequence.push(branch.node.clone());
                    sequence.extend(rest.clone());
                    branch.node = SkillRuntimeNode::Sequence(sequence);
                }
            }
            output.extend(child);
            return Ok(output);
        }
        output.extend(child);
    }
    Ok(output)
}

fn write_output(
    output: ExecutionOutput,
    intents: &mut MessageWriter<SkillIntent>,
    signals: &mut MessageWriter<SkillRuntimeSignal>,
    effect_requests: &mut MessageWriter<SkillEffectRequest>,
    effect_resolved: &mut MessageWriter<SkillEffectResolved>,
) {
    for intent in output.intents {
        intents.write(intent);
    }
    for signal in output.signals {
        signals.write(signal);
    }
    for request in output.effect_requests {
        effect_requests.write(request);
    }
    for resolved in output.effect_resolved {
        effect_resolved.write(resolved);
    }
}

fn write_output_deferred_signals(
    output: ExecutionOutput,
    intents: &mut MessageWriter<SkillIntent>,
    commands: &mut Commands,
    effect_requests: &mut MessageWriter<SkillEffectRequest>,
) {
    for intent in output.intents {
        intents.write(intent);
    }
    for signal in output.signals {
        commands.write_message(signal);
    }
    for request in output.effect_requests {
        effect_requests.write(request);
    }
    for resolved in output.effect_resolved {
        commands.write_message(resolved);
    }
}

fn write_output_commands(output: ExecutionOutput, commands: &mut Commands) {
    for intent in output.intents {
        commands.write_message(intent);
    }
    for signal in output.signals {
        commands.write_message(signal);
    }
    for request in output.effect_requests {
        commands.write_message(request);
    }
    for resolved in output.effect_resolved {
        commands.write_message(resolved);
    }
}

fn finish_or_store(
    entity: Entity,
    active: &ActiveSkill,
    next_branches: Vec<SkillBranch>,
    state: &mut SkillExecutionState,
    commands: &mut Commands,
    finished: &mut MessageWriter<SkillExecutionFinished>,
) {
    if next_branches.is_empty() {
        commands.entity(entity).despawn();
        finished.write(SkillExecutionFinished {
            skill: active.skill_id.clone(),
            execution_id: active.execution_id,
            caster: active.caster,
            target: active.target,
        });
    } else {
        state.branches = next_branches;
    }
}

impl ExecutionOutput {
    fn extend(&mut self, other: ExecutionOutput) {
        self.branches.extend(other.branches);
        self.intents.extend(other.intents);
        self.signals.extend(other.signals);
        self.effect_requests.extend(other.effect_requests);
        self.effect_resolved.extend(other.effect_resolved);
        self.effects.extend(other.effects);
    }
}

fn process_effect_spawns(
    mut output: ExecutionOutput,
    registry: &SkillActionRegistry,
    commands: &mut Commands,
    failed: &mut MessageWriter<SkillExecutionFailed>,
) -> ExecutionOutput {
    let mut effects = std::mem::take(&mut output.effects);
    while let Some(mut spawn) = effects.pop() {
        if let Some(on_add) = spawn.on_add.take() {
            let mut ctx = spawn.ctx.clone();
            match execute_node(&on_add, &mut ctx, registry) {
                Ok(hook_output) => {
                    output.extend(hook_output);
                    effects.extend(std::mem::take(&mut output.effects));
                }
                Err(err) => {
                    failed.write(SkillExecutionFailed {
                        skill: Some(spawn.ctx.skill_id.clone()),
                        execution_id: Some(spawn.ctx.execution_id),
                        skill_entity: spawn.ctx.skill_entity,
                        message: err.to_string(),
                    });
                }
            }
            spawn.ctx = ctx;
        }

        let timer = spawn
            .duration_seconds
            .map(|seconds| Timer::from_seconds(seconds.max(0.0) as f32, TimerMode::Once));
        commands.spawn(ActiveSkillEffect {
            execution_id: spawn.request.execution_id,
            skill_id: spawn.ctx.skill_id.clone(),
            caster: spawn.ctx.caster,
            target: spawn.request.target,
            kind: spawn.request.kind,
            timer,
            on_remove: spawn.on_remove,
            ctx: spawn.ctx,
        });
    }
    output
}

fn process_effect_spawns_commands(
    mut output: ExecutionOutput,
    registry: &SkillActionRegistry,
    commands: &mut Commands,
) -> ExecutionOutput {
    let mut effects = std::mem::take(&mut output.effects);
    while let Some(mut spawn) = effects.pop() {
        if let Some(on_add) = spawn.on_add.take() {
            let mut ctx = spawn.ctx.clone();
            match execute_node(&on_add, &mut ctx, registry) {
                Ok(hook_output) => {
                    output.extend(hook_output);
                    effects.extend(std::mem::take(&mut output.effects));
                }
                Err(err) => {
                    commands.write_message(SkillExecutionFailed {
                        skill: Some(spawn.ctx.skill_id.clone()),
                        execution_id: Some(spawn.ctx.execution_id),
                        skill_entity: spawn.ctx.skill_entity,
                        message: err.to_string(),
                    });
                }
            }
            spawn.ctx = ctx;
        }

        let timer = spawn
            .duration_seconds
            .map(|seconds| Timer::from_seconds(seconds.max(0.0) as f32, TimerMode::Once));
        commands.spawn(ActiveSkillEffect {
            execution_id: spawn.request.execution_id,
            skill_id: spawn.ctx.skill_id.clone(),
            caster: spawn.ctx.caster,
            target: spawn.request.target,
            kind: spawn.request.kind,
            timer,
            on_remove: spawn.on_remove,
            ctx: spawn.ctx,
        });
    }
    output
}

fn seed_effect_result(ctx: &mut SkillContext, resolved: &SkillEffectResolved) {
    ctx.source_event = Some(effect_resolved_signal(resolved));
}

fn effect_resolved_signal(resolved: &SkillEffectResolved) -> SkillRuntimeSignal {
    SkillRuntimeSignal {
        name: format!("{}_resolved", resolved.kind),
        payload: resolved.payload.clone(),
        execution_id: Some(resolved.execution_id),
        skill_entity: None,
        caster: resolved.source,
        target: resolved.target,
    }
}

fn next_protocol_request_id(ctx: &SkillContext, local_index: u64) -> u64 {
    let step_index = u64::from(ctx.step_budget.saturating_sub(ctx.step_budget_remaining));
    (ctx.execution_id << 32) | (step_index << 16) | local_index
}

fn number(value: &SkillValue) -> Result<f64, SkillError> {
    match value {
        SkillValue::Number(value) => Ok(*value),
        other => Err(SkillError::Runtime(format!(
            "expected number, got `{other:?}`"
        ))),
    }
}

fn truthy(value: &SkillValue) -> bool {
    match value {
        SkillValue::Bool(value) => *value,
        SkillValue::Number(value) => *value != 0.0,
        SkillValue::String(value) => !value.is_empty(),
        SkillValue::Null => false,
        SkillValue::List(value) => !value.is_empty(),
        SkillValue::Map(value) => !value.is_empty(),
        SkillValue::Special(_) => true,
    }
}

fn runtime_node_from_graph(graph: &SkillGraph) -> Result<SkillRuntimeNode, SkillError> {
    let root = graph
        .root
        .ok_or_else(|| SkillError::Runtime("skill graph has no root node".to_owned()))?;
    runtime_node_from_graph_id(graph, root)
}

fn runtime_node_from_graph_id(
    graph: &SkillGraph,
    id: SkillNodeId,
) -> Result<SkillRuntimeNode, SkillError> {
    let node = graph.node(id).ok_or_else(|| {
        SkillError::Runtime(format!("skill graph references missing node `{id:?}`"))
    })?;
    match &node.kind {
        SkillGraphNodeKind::Sequence => Ok(SkillRuntimeNode::Sequence(graph_child_nodes_or_empty(
            graph, id, "items",
        )?)),
        SkillGraphNodeKind::Parallel => Ok(SkillRuntimeNode::Parallel(graph_child_nodes_or_empty(
            graph, id, "branches",
        )?)),
        SkillGraphNodeKind::Delay { seconds } => {
            let child = single_graph_child(graph, id, "then")?;
            Ok(SkillRuntimeNode::Delay(
                SkillExpr::new(seconds.0.clone()),
                Box::new(child),
            ))
        }
        SkillGraphNodeKind::Repeat {
            times,
            duration,
            interval,
        } => {
            let child = single_graph_child(graph, id, "body")?;
            Ok(SkillRuntimeNode::Repeat {
                times: times.as_ref().map(|expr| SkillExpr::new(expr.0.clone())),
                duration: duration.as_ref().map(|expr| SkillExpr::new(expr.0.clone())),
                interval: interval.as_ref().map(|expr| SkillExpr::new(expr.0.clone())),
                node: Box::new(child),
            })
        }
        SkillGraphNodeKind::If { condition } => {
            let then_node = single_graph_child(graph, id, "then")?;
            let else_node = graph
                .node(id)
                .and_then(|node| node.children.get("else"))
                .and_then(|children| children.first().copied())
                .map(|child| runtime_node_from_graph_id(graph, child))
                .transpose()?;
            Ok(SkillRuntimeNode::If {
                condition: SkillExpr::new(condition.0.clone()),
                then_node: Box::new(then_node),
                else_node: else_node.map(Box::new),
            })
        }
        SkillGraphNodeKind::WaitEvent { event } => {
            let payload = graph_payload_nodes(graph, id, event)?;
            Ok(SkillRuntimeNode::On(
                event.clone(),
                Box::new(sequence_or_single(payload)),
            ))
        }
        SkillGraphNodeKind::EmitSkillEvent { event, payload } => {
            Ok(SkillRuntimeNode::Emit(event.clone(), payload.clone()))
        }
        SkillGraphNodeKind::SetSkillVar { name, value } => {
            let child = single_graph_child(graph, id, "then")?;
            Ok(SkillRuntimeNode::Let(
                name.clone(),
                value.clone(),
                Box::new(child),
            ))
        }
        SkillGraphNodeKind::WithSkillContext => Ok(SkillRuntimeNode::Sequence(
            graph_child_nodes_or_empty(graph, id, "then")
                .or_else(|_| graph_child_nodes_or_empty(graph, id, "items"))?,
        )),
        SkillGraphNodeKind::Extension { constructor, args } => {
            let mut input = SkillActionInput {
                args: args.clone(),
                payloads: IndexMap::new(),
            };
            let graph_node = graph.node(id).expect("validated node");
            for (slot, payloads) in &graph_node.payloads {
                let nodes = payloads
                    .iter()
                    .map(|payload| runtime_node_from_graph_id(graph, *payload))
                    .collect::<Result<Vec<_>, _>>()?;
                input
                    .payloads
                    .insert(slot.clone(), sequence_or_single(nodes));
            }
            Ok(SkillRuntimeNode::Action(constructor.clone(), input))
        }
    }
}

fn graph_child_nodes(
    graph: &SkillGraph,
    id: SkillNodeId,
    slot: &str,
) -> Result<Vec<SkillRuntimeNode>, SkillError> {
    graph
        .node(id)
        .and_then(|node| node.children.get(slot))
        .ok_or_else(|| SkillError::Runtime(format!("node `{id:?}` is missing `{slot}` children")))?
        .iter()
        .map(|child| runtime_node_from_graph_id(graph, *child))
        .collect()
}

fn graph_child_nodes_or_empty(
    graph: &SkillGraph,
    id: SkillNodeId,
    slot: &str,
) -> Result<Vec<SkillRuntimeNode>, SkillError> {
    graph
        .node(id)
        .and_then(|node| node.children.get(slot))
        .map(|children| {
            children
                .iter()
                .map(|child| runtime_node_from_graph_id(graph, *child))
                .collect()
        })
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn graph_payload_nodes(
    graph: &SkillGraph,
    id: SkillNodeId,
    slot: &str,
) -> Result<Vec<SkillRuntimeNode>, SkillError> {
    graph
        .node(id)
        .and_then(|node| node.payloads.get(slot))
        .ok_or_else(|| SkillError::Runtime(format!("node `{id:?}` is missing `{slot}` payload")))?
        .iter()
        .map(|payload| runtime_node_from_graph_id(graph, *payload))
        .collect()
}

fn single_graph_child(
    graph: &SkillGraph,
    id: SkillNodeId,
    slot: &str,
) -> Result<SkillRuntimeNode, SkillError> {
    let mut children = graph_child_nodes(graph, id, slot)?;
    if children.len() != 1 {
        return Err(SkillError::Runtime(format!(
            "node `{id:?}` expected exactly one `{slot}` child, got {}",
            children.len()
        )));
    }
    Ok(children.remove(0))
}

fn sequence_or_single(mut nodes: Vec<SkillRuntimeNode>) -> SkillRuntimeNode {
    if nodes.len() == 1 {
        nodes.remove(0)
    } else {
        SkillRuntimeNode::Sequence(nodes)
    }
}

fn graph_stats(graph: &SkillGraph) -> SkillArgs {
    graph.params.clone()
}

#[cfg(feature = "full_runtime_entities")]
fn materialize_runtime_entities(
    commands: &mut Commands,
    execution_entity: Entity,
    execution_id: u64,
    graph: &SkillGraph,
) {
    let mut entities = Vec::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let entity = commands
            .spawn((
                SkillRuntimeDebugNode {
                    execution_id,
                    graph_node: node.id,
                    kind: graph_node_kind_name(&node.kind).to_owned(),
                },
                ExecutionOfSkill {
                    skill: execution_entity,
                },
            ))
            .id();
        entities.push((node.id, entity));
    }

    let entity_for = |id: SkillNodeId, entities: &[(SkillNodeId, Entity)]| {
        entities
            .iter()
            .find_map(|(node_id, entity)| (*node_id == id).then_some(*entity))
    };

    if let Some(root) = graph.root.and_then(|id| entity_for(id, &entities)) {
        commands.entity(root).insert(SkillRootOf {
            graph: execution_entity,
        });
    }

    for node in &graph.nodes {
        let Some(parent) = entity_for(node.id, &entities) else {
            continue;
        };
        for (slot, children) in &node.children {
            for (order, child) in children.iter().enumerate() {
                if let Some(child_entity) = entity_for(*child, &entities) {
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
                if let Some(payload_entity) = entity_for(*payload, &entities) {
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

#[cfg(feature = "full_runtime_entities")]
fn graph_node_kind_name(kind: &SkillGraphNodeKind) -> &'static str {
    match kind {
        SkillGraphNodeKind::Sequence => "Sequence",
        SkillGraphNodeKind::Parallel => "Parallel",
        SkillGraphNodeKind::Delay { .. } => "Delay",
        SkillGraphNodeKind::Repeat { .. } => "Repeat",
        SkillGraphNodeKind::If { .. } => "If",
        SkillGraphNodeKind::WaitEvent { .. } => "WaitEvent",
        SkillGraphNodeKind::EmitSkillEvent { .. } => "EmitSkillEvent",
        SkillGraphNodeKind::SetSkillVar { .. } => "SetSkillVar",
        SkillGraphNodeKind::WithSkillContext => "WithSkillContext",
        SkillGraphNodeKind::Extension { .. } => "Extension",
    }
}
