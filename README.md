# bevy_skill_flow

A small, data-driven skill DSL and runtime for Bevy games.

`bevy_skill_flow` keeps game semantics out of the core crate. The core knows how
to parse, validate, compile, and run skill graphs; host games provide concrete
actions, modifiers, and cast models such as projectiles, damage, buffs, or deck
casting.

[中文文档](README.zh-CN.md)

## Workspace

This repository is organized as a Cargo workspace:

- `bevy_skill_flow`: the core DSL, compiler, registry, runtime, RON loading, and optional editor support.
- `bevy_skill_flow_gameplay`: a companion gameplay crate with reusable example primitives.

The companion crate currently provides:

- `TraceAction`
- `SpawnProjectileAction`
- `DamageAction`
- `SpellAction`
- `WandDeckCastModel`
- `register_gameplay_primitives`

Use the companion crate when you want a ready-made reference vocabulary. For a
real game, you can replace it with your own action, modifier, and cast-model
implementations.

## Features

- `demo_2d`: enables Bevy 2D support for the interactive demo.
- `editor`: enables the skill editor demo and pulls in `bevy_egui`.

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
Those identifiers are resolved through a `SkillRegistry`.

## Registering Game Semantics

Use `SkillRegistry` to bind skill node names to host-game behavior:

```rust
use bevy_skill_flow::{SkillLibrary, SkillRegistry};
use bevy_skill_flow_gameplay::register_gameplay_primitives;

let mut registry = SkillRegistry::with_core();
register_gameplay_primitives(&mut registry);

let mut library = SkillLibrary::default();
library.replace_from_ron(skill_source, &registry)?;
```

For your own game, implement these traits:

- `SkillAction`: validates and executes `Action(...)` nodes.
- `SkillModifier`: transforms stats or plans before compilation.
- `CastModel`: compiles alternate body formats such as deck-based casting.

## Runtime Model

Compiled skills are stored in `SkillLibrary`. Runtime execution is managed by
`PendingSkillExecutions`.

Typical flow:

1. Register actions, modifiers, and cast models.
2. Load RON into `SkillLibrary`.
3. Fetch a compiled skill by `SkillId`.
4. Call `PendingSkillExecutions::cast`.
5. Tick pending execution with elapsed time.
6. Trigger runtime events for `On(...)` nodes when game events happen.
7. Drain emitted events from `Emit(...)` nodes when needed.

`SkillDslPlugin` installs the core Bevy resources:

- `SkillRegistry`
- `SkillLibrary`
- `PendingSkillExecutions`

## Project Layout

```text
src/                         Core crate
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
cargo test -p bevy_skill_flow_gameplay
cargo check -p bevy_skill_flow --example comprehensive_2d --features demo_2d
cargo check -p bevy_skill_flow --example skill_editor --features editor
```

## Status

This project is early and experimental. The core API is intentionally small, and
the gameplay crate is a companion/reference layer rather than a required part of
the runtime.
