use bevy::prelude::{App, FixedUpdate, MinimalPlugins};
use bevy_skill_ecs::SkillActionRegistry;
use bevy_skill_flow::{
    SkillCastRequest, SkillDslPlugin, SkillIntent, SkillLibrary, SkillRegistry, SkillValue,
    StatModifier, StatOp, compile_skill, parse_skill_def,
};
use bevy_skill_flow_gameplay::SpawnProjectileAction;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, SkillDslPlugin));
    {
        let mut registry = app.world_mut().resource_mut::<SkillRegistry>();
        registry.register_skill_modifier(
            "greater_multiple_projectiles",
            StatModifier::new(
                vec!["projectile".to_owned()],
                vec![
                    StatOp::Add("projectile_count".to_owned(), SkillValue::Number(4.0)),
                    StatOp::Mul("projectile_damage".to_owned(), SkillValue::Number(0.74)),
                ],
            ),
        );
    }
    app.world_mut()
        .resource_mut::<SkillActionRegistry>()
        .register_skill_action("spawn_projectile", SpawnProjectileAction);

    let ron = r#"
        Skill(
          id: "fireball",
          tags: ["spell", "projectile", "aoe", "fire"],
          params: {
            "base_damage": 40.0,
            "projectile_damage": 1.0,
            "projectile_count": 1.0,
          },
          modifiers: ["greater_multiple_projectiles"],
          body: Action("spawn_projectile", {
            "prefab": "fireball",
            "count": Expr("stat.projectile_count ?? 1"),
          }),
        )
    "#;

    let registry = app.world().resource::<SkillRegistry>().clone();
    let compiled = compile_skill(&parse_skill_def(ron)?, &registry)?;
    let mut library = SkillLibrary::default();
    library.insert_compiled(compiled.clone());
    *app.world_mut().resource_mut::<SkillLibrary>() = library;

    let caster = app.world_mut().spawn_empty().id();
    app.world_mut().write_message(SkillCastRequest {
        skill: "fireball".into(),
        caster,
        target: None,
    });
    app.update();
    app.world_mut().run_schedule(FixedUpdate);
    let intents = app
        .world()
        .resource::<bevy::prelude::Messages<SkillIntent>>()
        .iter_current_update_messages()
        .cloned()
        .collect::<Vec<_>>();

    println!(
        "cast fireball intents={}, projectile_count={:?}, projectile_damage={:?}",
        intents.len(),
        compiled.graph.params.get("projectile_count"),
        compiled.graph.params.get("projectile_damage")
    );
    Ok(())
}
