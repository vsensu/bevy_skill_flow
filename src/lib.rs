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
pub use compile::*;
pub use dsl::*;
pub use expr::*;
pub use registry::*;
pub use runtime::*;

use bevy::prelude::{App, Plugin};

/// Installs the resources used by the runtime.
#[derive(Debug, Default)]
pub struct SkillDslPlugin;

impl Plugin for SkillDslPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<SkillRegistry>()
            .init_resource::<SkillLibrary>()
            .init_resource::<PendingSkillExecutions>();
    }
}
