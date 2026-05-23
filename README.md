# bevy_skill_flow

A small, data-driven skill DSL and runtime for Bevy games.

`bevy_skill_flow` is the human-authored RON DSL layer. It compiles skill files
into deterministic execution plans and an ECS-facing `SkillGraph` supplied by
`bevy_skill_ecs`. Game semantics stay outside both core crates: host games
provide concrete actions, modifiers, cast models, and systems such as
projectiles, damage, buffs, or deck casting.

[中文文档](README.zh-CN.md)

## Workspace

This repository is organized as a Cargo workspace:

- `bevy_skill_flow`: the core DSL, compiler, registry, runtime, RON loading, and optional editor support.
- `bevy_skill_ecs`: the lower-level ECS skill graph, runtime protocol, system sets, messages, relationships, and execution configuration.
- `bevy_skill_flow_gameplay`: a companion gameplay crate with reusable example primitives.

The companion crate currently provides:

- `TraceAction`
- `SpawnProjectileAction`
- `DamageAction`
- `ApplyBuffAction`
- `SpellAction`
- `WandDeckCastModel`
- `register_gameplay_primitives`

Use the companion crate when you want a ready-made reference vocabulary. For a
real game, you can replace it with your own action, modifier, and cast-model
implementations.

## Features

- `demo_2d`: enables Bevy 2D support for the interactive demo.
- `editor`: enables the skill editor demo and pulls in `bevy_egui`.
- `full_runtime_entities`: materializes runtime graph nodes as debug entities with root, child, payload, and execution relationship components.

The core dependency on Bevy is intentionally minimal:

```toml
bevy = { version = "0.18.1", default-features = false }
```

## Quick Start

Run the core tests:

```sh
cargo test -p bevy_skill_flow --all-targets --no-default-features
```

Run the gameplay companion tests:

```sh
cargo test -p bevy_skill_flow_gameplay
```

Run the small command-line examples:

```sh
cargo run -p bevy_skill_flow --example fireball --no-default-features
cargo run -p bevy_skill_flow --example wand_deck --no-default-features
```

Run the 2D demo:

```sh
cargo run -p bevy_skill_flow --example comprehensive_2d --features demo_2d
```

Run the editor demo:

```sh
cargo run -p bevy_skill_flow --example skill_editor --features editor
```

The editor demo reads and seeds example skills in `examples/assets/skills`.

## DSL Shape

Skills are usually authored as RON:

```ron
Skill(
  id: "fireball",
  tags: ["spell", "projectile", "fire"],
  params: {
    "base_damage": 40.0,
    "projectile_count": 1.0,
  },
  body: Action("spawn_projectile", {
    "prefab": "fireball",
    "count": Expr("stat.projectile_count"),
  }),
)
```

The core DSL supports nodes such as:

- `Action`
- `Sequence`
- `Parallel`
- `Repeat`
- `Delay`
- `On`
- `Emit`
- `Modifier`
- `Deck`
- `Spell`

The core does not decide what `spawn_projectile`, `damage`, or `spell` mean.
RON compilation preserves those identifiers as graph extension nodes; the ECS
runtime resolves them through `SkillActionRegistry`.

Compilation produces a runtime-authoritative `SkillGraph`: an ordered
ECS-facing graph with child slots and payload slots such as `on_hit` or
`on_expire`.

## Registering Game Semantics

Use `SkillRegistry` for compile-time modifiers/cast models, and
`SkillActionRegistry` for runtime action behavior:

```rust
use bevy_skill_ecs::SkillActionRegistry;
use bevy_skill_flow::{SkillAssetSources, SkillRegistry};
use bevy_skill_flow_gameplay::register_gameplay_primitives;

let mut registry = SkillRegistry::with_core();
let mut actions = SkillActionRegistry::new();
register_gameplay_primitives(&mut registry, &mut actions);

let mut sources = SkillAssetSources::default();
sources.set_source("memory://skill.ron", skill_source);
```

For your own game, implement these traits:

- `SkillAction`: runtime behavior for `Action(...)` graph extension nodes.
- `SkillModifier`: transforms skill params before graph compilation.
- `CastModel`: compiles alternate body formats such as deck-based casting.

## Runtime Model

Compiled skills are stored in `bevy_skill_ecs::SkillLibrary`. Runtime execution
lives in `bevy_skill_ecs` and uses the compiled `SkillGraph`: each cast request
spawns an `ActiveSkill` entity, drains the ordered graph queue, and exposes
gameplay effects as Bevy messages.

Typical flow:

1. Register actions in `SkillActionRegistry`; register modifiers and cast models in `SkillRegistry`.
2. Load RON into `SkillLibrary`.
3. Send `SkillCastRequest { skill, caster, target }`.
4. Let `SkillEcsPlugin` (installed by `SkillDslPlugin`) spawn and tick `ActiveSkill` entities.
5. Consume `SkillIntent` messages in normal gameplay systems.
6. Send `SkillRuntimeSignal` messages for `On(...)` nodes.
7. Read `SkillRuntimeSignal` messages emitted by `Emit(...)` nodes when needed.

`SkillDslPlugin` installs `SkillEcsPlugin` plus the DSL asset resources:

- `SkillRegistry`
- `SkillLibrary`
- `SkillAssetSources`
- `SkillRuntimeCounters`
- `SkillRuntimeConfig`
- `SkillResourcePools`
- `SkillCooldowns`

The runtime resources/messages/systems above are owned by `bevy_skill_ecs`;
`bevy_skill_flow` adds RON source compilation into `SkillLibrary`.

`SkillAssetSources` is a lightweight hot-reload source table. Insert or replace
RON text with `set_source`; the runtime compiles dirty sources, updates
`SkillLibrary`, and emits `SkillAssetReloaded` or `SkillAssetReloadFailed`.
New casts use the newly compiled `SkillGraph`.

The lower-level `SkillEcsPlugin` from `bevy_skill_ecs` installs the protocol
messages and fixed runtime sets:

```text
Asset -> Request -> Validate -> Execute -> Effect -> Message -> Trigger -> Cleanup
```

The runtime drains synchronous chains deterministically and uses
`SkillRuntimeConfig::step_budget` to stop runaway chains.

Skill requirements are validated before an execution starts. `Cost` checks and
spends values from `SkillResourcePools`; `Cooldown` checks and starts entries in
`SkillCooldowns`. These are generic protocol resources keyed by caster and
resource/skill id, so games can decide what names like `mana`, `energy`, or
`charges` mean.

The core runtime only knows generic `SkillEffectRequest` /
`SkillEffectResolved` messages. `DamageAction` in the gameplay companion maps
damage onto that protocol and exposes gameplay-level `DamageRequest` /
`DamageResolved` aliases in three settlement modes:

- `sync`: emits `DamageResolved` immediately and exposes `var.last_damage_amount`.
- `request`: emits the request and lets the skill continue.
- `await`: emits the request, pauses the execution branch, and resumes when a matching `DamageResolved` arrives.

The gameplay companion owns `ProjectileHit` and bridges it into
`SkillRuntimeSignal`, so `On("hit", ...)` / `On("projectile_hit", ...)`
payloads can resume without making projectiles a core concept. `ApplyBuffAction`
maps buff application onto a generic timed skill effect, runs optional `on_add`
hooks immediately, and stores optional `on_remove` hooks until its duration
expires.

For reactive Bevy Observer integration, trigger `SkillObserverTrigger`. The
plugin registers an observer that converts it into a runtime signal, so
`On("event_name", ...)` payloads can resume from either messages or observers.
Set `skill_entity` to target a specific active skill execution; `execution_id`
can narrow the signal further. `caster` and `target` are copied
into the event context for expressions such as `event.target`.

## Project Layout

```text
src/                         Flow DSL crate
crates/bevy_skill_ecs/       ECS graph and runtime protocol crate
crates/bevy_skill_flow_gameplay/
examples/                    Command-line, 2D, and editor demos
examples/assets/skills/      Example RON skills for the editor/demo
tests/                       Integration tests
```

## Development Checks

Useful local checks:

```sh
cargo fmt
cargo test -p bevy_skill_flow --all-targets --no-default-features
cargo test -p bevy_skill_ecs
cargo test -p bevy_skill_flow_gameplay
cargo check -p bevy_skill_flow --example comprehensive_2d --features demo_2d
cargo check -p bevy_skill_flow --example skill_editor --features editor
```

## Status

This project is early and experimental. The core API is intentionally small, and
the gameplay crate is a companion/reference layer rather than a required part of
the runtime.
