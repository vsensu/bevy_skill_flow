# bevy_skill_flow

一个面向 Bevy 游戏的小型数据驱动技能 DSL 与运行时。

`bevy_skill_flow` 是给人编写的 RON DSL 层。它会把技能文件编译成确定顺序的执行计划，以及由 `bevy_skill_ecs` 提供的 ECS-facing `SkillGraph`。具体游戏语义仍然不进入两个核心 crate：投射物、伤害、治疗、增益、卡组施法等概念由宿主游戏通过 action、modifier、cast model 和普通 systems 注册进来。

[English README](README.md)

## Workspace 结构

当前仓库是一个 Cargo workspace：

- `bevy_skill_flow`：上层 DSL、编译器、注册表、运行时、RON 加载，以及可选编辑器支持。
- `bevy_skill_ecs`：底层 ECS 技能图、运行时协议、系统阶段、消息、关系与执行配置。
- `bevy_skill_flow_gameplay`：附带的正式玩法扩展包，提供可复用的示例游戏语义。

`bevy_skill_flow_gameplay` 当前提供：

- `TraceAction`
- `SpawnProjectileAction`
- `DamageAction`
- `ApplyBuffAction`
- `SpellAction`
- `WandDeckCastModel`
- `register_gameplay_primitives`

如果你想快速试用一套现成的参考语义，可以使用这个 companion crate。真实项目里也可以完全替换成自己的 action、modifier 和 cast model。

## Features

- `demo_2d`：启用 Bevy 2D 支持，用于交互式 demo。
- `editor`：启用技能编辑器 demo，并引入 `bevy_egui`。
- `full_runtime_entities`：把运行时 graph 节点实体化为调试 entities，并附带 root、child、payload 和 execution 关系组件。

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

编译结果是运行时权威的 `SkillGraph`：面向 ECS 的有序技能图，保留 child slot 与 `on_hit`、`on_expire` 等 payload slot。

## 注册游戏语义

使用 `SkillRegistry` 把技能节点名绑定到游戏行为：

```rust
use bevy_skill_flow::{SkillAssetSources, SkillRegistry};
use bevy_skill_flow_gameplay::register_gameplay_primitives;

let mut registry = SkillRegistry::with_core();
register_gameplay_primitives(&mut registry);

let mut sources = SkillAssetSources::default();
sources.set_source("memory://skill.ron", skill_source);
```

如果要接入自己的游戏，需要实现这些 trait：

- `SkillAction`：校验 `Action(...)` 节点，并产生 `SkillIntent` 消息。
- `SkillModifier`：在 graph 编译前转换技能 params。
- `CastModel`：编译特殊施法模型，比如卡组式施法。

## 运行时模型

编译后的技能存放在 `SkillLibrary` 中。运行时执行使用编译后的 `SkillGraph`：
每次施法请求都会生成一个 `ActiveSkill` entity，按 graph queue 的确定顺序推进，具体玩法效果通过 Bevy 消息交给普通 systems 处理。

典型流程：

1. 注册 actions、modifiers 和 cast models。
2. 将 RON 加载到 `SkillLibrary`。
3. 发送 `SkillCastRequest { skill, caster, target }`。
4. 由 `SkillDslPlugin` 创建并 tick `ActiveSkill` entities。
5. 在普通 gameplay systems 中消费 `SkillIntent` 消息。
6. 当游戏事件发生时，发送 `SkillRuntimeSignal` 以恢复 `On(...)` 节点。
7. 需要时读取 `Emit(...)` 节点产生的 `SkillRuntimeSignal`。

`SkillDslPlugin` 会安装核心 Bevy resources：

- `SkillRegistry`
- `SkillLibrary`
- `SkillAssetSources`
- `SkillRuntimeCounters`
- `SkillRuntimeConfig`
- `SkillResourcePools`
- `SkillCooldowns`

它也会注册核心技能消息和运行时 systems。

`SkillAssetSources` 是一个轻量热重载 source table。通过 `set_source` 插入或替换 RON 文本后，runtime 会编译 dirty source、更新 `SkillLibrary`，并发出 `SkillAssetReloaded` 或 `SkillAssetReloadFailed`。新的释放会使用新编译出的 `SkillGraph`。

底层 `bevy_skill_ecs::SkillEcsPlugin` 会安装协议消息与固定运行时阶段：

```text
Asset -> Request -> Validate -> Execute -> Effect -> Message -> Trigger -> Cleanup
```

运行时会按确定顺序 drain 同步链，并通过 `SkillRuntimeConfig::step_budget` 阻止失控链式执行。

技能 requirements 会在 execution 开始前校验。`Cost` 会检查并扣除 `SkillResourcePools` 中的数值；`Cooldown` 会检查并写入 `SkillCooldowns`。这两者都是按 caster 与 resource/skill id 索引的通用协议资源，真实项目可以自行决定 `mana`、`energy`、`charges` 等名字的含义。

玩法扩展包里的 `DamageAction` 可以发出协议层 `DamageRequest`，支持三种结算模式：

- `sync`：立即发出 `DamageResolved`，并写入 `var.last_damage_amount`。
- `request`：只发请求，技能流程继续。
- `await`：发请求后暂停当前执行分支，等匹配的 `DamageResolved` 到达后继续。

`ProjectileHit` 消息会恢复同一 execution 上的 `On("hit", ...)` / `On("projectile_hit", ...)` payload。`ApplyBuffAction` 会发出 `ApplyBuffRequest`，立即执行可选 `on_add` hook，并把可选 `on_remove` hook 保存在 buff entity 上，持续时间结束时执行。

如果要接入 Bevy Observer，可以触发 `SkillObserverTrigger`。插件会注册 observer，把它转换成 runtime signal，因此 `On("event_name", ...)` payload 既可以由消息恢复，也可以由 observer 触发恢复。设置 `skill_entity` 可以定向恢复某一次 active skill execution，`execution_id` 可以进一步限定同一次执行；`caster` 和 `target` 会复制到事件上下文，供 `event.target` 之类的表达式读取。

## 项目布局

```text
src/                         Flow DSL crate
crates/bevy_skill_ecs/       ECS 图与运行时协议 crate
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
cargo test -p bevy_skill_ecs
cargo test -p bevy_skill_flow_gameplay
cargo check -p bevy_skill_flow --example comprehensive_2d --features demo_2d
cargo check -p bevy_skill_flow --example skill_editor --features editor
```

## 状态

项目仍处于早期实验阶段。核心 API 会尽量保持小而清晰；`bevy_skill_flow_gameplay` 是 companion/reference 层，不是运行时必须依赖的一部分。
