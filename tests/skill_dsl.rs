use bevy::ecs::relationship::RelationshipTarget;
use bevy::prelude::{App, FixedUpdate, Messages, Mut, Time};
use bevy_skill_ecs::{
    CompiledSkill, SettlementMode, SkillActionInput, SkillActionRegistry, SkillChildOf,
    SkillChildren, SkillEffectRequest, SkillEffectResolved, SkillExpr as EcsSkillExpr, SkillGraph,
    SkillGraphNodeKind, SkillGraphNodeRef, SkillId as EcsSkillId, SkillNodeOf,
    SkillParams as RuntimeArgs, SkillPayloadOf, SkillPayloads, SkillRoots, SkillRuntimeConfig,
    SkillValue as EcsSkillValue, eval_skill_expr as eval_runtime_skill_expr,
    replace_compiled_skill_world,
};
#[cfg(feature = "full_runtime_entities")]
use bevy_skill_ecs::{ExecutionOfSkill, SkillRootOf};
use bevy_skill_flow::{
    ActiveSkill, ActiveSkillEffect, ModifierDef, SkillAction, SkillActionOutput, SkillAssetSources,
    SkillCastRequest, SkillContext, SkillDslPlugin, SkillError, SkillExecutionFailed, SkillIntent,
    SkillObserverTrigger, SkillRegistry, SkillResourcePools, SkillResult, SkillRuntimeSignal,
    SkillValue, StatModifier, compile_skill, parse_skill_def, parse_skill_document,
};
use bevy_skill_flow::{SkillCompiled, SkillLibrary};
use bevy_skill_flow_gameplay::{
    ApplyBuffRequest, DamageResolved, ProjectileHit, SkillGameplayPlugin,
    register_gameplay_primitives,
};
use std::time::Duration;

struct TestRegistries {
    compile: SkillRegistry,
    actions: SkillActionRegistry,
}

impl std::ops::Deref for TestRegistries {
    type Target = SkillRegistry;

    fn deref(&self) -> &Self::Target {
        &self.compile
    }
}

impl std::ops::DerefMut for TestRegistries {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.compile
    }
}

impl TestRegistries {
    fn register_skill_action<A>(&mut self, id: impl Into<String>, action: A) -> &mut Self
    where
        A: SkillAction,
    {
        self.actions.register_skill_action(id, action);
        self
    }
}

fn registry() -> TestRegistries {
    let mut registries = TestRegistries {
        compile: SkillRegistry::with_core(),
        actions: SkillActionRegistry::new(),
    };
    register_gameplay_primitives(&mut registries.compile, &mut registries.actions);
    registries
}

fn skill_update(app: &mut App) {
    app.update();
    app.world_mut().run_schedule(FixedUpdate);
}

#[test]
fn parses_single_and_multi_skill_ron() {
    let single = r#"
        Skill(
          id: "fireball",
          tags: ["spell", "projectile"],
          body: Action("trace", { "label": "cast" }),
        )
    "#;
    let parsed = parse_skill_def(single).unwrap();
    assert_eq!(parsed.id.0, "fireball");
    assert_eq!(parsed.cast_model, "direct");

    let many = r#"
        [
          Skill(id: "a", body: Action("trace", { "label": "a" })),
          Skill(id: "b", body: Action("trace", { "label": "b" })),
        ]
    "#;
    assert_eq!(parse_skill_document(many).unwrap().len(), 2);
}

#[test]
fn compile_and_runtime_reject_unknown_primitives_at_their_boundary() {
    let skill = parse_skill_def(
        r#"
        Skill(id: "bad", body: Action("missing", {}))
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry()).unwrap();
    let mut app = runtime_app(registry(), compiled);
    let caster = app.world_mut().spawn_empty().id();
    app.world_mut().write_message(SkillCastRequest {
        skill: "bad".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    let failures = take_failures(&mut app);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].message.contains("missing"));

    let skill = parse_skill_def(
        r#"
        Skill(id: "bad", cast_model: "missing", body: Action("trace", {}))
    "#,
    )
    .unwrap();
    let err = compile_skill(&skill, &registry()).unwrap_err();
    assert_eq!(err, SkillError::UnknownCastModel("missing".to_owned()));

    let skill = parse_skill_def(
        r#"
        Skill(id: "bad", modifiers: ["missing"], body: Action("trace", {}))
    "#,
    )
    .unwrap();
    let err = compile_skill(&skill, &registry()).unwrap_err();
    assert_eq!(err, SkillError::UnknownModifier("missing".to_owned()));
}

#[test]
fn expr_eval_supports_paths_math_comparison_and_nullish() {
    let skill = compiled_with_stats(vec![
        ("base_damage".to_owned(), SkillValue::Number(40.0)),
        ("spell_damage".to_owned(), SkillValue::Number(1.5)),
    ]);
    let ctx = SkillContext::new(
        skill.id.clone(),
        skill.graph.params.clone(),
        skill.tags.clone(),
        None,
        1,
    );
    assert_eq!(
        eval_runtime_skill_expr(
            &EcsSkillExpr::new("stat.base_damage * stat.spell_damage"),
            &ctx
        )
        .unwrap(),
        EcsSkillValue::Number(60.0)
    );
    assert_eq!(
        eval_runtime_skill_expr(&EcsSkillExpr::new("stat.projectile_count ?? 1"), &ctx).unwrap(),
        EcsSkillValue::Number(1.0)
    );
    assert_eq!(
        eval_runtime_skill_expr(&EcsSkillExpr::new("stat.base_damage >= 40 && true"), &ctx)
            .unwrap(),
        EcsSkillValue::Bool(true)
    );
}

#[test]
fn modifier_applies_in_order_and_keeps_original_skill_def_clean() {
    let mut registry = registry();
    registry.register_skill_modifier(
        "greater_multiple_projectiles",
        StatModifier::new(
            vec!["projectile".to_owned()],
            vec![
                bevy_skill_flow::StatOp::Add(
                    "projectile_count".to_owned(),
                    SkillValue::Number(4.0),
                ),
                bevy_skill_flow::StatOp::Mul(
                    "projectile_damage".to_owned(),
                    SkillValue::Number(0.74),
                ),
                bevy_skill_flow::StatOp::Set(
                    "projectile_spread_degrees".to_owned(),
                    SkillValue::Number(35.0),
                ),
            ],
        ),
    );
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "fireball",
          tags: ["spell", "projectile", "fire"],
          params: {
            "projectile_count": 1.0,
            "projectile_damage": 1.0,
          },
          modifiers: ["greater_multiple_projectiles"],
          body: Action("spawn_projectile", { "prefab": "fireball" }),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    assert_eq!(
        compiled.graph.params.get("projectile_count"),
        Some(&EcsSkillValue::Number(5.0))
    );
    assert_eq!(
        compiled.graph.params.get("projectile_damage"),
        Some(&EcsSkillValue::Number(0.74))
    );
    assert_eq!(
        skill.params.get("projectile_count"),
        Some(&SkillValue::Number(1.0))
    );
}

#[test]
fn modifier_def_parses_from_ron() {
    let modifier: ModifierDef = ron::from_str(
        r#"
        ModifierDef(
          id: "greater_multiple_projectiles",
          applies_to_tags: ["projectile"],
          ops: [
            Add("projectile_count", 4.0),
            Mul("projectile_damage", 0.74),
            Set("projectile_spread_degrees", 35.0),
          ],
        )
    "#,
    )
    .unwrap();
    assert_eq!(modifier.id, "greater_multiple_projectiles");
    assert_eq!(modifier.ops.len(), 3);
}

#[test]
fn flow_ron_compiles_to_ordered_ecs_skill_graph() {
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "ordered",
          body: Sequence([
            Action("trace", { "label": "first" }),
            Action("trace", { "label": "second" }),
            Action("spawn_projectile", {
              "prefab": "bolt",
              "on_hit": On("hit", Sequence([
                Action("damage", { "amount": 10.0 }),
                Action("trace", { "label": "after_damage" }),
              ])),
            }),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry()).unwrap();
    compiled.graph.validate().unwrap();

    let root = compiled.graph.node(compiled.graph.root.unwrap()).unwrap();
    assert!(matches!(root.kind, SkillGraphNodeKind::Sequence));
    let children = root.children.get("items").unwrap();
    assert_eq!(children.len(), 3);
    assert_extension(&compiled.graph, children[0], "trace");
    assert_extension(&compiled.graph, children[1], "trace");
    assert_extension(&compiled.graph, children[2], "spawn_projectile");

    let projectile = compiled.graph.node(children[2]).unwrap();
    let payload = projectile.payloads.get("on_hit").unwrap();
    assert_eq!(payload.len(), 1);
    let wait = compiled.graph.node(payload[0]).unwrap();
    assert!(matches!(
        &wait.kind,
        SkillGraphNodeKind::WaitEvent { event } if event == "hit"
    ));
    let hit_children = wait.payloads.get("hit").unwrap();
    assert_eq!(hit_children.len(), 1);
    let hit_sequence = compiled.graph.node(hit_children[0]).unwrap();
    let hit_order = hit_sequence.children.get("items").unwrap();
    assert_extension(&compiled.graph, hit_order[0], "damage");
    assert_extension(&compiled.graph, hit_order[1], "trace");
}

#[test]
fn compiled_skill_materializes_default_entity_relationship_graph() {
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "relationship_graph",
          body: Sequence([
            Action("trace", { "label": "first" }),
            Action("spawn_projectile", {
              "prefab": "bolt",
              "on_hit": On("hit", Action("trace", { "label": "hit" })),
            }),
          ]),
        )
    "#,
    )
    .unwrap();
    let replacement = parse_skill_def(
        r#"
        Skill(id: "relationship_graph", body: Action("trace", { "label": "replacement" }))
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry()).unwrap();
    let replacement = compile_skill(&replacement, &registry()).unwrap();
    let original_node_count = compiled.graph.nodes.len();
    let mut app = runtime_app(registry(), compiled);

    let skill_entity = app
        .world()
        .resource::<SkillLibrary>()
        .get_entity(&"relationship_graph".into())
        .unwrap();
    assert!(app.world().entity(skill_entity).contains::<CompiledSkill>());

    let roots = app
        .world()
        .entity(skill_entity)
        .get::<SkillRoots>()
        .unwrap();
    assert_eq!(roots.len(), 1);
    let root = roots.iter().next().unwrap();
    assert!(app.world().entity(root).contains::<SkillNodeOf>());

    let root_children = app.world().entity(root).get::<SkillChildren>().unwrap();
    let mut ordered_children = root_children
        .iter()
        .filter_map(|child| {
            let edge = app.world().entity(child).get::<SkillChildOf>()?;
            (edge.slot == "items").then_some((edge.order, child))
        })
        .collect::<Vec<_>>();
    ordered_children.sort_by_key(|(order, _)| *order);
    assert_eq!(ordered_children.len(), 2);
    assert_eq!(ordered_children[0].0, 0);
    assert_eq!(ordered_children[1].0, 1);

    let projectile = ordered_children[1].1;
    let payloads = app
        .world()
        .entity(projectile)
        .get::<SkillPayloads>()
        .unwrap();
    let payload_edges = payloads
        .iter()
        .filter_map(|payload| app.world().entity(payload).get::<SkillPayloadOf>())
        .filter(|edge| edge.slot == "on_hit")
        .count();
    assert_eq!(payload_edges, 1);

    app.world_mut()
        .resource_scope(|world, mut library: Mut<SkillLibrary>| {
            replace_compiled_skill_world(world, &mut library, replacement);
        });

    assert!(!app.world().entities().contains(skill_entity));
    let mut node_query = app.world_mut().query::<&SkillGraphNodeRef>();
    assert!(node_query.iter(app.world()).count() < original_node_count);
}

#[test]
fn skill_graph_validation_reports_missing_root_and_cycles() {
    let mut graph = SkillGraph::new(EcsSkillId::new("broken"));
    assert_eq!(
        graph.validate().unwrap_err().to_string(),
        "skill graph has no root node"
    );

    let first = graph.add_node(SkillGraphNodeKind::Sequence);
    let second = graph.add_node(SkillGraphNodeKind::Parallel);
    graph.set_root(first);
    graph.push_child(first, "items", second).unwrap();
    graph.push_child(second, "branches", first).unwrap();
    assert!(graph.validate().unwrap_err().to_string().contains("cycle"));
}

#[test]
fn execution_sequence_delay_and_on_event_resume() {
    let mut registry = registry();
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "timed",
          body: Sequence([
            Action("record", { "label": "start" }),
            Delay(Expr("0.5"), Action("record", { "label": "after_delay" })),
            On("hit", Action("record", { "label": Expr("event.target") })),
          ]),
        )
    "#,
    )
    .unwrap();
    registry.register_skill_action("record", RecordAction);
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();
    app.world_mut().write_message(SkillCastRequest {
        skill: "timed".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["start"]);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(500));
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["after_delay"]);
    assert_eq!(active_skill_count(&mut app), 1);

    let mut payload = RuntimeArgs::new();
    payload.insert(
        "target".to_owned(),
        EcsSkillValue::String("dummy".to_owned()),
    );
    app.world_mut()
        .write_message(SkillRuntimeSignal::new("hit", payload));
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["dummy"]);
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn emit_nodes_are_bevy_runtime_signals() {
    let registry = registry();
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "emits",
          body: Sequence([
            Emit("cast_started", { "skill": "emits" }),
            Delay(Expr("0.25"), Emit("delay_ready", { "step": 1.0 })),
            On("impact", Emit("impact_seen", { "target": Expr("event.target") })),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "emits".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    let events = take_runtime_signals(&mut app);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "cast_started");
    assert_eq!(
        events[0].payload.get("skill"),
        Some(&EcsSkillValue::String("emits".to_owned()))
    );

    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(250));
    skill_update(&mut app);
    let events = take_runtime_signals(&mut app);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "delay_ready");
    assert_eq!(
        events[0].payload.get("step"),
        Some(&EcsSkillValue::Number(1.0))
    );

    let mut payload = RuntimeArgs::new();
    payload.insert(
        "target".to_owned(),
        EcsSkillValue::String("dummy".to_owned()),
    );
    app.world_mut()
        .write_message(SkillRuntimeSignal::new("impact", payload));
    skill_update(&mut app);
    let events = take_runtime_signals(&mut app);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "impact_seen");
    assert_eq!(
        events[0].payload.get("target"),
        Some(&EcsSkillValue::String("dummy".to_owned()))
    );
}

#[test]
fn parallel_waits_keep_skill_entity_until_all_branches_finish() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "parallel",
          body: Parallel([
            Delay(Expr("0.1"), Action("record", { "label": "a" })),
            Delay(Expr("0.2"), Action("record", { "label": "b" })),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "parallel".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(150));
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["a"]);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(100));
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["b"]);
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn if_and_repeat_execute_deterministically_in_one_update() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "control_flow",
          params: { "enabled": true },
          body: Sequence([
            If(
              condition: Expr("stat.enabled"),
              then_node: Action("record", { "label": "then" }),
              else_node: Some(Action("record", { "label": "else" })),
            ),
            Repeat(
              times: Some(Expr("3")),
              node: Action("record", { "label": "repeat" }),
            ),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "control_flow".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);

    assert_eq!(
        take_record_intents(&mut app),
        vec!["then", "repeat", "repeat", "repeat"]
    );
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn step_budget_stops_overlong_synchronous_chains() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "too_long",
          body: Sequence([
            Action("record", { "label": "a" }),
            Action("record", { "label": "b" }),
            Action("record", { "label": "c" }),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    app.world_mut()
        .resource_mut::<SkillRuntimeConfig>()
        .step_budget = 2;
    let caster = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "too_long".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);

    let failures = take_failures(&mut app);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].message.contains("step budget exhausted"));
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn cost_and_cooldown_requirements_validate_pay_and_reject() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "gated",
          params: { "mana_cost": 10.0 },
          requirements: [
            Cost(resource: "mana", amount: Expr("stat.mana_cost")),
            Cooldown(seconds: Expr("0.5")),
          ],
          body: Action("record", { "label": "cast" }),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();
    app.world_mut()
        .resource_mut::<SkillResourcePools>()
        .set(caster, "mana", 15.0);

    app.world_mut().write_message(SkillCastRequest {
        skill: "gated".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["cast"]);
    assert_eq!(
        app.world()
            .resource::<SkillResourcePools>()
            .get(caster, "mana"),
        5.0
    );

    app.world_mut().write_message(SkillCastRequest {
        skill: "gated".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    let rejected = take_rejections(&mut app);
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].message.contains("cooldown"));
    assert_eq!(
        app.world()
            .resource::<SkillResourcePools>()
            .get(caster, "mana"),
        5.0
    );

    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(600));
    app.world_mut().write_message(SkillCastRequest {
        skill: "gated".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    let rejected = take_rejections(&mut app);
    assert_eq!(rejected.len(), 1);
    assert!(rejected[0].message.contains("not enough `mana`"));
    assert_eq!(
        app.world()
            .resource::<SkillResourcePools>()
            .get(caster, "mana"),
        5.0
    );
}

#[test]
fn dirty_skill_asset_sources_hot_reload_new_casts() {
    let mut app = App::new();
    app.add_plugins(SkillDslPlugin);
    app.init_resource::<Time>();
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    *app.world_mut().resource_mut::<SkillRegistry>() = registry.compile;
    *app.world_mut().resource_mut::<SkillActionRegistry>() = registry.actions;

    app.world_mut()
        .resource_mut::<SkillAssetSources>()
        .set_source(
            "memory://reloadable.skill.ron",
            r#"Skill(id: "reloadable", body: Action("record", { "label": "v1" }))"#,
        );
    skill_update(&mut app);
    assert!(
        app.world()
            .resource::<SkillLibrary>()
            .get(&"reloadable".into())
            .is_some()
    );

    let caster = app.world_mut().spawn_empty().id();
    app.world_mut().write_message(SkillCastRequest {
        skill: "reloadable".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["v1"]);

    app.world_mut()
        .resource_mut::<SkillAssetSources>()
        .set_source(
            "memory://reloadable.skill.ron",
            r#"Skill(id: "reloadable", body: Action("record", { "label": "v2" }))"#,
        );
    skill_update(&mut app);

    app.world_mut().write_message(SkillCastRequest {
        skill: "reloadable".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["v2"]);
}

#[test]
fn invalid_skill_asset_reload_removes_old_compiled_graph() {
    let mut app = App::new();
    app.add_plugins(SkillDslPlugin);
    app.init_resource::<Time>();
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    *app.world_mut().resource_mut::<SkillRegistry>() = registry.compile;
    *app.world_mut().resource_mut::<SkillActionRegistry>() = registry.actions;

    app.world_mut()
        .resource_mut::<SkillAssetSources>()
        .set_source(
            "memory://invalidating.skill.ron",
            r#"Skill(id: "invalidating", body: Action("record", { "label": "live" }))"#,
        );
    skill_update(&mut app);
    let old_entity = app
        .world()
        .resource::<SkillLibrary>()
        .get_entity(&"invalidating".into())
        .unwrap();
    assert!(app.world().entities().contains(old_entity));

    app.world_mut()
        .resource_mut::<SkillAssetSources>()
        .set_source(
            "memory://invalidating.skill.ron",
            r#"Skill(id: "invalidating", modifiers: ["missing"], body: Action("record", {}))"#,
        );
    skill_update(&mut app);

    let library = app.world().resource::<SkillLibrary>();
    assert!(library.get_entity(&"invalidating".into()).is_none());
    assert!(library.invalid(&"invalidating".into()).is_some());
    assert!(!app.world().entities().contains(old_entity));
}

#[test]
fn damage_sync_request_and_await_modes_drive_messages_and_resume() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "damage_modes",
          body: Sequence([
            Action("damage", { "amount": 7.0, "mode": "sync" }),
            Action("record", { "label": Expr("var.last_damage_amount") }),
            Action("damage", { "amount": 11.0, "mode": "request" }),
            Action("damage", { "amount": 13.0, "mode": "await" }),
            Action("record", { "label": Expr("event.amount") }),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();
    let target = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "damage_modes".into(),
        caster,
        target: Some(target),
    });
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["7"]);
    let requests = take_damage_requests(&mut app);
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].mode, SettlementMode::Sync);
    assert_eq!(requests[1].mode, SettlementMode::Request);
    assert_eq!(requests[2].mode, SettlementMode::Await);
    assert_eq!(take_damage_resolved(&mut app).len(), 1);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut().write_message(DamageResolved {
        request_id: requests[2].request_id,
        execution_id: 1,
        source: Some(caster),
        target: Some(target),
        kind: "damage".to_owned(),
        payload: [("amount".to_owned(), EcsSkillValue::Number(13.0))]
            .into_iter()
            .collect(),
    });
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["13"]);
    assert_eq!(active_skill_count(&mut app), 0);
}

#[cfg(feature = "full_runtime_entities")]
#[test]
fn full_runtime_entities_materializes_graph_relationships() {
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "debug_entities",
          body: Sequence([
            Action("trace", { "label": "start" }),
            Action("spawn_projectile", {
              "prefab": "bolt",
              "on_hit": On("hit", Action("trace", { "label": "hit" })),
            }),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry()).unwrap();
    let graph_node_count = compiled.graph.nodes.len();
    let mut app = runtime_app(registry(), compiled);
    let caster = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "debug_entities".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);

    let mut skill_query = app.world_mut().query::<&CompiledSkill>();
    assert_eq!(skill_query.iter(app.world()).count(), 1);
    let mut node_query = app.world_mut().query::<&SkillGraphNodeRef>();
    assert_eq!(node_query.iter(app.world()).count(), graph_node_count);
    assert_eq!(
        app.world_mut()
            .query::<&SkillRootOf>()
            .iter(app.world())
            .count(),
        1
    );
    assert!(
        app.world_mut()
            .query::<&SkillChildOf>()
            .iter(app.world())
            .count()
            >= 2
    );
    assert_eq!(
        app.world_mut()
            .query::<&SkillPayloadOf>()
            .iter(app.world())
            .count(),
        2
    );
    assert_eq!(
        app.world_mut()
            .query::<&SkillNodeOf>()
            .iter(app.world())
            .count(),
        graph_node_count
    );
    assert_eq!(
        app.world_mut()
            .query::<&ExecutionOfSkill>()
            .iter(app.world())
            .count(),
        0
    );
}

#[test]
fn projectile_hit_message_resumes_hit_payload() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "projectile_payload",
          body: On("hit", Action("record", { "label": "hit_payload" })),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();
    let projectile = app.world_mut().spawn_empty().id();
    let target = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "projectile_payload".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut().write_message(ProjectileHit {
        execution_id: 1,
        projectile,
        target: Some(target),
        position: Some([1.0, 2.0, 0.0]),
    });
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["hit_payload"]);
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn fireball_flow_releases_hits_damages_burns_and_explodes() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "fireball_acceptance",
          body: Parallel([
            Action("spawn_projectile", { "prefab": "fireball" }),
            On("hit", Sequence([
              Action("damage", { "amount": 40.0, "mode": "request" }),
              Action("apply_buff", {
                "buff": "burning",
                "duration": 3.0,
                "on_add": Action("record", { "label": "burning" }),
              }),
            ])),
            On("expire", Action("record", { "label": "explode" })),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();
    let target = app.world_mut().spawn_empty().id();
    let projectile = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "fireball_acceptance".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(intent_count(&mut app, "spawn_projectile"), 1);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut().write_message(ProjectileHit {
        execution_id: 1,
        projectile,
        target: Some(target),
        position: Some([3.0, 0.0, 0.0]),
    });
    skill_update(&mut app);
    let effect_requests = take_effect_requests(&mut app);
    assert_eq!(
        effect_requests
            .iter()
            .filter(|request| request.kind == "damage")
            .count(),
        1
    );
    assert_eq!(
        effect_requests
            .iter()
            .filter(|request| request.kind == "apply_buff")
            .count(),
        1
    );
    assert_eq!(take_record_intents(&mut app), vec!["burning"]);
    assert_eq!(active_skill_count(&mut app), 1);

    app.world_mut()
        .write_message(SkillRuntimeSignal::new("expire", RuntimeArgs::new()));
    skill_update(&mut app);
    assert_eq!(take_record_intents(&mut app), vec!["explode"]);
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn observer_trigger_resumes_waiting_payload() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "observer_payload",
          body: On("observed_hit", Action("record", { "label": Expr("event.label") })),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "observer_payload".into(),
        caster,
        target: None,
    });
    skill_update(&mut app);
    assert_eq!(active_skill_count(&mut app), 1);
    let skill_entity = app
        .world_mut()
        .query::<(bevy::prelude::Entity, &ActiveSkill)>()
        .iter(app.world())
        .next()
        .map(|(entity, _)| entity)
        .unwrap();

    let mut payload = RuntimeArgs::new();
    payload.insert(
        "label".to_owned(),
        EcsSkillValue::String("observer".to_owned()),
    );
    let mut trigger = SkillObserverTrigger::new("observed_hit", payload.clone());
    trigger.skill_entity = Some(skill_entity);
    trigger.execution_id = Some(999);
    app.world_mut().trigger(trigger);
    skill_update(&mut app);
    assert!(take_record_intents(&mut app).is_empty());
    assert_eq!(active_skill_count(&mut app), 1);

    let mut trigger = SkillObserverTrigger::new("observed_hit", payload);
    trigger.skill_entity = Some(skill_entity);
    trigger.execution_id = Some(1);
    app.world_mut().trigger(trigger);
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["observer"]);
    assert_eq!(active_skill_count(&mut app), 0);
}

#[test]
fn buff_add_and_remove_hooks_execute_as_skill_subgraphs() {
    let mut registry = registry();
    registry.register_skill_action("record", RecordAction);
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "buff_hooks",
          body: Action("apply_buff", {
            "buff": "burning",
            "duration": 0.1,
            "on_add": Action("record", { "label": "add" }),
            "on_remove": Action("record", { "label": "remove" }),
          }),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut app = runtime_app(registry, compiled);
    let caster = app.world_mut().spawn_empty().id();
    let target = app.world_mut().spawn_empty().id();

    app.world_mut().write_message(SkillCastRequest {
        skill: "buff_hooks".into(),
        caster,
        target: Some(target),
    });
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["add"]);
    assert_eq!(take_apply_buff_requests(&mut app).len(), 1);
    assert_eq!(active_buff_count(&mut app), 1);

    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(Duration::from_millis(200));
    skill_update(&mut app);

    assert_eq!(take_record_intents(&mut app), vec!["remove"]);
    assert_eq!(active_buff_count(&mut app), 0);
}

#[test]
fn poe_fireball_gmp_compiles_payload_expressions() {
    let mut registry = registry();
    registry.register_skill_modifier(
        "greater_multiple_projectiles",
        StatModifier::new(
            vec!["projectile".to_owned()],
            vec![
                bevy_skill_flow::StatOp::Add(
                    "projectile_count".to_owned(),
                    SkillValue::Number(4.0),
                ),
                bevy_skill_flow::StatOp::Mul(
                    "projectile_damage".to_owned(),
                    SkillValue::Number(0.74),
                ),
            ],
        ),
    );
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "fireball",
          tags: ["spell", "projectile", "aoe", "fire"],
          params: {
            "base_damage": 40.0,
            "spell_damage": 1.0,
            "projectile_damage": 1.0,
            "projectile_count": 1.0,
          },
          modifiers: ["greater_multiple_projectiles"],
          body: Action("spawn_projectile", {
            "prefab": "fireball",
            "count": Expr("stat.projectile_count ?? 1"),
            "payload": On("hit", Action("damage", {
              "target": Expr("event.target"),
              "amount": Expr("stat.base_damage * stat.spell_damage * stat.projectile_damage"),
              "type": "fire",
            })),
          }),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry).unwrap();
    assert_eq!(
        compiled.graph.params.get("projectile_count"),
        Some(&EcsSkillValue::Number(5.0))
    );
    assert_eq!(
        compiled.graph.params.get("projectile_damage"),
        Some(&EcsSkillValue::Number(0.74))
    );
}

#[test]
fn noita_wand_deck_double_cast_consumes_two_spells() {
    let skill = parse_skill_def(
        r#"
        Skill(
          id: "basic_wand",
          cast_model: "wand_deck",
          body: Deck([
            Modifier("double_cast", {}),
            Modifier("damage_plus", { "amount": 12.0 }),
            Spell("spark_bolt", {}),
            Spell("magic_missile", {}),
          ]),
        )
    "#,
    )
    .unwrap();
    let compiled = compile_skill(&skill, &registry()).unwrap();
    let root = compiled.graph.node(compiled.graph.root.unwrap()).unwrap();
    assert!(matches!(root.kind, SkillGraphNodeKind::Sequence));
    let spells = root.children.get("items").unwrap();
    assert_eq!(spells.len(), 2);
    let first = compiled.graph.node(spells[0]).unwrap();
    let second = compiled.graph.node(spells[1]).unwrap();
    assert!(matches!(
        &first.kind,
        SkillGraphNodeKind::Extension { constructor, args }
            if constructor == "spell"
                && args.get("id") == Some(&EcsSkillValue::String("spark_bolt".to_owned()))
                && args.get("damage_bonus") == Some(&EcsSkillValue::Number(12.0))
    ));
    assert!(matches!(
        &second.kind,
        SkillGraphNodeKind::Extension { constructor, args }
            if constructor == "spell"
                && args.get("id") == Some(&EcsSkillValue::String("magic_missile".to_owned()))
                && args.get("damage_bonus") == Some(&EcsSkillValue::Number(12.0))
    ));
}

#[test]
fn bevy_plugin_installs_core_resources() {
    let mut app = App::new();
    app.add_plugins(SkillDslPlugin);
    assert!(
        app.world()
            .resource::<SkillRegistry>()
            .has_cast_model("direct")
    );
    assert!(app.world().contains_resource::<SkillLibrary>());
    assert!(
        app.world()
            .contains_resource::<bevy_skill_flow::SkillRuntimeCounters>()
    );
    assert!(app.world().contains_resource::<SkillResourcePools>());
    assert!(app.world().contains_resource::<SkillAssetSources>());
    assert!(
        app.world()
            .contains_resource::<bevy_skill_flow::SkillCooldowns>()
    );
    assert!(
        app.world()
            .contains_resource::<Messages<SkillCastRequest>>()
    );
    assert!(app.world().contains_resource::<Messages<SkillIntent>>());
}

struct RecordAction;

impl SkillAction for RecordAction {
    fn validate(
        &self,
        _args: &RuntimeArgs,
        _registry: &SkillActionRegistry,
    ) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        input: &SkillActionInput,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        let args = &input.args;
        let label = match args.get("label") {
            Some(EcsSkillValue::String(value)) => value.clone(),
            Some(EcsSkillValue::Number(value)) => value.to_string(),
            other => format!("{other:?}"),
        };
        let mut payload = RuntimeArgs::new();
        payload.insert("label".to_owned(), EcsSkillValue::String(label));
        out.emit_intent(ctx, "record", payload)
    }
}

fn runtime_app(registry: TestRegistries, compiled: SkillCompiled) -> App {
    let mut app = App::new();
    app.add_plugins((SkillDslPlugin, SkillGameplayPlugin));
    app.init_resource::<Time>();
    *app.world_mut().resource_mut::<SkillRegistry>() = registry.compile;
    *app.world_mut().resource_mut::<SkillActionRegistry>() = registry.actions;
    app.world_mut()
        .resource_scope(|world, mut library: Mut<SkillLibrary>| {
            replace_compiled_skill_world(world, &mut library, compiled);
        });
    app
}

fn take_record_intents(app: &mut App) -> Vec<String> {
    let mut messages = app.world_mut().resource_mut::<Messages<SkillIntent>>();
    let records = messages
        .iter_current_update_messages()
        .filter(|intent| intent.kind == "record")
        .filter_map(|intent| match intent.payload.get("label") {
            Some(EcsSkillValue::String(value)) => Some(value.clone()),
            _ => None,
        })
        .collect();
    messages.clear();
    records
}

fn intent_count(app: &mut App, kind: &str) -> usize {
    let mut messages = app.world_mut().resource_mut::<Messages<SkillIntent>>();
    let count = messages
        .iter_current_update_messages()
        .filter(|intent| intent.kind == kind)
        .count();
    messages.clear();
    count
}

fn take_runtime_signals(app: &mut App) -> Vec<SkillRuntimeSignal> {
    let mut messages = app
        .world_mut()
        .resource_mut::<Messages<SkillRuntimeSignal>>();
    let signals = messages.iter_current_update_messages().cloned().collect();
    messages.clear();
    signals
}

fn take_failures(app: &mut App) -> Vec<SkillExecutionFailed> {
    let mut messages = app
        .world_mut()
        .resource_mut::<Messages<SkillExecutionFailed>>();
    let failures = messages.iter_current_update_messages().cloned().collect();
    messages.clear();
    failures
}

fn take_rejections(app: &mut App) -> Vec<bevy_skill_flow::SkillCastRejected> {
    let mut messages = app
        .world_mut()
        .resource_mut::<Messages<bevy_skill_flow::SkillCastRejected>>();
    let rejected = messages.iter_current_update_messages().cloned().collect();
    messages.clear();
    rejected
}

fn take_damage_requests(app: &mut App) -> Vec<SkillEffectRequest> {
    take_effect_requests(app)
        .into_iter()
        .filter(|request| request.kind == "damage")
        .collect()
}

fn take_damage_resolved(app: &mut App) -> Vec<SkillEffectResolved> {
    let mut messages = app
        .world_mut()
        .resource_mut::<Messages<SkillEffectResolved>>();
    let resolved = messages
        .iter_current_update_messages()
        .filter(|resolved| resolved.kind == "damage")
        .cloned()
        .collect();
    messages.clear();
    resolved
}

fn take_apply_buff_requests(app: &mut App) -> Vec<ApplyBuffRequest> {
    take_effect_requests(app)
        .into_iter()
        .filter(|request| request.kind == "apply_buff")
        .collect()
}

fn take_effect_requests(app: &mut App) -> Vec<SkillEffectRequest> {
    let mut messages = app.world_mut().resource_mut::<Messages<ApplyBuffRequest>>();
    let requests = messages.iter_current_update_messages().cloned().collect();
    messages.clear();
    requests
}

fn active_skill_count(app: &mut App) -> usize {
    app.world_mut()
        .query::<&ActiveSkill>()
        .iter(app.world())
        .count()
}

fn active_buff_count(app: &mut App) -> usize {
    app.world_mut()
        .query::<&ActiveSkillEffect>()
        .iter(app.world())
        .count()
}

fn compiled_with_stats(stats: Vec<(String, SkillValue)>) -> SkillCompiled {
    let mut graph = SkillGraph::new(EcsSkillId::new("test"));
    graph.params = stats
        .into_iter()
        .map(|(key, value)| (key, flow_value_to_ecs(value)))
        .collect();
    let root = graph.add_node(SkillGraphNodeKind::Sequence);
    graph.set_root(root);
    SkillCompiled {
        id: "test".into(),
        tags: Default::default(),
        cast_model: "direct".to_owned(),
        graph,
    }
}

fn flow_value_to_ecs(value: SkillValue) -> EcsSkillValue {
    match value {
        SkillValue::Special(special) => EcsSkillValue::Special(match special {
            bevy_skill_flow::SkillSpecialValue::Expr(expr) => {
                bevy_skill_ecs::SkillSpecialValue::Expr(expr)
            }
            bevy_skill_flow::SkillSpecialValue::Ref(reference) => {
                bevy_skill_ecs::SkillSpecialValue::Ref(reference)
            }
            bevy_skill_flow::SkillSpecialValue::Tag(tag) => {
                bevy_skill_ecs::SkillSpecialValue::Tag(tag)
            }
            bevy_skill_flow::SkillSpecialValue::Stat(stat) => {
                bevy_skill_ecs::SkillSpecialValue::Stat(stat)
            }
        }),
        SkillValue::Map(values) => EcsSkillValue::Map(
            values
                .into_iter()
                .map(|(key, value)| (key, flow_value_to_ecs(value)))
                .collect(),
        ),
        SkillValue::List(values) => {
            EcsSkillValue::List(values.into_iter().map(flow_value_to_ecs).collect())
        }
        SkillValue::Number(value) => EcsSkillValue::Number(value),
        SkillValue::Bool(value) => EcsSkillValue::Bool(value),
        SkillValue::String(value) => EcsSkillValue::String(value),
        SkillValue::Node(_) => EcsSkillValue::Null,
        SkillValue::Null => EcsSkillValue::Null,
    }
}

fn assert_extension(graph: &SkillGraph, id: bevy_skill_ecs::SkillNodeId, expected: &str) {
    let node = graph.node(id).unwrap();
    assert!(matches!(
        &node.kind,
        SkillGraphNodeKind::Extension { constructor, .. } if constructor == expected
    ));
}

#[cfg(feature = "editor")]
mod editor_tests {
    use super::registry;
    use bevy_skill_flow::editor::{
        SkillEditorConfig, SkillEditorState, next_new_skill_path, serialize_skill_def,
    };
    use bevy_skill_flow::{
        SkillArgs, SkillDef, SkillExpr, SkillId, SkillLibrary, SkillNode, SkillValue,
        parse_skill_def,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn editor_scans_skill_files_and_keeps_invalid_file_diagnostics() {
        let dir = temp_skills_dir("scan");
        fs::write(
            dir.join("good.skill.ron"),
            r#"Skill(id: "good", body: Action("trace", { "label": "ok" }))"#,
        )
        .unwrap();
        fs::write(dir.join("broken.skill.ron"), "Skill(").unwrap();
        fs::write(dir.join("ignored.ron"), "not a skill").unwrap();

        let config = SkillEditorConfig {
            skills_dir: dir.clone(),
            autosave_on_compile_success: false,
        };
        let registry = registry();
        let mut library = SkillLibrary::default();
        let mut editor = SkillEditorState::default();
        editor.load_dir(&config, &registry, &mut library).unwrap();

        assert_eq!(editor.files.len(), 2);
        assert!(library.get(&SkillId::new("good")).is_some());
        assert!(
            editor
                .files
                .iter()
                .any(|file| file.path.ends_with("broken.skill.ron") && file.parse_error.is_some())
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editor_revalidates_dirty_source_without_polluting_last_compiled_skill() {
        let dir = temp_skills_dir("dirty");
        let path = dir.join("live.skill.ron");
        fs::write(
            &path,
            r#"Skill(id: "live", body: Action("trace", { "label": "v1" }))"#,
        )
        .unwrap();

        let config = SkillEditorConfig {
            skills_dir: dir.clone(),
            autosave_on_compile_success: false,
        };
        let registry = registry();
        let mut library = SkillLibrary::default();
        let mut editor = SkillEditorState::default();
        editor.load_dir(&config, &registry, &mut library).unwrap();
        assert!(library.get(&SkillId::new("live")).is_some());
        assert_eq!(
            editor.selected_preview_skill_id(),
            Some(SkillId::new("live"))
        );

        editor.edit_source(
            r#"Skill(id: "live", body: Action("missing", {}))"#.to_owned(),
            &registry,
            &mut library,
        );

        assert!(editor.dirty);
        assert!(editor.diagnostics.compile_error.is_some());
        assert!(library.get(&SkillId::new("live")).is_some());
        assert!(editor.compiled_cache.contains_key(&SkillId::new("live")));
        assert_eq!(
            editor.selected_preview_skill_id(),
            Some(SkillId::new("live"))
        );
        assert!(editor.preview_is_stale());

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editor_save_rules_keep_paths_and_generate_new_suffixes() {
        let dir = temp_skills_dir("save");
        fs::write(dir.join("new_skill.skill.ron"), "").unwrap();
        assert_eq!(
            next_new_skill_path(&dir)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("new_skill_2.skill.ron")
        );

        let path = dir.join("existing.skill.ron");
        fs::write(
            &path,
            r#"Skill(id: "before", body: Action("trace", { "label": "v1" }))"#,
        )
        .unwrap();
        let config = SkillEditorConfig {
            skills_dir: dir.clone(),
            autosave_on_compile_success: false,
        };
        let registry = registry();
        let mut library = SkillLibrary::default();
        let mut editor = SkillEditorState::default();
        editor.load_dir(&config, &registry, &mut library).unwrap();
        editor
            .open_file(path.clone(), &registry, &mut library)
            .unwrap();
        editor.edit_source(
            r#"Skill(id: "after", body: Action("trace", { "label": "v2" }))"#.to_owned(),
            &registry,
            &mut library,
        );

        let saved_path = editor.save_current(&registry).unwrap();
        assert_eq!(saved_path, path);
        assert!(!dir.join("after.skill.ron").exists());
        assert!(fs::read_to_string(path).unwrap().contains(r#"id: "after""#));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editor_structured_edit_updates_source_diagnostics_and_preview() {
        let dir = temp_skills_dir("structured");
        let path = dir.join("live.skill.ron");
        fs::write(
            &path,
            r#"Skill(id: "live", tags: ["spell"], body: Action("trace", { "label": "v1" }))"#,
        )
        .unwrap();
        let config = SkillEditorConfig {
            skills_dir: dir.clone(),
            autosave_on_compile_success: false,
        };
        let registry = registry();
        let mut library = SkillLibrary::default();
        let mut editor = SkillEditorState::default();
        editor.load_dir(&config, &registry, &mut library).unwrap();

        let mut def = editor.current_def.clone().unwrap();
        def.id = SkillId::new("edited");
        def.tags.push("projectile".to_owned());
        def.params
            .insert("damage".to_owned(), SkillValue::Number(42.0));
        editor.edit_def(def, &registry, &mut library).unwrap();

        assert!(editor.dirty);
        assert_eq!(editor.diagnostics.skill_id, Some(SkillId::new("edited")));
        assert!(editor.source.contains(r#"id: "edited""#));
        assert!(editor.source.contains(r#""damage": 42.0"#));
        assert!(library.get(&SkillId::new("edited")).is_some());
        assert_eq!(
            editor.selected_preview_skill_id(),
            Some(SkillId::new("edited"))
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn editor_structured_save_round_trips_pretty_ron_to_disk() {
        let dir = temp_skills_dir("structured_save");
        let path = dir.join("live.skill.ron");
        fs::write(
            &path,
            r#"Skill(id: "live", body: Action("trace", { "label": "v1" }))"#,
        )
        .unwrap();
        let config = SkillEditorConfig {
            skills_dir: dir.clone(),
            autosave_on_compile_success: false,
        };
        let registry = registry();
        let mut library = SkillLibrary::default();
        let mut editor = SkillEditorState::default();
        editor.load_dir(&config, &registry, &mut library).unwrap();

        let mut def = editor.current_def.clone().unwrap();
        let mut first_args = SkillArgs::new();
        first_args.insert("label".to_owned(), SkillValue::String("a".to_owned()));
        let mut delayed_args = SkillArgs::new();
        delayed_args.insert("label".to_owned(), SkillValue::String("b".to_owned()));
        def.body = SkillNode::Sequence(vec![
            SkillNode::Action("trace".to_owned(), first_args),
            SkillNode::Delay(
                SkillExpr::new("0.25"),
                Box::new(SkillNode::Action("trace".to_owned(), delayed_args)),
            ),
        ]);
        editor.edit_def(def, &registry, &mut library).unwrap();
        editor
            .save_current_with_library(&registry, &mut library)
            .unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.starts_with("Skill(\n"));
        assert!(parse_skill_def(&saved).is_ok());
        assert!(!editor.dirty);
        assert!(editor.files.iter().any(|file| file.path == path
            && file.parse_error.is_none()
            && file.compile_error.is_none()));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn serialize_skill_def_outputs_named_skill_ron() {
        let def: SkillDef =
            parse_skill_def(r#"Skill(id: "pretty", body: Action("trace", {}))"#).unwrap();
        let source = serialize_skill_def(&def).unwrap();
        assert!(source.starts_with("Skill(\n"));
        assert!(source.ends_with('\n'));
        assert_eq!(parse_skill_def(&source).unwrap().id, SkillId::new("pretty"));
    }

    fn temp_skills_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir()
            .join("bevy_skill_flow_editor_tests")
            .join(format!("{label}_{unique}"))
            .join("skills");
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
