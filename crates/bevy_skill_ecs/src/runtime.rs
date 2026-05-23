use crate::expr::{eval_skill_expr, resolve_args};
use crate::{
    CompiledSkill, CompiledSkillParams, CompiledSkillTags, Delay, EmitSkillEvent, ExecutionOfSkill,
    If, Parallel, Repeat, Sequence, SetSkillVar, SettlementMode, SkillCastAccepted,
    SkillCastRejected, SkillCastRequest, SkillChildOf, SkillChildren, SkillEffectRequest,
    SkillEffectResolved, SkillExecutionFinished, SkillExpr, SkillExtension, SkillId, SkillLibrary,
    SkillParams as SkillArgs, SkillPayloadOf, SkillPayloads, SkillRequirement, SkillRequirements,
    SkillRoots, SkillRuntimeConfig, SkillValue, WaitEvent, WithSkillContext,
};
use bevy::ecs::relationship::RelationshipTarget;
use bevy::ecs::system::SystemParam;
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
    pub fn new(
        skill_id: SkillId,
        stats: SkillArgs,
        tags: IndexSet<String>,
        caster: Option<Entity>,
        execution_id: u64,
    ) -> Self {
        Self {
            skill_entity: None,
            caster,
            skill_id,
            current_target: None,
            source_event: None,
            vars: SkillArgs::new(),
            stats,
            tags,
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SkillActionInput {
    pub args: SkillArgs,
    pub payloads: IndexMap<String, Vec<Entity>>,
}

impl SkillActionInput {
    pub fn payload(&self, name: &str) -> Option<Entity> {
        self.payloads
            .get(name)
            .and_then(|payloads| payloads.first().copied())
    }

    pub fn payloads(&self, name: &str) -> &[Entity] {
        self.payloads.get(name).map(Vec::as_slice).unwrap_or(&[])
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
    pub compiled_skill: Entity,
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
    pub continuation: Vec<SkillContinuation>,
    pub ctx: SkillContext,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkillContinuation {
    Node(Entity),
    DelayThen { seconds: SkillExpr, node: Entity },
}

impl SkillContinuation {
    fn node(&self) -> Entity {
        match self {
            Self::Node(node) | Self::DelayThen { node, .. } => *node,
        }
    }
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
    pub on_add: Option<Entity>,
    pub on_remove: Option<Entity>,
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
        on_add: Option<Entity>,
        on_remove: Option<Entity>,
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
    pub on_remove: Option<Entity>,
    pub ctx: SkillContext,
}

#[derive(SystemParam)]
pub struct SkillNodeQueries<'w, 's> {
    compiled: Query<
        'w,
        's,
        (
            &'static CompiledSkill,
            &'static CompiledSkillParams,
            &'static CompiledSkillTags,
            &'static SkillRequirements,
            Option<&'static SkillRoots>,
        ),
    >,
    sequence: Query<'w, 's, &'static Sequence>,
    parallel: Query<'w, 's, &'static Parallel>,
    delay: Query<'w, 's, &'static Delay>,
    repeat: Query<'w, 's, &'static Repeat>,
    if_node: Query<'w, 's, &'static If>,
    wait_event: Query<'w, 's, &'static WaitEvent>,
    emit: Query<'w, 's, &'static EmitSkillEvent>,
    set_var: Query<'w, 's, &'static SetSkillVar>,
    with_context: Query<'w, 's, &'static WithSkillContext>,
    extension: Query<'w, 's, &'static SkillExtension>,
    children: Query<'w, 's, &'static SkillChildren>,
    child_of: Query<'w, 's, &'static SkillChildOf>,
    payloads: Query<'w, 's, &'static SkillPayloads>,
    payload_of: Query<'w, 's, &'static SkillPayloadOf>,
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
    nodes: SkillNodeQueries,
) {
    for request in requests.read() {
        let Some(compiled_entity) = library.get_entity(&request.skill) else {
            commands.write_message(SkillCastRejected {
                skill: request.skill.clone(),
                caster: request.caster,
                target: request.target,
                message: "compiled skill not found".to_owned(),
            });
            continue;
        };
        let Ok((compiled, params, tags, requirements, roots)) = nodes.compiled.get(compiled_entity)
        else {
            commands.write_message(SkillCastRejected {
                skill: request.skill.clone(),
                caster: request.caster,
                target: request.target,
                message: "compiled skill entity is missing runtime components".to_owned(),
            });
            continue;
        };
        let Some(root) = root_node_from_roots(roots) else {
            commands.write_message(SkillCastRejected {
                skill: request.skill.clone(),
                caster: request.caster,
                target: request.target,
                message: "compiled skill has no materialized root node".to_owned(),
            });
            continue;
        };

        let mut requirement_ctx = SkillContext::new(
            compiled.id.clone(),
            params.0.clone(),
            tags.0.clone(),
            Some(request.caster),
            0,
        );
        requirement_ctx.current_target = request.target;
        if let Err(message) = validate_and_pay_requirements(
            compiled,
            requirements,
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
        let skill_entity = commands
            .spawn(ExecutionOfSkill {
                skill: compiled_entity,
            })
            .id();
        let active = ActiveSkill {
            skill_id: compiled.id.clone(),
            execution_id,
            compiled_skill: compiled_entity,
            caster: request.caster,
            target: request.target,
        };
        commands.entity(skill_entity).insert(active.clone());
        commands.write_message(SkillCastAccepted {
            skill: compiled.id.clone(),
            execution_id,
            caster: request.caster,
            target: request.target,
        });

        let mut ctx = SkillContext::new(
            compiled.id.clone(),
            params.0.clone(),
            tags.0.clone(),
            Some(request.caster),
            execution_id,
        );
        ctx.step_budget = config.step_budget;
        ctx.step_budget_remaining = config.step_budget;
        ctx.current_target = request.target;
        ctx.skill_entity = Some(skill_entity);
        match execute_node(root, &mut ctx, &registry, &nodes) {
            Ok(output) if output.branches.is_empty() => {
                let output =
                    process_effect_spawns_commands(output, &registry, &nodes, &mut commands);
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
                let output =
                    process_effect_spawns_commands(output, &registry, &nodes, &mut commands);
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
    compiled: &CompiledSkill,
    requirements: &SkillRequirements,
    caster: Entity,
    ctx: &SkillContext,
    resources: &mut SkillResourcePools,
    cooldowns: &mut SkillCooldowns,
) -> Result<(), String> {
    let mut costs = Vec::new();
    let mut cooldown_seconds = None;

    for requirement in &requirements.0 {
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
    nodes: SkillNodeQueries,
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
                        match execute_continuation(
                            &branch.continuation,
                            &mut branch.ctx,
                            &registry,
                            &nodes,
                        ) {
                            Ok(output) => {
                                next_branches.extend(output.branches.clone());
                                let output = process_effect_spawns(
                                    output,
                                    &registry,
                                    &nodes,
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
    nodes: SkillNodeQueries,
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
                match execute_continuation(&branch.continuation, &mut branch.ctx, &registry, &nodes)
                {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output = process_effect_spawns(
                            output,
                            &registry,
                            &nodes,
                            &mut commands,
                            &mut failed,
                        );
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
    nodes: SkillNodeQueries,
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
                match execute_continuation(&branch.continuation, &mut branch.ctx, &registry, &nodes)
                {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output = process_effect_spawns(
                            output,
                            &registry,
                            &nodes,
                            &mut commands,
                            &mut failed,
                        );
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
    nodes: SkillNodeQueries,
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
            match execute_node(on_remove, &mut effect.ctx, &registry, &nodes) {
                Ok(output) => {
                    let output = process_effect_spawns(
                        output,
                        &registry,
                        &nodes,
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

fn root_node_from_roots(roots: Option<&SkillRoots>) -> Option<Entity> {
    roots.and_then(|roots| roots.iter().next())
}

fn node_children(
    nodes: &SkillNodeQueries,
    parent: Entity,
    slot: &str,
) -> Result<Vec<SkillContinuation>, SkillError> {
    let Some(children) = nodes.children.get(parent).ok() else {
        return Ok(Vec::new());
    };
    let mut ordered = children
        .iter()
        .filter_map(|child| {
            let relation = nodes.child_of.get(child).ok()?;
            (relation.parent == parent && relation.slot == slot)
                .then_some((relation.order, SkillContinuation::Node(child)))
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(order, _)| *order);
    Ok(ordered.into_iter().map(|(_, child)| child).collect())
}

fn node_payloads(
    nodes: &SkillNodeQueries,
    parent: Entity,
    slot: &str,
) -> Result<Vec<SkillContinuation>, SkillError> {
    let Some(payloads) = nodes.payloads.get(parent).ok() else {
        return Ok(Vec::new());
    };
    let mut ordered = payloads
        .iter()
        .filter_map(|payload| {
            let relation = nodes.payload_of.get(payload).ok()?;
            (relation.parent == parent && relation.slot == slot)
                .then_some((relation.order, SkillContinuation::Node(payload)))
        })
        .collect::<Vec<_>>();
    ordered.sort_by_key(|(order, _)| *order);
    Ok(ordered.into_iter().map(|(_, payload)| payload).collect())
}

fn node_payload_map(
    nodes: &SkillNodeQueries,
    parent: Entity,
) -> Result<IndexMap<String, Vec<Entity>>, SkillError> {
    let mut payloads_by_slot: IndexMap<String, Vec<(u32, Entity)>> = IndexMap::new();
    if let Ok(payloads) = nodes.payloads.get(parent) {
        for payload in payloads.iter() {
            let Ok(relation) = nodes.payload_of.get(payload) else {
                continue;
            };
            if relation.parent == parent {
                payloads_by_slot
                    .entry(relation.slot.clone())
                    .or_default()
                    .push((relation.order, payload));
            }
        }
    }
    Ok(payloads_by_slot
        .into_iter()
        .map(|(slot, mut payloads)| {
            payloads.sort_by_key(|(order, _)| *order);
            (
                slot,
                payloads
                    .into_iter()
                    .map(|(_, payload)| payload)
                    .collect::<Vec<_>>(),
            )
        })
        .collect())
}

fn single_child(
    nodes: &SkillNodeQueries,
    parent: Entity,
    slot: &str,
) -> Result<Entity, SkillError> {
    let children = node_children(nodes, parent, slot)?;
    if children.len() != 1 {
        return Err(SkillError::Runtime(format!(
            "node `{parent:?}` expected exactly one `{slot}` child, got {}",
            children.len()
        )));
    }
    Ok(children[0].node())
}

fn execute_node(
    node: Entity,
    ctx: &mut SkillContext,
    registry: &SkillActionRegistry,
    nodes: &SkillNodeQueries,
) -> Result<ExecutionOutput, SkillError> {
    spend_step(ctx)?;
    if nodes.sequence.get(node).is_ok() {
        return execute_continuation(&node_children(nodes, node, "items")?, ctx, registry, nodes);
    }
    if nodes.parallel.get(node).is_ok() {
        let mut output = ExecutionOutput::default();
        for child in node_children(nodes, node, "branches")? {
            output.extend(execute_node(child.node(), ctx, registry, nodes)?);
        }
        return Ok(output);
    }
    if let Ok(delay) = nodes.delay.get(node) {
        let seconds = number(&eval_skill_expr(&delay.seconds, ctx)?)?;
        let child = single_child(nodes, node, "then")?;
        return Ok(ExecutionOutput {
            branches: vec![SkillBranch {
                wait: SkillWait::Delay(Timer::from_seconds(seconds as f32, TimerMode::Once)),
                continuation: vec![SkillContinuation::Node(child)],
                ctx: ctx.clone(),
            }],
            ..Default::default()
        });
    }
    if let Ok(repeat) = nodes.repeat.get(node) {
        let times = match &repeat.times {
            Some(expr) => number(&eval_skill_expr(expr, ctx)?)? as usize,
            None => 1,
        };
        let body = single_child(nodes, node, "body")?;
        let mut frames = Vec::with_capacity(times);
        for _ in 0..times {
            if let Some(interval) = &repeat.interval {
                frames.push(SkillContinuation::DelayThen {
                    seconds: interval.clone(),
                    node: body,
                });
            } else {
                frames.push(SkillContinuation::Node(body));
            }
        }
        return execute_continuation(&frames, ctx, registry, nodes);
    }
    if let Ok(if_node) = nodes.if_node.get(node) {
        let slot = if truthy(&eval_skill_expr(&if_node.condition, ctx)?) {
            "then"
        } else {
            "else"
        };
        let children = node_children(nodes, node, slot)?;
        return execute_continuation(&children, ctx, registry, nodes);
    }
    if let Ok(set_var) = nodes.set_var.get(node) {
        let child = single_child(nodes, node, "then")?;
        let previous = ctx.vars.insert(set_var.name.clone(), set_var.value.clone());
        let result = execute_node(child, ctx, registry, nodes);
        match previous {
            Some(previous) => ctx.vars.insert(set_var.name.clone(), previous),
            None => ctx.vars.shift_remove(&set_var.name),
        };
        return result;
    }
    if let Ok(wait_event) = nodes.wait_event.get(node) {
        return Ok(ExecutionOutput {
            branches: vec![SkillBranch {
                wait: SkillWait::Signal(wait_event.event.clone()),
                continuation: node_payloads(nodes, node, &wait_event.event)?,
                ctx: ctx.clone(),
            }],
            ..Default::default()
        });
    }
    if let Ok(emit) = nodes.emit.get(node) {
        let payload = resolve_args(&emit.payload, ctx)?;
        return Ok(ExecutionOutput {
            signals: vec![SkillRuntimeSignal {
                name: emit.event.clone(),
                payload,
                execution_id: Some(ctx.execution_id),
                skill_entity: ctx.skill_entity,
                caster: ctx.caster,
                target: ctx.current_target,
            }],
            ..Default::default()
        });
    }
    if nodes.with_context.get(node).is_ok() {
        let children =
            node_children(nodes, node, "then").or_else(|_| node_children(nodes, node, "items"))?;
        return execute_continuation(&children, ctx, registry, nodes);
    }
    if let Ok(extension) = nodes.extension.get(node) {
        let action = registry
            .action(&extension.constructor)
            .ok_or_else(|| SkillError::UnknownAction(extension.constructor.clone()))?;
        let input = SkillActionInput {
            args: resolve_args(&extension.args, ctx)?,
            payloads: node_payload_map(nodes, node)?,
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
                continuation: Vec::new(),
                ctx: ctx.clone(),
            })
            .collect();
        return Ok(ExecutionOutput {
            branches,
            intents: output.intents,
            effect_requests: output.effect_requests,
            effect_resolved: output.effect_resolved,
            effects: output.effects,
            ..Default::default()
        });
    }
    Err(SkillError::Runtime(format!(
        "node entity `{node:?}` has no skill node component"
    )))
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

fn execute_continuation(
    frames: &[SkillContinuation],
    ctx: &mut SkillContext,
    registry: &SkillActionRegistry,
    nodes: &SkillNodeQueries,
) -> Result<ExecutionOutput, SkillError> {
    let mut output = ExecutionOutput::default();
    for (index, frame) in frames.iter().enumerate() {
        let mut child = match frame {
            SkillContinuation::Node(node) => execute_node(*node, ctx, registry, nodes)?,
            SkillContinuation::DelayThen { seconds, node } => {
                let seconds = number(&eval_skill_expr(seconds, ctx)?)?;
                ExecutionOutput {
                    branches: vec![SkillBranch {
                        wait: SkillWait::Delay(Timer::from_seconds(
                            seconds as f32,
                            TimerMode::Once,
                        )),
                        continuation: vec![SkillContinuation::Node(*node)],
                        ctx: ctx.clone(),
                    }],
                    ..Default::default()
                }
            }
        };
        if !child.branches.is_empty() {
            let rest = frames[index + 1..].to_vec();
            if !rest.is_empty() {
                for branch in &mut child.branches {
                    branch.continuation.extend(rest.clone());
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
    nodes: &SkillNodeQueries,
    commands: &mut Commands,
    failed: &mut MessageWriter<SkillExecutionFailed>,
) -> ExecutionOutput {
    let mut effects = std::mem::take(&mut output.effects);
    while let Some(mut spawn) = effects.pop() {
        if let Some(on_add) = spawn.on_add.take() {
            let mut ctx = spawn.ctx.clone();
            match execute_node(on_add, &mut ctx, registry, nodes) {
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
    nodes: &SkillNodeQueries,
    commands: &mut Commands,
) -> ExecutionOutput {
    let mut effects = std::mem::take(&mut output.effects);
    while let Some(mut spawn) = effects.pop() {
        if let Some(on_add) = spawn.on_add.take() {
            let mut ctx = spawn.ctx.clone();
            match execute_node(on_add, &mut ctx, registry, nodes) {
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
