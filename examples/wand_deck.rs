use bevy::prelude::{App, FixedUpdate, MinimalPlugins};
use bevy_skill_ecs::SkillActionRegistry;
use bevy_skill_flow::{
    SkillCastRequest, SkillDslPlugin, SkillIntent, SkillLibrary, SkillRegistry, compile_skill,
    parse_skill_def,
};
use bevy_skill_flow_gameplay::{SpellAction, WandDeckCastModel};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, SkillDslPlugin));
    {
        let mut registry = app.world_mut().resource_mut::<SkillRegistry>();
        registry.register_cast_model("wand_deck", WandDeckCastModel);
    }
    app.world_mut()
        .resource_mut::<SkillActionRegistry>()
        .register_skill_action("spell", SpellAction);

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

    let registry = app.world().resource::<SkillRegistry>().clone();
    let mut library = SkillLibrary::default();
    library.insert_compiled(compile_skill(&parse_skill_def(ron)?, &registry)?);
    *app.world_mut().resource_mut::<SkillLibrary>() = library;

    let caster = app.world_mut().spawn_empty().id();
    app.world_mut().write_message(SkillCastRequest {
        skill: "basic_wand".into(),
        caster,
        target: None,
    });
    app.update();
    app.world_mut().run_schedule(FixedUpdate);
    let intents = app
        .world()
        .resource::<bevy::prelude::Messages<SkillIntent>>()
        .iter_current_update_messages()
        .count();

    println!("cast wand deck intents={intents}");
    Ok(())
}
