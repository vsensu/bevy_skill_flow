//! A small, data-driven skill DSL for Bevy games.
//!
//! The crate keeps game semantics out of the core. Concrete gameplay concepts
//! are registered as actions, modifiers, or cast models by the host game.

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
    SettlementMode, SkillCastAccepted, SkillCastRejected, SkillCastRequest, SkillEcsPlugin,
    SkillEffectRequest, SkillEffectResolved, SkillExecutionFinished, SkillGraph, SkillGraphError,
    SkillGraphHandle, SkillGraphNode, SkillGraphNodeKind, SkillNodeId, SkillRuntimeConfig,
    SkillRuntimeSet,
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
        app.add_plugins(SkillEcsPlugin)
            .init_resource::<SkillRegistry>()
            .init_resource::<SkillAssetSources>()
            .add_message::<SkillAssetReloaded>()
            .add_message::<SkillAssetReloadFailed>()
            .configure_sets(
                FixedUpdate,
                SkillRuntimeSet::Asset.before(SkillRuntimeSet::Request),
            )
            .add_systems(
                FixedUpdate,
                compile_dirty_skill_assets.in_set(SkillRuntimeSet::Asset),
            );
    }
}
