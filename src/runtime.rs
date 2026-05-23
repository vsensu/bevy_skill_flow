use crate::SkillLibrary;
use crate::dsl::{
    SkillArgs, SkillCompiled, SkillContext, SkillExecutionFailed, SkillExpr, SkillId, SkillIntent,
    SkillNode, SkillObserverTrigger, SkillRequirement, SkillRuntimeSignal, SkillSpecialValue,
    SkillValue,
};
use crate::expr::{eval_skill_expr, resolve_args};
use crate::registry::{SkillError, SkillRegistry};
use bevy::prelude::{
    Commands, Component, Entity, MessageReader, MessageWriter, On, Query, Res, ResMut, Resource,
    Time, Timer, TimerMode,
};
use bevy_skill_ecs::{
    ApplyBuffRequest, DamageRequest, DamageResolved, ProjectileHit, SettlementMode,
    SkillCastAccepted, SkillCastRejected, SkillCastRequest, SkillExecutionFinished, SkillGraph,
    SkillGraphNodeKind, SkillId as EcsSkillId, SkillNodeId, SkillRuntimeConfig,
    SkillSpecialValue as EcsSkillSpecialValue, SkillValue as EcsSkillValue,
};
#[cfg(feature = "full_runtime_entities")]
use bevy_skill_ecs::{ExecutionOfSkill, SkillChildOf, SkillPayloadOf, SkillRootOf};
use std::collections::HashMap;

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

fn flow_skill_id(id: &EcsSkillId) -> SkillId {
    SkillId::new(id.0.clone())
}

fn ecs_skill_id(id: &SkillId) -> EcsSkillId {
    EcsSkillId::new(id.0.clone())
}

#[derive(Component, Clone, Debug, PartialEq)]
pub struct ActiveSkill {
    pub skill_id: crate::SkillId,
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
    pub node: SkillNode,
    pub ctx: SkillContext,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SkillWait {
    Delay(Timer),
    Signal(String),
    DamageResolved { request_id: u64 },
}

#[derive(Default)]
pub struct SkillActionOutput {
    pub intents: Vec<SkillIntent>,
    pub damage_requests: Vec<DamageRequest>,
    pub damage_resolved: Vec<DamageResolved>,
    pub apply_buff_requests: Vec<ApplyBuffRequest>,
    pub waits: Vec<SkillActionWait>,
    pub buffs: Vec<SkillBuffSpawn>,
    pub vars: SkillArgs,
}

pub enum SkillActionWait {
    DamageResolved { request_id: u64 },
}

pub struct SkillBuffSpawn {
    pub request: ApplyBuffRequest,
    pub on_add: Option<SkillNode>,
    pub on_remove: Option<SkillNode>,
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

    pub fn emit_damage_request(
        &mut self,
        ctx: &SkillContext,
        target: Entity,
        amount: f64,
        mode: SettlementMode,
    ) -> Result<(), SkillError> {
        let request_id = next_protocol_request_id(ctx, self.damage_requests.len() as u64);
        let request = DamageRequest {
            request_id,
            execution_id: ctx.execution_id,
            source: ctx.caster,
            target,
            amount,
            mode,
        };
        self.damage_requests.push(request);
        match mode {
            SettlementMode::Sync => {
                self.damage_resolved.push(DamageResolved {
                    request_id,
                    execution_id: ctx.execution_id,
                    source: ctx.caster,
                    target,
                    amount,
                });
                self.set_var("last_damage_amount", SkillValue::Number(amount));
            }
            SettlementMode::Request => {}
            SettlementMode::Await => self
                .waits
                .push(SkillActionWait::DamageResolved { request_id }),
        }
        Ok(())
    }

    pub fn emit_apply_buff_request(
        &mut self,
        ctx: &SkillContext,
        target: Entity,
        buff: impl Into<String>,
        duration_seconds: Option<f64>,
        on_add: Option<SkillNode>,
        on_remove: Option<SkillNode>,
    ) {
        let request = ApplyBuffRequest {
            execution_id: ctx.execution_id,
            target,
            buff: buff.into(),
            duration_seconds,
        };
        self.apply_buff_requests.push(request.clone());
        self.buffs.push(SkillBuffSpawn {
            request,
            on_add,
            on_remove,
            ctx: ctx.clone(),
        });
    }
}

#[derive(Default)]
struct ExecutionOutput {
    branches: Vec<SkillBranch>,
    intents: Vec<SkillIntent>,
    signals: Vec<SkillRuntimeSignal>,
    damage_requests: Vec<DamageRequest>,
    damage_resolved: Vec<DamageResolved>,
    apply_buff_requests: Vec<ApplyBuffRequest>,
    buffs: Vec<SkillBuffSpawn>,
}

#[derive(Component, Clone, Debug, PartialEq)]
pub struct ActiveSkillBuff {
    pub execution_id: u64,
    pub skill_id: crate::SkillId,
    pub caster: Option<Entity>,
    pub target: Entity,
    pub buff: String,
    pub timer: Option<Timer>,
    pub on_remove: Option<SkillNode>,
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
    registry: Res<SkillRegistry>,
    config: Res<SkillRuntimeConfig>,
    mut resources: ResMut<SkillResourcePools>,
    mut cooldowns: ResMut<SkillCooldowns>,
    mut counters: ResMut<SkillRuntimeCounters>,
) {
    for request in requests.read() {
        let requested_skill = flow_skill_id(&request.skill);
        let Some(compiled) = library.get(&requested_skill) else {
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
            skill: ecs_skill_id(&compiled.id),
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
                let output = process_buff_spawns_commands(output, &registry, &mut commands);
                write_output_commands(output, &mut commands);
                commands.entity(skill_entity).despawn();
                commands.write_message(SkillExecutionFinished {
                    skill: ecs_skill_id(&compiled.id),
                    execution_id,
                    caster: request.caster,
                    target: request.target,
                });
            }
            Ok(output) => {
                let branches = output.branches.clone();
                let output = process_buff_spawns_commands(output, &registry, &mut commands);
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

    for requirement in &compiled.requirements {
        match requirement {
            SkillRequirement::Cost { resource, amount } => {
                let amount = number(&eval_skill_expr(amount, ctx).map_err(|err| err.to_string())?)
                    .map_err(|err| err.to_string())?;
                if amount > 0.0 {
                    costs.push((resource.clone(), amount));
                }
            }
            SkillRequirement::Cooldown { seconds } => {
                let seconds =
                    number(&eval_skill_expr(seconds, ctx).map_err(|err| err.to_string())?)
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
    registry: Res<SkillRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
    mut damage_requests: MessageWriter<DamageRequest>,
    mut damage_resolved: MessageWriter<DamageResolved>,
    mut apply_buff_requests: MessageWriter<ApplyBuffRequest>,
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
                                let output = process_buff_spawns(
                                    output,
                                    &registry,
                                    &mut commands,
                                    &mut failed,
                                );
                                write_output(
                                    output,
                                    &mut intents,
                                    &mut signals,
                                    &mut damage_requests,
                                    &mut damage_resolved,
                                    &mut apply_buff_requests,
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
                SkillWait::Signal(_) | SkillWait::DamageResolved { .. } => {
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
    registry: Res<SkillRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut damage_requests: MessageWriter<DamageRequest>,
    mut apply_buff_requests: MessageWriter<ApplyBuffRequest>,
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
                SkillWait::Delay(_) | SkillWait::DamageResolved { .. } => None,
            };

            if let Some(signal) = matching {
                branch.ctx.source_event = Some(signal.clone());
                match execute_node(&branch.node, &mut branch.ctx, &registry) {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output =
                            process_buff_spawns(output, &registry, &mut commands, &mut failed);
                        write_output_deferred_signals(
                            output,
                            &mut intents,
                            &mut commands,
                            &mut damage_requests,
                            &mut apply_buff_requests,
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

pub fn resume_damage_resolved(
    mut commands: Commands,
    mut resolved_reader: MessageReader<DamageResolved>,
    registry: Res<SkillRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut damage_requests: MessageWriter<DamageRequest>,
    mut apply_buff_requests: MessageWriter<ApplyBuffRequest>,
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
                SkillWait::DamageResolved { request_id } => incoming.iter().find(|resolved| {
                    resolved.execution_id == branch.ctx.execution_id
                        && resolved.request_id == *request_id
                }),
                SkillWait::Delay(_) | SkillWait::Signal(_) => None,
            };

            if let Some(resolved) = matching {
                seed_damage_result(&mut branch.ctx, resolved);
                match execute_node(&branch.node, &mut branch.ctx, &registry) {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output =
                            process_buff_spawns(output, &registry, &mut commands, &mut failed);
                        write_output_deferred_signals(
                            output,
                            &mut intents,
                            &mut commands,
                            &mut damage_requests,
                            &mut apply_buff_requests,
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

pub fn resume_projectile_hits(
    mut commands: Commands,
    mut hit_reader: MessageReader<ProjectileHit>,
    registry: Res<SkillRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillExecutionFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
    mut damage_requests: MessageWriter<DamageRequest>,
    mut damage_resolved: MessageWriter<DamageResolved>,
    mut apply_buff_requests: MessageWriter<ApplyBuffRequest>,
) {
    let incoming = hit_reader.read().cloned().collect::<Vec<_>>();
    if incoming.is_empty() {
        return;
    }

    for (entity, active, mut state) in &mut query {
        let mut next_branches = Vec::new();
        let mut failed_entity = false;
        for mut branch in state.branches.drain(..) {
            let matching = match &branch.wait {
                SkillWait::Signal(name) if name == "hit" || name == "projectile_hit" => incoming
                    .iter()
                    .find(|hit| hit.execution_id == branch.ctx.execution_id),
                SkillWait::Delay(_) | SkillWait::Signal(_) | SkillWait::DamageResolved { .. } => {
                    None
                }
            };

            if let Some(hit) = matching {
                branch.ctx.source_event = Some(projectile_hit_signal(hit));
                branch.ctx.current_target = hit.target.or(branch.ctx.current_target);
                match execute_node(&branch.node, &mut branch.ctx, &registry) {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        let output =
                            process_buff_spawns(output, &registry, &mut commands, &mut failed);
                        write_output(
                            output,
                            &mut intents,
                            &mut signals,
                            &mut damage_requests,
                            &mut damage_resolved,
                            &mut apply_buff_requests,
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

pub fn tick_skill_buffs(
    mut commands: Commands,
    time: Res<Time>,
    registry: Res<SkillRegistry>,
    mut query: Query<(Entity, &mut ActiveSkillBuff)>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
    mut damage_requests: MessageWriter<DamageRequest>,
    mut damage_resolved: MessageWriter<DamageResolved>,
    mut apply_buff_requests: MessageWriter<ApplyBuffRequest>,
) {
    for (entity, mut buff) in &mut query {
        let Some(timer) = &mut buff.timer else {
            continue;
        };
        timer.tick(time.delta());
        if !timer.is_finished() {
            continue;
        }

        if let Some(on_remove) = buff.on_remove.clone() {
            match execute_node(&on_remove, &mut buff.ctx, &registry) {
                Ok(output) => {
                    let output = process_buff_spawns(output, &registry, &mut commands, &mut failed);
                    write_output(
                        output,
                        &mut intents,
                        &mut signals,
                        &mut damage_requests,
                        &mut damage_resolved,
                        &mut apply_buff_requests,
                    );
                }
                Err(err) => {
                    failed.write(SkillExecutionFailed {
                        skill: Some(buff.skill_id.clone()),
                        execution_id: Some(buff.execution_id),
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
    node: &SkillNode,
    ctx: &mut SkillContext,
    registry: &SkillRegistry,
) -> Result<ExecutionOutput, SkillError> {
    spend_step(ctx)?;
    match node {
        SkillNode::Sequence(nodes) => execute_sequence(nodes, ctx, registry),
        SkillNode::Parallel(nodes) => {
            let mut output = ExecutionOutput::default();
            for node in nodes {
                output.extend(execute_node(node, ctx, registry)?);
            }
            Ok(output)
        }
        SkillNode::Delay(seconds, node) => {
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
        SkillNode::Repeat {
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
                    sequence.push(SkillNode::Delay(interval.clone(), node.clone()));
                } else {
                    sequence.push((**node).clone());
                }
            }
            execute_node(&SkillNode::Sequence(sequence), ctx, registry)
        }
        SkillNode::If {
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
        SkillNode::Let(name, value, node) => {
            let previous = ctx.vars.insert(name.clone(), value.clone());
            let result = execute_node(node, ctx, registry);
            match previous {
                Some(previous) => ctx.vars.insert(name.clone(), previous),
                None => ctx.vars.shift_remove(name),
            };
            result
        }
        SkillNode::On(name, node) => Ok(ExecutionOutput {
            branches: vec![SkillBranch {
                wait: SkillWait::Signal(name.clone()),
                node: (**node).clone(),
                ctx: ctx.clone(),
            }],
            ..Default::default()
        }),
        SkillNode::Emit(name, payload) => {
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
        SkillNode::Action(id, args) => {
            let action = registry
                .action(id)
                .ok_or_else(|| SkillError::UnknownAction(id.clone()))?;
            let args = resolve_args(args, ctx)?;
            let mut output = SkillActionOutput::default();
            action.emit(ctx, &args, &mut output)?;
            for (key, value) in output.vars {
                ctx.vars.insert(key, value);
            }
            let branches = output
                .waits
                .into_iter()
                .map(|wait| SkillBranch {
                    wait: match wait {
                        SkillActionWait::DamageResolved { request_id } => {
                            SkillWait::DamageResolved { request_id }
                        }
                    },
                    node: SkillNode::Sequence(Vec::new()),
                    ctx: ctx.clone(),
                })
                .collect();
            Ok(ExecutionOutput {
                branches,
                intents: output.intents,
                damage_requests: output.damage_requests,
                damage_resolved: output.damage_resolved,
                apply_buff_requests: output.apply_buff_requests,
                buffs: output.buffs,
                ..Default::default()
            })
        }
        SkillNode::Deck(_) | SkillNode::Spell(_, _) | SkillNode::Modifier(_, _) => Err(
            SkillError::Runtime("extension node cannot be executed by the core runner".to_owned()),
        ),
    }
}

fn spend_step(ctx: &mut SkillContext) -> Result<(), SkillError> {
    if ctx.step_budget_remaining == 0 {
        return Err(SkillError::Runtime(
            bevy_skill_ecs::SkillExecutionError::StepBudgetExhausted {
                steps: ctx.step_budget,
            }
            .to_string(),
        ));
    }
    ctx.step_budget_remaining -= 1;
    Ok(())
}

fn execute_sequence(
    nodes: &[SkillNode],
    ctx: &mut SkillContext,
    registry: &SkillRegistry,
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
                    branch.node = SkillNode::Sequence(sequence);
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
    damage_requests: &mut MessageWriter<DamageRequest>,
    damage_resolved: &mut MessageWriter<DamageResolved>,
    apply_buff_requests: &mut MessageWriter<ApplyBuffRequest>,
) {
    for intent in output.intents {
        intents.write(intent);
    }
    for signal in output.signals {
        signals.write(signal);
    }
    for request in output.damage_requests {
        damage_requests.write(request);
    }
    for resolved in output.damage_resolved {
        damage_resolved.write(resolved);
    }
    for request in output.apply_buff_requests {
        apply_buff_requests.write(request);
    }
}

fn write_output_deferred_signals(
    output: ExecutionOutput,
    intents: &mut MessageWriter<SkillIntent>,
    commands: &mut Commands,
    damage_requests: &mut MessageWriter<DamageRequest>,
    apply_buff_requests: &mut MessageWriter<ApplyBuffRequest>,
) {
    for intent in output.intents {
        intents.write(intent);
    }
    for signal in output.signals {
        commands.write_message(signal);
    }
    for request in output.damage_requests {
        damage_requests.write(request);
    }
    for resolved in output.damage_resolved {
        commands.write_message(resolved);
    }
    for request in output.apply_buff_requests {
        apply_buff_requests.write(request);
    }
}

fn write_output_commands(output: ExecutionOutput, commands: &mut Commands) {
    for intent in output.intents {
        commands.write_message(intent);
    }
    for signal in output.signals {
        commands.write_message(signal);
    }
    for request in output.damage_requests {
        commands.write_message(request);
    }
    for resolved in output.damage_resolved {
        commands.write_message(resolved);
    }
    for request in output.apply_buff_requests {
        commands.write_message(request);
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
            skill: ecs_skill_id(&active.skill_id),
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
        self.damage_requests.extend(other.damage_requests);
        self.damage_resolved.extend(other.damage_resolved);
        self.apply_buff_requests.extend(other.apply_buff_requests);
        self.buffs.extend(other.buffs);
    }
}

fn process_buff_spawns(
    mut output: ExecutionOutput,
    registry: &SkillRegistry,
    commands: &mut Commands,
    failed: &mut MessageWriter<SkillExecutionFailed>,
) -> ExecutionOutput {
    let mut buffs = std::mem::take(&mut output.buffs);
    while let Some(mut spawn) = buffs.pop() {
        if let Some(on_add) = spawn.on_add.take() {
            let mut ctx = spawn.ctx.clone();
            match execute_node(&on_add, &mut ctx, registry) {
                Ok(hook_output) => {
                    output.extend(hook_output);
                    buffs.extend(std::mem::take(&mut output.buffs));
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
            .request
            .duration_seconds
            .map(|seconds| Timer::from_seconds(seconds.max(0.0) as f32, TimerMode::Once));
        commands.spawn(ActiveSkillBuff {
            execution_id: spawn.request.execution_id,
            skill_id: spawn.ctx.skill_id.clone(),
            caster: spawn.ctx.caster,
            target: spawn.request.target,
            buff: spawn.request.buff,
            timer,
            on_remove: spawn.on_remove,
            ctx: spawn.ctx,
        });
    }
    output
}

fn process_buff_spawns_commands(
    mut output: ExecutionOutput,
    registry: &SkillRegistry,
    commands: &mut Commands,
) -> ExecutionOutput {
    let mut buffs = std::mem::take(&mut output.buffs);
    while let Some(mut spawn) = buffs.pop() {
        if let Some(on_add) = spawn.on_add.take() {
            let mut ctx = spawn.ctx.clone();
            match execute_node(&on_add, &mut ctx, registry) {
                Ok(hook_output) => {
                    output.extend(hook_output);
                    buffs.extend(std::mem::take(&mut output.buffs));
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
            .request
            .duration_seconds
            .map(|seconds| Timer::from_seconds(seconds.max(0.0) as f32, TimerMode::Once));
        commands.spawn(ActiveSkillBuff {
            execution_id: spawn.request.execution_id,
            skill_id: spawn.ctx.skill_id.clone(),
            caster: spawn.ctx.caster,
            target: spawn.request.target,
            buff: spawn.request.buff,
            timer,
            on_remove: spawn.on_remove,
            ctx: spawn.ctx,
        });
    }
    output
}

fn seed_damage_result(ctx: &mut SkillContext, resolved: &DamageResolved) {
    ctx.vars.insert(
        "last_damage_amount".to_owned(),
        SkillValue::Number(resolved.amount),
    );
    ctx.source_event = Some(damage_resolved_signal(resolved));
}

fn damage_resolved_signal(resolved: &DamageResolved) -> SkillRuntimeSignal {
    let mut payload = SkillArgs::new();
    payload.insert("amount".to_owned(), SkillValue::Number(resolved.amount));
    SkillRuntimeSignal {
        name: "damage_resolved".to_owned(),
        payload,
        execution_id: None,
        skill_entity: None,
        caster: resolved.source,
        target: Some(resolved.target),
    }
}

fn next_protocol_request_id(ctx: &SkillContext, local_index: u64) -> u64 {
    let step_index = u64::from(ctx.step_budget.saturating_sub(ctx.step_budget_remaining));
    (ctx.execution_id << 32) | (step_index << 16) | local_index
}

fn projectile_hit_signal(hit: &ProjectileHit) -> SkillRuntimeSignal {
    let mut payload = SkillArgs::new();
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
    SkillRuntimeSignal {
        name: "hit".to_owned(),
        payload,
        execution_id: Some(hit.execution_id),
        skill_entity: None,
        caster: None,
        target: hit.target,
    }
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
        SkillValue::Special(_) | SkillValue::Node(_) => true,
    }
}

fn runtime_node_from_graph(graph: &SkillGraph) -> Result<SkillNode, SkillError> {
    let root = graph
        .root
        .ok_or_else(|| SkillError::Runtime("skill graph has no root node".to_owned()))?;
    runtime_node_from_graph_id(graph, root)
}

fn runtime_node_from_graph_id(
    graph: &SkillGraph,
    id: SkillNodeId,
) -> Result<SkillNode, SkillError> {
    let node = graph.node(id).ok_or_else(|| {
        SkillError::Runtime(format!("skill graph references missing node `{id:?}`"))
    })?;
    match &node.kind {
        SkillGraphNodeKind::Sequence => Ok(SkillNode::Sequence(graph_child_nodes_or_empty(
            graph, id, "items",
        )?)),
        SkillGraphNodeKind::Parallel => Ok(SkillNode::Parallel(graph_child_nodes_or_empty(
            graph, id, "branches",
        )?)),
        SkillGraphNodeKind::Delay { seconds } => {
            let child = single_graph_child(graph, id, "then")?;
            Ok(SkillNode::Delay(
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
            Ok(SkillNode::Repeat {
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
            Ok(SkillNode::If {
                condition: SkillExpr::new(condition.0.clone()),
                then_node: Box::new(then_node),
                else_node: else_node.map(Box::new),
            })
        }
        SkillGraphNodeKind::WaitEvent { event } => {
            let payload = graph_payload_nodes(graph, id, event)?;
            Ok(SkillNode::On(
                event.clone(),
                Box::new(sequence_or_single(payload)),
            ))
        }
        SkillGraphNodeKind::EmitSkillEvent { event, payload } => Ok(SkillNode::Emit(
            event.clone(),
            payload
                .iter()
                .map(|(key, value)| Ok((key.clone(), flow_value_from_ecs(value)?)))
                .collect::<Result<_, SkillError>>()?,
        )),
        SkillGraphNodeKind::SetSkillVar { name, value } => {
            let child = single_graph_child(graph, id, "then")?;
            Ok(SkillNode::Let(
                name.clone(),
                flow_value_from_ecs(value)?,
                Box::new(child),
            ))
        }
        SkillGraphNodeKind::WithSkillContext => Ok(SkillNode::Sequence(
            graph_child_nodes_or_empty(graph, id, "then")
                .or_else(|_| graph_child_nodes_or_empty(graph, id, "items"))?,
        )),
        SkillGraphNodeKind::Extension { constructor, args } => {
            let mut action_args = args
                .iter()
                .map(|(key, value)| Ok((key.clone(), flow_value_from_ecs(value)?)))
                .collect::<Result<SkillArgs, SkillError>>()?;
            let graph_node = graph.node(id).expect("validated node");
            for (slot, payloads) in &graph_node.payloads {
                let nodes = payloads
                    .iter()
                    .map(|payload| runtime_node_from_graph_id(graph, *payload))
                    .collect::<Result<Vec<_>, _>>()?;
                action_args.insert(
                    slot.clone(),
                    SkillValue::Node(Box::new(sequence_or_single(nodes))),
                );
            }
            Ok(SkillNode::Action(constructor.clone(), action_args))
        }
    }
}

fn graph_child_nodes(
    graph: &SkillGraph,
    id: SkillNodeId,
    slot: &str,
) -> Result<Vec<SkillNode>, SkillError> {
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
) -> Result<Vec<SkillNode>, SkillError> {
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
) -> Result<Vec<SkillNode>, SkillError> {
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
) -> Result<SkillNode, SkillError> {
    let mut children = graph_child_nodes(graph, id, slot)?;
    if children.len() != 1 {
        return Err(SkillError::Runtime(format!(
            "node `{id:?}` expected exactly one `{slot}` child, got {}",
            children.len()
        )));
    }
    Ok(children.remove(0))
}

fn sequence_or_single(mut nodes: Vec<SkillNode>) -> SkillNode {
    if nodes.len() == 1 {
        nodes.remove(0)
    } else {
        SkillNode::Sequence(nodes)
    }
}

fn graph_stats(graph: &SkillGraph) -> SkillArgs {
    graph
        .params
        .iter()
        .filter_map(|(key, value)| {
            flow_value_from_ecs(value)
                .ok()
                .map(|value| (key.clone(), value))
        })
        .collect()
}

fn flow_value_from_ecs(value: &EcsSkillValue) -> Result<SkillValue, SkillError> {
    Ok(match value {
        EcsSkillValue::Special(EcsSkillSpecialValue::Expr(value)) => {
            SkillValue::Special(SkillSpecialValue::Expr(value.clone()))
        }
        EcsSkillValue::Special(EcsSkillSpecialValue::Ref(value)) => {
            SkillValue::Special(SkillSpecialValue::Ref(value.clone()))
        }
        EcsSkillValue::Special(EcsSkillSpecialValue::Tag(value)) => {
            SkillValue::Special(SkillSpecialValue::Tag(value.clone()))
        }
        EcsSkillValue::Special(EcsSkillSpecialValue::Stat(value)) => {
            SkillValue::Special(SkillSpecialValue::Stat(value.clone()))
        }
        EcsSkillValue::Map(map) => SkillValue::Map(
            map.iter()
                .map(|(key, value)| Ok((key.clone(), flow_value_from_ecs(value)?)))
                .collect::<Result<_, SkillError>>()?,
        ),
        EcsSkillValue::List(values) => SkillValue::List(
            values
                .iter()
                .map(flow_value_from_ecs)
                .collect::<Result<Vec<_>, SkillError>>()?,
        ),
        EcsSkillValue::Number(value) => SkillValue::Number(*value),
        EcsSkillValue::Bool(value) => SkillValue::Bool(*value),
        EcsSkillValue::String(value) => SkillValue::String(value.clone()),
        EcsSkillValue::Null => SkillValue::Null,
    })
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
