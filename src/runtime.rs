//! Runtime protocol re-exported from `bevy_skill_ecs`.
//!
//! `bevy_skill_flow` owns the RON DSL and graph compiler. Execution lives in
//! `bevy_skill_ecs`.

pub use bevy_skill_ecs::{
    ActiveSkill, ActiveSkillEffect, SkillAction, SkillActionInput, SkillActionOutput,
    SkillActionRegistry, SkillActionWait, SkillBranch, SkillContext, SkillContinuation,
    SkillCooldowns, SkillError, SkillExecutionFailed, SkillExecutionState, SkillIntent,
    SkillObserverTrigger, SkillResourcePools, SkillResult, SkillRuntimeCounters,
    SkillRuntimeSignal, SkillWait, handle_skill_cast_requests, resume_effect_resolved,
    resume_skill_signals, skill_observer_trigger_bridge, tick_skill_cooldowns, tick_skill_delays,
    tick_skill_effects,
};
