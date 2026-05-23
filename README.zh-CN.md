# bevy_skill_flow

一个面向 Bevy 游戏的小型数据驱动技能 DSL 与运行时。

`bevy_skill_flow` 的核心目标是：核心 crate 不包含具体游戏语义。核心只负责解析、校验、编译和运行技能图；投射物、伤害、治疗、增益、卡组施法等概念由宿主游戏通过 action、modifier 和 cast model 注册进来。

[English README](README.md)

## Workspace 结构

当前仓库是一个 Cargo workspace：

- `bevy_skill_flow`：核心 DSL、编译器、注册表、运行时、RON 加载，以及可选编辑器支持。
- `bevy_skill_flow_gameplay`：附带的正式玩法扩展包，提供可复用的示例游戏语义。

`bevy_skill_flow_gameplay` 当前提供：

- `TraceAction`
- `SpawnProjectileAction`
- `DamageAction`
- `SpellAction`
- `WandDeckCastModel`
- `register_gameplay_primitives`

如果你想快速试用一套现成的参考语义，可以使用这个 companion crate。真实项目里也可以完全替换成自己的 action、modifier 和 cast model。

## Features

- `demo_2d`：启用 Bevy 2D 支持，用于交互式 demo。
- `editor`：启用技能编辑器 demo，并引入 `bevy_egui`。

核心 crate 对 Bevy 的依赖保持得比较轻：

```toml
bevy = { version = "0.18.1", default-features = false }
```

## 快速开始

运行核心测试：

```sh
cargo test -p bevy_skill_flow --all-targets --no-default-features
```

运行玩法扩展包测试：

```sh
cargo test -p bevy_skill_flow_gameplay
```

运行两个命令行示例：

```sh
cargo run -p bevy_skill_flow --example fireball --no-default-features
cargo run -p bevy_skill_flow --example wand_deck --no-default-features
```

运行 2D demo：

```sh
cargo run -p bevy_skill_flow --example comprehensive_2d --features demo_2d
```

运行技能编辑器 demo：

```sh
cargo run -p bevy_skill_flow --example skill_editor --features editor
```

编辑器 demo 会读取并初始化 `examples/assets/skills` 下的示例技能。

## DSL 形态

技能通常用 RON 编写：

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

核心 DSL 支持这些节点：

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

核心不会定义 `spawn_projectile`、`damage` 或 `spell` 的真实含义。这些名字会通过 `SkillRegistry` 绑定到宿主游戏提供的实现。

## 注册游戏语义

使用 `SkillRegistry` 把技能节点名绑定到游戏行为：

```rust
use bevy_skill_flow::{SkillLibrary, SkillRegistry};
use bevy_skill_flow_gameplay::register_gameplay_primitives;

let mut registry = SkillRegistry::with_core();
register_gameplay_primitives(&mut registry);

let mut library = SkillLibrary::default();
library.replace_from_ron(skill_source, &registry)?;
```

如果要接入自己的游戏，需要实现这些 trait：

- `SkillAction`：校验并执行 `Action(...)` 节点。
- `SkillModifier`：在编译前转换 stats 或技能计划。
- `CastModel`：编译特殊施法模型，比如卡组式施法。

## 运行时模型

编译后的技能存放在 `SkillLibrary` 中。运行时执行由 `PendingSkillExecutions` 管理。

典型流程：

1. 注册 actions、modifiers 和 cast models。
2. 将 RON 加载到 `SkillLibrary`。
3. 通过 `SkillId` 取得编译后的技能。
4. 调用 `PendingSkillExecutions::cast`。
5. 用经过的时间 tick 待执行技能。
6. 当游戏事件发生时，为 `On(...)` 节点触发 runtime event。
7. 需要时读取 `Emit(...)` 节点产生的事件。

`SkillDslPlugin` 会安装核心 Bevy resources：

- `SkillRegistry`
- `SkillLibrary`
- `PendingSkillExecutions`

## 项目布局

```text
src/                         核心 crate
crates/bevy_skill_flow_gameplay/
examples/                    命令行、2D 和编辑器 demo
examples/assets/skills/      供编辑器/demo 使用的 RON 示例技能
tests/                       集成测试
```

## 开发检查

常用本地检查命令：

```sh
cargo fmt
cargo test -p bevy_skill_flow --all-targets --no-default-features
cargo test -p bevy_skill_flow_gameplay
cargo check -p bevy_skill_flow --example comprehensive_2d --features demo_2d
cargo check -p bevy_skill_flow --example skill_editor --features editor
```

## 状态

项目仍处于早期实验阶段。核心 API 会尽量保持小而清晰；`bevy_skill_flow_gameplay` 是 companion/reference 层，不是运行时必须依赖的一部分。
