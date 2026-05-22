use bevy::prelude::World;
use bevy_skill_dsl::examples::{SpellAction, WandDeckCastModel};
use bevy_skill_dsl::{PendingSkillExecutions, SkillLibrary, SkillRegistry};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut registry = SkillRegistry::with_core();
    registry
        .register_skill_action("spell", SpellAction)
        .register_cast_model("wand_deck", WandDeckCastModel);

    let ron = r#"
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
    "#;

    let mut library = SkillLibrary::default();
    library.replace_from_ron(ron, &registry)?;
    let compiled = library.get(&"basic_wand".into()).expect("compiled skill");

    let mut world = World::new();
    let caster = world.spawn_empty().id();
    let mut pending = PendingSkillExecutions::default();
    let execution_id = pending.cast(compiled, caster, &mut world, &registry)?;

    println!("cast wand deck execution={execution_id}");
    Ok(())
}
