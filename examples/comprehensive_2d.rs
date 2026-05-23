use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy_skill_ecs::{
    SkillActionInput, SkillActionRegistry, SkillParams as SkillArgs, SkillValue,
    replace_compiled_skill,
};
use bevy_skill_flow::{
    ActiveSkill, SkillAction, SkillActionOutput, SkillCastRequest, SkillContext, SkillDslPlugin,
    SkillError, SkillId, SkillIntent, SkillRegistry, SkillResult, SkillRuntimeSignal,
    SkillValue as DslSkillValue, StatModifier, StatOp, compile_skill, parse_skill_document,
};

const ARENA_HALF: Vec2 = Vec2::new(520.0, 310.0);
const PLAYER_SPEED: f32 = 285.0;
const ENEMY_SPEED: f32 = 82.0;
const MAX_ENEMIES: usize = 14;

const SKILLS_RON: &str = r#"
[
  Skill(
    id: "split_fireball",
    tags: ["spell", "projectile", "fire"],
    params: {
      "base_damage": 24.0,
      "projectile_count": 1.0,
      "projectile_damage": 1.0,
      "projectile_spread_degrees": 0.0,
    },
    modifiers: ["fan_out"],
    body: Sequence([
      Emit("skill_cast", { "label": "Split Fireball" }),
      If(
        condition: Expr("stat.projectile_count > 1"),
        then_node: Emit("skill_note", { "label": "modifier widened the projectile fan" }),
      ),
      Action("spawn_projectile", {
        "kind": "fire",
        "count": Expr("stat.projectile_count"),
        "damage": Expr("stat.base_damage * stat.projectile_damage"),
        "speed": 540.0,
        "radius": 18.0,
        "spread_degrees": Expr("stat.projectile_spread_degrees"),
        "hit_event": "fireball_hit",
      }),
      On("fireball_hit", Action("area_damage", {
        "x": Expr("event.x"),
        "y": Expr("event.y"),
        "radius": 62.0,
        "amount": 12.0,
        "kind": "burn",
      })),
    ]),
  ),
  Skill(
    id: "delayed_blast",
    tags: ["spell", "aoe", "arcane"],
    params: {
      "blast_damage": 52.0,
      "blast_radius": 104.0,
    },
    body: Sequence([
      Emit("skill_cast", { "label": "Delayed Blast" }),
      Action("mark_blast", {
        "radius": Expr("stat.blast_radius"),
        "delay": 0.7,
      }),
      Delay(Expr("0.7"), Parallel([
        Action("detonate_marked_blast", {
          "radius": Expr("stat.blast_radius"),
          "amount": Expr("stat.blast_damage"),
        }),
        Action("heal_or_shield", { "amount": 8.0, "mode": "shield" }),
        Emit("blast_ready", { "label": "blast detonated" }),
      ])),
    ]),
  ),
  Skill(
    id: "arc_trap",
    tags: ["spell", "trap", "lightning"],
    params: {
      "trap_damage": 38.0,
      "trap_radius": 92.0,
    },
    body: Sequence([
      Emit("skill_cast", { "label": "Arc Trap" }),
      Action("spawn_zone", {
        "kind": "trap",
        "radius": Expr("stat.trap_radius"),
        "ttl": 5.0,
        "trigger_event": "trap_triggered",
      }),
      On("trap_triggered", Parallel([
        Action("area_damage", {
          "x": Expr("event.x"),
          "y": Expr("event.y"),
          "radius": Expr("stat.trap_radius"),
          "amount": Expr("stat.trap_damage"),
          "kind": "shock",
        }),
        Action("combat_log", { "message": "trap triggered" }),
      ])),
    ]),
  ),
  Skill(
    id: "burst_shot",
    tags: ["spell", "projectile", "physical"],
    params: {
      "burst_count": 5.0,
      "shot_damage": 13.0,
    },
    body: Sequence([
      Emit("skill_cast", { "label": "Burst Shot" }),
      Repeat(
        times: Some(Expr("stat.burst_count")),
        interval: Some(Expr("0.08")),
        node: Parallel([
          Action("spawn_projectile", {
            "kind": "bolt",
            "count": 1.0,
            "damage": Expr("stat.shot_damage"),
            "speed": 700.0,
            "radius": 10.0,
            "spread_degrees": 5.0,
          }),
          Emit("burst_tick", { "label": "burst projectile" }),
        ]),
      ),
    ]),
  ),
]
"#;

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "bevy_skill_flow - comprehensive 2D demo".to_owned(),
                    resolution: WindowResolution::new(1180, 720),
                    ..default()
                }),
                ..default()
            }),
            SkillDslPlugin,
        ))
        .init_resource::<AimWorld>()
        .init_resource::<CombatLog>()
        .init_resource::<BlastQueue>()
        .insert_resource(PlayerStats { shield: 0.0 })
        .insert_resource(EnemySpawnTimer(Timer::from_seconds(
            1.0,
            TimerMode::Repeating,
        )))
        .insert_resource(SkillBar::default())
        .add_systems(Startup, (setup_scene, setup_skill_library))
        .add_systems(
            Update,
            (
                update_aim,
                move_player,
                enemy_spawner,
                enemy_chase,
                handle_skill_input,
                consume_skill_intents,
                update_projectiles,
                update_zones,
                drain_skill_events,
                update_lifetimes,
                update_hud,
                draw_arena_gizmos,
            ),
        )
        .run();
}

#[derive(Component)]
struct Player;

#[derive(Component)]
struct Enemy {
    hp: f32,
    max_hp: f32,
}

#[derive(Component)]
struct Projectile {
    velocity: Vec2,
    damage: f32,
    radius: f32,
    hit_event: Option<String>,
}

#[derive(Component)]
struct Zone {
    radius: f32,
    ttl: Timer,
    trigger_event: Option<String>,
}

#[derive(Component)]
struct BlastMarker {
    radius: f32,
}

#[derive(Component)]
struct Lifetime {
    timer: Timer,
}

#[derive(Component)]
struct FloatingText;

#[derive(Component)]
struct Flash;

#[derive(Component)]
struct HudText;

#[derive(Resource, Default)]
struct AimWorld(Vec2);

#[derive(Resource)]
struct CastIntent {
    origin: Vec2,
    target: Vec2,
    direction: Vec2,
}

#[derive(Resource)]
struct PlayerStats {
    shield: f32,
}

#[derive(Resource)]
struct EnemySpawnTimer(Timer);

#[derive(Resource, Default)]
struct CombatLog {
    lines: Vec<String>,
}

#[derive(Resource, Default)]
struct BlastQueue {
    positions: Vec<Vec2>,
}

#[derive(Resource)]
struct SkillBar {
    slots: Vec<SkillSlot>,
}

impl Default for SkillBar {
    fn default() -> Self {
        Self {
            slots: vec![
                SkillSlot::new("split_fireball", "Split Fireball", 0.55),
                SkillSlot::new("delayed_blast", "Delayed Blast", 1.45),
                SkillSlot::new("arc_trap", "Arc Trap", 1.2),
                SkillSlot::new("burst_shot", "Burst Shot", 0.85),
            ],
        }
    }
}

struct SkillSlot {
    id: SkillId,
    label: &'static str,
    cooldown: f32,
    remaining: f32,
    key: KeyCode,
}

impl SkillSlot {
    fn new(id: &'static str, label: &'static str, cooldown: f32) -> Self {
        let key = match id {
            "split_fireball" => KeyCode::Digit1,
            "delayed_blast" => KeyCode::Digit2,
            "arc_trap" => KeyCode::Digit3,
            _ => KeyCode::Digit4,
        };
        Self {
            id: SkillId::new(id),
            label,
            cooldown,
            remaining: 0.0,
            key,
        }
    }
}

fn setup_scene(mut commands: Commands) {
    commands.spawn(Camera2d);

    commands.spawn((
        Sprite::from_color(Color::srgb(0.17, 0.21, 0.23), ARENA_HALF * 2.0),
        Transform::from_xyz(0.0, 0.0, -5.0),
    ));

    commands.spawn((
        Sprite::from_color(Color::srgb(0.25, 0.85, 0.72), Vec2::new(30.0, 30.0)),
        Transform::from_xyz(0.0, -80.0, 10.0),
        Player,
    ));

    for i in 0..6 {
        spawn_enemy(&mut commands, Vec2::new(-360.0 + i as f32 * 140.0, 220.0));
    }

    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: 18.0,
            ..default()
        },
        TextColor(Color::srgb(0.88, 0.92, 0.86)),
        Node {
            position_type: PositionType::Absolute,
            left: px(16),
            top: px(12),
            ..default()
        },
        HudText,
    ));
}

fn setup_skill_library(
    mut commands: Commands,
    mut registry: ResMut<SkillRegistry>,
    mut actions: ResMut<SkillActionRegistry>,
    mut library: ResMut<bevy_skill_flow::SkillLibrary>,
) {
    actions
        .register_skill_action("spawn_projectile", SpawnGameplayProjectile)
        .register_skill_action("spawn_zone", SpawnZoneAction)
        .register_skill_action("area_damage", AreaDamageAction)
        .register_skill_action("combat_log", CombatLogAction)
        .register_skill_action("mark_blast", MarkBlastAction)
        .register_skill_action("detonate_marked_blast", DetonateMarkedBlastAction)
        .register_skill_action("heal_or_shield", HealOrShieldAction);
    registry.register_skill_modifier(
        "fan_out",
        StatModifier::new(
            vec!["projectile".to_owned()],
            vec![
                StatOp::Add("projectile_count".to_owned(), DslSkillValue::Number(4.0)),
                StatOp::Set(
                    "projectile_spread_degrees".to_owned(),
                    DslSkillValue::Number(34.0),
                ),
                StatOp::Mul("projectile_damage".to_owned(), DslSkillValue::Number(0.68)),
            ],
        ),
    );

    for skill in parse_skill_document(SKILLS_RON).expect("demo skill RON should parse") {
        replace_compiled_skill(
            &mut commands,
            &mut library,
            compile_skill(&skill, &registry).expect("demo skill RON should compile"),
        );
    }
}

fn update_aim(
    mut aim: ResMut<AimWorld>,
    camera_query: Query<(&Camera, &GlobalTransform)>,
    window: Single<&Window>,
) {
    let Ok((camera, camera_transform)) = camera_query.single() else {
        return;
    };
    if let Some(cursor_position) = window.cursor_position()
        && let Ok(world_pos) = camera.viewport_to_world_2d(camera_transform, cursor_position)
    {
        aim.0 = world_pos;
    }
}

fn move_player(
    keyboard: Res<ButtonInput<KeyCode>>,
    time: Res<Time>,
    aim: Res<AimWorld>,
    mut player: Query<&mut Transform, With<Player>>,
) {
    let Ok(mut transform) = player.single_mut() else {
        return;
    };
    let mut input = Vec2::ZERO;
    if keyboard.pressed(KeyCode::KeyW) {
        input.y += 1.0;
    }
    if keyboard.pressed(KeyCode::KeyS) {
        input.y -= 1.0;
    }
    if keyboard.pressed(KeyCode::KeyA) {
        input.x -= 1.0;
    }
    if keyboard.pressed(KeyCode::KeyD) {
        input.x += 1.0;
    }
    if input.length_squared() > 0.0 {
        let delta = input.normalize() * PLAYER_SPEED * time.delta_secs();
        transform.translation.x = (transform.translation.x + delta.x).clamp(-500.0, 500.0);
        transform.translation.y = (transform.translation.y + delta.y).clamp(-292.0, 292.0);
    }

    let facing = aim.0 - transform.translation.xy();
    if facing.length_squared() > 0.001 {
        transform.rotation = Quat::from_rotation_z(facing.to_angle());
    }
}

fn enemy_spawner(
    mut commands: Commands,
    time: Res<Time>,
    mut timer: ResMut<EnemySpawnTimer>,
    enemies: Query<(), With<Enemy>>,
) {
    if enemies.iter().count() >= MAX_ENEMIES || !timer.0.tick(time.delta()).just_finished() {
        return;
    }
    let t = time.elapsed_secs();
    let side = (t as i32).rem_euclid(4);
    let wave = (t * 1.83).sin();
    let pos = match side {
        0 => Vec2::new(-ARENA_HALF.x + 24.0, wave * ARENA_HALF.y * 0.8),
        1 => Vec2::new(ARENA_HALF.x - 24.0, wave * ARENA_HALF.y * 0.8),
        2 => Vec2::new(wave * ARENA_HALF.x * 0.8, -ARENA_HALF.y + 24.0),
        _ => Vec2::new(wave * ARENA_HALF.x * 0.8, ARENA_HALF.y - 24.0),
    };
    spawn_enemy(&mut commands, pos);
}

fn enemy_chase(
    time: Res<Time>,
    mut stats: ResMut<PlayerStats>,
    player: Query<&Transform, With<Player>>,
    mut enemies: Query<&mut Transform, (With<Enemy>, Without<Player>)>,
) {
    let Ok(player_transform) = player.single() else {
        return;
    };
    let player_pos = player_transform.translation.xy();
    for mut transform in &mut enemies {
        let offset = player_pos - transform.translation.xy();
        if offset.length_squared() > 34.0 * 34.0 {
            transform.translation +=
                (offset.normalize_or_zero() * ENEMY_SPEED * time.delta_secs()).extend(0.0);
        } else {
            stats.shield = (stats.shield - 12.0 * time.delta_secs()).max(0.0);
        }
    }
}

fn handle_skill_input(world: &mut World) {
    let delta = world.resource::<Time>().delta_secs();
    {
        let mut skill_bar = world.resource_mut::<SkillBar>();
        for slot in &mut skill_bar.slots {
            slot.remaining = (slot.remaining - delta).max(0.0);
        }
    }

    let pressed_slot = {
        let keyboard = world.resource::<ButtonInput<KeyCode>>();
        let skill_bar = world.resource::<SkillBar>();
        skill_bar
            .slots
            .iter()
            .position(|slot| slot.remaining <= 0.0 && keyboard.just_pressed(slot.key))
    };
    let Some(slot_index) = pressed_slot else {
        return;
    };

    let mut player_query = world.query_filtered::<(Entity, &Transform), With<Player>>();
    let Ok((caster, player_transform)) = player_query.single(world) else {
        return;
    };
    let origin = player_transform.translation.xy();
    let target = world.resource::<AimWorld>().0;
    let direction = (target - origin).normalize_or_zero();
    let direction = if direction.length_squared() > 0.0 {
        direction
    } else {
        Vec2::X
    };

    world.insert_resource(CastIntent {
        origin,
        target,
        direction,
    });

    let (skill_id, cooldown) = {
        let skill_bar = world.resource::<SkillBar>();
        (
            skill_bar.slots[slot_index].id.clone(),
            skill_bar.slots[slot_index].cooldown,
        )
    };
    if world
        .resource::<bevy_skill_flow::SkillLibrary>()
        .get(&skill_id)
        .is_none()
    {
        push_log(world, format!("missing compiled skill `{skill_id}`"));
        return;
    }
    world.write_message(SkillCastRequest {
        skill: skill_id,
        caster,
        target: None,
    });
    world.resource_mut::<SkillBar>().slots[slot_index].remaining = cooldown;
}

fn consume_skill_intents(world: &mut World) {
    let intents = {
        let mut messages = world.resource_mut::<Messages<SkillIntent>>();
        let intents = messages
            .iter_current_update_messages()
            .cloned()
            .collect::<Vec<_>>();
        messages.clear();
        intents
    };

    for intent in intents {
        match intent.kind.as_str() {
            "spawn_projectile" => spawn_projectile_intent(world, &intent.payload),
            "spawn_zone" => spawn_zone_intent(world, &intent.payload),
            "area_damage" => area_damage_intent(world, &intent.payload),
            "combat_log" => {
                let message = text_arg(&intent.payload, "message")
                    .unwrap_or_else(|| "skill event".to_owned());
                push_log(world, message);
            }
            "mark_blast" => mark_blast_intent(world, &intent.payload),
            "detonate_marked_blast" => {
                let Some(position) = world.resource_mut::<BlastQueue>().positions.pop() else {
                    continue;
                };
                let mut area_args = intent.payload.clone();
                area_args.insert("x".to_owned(), SkillValue::Number(position.x as f64));
                area_args.insert("y".to_owned(), SkillValue::Number(position.y as f64));
                area_args.insert("kind".to_owned(), SkillValue::String("blast".to_owned()));
                area_damage_intent(world, &area_args);
            }
            "heal_or_shield" => {
                let amount = number_arg(&intent.payload, "amount", 5.0);
                let mode = text_arg(&intent.payload, "mode").unwrap_or_else(|| "shield".to_owned());
                let mut stats = world.resource_mut::<PlayerStats>();
                stats.shield = (stats.shield + amount).min(100.0);
                push_log(world, format!("{mode} +{amount:.0}"));
            }
            _ => {}
        }
    }
}

fn update_projectiles(world: &mut World) {
    let delta = world.resource::<Time>().delta_secs();
    let mut hits = Vec::new();
    let mut despawn = Vec::new();

    let mut projectile_query = world.query::<(Entity, &mut Transform, &Projectile)>();
    let projectiles = projectile_query
        .iter_mut(world)
        .map(|(entity, mut transform, projectile)| {
            transform.translation += (projectile.velocity * delta).extend(0.0);
            (
                entity,
                transform.translation.xy(),
                projectile.damage,
                projectile.radius,
                projectile.hit_event.clone(),
            )
        })
        .collect::<Vec<_>>();

    for (projectile_entity, position, damage, radius, hit_event) in projectiles {
        if position.x.abs() > ARENA_HALF.x + 40.0 || position.y.abs() > ARENA_HALF.y + 40.0 {
            despawn.push(projectile_entity);
            continue;
        }

        let mut enemy_query = world.query::<(Entity, &mut Enemy, &Transform)>();
        let mut hit_enemy = None;
        for (enemy_entity, mut enemy, enemy_transform) in enemy_query.iter_mut(world) {
            if enemy_transform.translation.xy().distance_squared(position)
                <= (radius + 18.0).powi(2)
            {
                enemy.hp -= damage;
                hit_enemy = Some((enemy_entity, enemy.hp <= 0.0));
                break;
            }
        }

        if let Some((enemy_entity, died)) = hit_enemy {
            spawn_floating_text(world, position + Vec2::Y * 20.0, format!("{damage:.0}"));
            spawn_flash(world, position, Color::srgba(1.0, 0.52, 0.22, 0.85), 34.0);
            despawn.push(projectile_entity);
            if died {
                despawn.push(enemy_entity);
                push_log(world, "enemy defeated".to_owned());
            }
            if let Some(name) = hit_event {
                hits.push(SkillRuntimeSignal::new(
                    name,
                    [
                        ("x".to_owned(), SkillValue::Number(position.x as f64)),
                        ("y".to_owned(), SkillValue::Number(position.y as f64)),
                    ]
                    .into_iter()
                    .collect(),
                ));
            }
        }
    }

    for entity in despawn {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }

    for event in hits {
        world.write_message(event);
    }
}

fn update_zones(world: &mut World) {
    let delta = world.resource::<Time>().delta();
    let mut expired = Vec::new();
    let mut triggered = Vec::new();

    let mut zone_query = world.query::<(Entity, &mut Zone, &Transform)>();
    let zones = zone_query
        .iter_mut(world)
        .map(|(zone_entity, mut zone, transform)| {
            zone.ttl.tick(delta);
            (
                zone_entity,
                zone.ttl.is_finished(),
                zone.radius,
                zone.trigger_event.clone(),
                transform.translation.xy(),
            )
        })
        .collect::<Vec<_>>();

    for (zone_entity, is_finished, radius, trigger_event, position) in zones {
        if is_finished {
            expired.push(zone_entity);
            continue;
        }
        let Some(name) = trigger_event else {
            continue;
        };
        let mut enemy_query = world.query_filtered::<&Transform, With<Enemy>>();
        let touched = enemy_query.iter(world).any(|enemy_transform| {
            enemy_transform.translation.xy().distance_squared(position) <= radius.powi(2)
        });
        if touched {
            triggered.push((
                zone_entity,
                SkillRuntimeSignal::new(
                    name.clone(),
                    [
                        ("x".to_owned(), SkillValue::Number(position.x as f64)),
                        ("y".to_owned(), SkillValue::Number(position.y as f64)),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ));
        }
    }

    for (entity, event) in triggered {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
        spawn_flash(
            world,
            event_coord(&event),
            Color::srgba(0.5, 0.9, 1.0, 0.75),
            92.0,
        );
        world.write_message(event);
    }

    for entity in expired {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }
}

fn drain_skill_events(world: &mut World) {
    let events = world
        .resource::<Messages<SkillRuntimeSignal>>()
        .iter_current_update_messages()
        .cloned()
        .collect::<Vec<_>>();

    for event in events {
        let label = text_arg(&event.payload, "label").unwrap_or_else(|| event.name.clone());
        push_log(world, format!("emit: {label}"));
        if event.name == "skill_cast" {
            let target = world.resource::<AimWorld>().0;
            spawn_flash(world, target, Color::srgba(1.0, 1.0, 1.0, 0.35), 24.0);
        }
    }
}

fn update_lifetimes(
    mut commands: Commands,
    time: Res<Time>,
    mut sprites: Query<(
        Entity,
        &mut Transform,
        Option<&mut Sprite>,
        Option<&mut TextColor>,
        &mut Lifetime,
    )>,
) {
    for (entity, mut transform, sprite, text_color, mut lifetime) in &mut sprites {
        lifetime.timer.tick(time.delta());
        let left = lifetime.timer.fraction_remaining();
        if lifetime.timer.is_finished() {
            commands.entity(entity).despawn();
            continue;
        }
        if text_color.is_some() {
            transform.translation.y += 42.0 * time.delta_secs();
        }
        if let Some(mut sprite) = sprite {
            sprite.color = sprite.color.with_alpha(left.clamp(0.0, 1.0));
            transform.scale = Vec3::splat(1.0 + (1.0 - left) * 0.7);
        }
        if let Some(mut text_color) = text_color {
            text_color.0 = text_color.0.with_alpha(left.clamp(0.0, 1.0));
        }
    }
}

fn update_hud(
    skill_bar: Res<SkillBar>,
    stats: Res<PlayerStats>,
    active_skills: Query<&ActiveSkill>,
    enemies: Query<&Enemy>,
    log: Res<CombatLog>,
    mut hud: Single<&mut Text, With<HudText>>,
) {
    let mut text = String::from("WASD move | Mouse aim | 1-4 cast\n");
    text.push_str(&format!(
        "Shield: {:>4.0} | Enemies: {} | Pending runtime nodes: {}\n\n",
        stats.shield,
        enemies.iter().count(),
        active_skills.iter().count()
    ));
    for (index, slot) in skill_bar.slots.iter().enumerate() {
        let state = if slot.remaining > 0.0 {
            format!("{:.1}s", slot.remaining)
        } else {
            "ready".to_owned()
        };
        text.push_str(&format!("{}  {:<16} {}\n", index + 1, slot.label, state));
    }
    text.push('\n');
    for line in log.lines.iter().rev().take(7) {
        text.push_str(line);
        text.push('\n');
    }
    hud.0 = text;
}

fn draw_arena_gizmos(
    mut gizmos: Gizmos,
    aim: Res<AimWorld>,
    player: Query<&Transform, With<Player>>,
    enemies: Query<(&Enemy, &Transform)>,
    zones: Query<(&Zone, &Transform)>,
    blasts: Query<(&BlastMarker, &Transform)>,
) {
    gizmos.rect_2d(
        Isometry2d::IDENTITY,
        ARENA_HALF * 2.0,
        Color::srgb(0.42, 0.48, 0.46),
    );
    if let Ok(player_transform) = player.single() {
        gizmos.line_2d(
            player_transform.translation.xy(),
            aim.0,
            Color::srgb(0.8, 0.92, 0.88),
        );
    }
    gizmos.cross_2d(aim.0, 12.0, Color::srgb(1.0, 0.95, 0.55));
    for (enemy, transform) in &enemies {
        let center = transform.translation.xy() + Vec2::new(0.0, 25.0);
        let width = 42.0 * (enemy.hp / enemy.max_hp).clamp(0.0, 1.0);
        gizmos.line_2d(
            center - Vec2::X * 21.0,
            center + Vec2::X * 21.0,
            Color::srgb(0.18, 0.12, 0.13),
        );
        gizmos.line_2d(
            center - Vec2::X * 21.0,
            center - Vec2::X * 21.0 + Vec2::X * width,
            Color::srgb(0.95, 0.22, 0.22),
        );
    }
    for (zone, transform) in &zones {
        gizmos.circle_2d(
            transform.translation.xy(),
            zone.radius,
            Color::srgba(0.45, 0.88, 1.0, 0.8),
        );
    }
    for (blast, transform) in &blasts {
        gizmos.circle_2d(
            transform.translation.xy(),
            blast.radius,
            Color::srgba(1.0, 0.8, 0.25, 0.9),
        );
    }
}

fn spawn_enemy(commands: &mut Commands, position: Vec2) {
    commands.spawn((
        Sprite::from_color(Color::srgb(0.86, 0.24, 0.31), Vec2::new(34.0, 34.0)),
        Transform::from_xyz(position.x, position.y, 5.0),
        Enemy {
            hp: 72.0,
            max_hp: 72.0,
        },
    ));
}

fn spawn_projectile_intent(world: &mut World, args: &SkillArgs) {
    let intent = world.resource::<CastIntent>();
    let count = number_arg(args, "count", 1.0).round().max(1.0) as usize;
    let speed = number_arg(args, "speed", 520.0);
    let damage = number_arg(args, "damage", 10.0);
    let radius = number_arg(args, "radius", 12.0);
    let spread = number_arg(args, "spread_degrees", 0.0).to_radians();
    let kind = text_arg(args, "kind").unwrap_or_else(|| "bolt".to_owned());
    let hit_event = text_arg(args, "hit_event");
    let base_angle = intent.direction.to_angle();
    let color = match kind.as_str() {
        "fire" => Color::srgb(1.0, 0.38, 0.12),
        "bolt" => Color::srgb(0.88, 0.92, 1.0),
        _ => Color::srgb(0.8, 0.8, 0.8),
    };
    let origin = intent.origin;

    for index in 0..count {
        let t = if count == 1 {
            0.0
        } else {
            index as f32 / (count - 1) as f32 - 0.5
        };
        let direction = Vec2::from_angle(base_angle + spread * t);
        world.spawn((
            Sprite::from_color(color, Vec2::new(radius * 1.9, radius * 0.9)),
            Transform {
                translation: (origin + direction * 34.0).extend(12.0),
                rotation: Quat::from_rotation_z(direction.to_angle()),
                ..default()
            },
            Projectile {
                velocity: direction * speed,
                damage,
                radius,
                hit_event: hit_event.clone(),
            },
            Lifetime {
                timer: Timer::from_seconds(1.7, TimerMode::Once),
            },
        ));
    }
}

fn spawn_zone_intent(world: &mut World, args: &SkillArgs) {
    let intent = world.resource::<CastIntent>();
    let radius = number_arg(args, "radius", 80.0);
    let ttl = number_arg(args, "ttl", 4.0);
    let trigger_event = text_arg(args, "trigger_event");
    let target = clamp_to_arena(intent.target);
    world.spawn((
        Sprite::from_color(
            Color::srgba(0.25, 0.65, 1.0, 0.18),
            Vec2::splat(radius * 2.0),
        ),
        Transform::from_xyz(target.x, target.y, 1.0),
        Zone {
            radius,
            ttl: Timer::from_seconds(ttl, TimerMode::Once),
            trigger_event,
        },
    ));
}

fn area_damage_intent(world: &mut World, args: &SkillArgs) {
    let fallback = world.resource::<CastIntent>().target;
    let center = Vec2::new(
        number_arg(args, "x", fallback.x),
        number_arg(args, "y", fallback.y),
    );
    let radius = number_arg(args, "radius", 80.0);
    let amount = number_arg(args, "amount", 20.0);
    let kind = text_arg(args, "kind").unwrap_or_else(|| "hit".to_owned());
    let mut dead = Vec::new();
    let mut floats = Vec::new();
    let mut hit_count = 0;

    let mut enemy_query = world.query::<(Entity, &mut Enemy, &Transform)>();
    for (entity, mut enemy, transform) in enemy_query.iter_mut(world) {
        if transform.translation.xy().distance_squared(center) <= radius.powi(2) {
            enemy.hp -= amount;
            hit_count += 1;
            floats.push(transform.translation.xy() + Vec2::new(0.0, 28.0));
            if enemy.hp <= 0.0 {
                dead.push(entity);
            }
        }
    }
    for position in floats {
        spawn_floating_text(world, position, format!("{amount:.0}"));
    }
    for entity in dead {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }
    spawn_flash(
        world,
        center,
        Color::srgba(1.0, 0.78, 0.2, 0.55),
        radius * 1.05,
    );
    push_log(world, format!("{kind} hit {hit_count} enemies"));
}

fn mark_blast_intent(world: &mut World, args: &SkillArgs) {
    let intent = world.resource::<CastIntent>();
    let position = clamp_to_arena(intent.target);
    let radius = number_arg(args, "radius", 100.0);
    let delay = number_arg(args, "delay", 0.7);
    world.resource_mut::<BlastQueue>().positions.push(position);
    world.spawn((
        Sprite::from_color(
            Color::srgba(1.0, 0.78, 0.16, 0.22),
            Vec2::splat(radius * 2.0),
        ),
        Transform::from_xyz(position.x, position.y, 2.0),
        BlastMarker { radius },
        Lifetime {
            timer: Timer::from_seconds(delay, TimerMode::Once),
        },
    ));
}

#[derive(Clone, Debug, Default)]
struct SpawnGameplayProjectile;

impl SkillAction for SpawnGameplayProjectile {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "spawn_projectile", input.args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct SpawnZoneAction;

impl SkillAction for SpawnZoneAction {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "spawn_zone", input.args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct AreaDamageAction;

impl SkillAction for AreaDamageAction {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "area_damage", input.args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct CombatLogAction;

impl SkillAction for CombatLogAction {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "combat_log", input.args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct MarkBlastAction;

impl SkillAction for MarkBlastAction {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "mark_blast", input.args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct DetonateMarkedBlastAction;

impl SkillAction for DetonateMarkedBlastAction {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "detonate_marked_blast", input.args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct HealOrShieldAction;

impl SkillAction for HealOrShieldAction {
    fn validate(
        &self,
        _args: &SkillArgs,
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
        out.emit_intent(ctx, "heal_or_shield", input.args.clone())
    }
}

fn spawn_floating_text(world: &mut World, position: Vec2, text: String) {
    world.spawn((
        Text2d::new(text),
        TextFont {
            font_size: 20.0,
            ..default()
        },
        TextColor(Color::srgb(1.0, 0.95, 0.62)),
        Transform::from_xyz(position.x, position.y, 30.0),
        FloatingText,
        Lifetime {
            timer: Timer::from_seconds(0.75, TimerMode::Once),
        },
    ));
}

fn spawn_flash(world: &mut World, position: Vec2, color: Color, size: f32) {
    world.spawn((
        Sprite::from_color(color, Vec2::splat(size)),
        Transform::from_xyz(position.x, position.y, 20.0),
        Flash,
        Lifetime {
            timer: Timer::from_seconds(0.22, TimerMode::Once),
        },
    ));
}

fn push_log(world: &mut World, line: String) {
    let mut log = world.resource_mut::<CombatLog>();
    log.lines.push(line);
    if log.lines.len() > 10 {
        log.lines.remove(0);
    }
}

fn number_arg(args: &SkillArgs, key: &str, fallback: f32) -> f32 {
    match args.get(key) {
        Some(SkillValue::Number(value)) => *value as f32,
        _ => fallback,
    }
}

fn text_arg(args: &SkillArgs, key: &str) -> Option<String> {
    match args.get(key) {
        Some(SkillValue::String(value)) => Some(value.clone()),
        Some(SkillValue::Number(value)) => Some(value.to_string()),
        Some(SkillValue::Bool(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn clamp_to_arena(value: Vec2) -> Vec2 {
    Vec2::new(
        value.x.clamp(-ARENA_HALF.x + 20.0, ARENA_HALF.x - 20.0),
        value.y.clamp(-ARENA_HALF.y + 20.0, ARENA_HALF.y - 20.0),
    )
}

fn event_coord(event: &SkillRuntimeSignal) -> Vec2 {
    Vec2::new(
        number_arg(&event.payload, "x", 0.0),
        number_arg(&event.payload, "y", 0.0),
    )
}
