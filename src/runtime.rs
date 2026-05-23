//! Runtime protocol re-exported from `bevy_skill_ecs`.
//!
//! `bevy_skill_flow` owns the RON DSL and graph compiler. Execution lives in
//! `bevy_skill_ecs`.

pub use bevy_skill_ecs::{
    ActiveSkill, ActiveSkillEffect, SkillAction, SkillActionInput, SkillActionOutput,
    SkillActionRegistry, SkillActionWait, SkillBranch, SkillContext, SkillCooldowns, SkillError,
    SkillExecutionFailed, SkillExecutionState, SkillIntent, SkillObserverTrigger,
    SkillResourcePools, SkillResult, SkillRuntimeCounters, SkillRuntimeNode, SkillRuntimeSignal,
    SkillWait, handle_skill_cast_requests, resume_effect_resolved, resume_skill_signals,
    skill_observer_trigger_bridge, tick_skill_cooldowns, tick_skill_delays, tick_skill_effects,
};

#[cfg(feature = "full_runtime_entities")]
pub use bevy_skill_ecs::SkillRuntimeDebugNode;
