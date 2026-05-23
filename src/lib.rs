//! A small, data-driven skill DSL for Bevy games.
//!
//! The crate keeps game semantics out of the core. Damage, healing,
//! projectiles, buffs, deck casting, and similar concepts are registered as
//! actions, modifiers, or cast models by the host game.

pub mod asset;
pub mod compile;
pub mod dsl;
#[cfg(feature = "editor")]
pub mod editor;
pub mod expr;
pub mod registry;
pub mod runtime;

pub use asset::*;
pub use bevy_skill_ecs::{
    ApplyBuffRequest, DamageRequest, DamageResolved, ProjectileHit, SettlementMode,
    SkillCastAccepted, SkillCastRejected, SkillCastRequest, SkillEcsPlugin, SkillExecutionFinished,
    SkillGraph, SkillGraphError, SkillGraphHandle, SkillGraphNode, SkillGraphNodeKind, SkillNodeId,
    SkillRuntimeConfig, SkillRuntimeSet,
};
pub use compile::*;
pub use dsl::*;
pub use expr::*;
pub use registry::*;
pub use runtime::*;

use bevy::prelude::{App, FixedUpdate, IntoScheduleConfigs, Plugin};

/// Installs the resources used by the runtime.
#[derive(Debug, Default)]
pub struct SkillDslPlugin;

impl Plugin for SkillDslPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SkillRegistry>()
            .init_resource::<SkillLibrary>()
            .init_resource::<SkillAssetSources>()
            .init_resource::<SkillRuntimeCounters>()
            .init_resource::<SkillRuntimeConfig>()
            .init_resource::<SkillResourcePools>()
            .init_resource::<SkillCooldowns>()
            .add_message::<SkillCastRequest>()
            .add_message::<SkillCastAccepted>()
            .add_message::<SkillCastRejected>()
            .add_message::<SkillExecutionFinished>()
            .add_message::<SkillExecutionFailed>()
            .add_message::<SkillRuntimeSignal>()
            .add_message::<SkillIntent>()
            .add_message::<SkillAssetReloaded>()
            .add_message::<SkillAssetReloadFailed>()
            .add_message::<DamageRequest>()
            .add_message::<DamageResolved>()
            .add_message::<ApplyBuffRequest>()
            .add_message::<ProjectileHit>()
            .add_observer(skill_observer_trigger_bridge)
            .configure_sets(
                FixedUpdate,
                (
                    SkillRuntimeSet::Asset,
                    SkillRuntimeSet::Request,
                    SkillRuntimeSet::Validate,
                    SkillRuntimeSet::Execute,
                    SkillRuntimeSet::Effect,
                    SkillRuntimeSet::Message,
                    SkillRuntimeSet::Trigger,
                    SkillRuntimeSet::Cleanup,
                )
                    .chain(),
            )
            .add_systems(
                FixedUpdate,
                (
                    compile_dirty_skill_assets.in_set(SkillRuntimeSet::Asset),
                    tick_skill_cooldowns.in_set(SkillRuntimeSet::Request),
                    (handle_skill_cast_requests, tick_skill_delays)
                        .chain()
                        .in_set(SkillRuntimeSet::Execute),
                    resume_damage_resolved.in_set(SkillRuntimeSet::Message),
                    (resume_projectile_hits, resume_skill_signals)
                        .chain()
                        .in_set(SkillRuntimeSet::Trigger),
                    tick_skill_buffs.in_set(SkillRuntimeSet::Cleanup),
                ),
            );
    }
}
