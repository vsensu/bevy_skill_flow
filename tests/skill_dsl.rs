use bevy::prelude::{App, World};
use bevy_skill_flow::{
    ModifierDef, PendingSkillExecutions, SkillAction, SkillArgs, SkillContext, SkillError,
    SkillExpr, SkillNode, SkillRegistry, SkillResult, SkillRuntimeEvent, SkillValue, StatModifier,
    compile_skill, eval_skill_expr, parse_skill_def, parse_skill_document,
};
use bevy_skill_flow::{SkillCompiled, SkillDslPlugin, SkillLibrary, SkillPlan};
use bevy_skill_flow_gameplay::register_gameplay_primitives;

fn registry() -> SkillRegistry {
    let mut registry = SkillRegistry::with_core();
    register_gameplay_primitives(&mut registry);
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
