use crate::SkillLibrary;
use crate::dsl::{
    SkillArgs, SkillCastFinished, SkillCastRejected, SkillCastRequest, SkillCastStarted,
    SkillContext, SkillExecutionFailed, SkillIntent, SkillNode, SkillRuntimeSignal, SkillValue,
};
use crate::expr::{eval_skill_expr, resolve_args};
use crate::registry::{SkillError, SkillRegistry};
use bevy::prelude::{
    Commands, Component, Entity, MessageReader, MessageWriter, Query, Res, ResMut, Resource, Time,
    Timer, TimerMode,
};

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
}

#[derive(Default)]
pub struct SkillActionOutput {
    pub intents: Vec<SkillIntent>,
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
}

#[derive(Default)]
struct ExecutionOutput {
    branches: Vec<SkillBranch>,
    intents: Vec<SkillIntent>,
    signals: Vec<SkillRuntimeSignal>,
}

pub fn handle_skill_cast_requests(
    mut commands: Commands,
    mut requests: MessageReader<SkillCastRequest>,
    library: Res<SkillLibrary>,
    registry: Res<SkillRegistry>,
    mut counters: ResMut<SkillRuntimeCounters>,
    mut started: MessageWriter<SkillCastStarted>,
    mut finished: MessageWriter<SkillCastFinished>,
    mut rejected: MessageWriter<SkillCastRejected>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
) {
    for request in requests.read() {
        let Some(compiled) = library.get(&request.skill) else {
            rejected.write(SkillCastRejected {
                skill: request.skill.clone(),
                caster: request.caster,
                target: request.target,
                message: "compiled skill not found".to_owned(),
            });
            continue;
        };

        let execution_id = counters.next_execution_id();
        let skill_entity = commands.spawn_empty().id();
        let active = ActiveSkill {
            skill_id: compiled.id.clone(),
            execution_id,
            caster: request.caster,
            target: request.target,
        };
        commands.entity(skill_entity).insert(active.clone());
        started.write(SkillCastStarted {
            skill: compiled.id.clone(),
            execution_id,
            skill_entity,
            caster: request.caster,
            target: request.target,
        });

        let mut ctx = SkillContext::new(compiled, Some(request.caster), execution_id);
        ctx.current_target = request.target;
        ctx.skill_entity = Some(skill_entity);
        match execute_node(&compiled.plan.root, &mut ctx, &registry) {
            Ok(output) if output.branches.is_empty() => {
                write_output(output, &mut intents, &mut signals);
                commands.entity(skill_entity).despawn();
                finished.write(SkillCastFinished {
                    skill: compiled.id.clone(),
                    execution_id,
                    skill_entity,
                    caster: request.caster,
                    target: request.target,
                });
            }
            Ok(output) => {
                let branches = output.branches.clone();
                write_output(output, &mut intents, &mut signals);
                commands
                    .entity(skill_entity)
                    .insert(SkillExecutionState::new(branches));
            }
            Err(err) => {
                commands.entity(skill_entity).despawn();
                failed.write(SkillExecutionFailed {
                    skill: Some(compiled.id.clone()),
                    execution_id: Some(execution_id),
                    skill_entity: Some(skill_entity),
                    message: err.to_string(),
                });
            }
        }
    }
}

pub fn tick_skill_delays(
    mut commands: Commands,
    time: Res<Time>,
    registry: Res<SkillRegistry>,
    mut query: Query<(Entity, &ActiveSkill, &mut SkillExecutionState)>,
    mut finished: MessageWriter<SkillCastFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
    mut signals: MessageWriter<SkillRuntimeSignal>,
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
                                write_output(output, &mut intents, &mut signals);
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
                SkillWait::Signal(_) => next_branches.push(branch),
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
    mut finished: MessageWriter<SkillCastFinished>,
    mut failed: MessageWriter<SkillExecutionFailed>,
    mut intents: MessageWriter<SkillIntent>,
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
                SkillWait::Signal(name) => incoming.iter().find(|signal| signal.name == *name),
                SkillWait::Delay(_) => None,
            };

            if let Some(signal) = matching {
                branch.ctx.source_event = Some(signal.clone());
                match execute_node(&branch.node, &mut branch.ctx, &registry) {
                    Ok(output) => {
                        next_branches.extend(output.branches.clone());
                        write_output_deferred_signals(output, &mut intents, &mut commands);
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

fn execute_node(
    node: &SkillNode,
    ctx: &mut SkillContext,
    registry: &SkillRegistry,
) -> Result<ExecutionOutput, SkillError> {
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
            Ok(ExecutionOutput {
                intents: output.intents,
                ..Default::default()
            })
        }
        SkillNode::Deck(_) | SkillNode::Spell(_, _) | SkillNode::Modifier(_, _) => Err(
            SkillError::Runtime("extension node cannot be executed by the core runner".to_owned()),
        ),
    }
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
) {
    for intent in output.intents {
        intents.write(intent);
    }
    for signal in output.signals {
        signals.write(signal);
    }
}

fn write_output_deferred_signals(
    output: ExecutionOutput,
    intents: &mut MessageWriter<SkillIntent>,
    commands: &mut Commands,
) {
    for intent in output.intents {
        intents.write(intent);
    }
    for signal in output.signals {
        commands.write_message(signal);
    }
}

fn finish_or_store(
    entity: Entity,
    active: &ActiveSkill,
    next_branches: Vec<SkillBranch>,
    state: &mut SkillExecutionState,
    commands: &mut Commands,
    finished: &mut MessageWriter<SkillCastFinished>,
) {
    if next_branches.is_empty() {
        commands.entity(entity).despawn();
        finished.write(SkillCastFinished {
            skill: active.skill_id.clone(),
            execution_id: active.execution_id,
            skill_entity: entity,
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
