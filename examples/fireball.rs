use bevy::prelude::World;
use bevy_skill_dsl::examples::SpawnProjectileAction;
use bevy_skill_dsl::{
    PendingSkillExecutions, SkillLibrary, SkillRegistry, SkillValue, StatModifier, StatOp,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = SkillRegistry::with_core();
    registry.register_skill_action("spawn_projectile", SpawnProjectileAction);
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

    let mut library = SkillLibrary::default();
    library.replace_from_ron(ron, &registry)?;
    let compiled = library.get(&"fireball".into()).expect("compiled skill");

    let mut world = World::new();
    let caster = world.spawn_empty().id();
    let mut pending = PendingSkillExecutions::default();
    let execution_id = pending.cast(compiled, caster, &mut world, &registry)?;

    println!(
        "cast fireball execution={execution_id}, projectile_count={:?}, projectile_damage={:?}",
        compiled.plan.stats.get("projectile_count"),
        compiled.plan.stats.get("projectile_damage")
    );
    Ok(())
}
