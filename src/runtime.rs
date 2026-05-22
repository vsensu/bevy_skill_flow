use crate::dsl::{
    SkillArgs, SkillCompiled, SkillContext, SkillNode, SkillRuntimeEvent, SkillValue,
};
use crate::expr::{eval_skill_expr, resolve_args};
use crate::registry::{SkillError, SkillRegistry};
use bevy::prelude::{Entity, Resource, World};

#[derive(Resource, Default, Clone, Debug)]
pub struct PendingSkillExecutions {
    next_execution_id: u64,
    pending: Vec<PendingSkillExecution>,
    emitted: Vec<SkillRuntimeEvent>,
}

impl PendingSkillExecutions {
    pub fn cast(
        &mut self,
        compiled: &SkillCompiled,
        caster: Entity,
        world: &mut World,
        registry: &SkillRegistry,
    ) -> Result<u64, SkillError> {
        self.next_execution_id += 1;
        let execution_id = self.next_execution_id;
        let mut ctx = SkillContext::new(compiled, Some(caster), execution_id);
        let result = execute_node(&compiled.plan.root, world, &mut ctx, registry)?;
        self.emitted.append(&mut ctx.emitted);
        self.extend_pending(result.pending);
        Ok(execution_id)
    }

    pub fn tick(
        &mut self,
        delta_seconds: f64,
        world: &mut World,
        registry: &SkillRegistry,
    ) -> Result<(), SkillError> {
        let mut still_pending = Vec::new();
        let mut ready = Vec::new();
        for pending in self.pending.drain(..) {
            match pending.kind {
                PendingKind::Delay { remaining } if remaining > delta_seconds => {
                    still_pending.push(PendingSkillExecution {
                        kind: PendingKind::Delay {
                            remaining: remaining - delta_seconds,
                        },
                        ..pending
                    });
                }
                PendingKind::Delay { .. } => ready.push(pending),
                PendingKind::Event { .. } => still_pending.push(pending),
            }
        }
        self.pending = still_pending;
        for mut pending in ready {
            let result = execute_node(&pending.node, world, &mut pending.ctx, registry)?;
            self.emitted.append(&mut pending.ctx.emitted);
            self.extend_pending(result.pending);
        }
        Ok(())
    }

    pub fn trigger_event(
        &mut self,
        event: SkillRuntimeEvent,
        world: &mut World,
        registry: &SkillRegistry,
    ) -> Result<(), SkillError> {
        let mut still_pending = Vec::new();
        let mut ready = Vec::new();
        for pending in self.pending.drain(..) {
            match &pending.kind {
                PendingKind::Event { name } if name == &event.name => ready.push(pending),
                _ => still_pending.push(pending),
            }
        }
        self.pending = still_pending;
        for mut pending in ready {
            pending.ctx.source_event = Some(event.clone());
            let result = execute_node(&pending.node, world, &mut pending.ctx, registry)?;
            self.emitted.append(&mut pending.ctx.emitted);
            self.extend_pending(result.pending);
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn drain_emitted_events(&mut self) -> impl Iterator<Item = SkillRuntimeEvent> + '_ {
        self.emitted.drain(..)
    }

    fn extend_pending(&mut self, pending: impl IntoIterator<Item = PendingSkillExecution>) {
        self.pending.extend(pending.into_iter().map(|mut pending| {
            pending.ctx.emitted.clear();
            pending
        }));
    }
}

#[derive(Clone, Debug)]
pub struct PendingSkillExecution {
    kind: PendingKind,
    node: SkillNode,
    ctx: SkillContext,
}

#[derive(Clone, Debug)]
enum PendingKind {
    Delay { remaining: f64 },
    Event { name: String },
}

#[derive(Default)]
struct ExecResult {
    pending: Vec<PendingSkillExecution>,
}

fn execute_node(
    node: &SkillNode,
    world: &mut World,
    ctx: &mut SkillContext,
    registry: &SkillRegistry,
) -> Result<ExecResult, SkillError> {
    match node {
        SkillNode::Sequence(nodes) => execute_sequence(nodes, world, ctx, registry),
        SkillNode::Parallel(nodes) => {
            let mut result = ExecResult::default();
            for node in nodes {
                result
                    .pending
                    .extend(execute_node(node, world, ctx, registry)?.pending);
            }
            Ok(result)
        }
        SkillNode::Delay(seconds, node) => {
            let seconds = number(&eval_skill_expr(seconds, ctx)?)?;
            Ok(ExecResult {
                pending: vec![PendingSkillExecution {
                    kind: PendingKind::Delay { remaining: seconds },
                    node: (**node).clone(),
                    ctx: ctx.clone(),
                }],
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
            execute_node(&SkillNode::Sequence(sequence), world, ctx, registry)
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
                execute_node(branch, world, ctx, registry)
            } else {
                Ok(ExecResult::default())
            }
        }
        SkillNode::Let(name, value, node) => {
            let previous = ctx.vars.insert(name.clone(), value.clone());
            let result = execute_node(node, world, ctx, registry);
            match previous {
                Some(previous) => ctx.vars.insert(name.clone(), previous),
                None => ctx.vars.shift_remove(name),
            };
            result
        }
        SkillNode::On(name, node) => Ok(ExecResult {
            pending: vec![PendingSkillExecution {
                kind: PendingKind::Event { name: name.clone() },
                node: (**node).clone(),
                ctx: ctx.clone(),
            }],
        }),
        SkillNode::Emit(name, payload) => {
            let payload = resolve_args(payload, ctx)?;
            ctx.emitted
                .push(SkillRuntimeEvent::new(name.clone(), payload));
            Ok(ExecResult::default())
        }
        SkillNode::Action(id, args) => {
            let action = registry
                .action(id)
                .ok_or_else(|| SkillError::UnknownAction(id.clone()))?;
            let args = resolve_args(args, ctx)?;
            action.execute(world, ctx, &args)?;
            Ok(ExecResult::default())
        }
        SkillNode::Deck(_) | SkillNode::Spell(_, _) | SkillNode::Modifier(_, _) => Err(
            SkillError::Runtime("extension node cannot be executed by the core runner".to_owned()),
        ),
    }
}

fn execute_sequence(
    nodes: &[SkillNode],
    world: &mut World,
    ctx: &mut SkillContext,
    registry: &SkillRegistry,
) -> Result<ExecResult, SkillError> {
    let mut result = ExecResult::default();
    for (index, node) in nodes.iter().enumerate() {
        let mut child = execute_node(node, world, ctx, registry)?;
        if !child.pending.is_empty() {
            let rest = nodes[index + 1..].to_vec();
            if !rest.is_empty() {
                for pending in &mut child.pending {
                    let mut sequence = Vec::with_capacity(rest.len() + 1);
                    sequence.push(pending.node.clone());
                    sequence.extend(rest.clone());
                    pending.node = SkillNode::Sequence(sequence);
                }
            }
            result.pending.extend(child.pending);
            return Ok(result);
        }
    }
    Ok(result)
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

pub fn trace_arg(args: &SkillArgs, key: &str) -> Option<String> {
    match args.get(key) {
        Some(SkillValue::String(value)) => Some(value.clone()),
        Some(SkillValue::Number(value)) => Some(value.to_string()),
        Some(SkillValue::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

pub fn spawn_empty(world: &mut World) -> Entity {
    world.spawn_empty().id()
}
