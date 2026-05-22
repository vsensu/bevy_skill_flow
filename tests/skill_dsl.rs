use bevy::prelude::{App, World};
use bevy_skill_dsl::examples::{
    DamageAction, SpawnProjectileAction, SpellAction, TraceAction, WandDeckCastModel,
};
use bevy_skill_dsl::{
    ModifierDef, PendingSkillExecutions, SkillAction, SkillArgs, SkillContext, SkillError,
    SkillExpr, SkillNode, SkillRegistry, SkillResult, SkillRuntimeEvent, SkillValue, StatModifier,
    compile_skill, eval_skill_expr, parse_skill_def, parse_skill_document,
};
use bevy_skill_dsl::{SkillCompiled, SkillDslPlugin, SkillLibrary, SkillPlan};

fn registry() -> SkillRegistry {
    let mut registry = SkillRegistry::with_core();
    registry
        .register_skill_action("trace", TraceAction)
        .register_skill_action("spawn_projectile", SpawnProjectileAction)
        .register_skill_action("damage", DamageAction)
        .register_skill_action("spell", SpellAction)
        .register_cast_model("wand_deck", WandDeckCastModel);
    registry
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
fn registry_validation_rejects_unknown_primitives() {
    let skill = parse_skill_def(
        r#"
        Skill(id: "bad", body: Action("missing", {}))
    "#,
    )
    .unwrap();
    let err = compile_skill(&skill, &registry()).unwrap_err();
    assert_eq!(err, SkillError::UnknownAction("missing".to_owned()));

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
    let ctx = SkillContext::new(&skill, None, 1);
    assert_eq!(
        eval_skill_expr(
            &SkillExpr::new("stat.base_damage * stat.spell_damage"),
            &ctx
        )
        .unwrap(),
        SkillValue::Number(60.0)
    );
    assert_eq!(
        eval_skill_expr(&SkillExpr::new("stat.projectile_count ?? 1"), &ctx).unwrap(),
        SkillValue::Number(1.0)
    );
    assert_eq!(
        eval_skill_expr(&SkillExpr::new("stat.base_damage >= 40 && true"), &ctx).unwrap(),
        SkillValue::Bool(true)
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
                bevy_skill_dsl::StatOp::Add("projectile_count".to_owned(), SkillValue::Number(4.0)),
                bevy_skill_dsl::StatOp::Mul(
                    "projectile_damage".to_owned(),
                    SkillValue::Number(0.74),
                ),
                bevy_skill_dsl::StatOp::Set(
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
        compiled.plan.stats.get("projectile_count"),
        Some(&SkillValue::Number(5.0))
    );
    assert_eq!(
        compiled.plan.stats.get("projectile_damage"),
        Some(&SkillValue::Number(0.74))
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
fn execution_sequence_delay_and_on_event_resume() {
    let registry = registry();
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

    let mut registry = registry;
    registry.register_skill_action("record", RecordAction);
    let compiled = compile_skill(&skill, &registry).unwrap();
    let mut world = World::new();
    world.insert_resource(Records::default());
    let caster = world.spawn_empty().id();
    let mut pending = PendingSkillExecutions::default();
    pending
        .cast(&compiled, caster, &mut world, &registry)
        .unwrap();

    assert_eq!(records(&world), vec!["start"]);
    assert_eq!(pending.len(), 1);
    pending.tick(0.5, &mut world, &registry).unwrap();
    assert_eq!(records(&world), vec!["start", "after_delay"]);
    assert_eq!(pending.len(), 1);

    let mut payload = SkillArgs::new();
    payload.insert("target".to_owned(), SkillValue::String("dummy".to_owned()));
    pending
        .trigger_event(
            SkillRuntimeEvent::new("hit", payload),
            &mut world,
            &registry,
        )
        .unwrap();
    assert_eq!(records(&world), vec!["start", "after_delay", "dummy"]);
    assert!(pending.is_empty());
}

#[test]
fn emitted_events_can_be_drained_after_cast_tick_and_event_resume() {
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
    let mut world = World::new();
    let caster = world.spawn_empty().id();
    let mut pending = PendingSkillExecutions::default();

    pending
        .cast(&compiled, caster, &mut world, &registry)
        .unwrap();
    let events = pending.drain_emitted_events().collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "cast_started");
    assert_eq!(
        events[0].payload.get("skill"),
        Some(&SkillValue::String("emits".to_owned()))
    );

    pending.tick(0.25, &mut world, &registry).unwrap();
    let events = pending.drain_emitted_events().collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "delay_ready");
    assert_eq!(
        events[0].payload.get("step"),
        Some(&SkillValue::Number(1.0))
    );

    let mut payload = SkillArgs::new();
    payload.insert("target".to_owned(), SkillValue::String("dummy".to_owned()));
    pending
        .trigger_event(
            SkillRuntimeEvent::new("impact", payload),
            &mut world,
            &registry,
        )
        .unwrap();
    let events = pending.drain_emitted_events().collect::<Vec<_>>();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].name, "impact_seen");
    assert_eq!(
        events[0].payload.get("target"),
        Some(&SkillValue::String("dummy".to_owned()))
    );
}

#[test]
fn poe_fireball_gmp_compiles_payload_expressions() {
    let mut registry = registry();
    registry.register_skill_modifier(
        "greater_multiple_projectiles",
        StatModifier::new(
            vec!["projectile".to_owned()],
            vec![
                bevy_skill_dsl::StatOp::Add("projectile_count".to_owned(), SkillValue::Number(4.0)),
                bevy_skill_dsl::StatOp::Mul(
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
        compiled.plan.stats.get("projectile_count"),
        Some(&SkillValue::Number(5.0))
    );
    assert_eq!(
        compiled.plan.stats.get("projectile_damage"),
        Some(&SkillValue::Number(0.74))
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
    assert_eq!(
        compiled.plan.root,
        SkillNode::Sequence(vec![
            SkillNode::Action(
                "spell".to_owned(),
                [
                    ("id".to_owned(), SkillValue::String("spark_bolt".to_owned())),
                    ("damage_bonus".to_owned(), SkillValue::Number(12.0)),
                ]
                .into_iter()
                .collect()
            ),
            SkillNode::Action(
                "spell".to_owned(),
                [
                    (
                        "id".to_owned(),
                        SkillValue::String("magic_missile".to_owned())
                    ),
                    ("damage_bonus".to_owned(), SkillValue::Number(12.0)),
                ]
                .into_iter()
                .collect()
            ),
        ])
    );
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
    assert!(app.world().contains_resource::<PendingSkillExecutions>());
}

#[test]
fn library_replace_from_ron_supports_hot_reload_and_invalid_assets() {
    let registry = registry();
    let mut library = SkillLibrary::default();
    library
        .replace_from_ron(
            r#"Skill(id: "reloadable", body: Action("trace", { "label": "v1" }))"#,
            &registry,
        )
        .unwrap();
    assert!(library.get(&"reloadable".into()).is_some());

    let err = library
        .replace_from_ron(
            r#"Skill(id: "reloadable", body: Action("missing", {}))"#,
            &registry,
        )
        .unwrap_err();
    assert_eq!(err, SkillError::UnknownAction("missing".to_owned()));
    assert!(library.get(&"reloadable".into()).is_none());
    assert!(library.invalid(&"reloadable".into()).is_some());
}

#[derive(bevy::prelude::Resource, Default)]
struct Records(Vec<String>);

struct RecordAction;

impl SkillAction for RecordAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let label = match args.get("label") {
            Some(SkillValue::String(value)) => value.clone(),
            Some(SkillValue::Number(value)) => value.to_string(),
            other => format!("{other:?}"),
        };
        world.resource_mut::<Records>().0.push(label);
        Ok(())
    }
}

fn records(world: &World) -> Vec<String> {
    world.resource::<Records>().0.clone()
}

fn compiled_with_stats(stats: Vec<(String, SkillValue)>) -> SkillCompiled {
    SkillCompiled {
        id: "test".into(),
        tags: Default::default(),
        cast_model: "direct".to_owned(),
        plan: SkillPlan::new(SkillNode::Sequence(Vec::new()), stats.into_iter().collect()),
    }
}
