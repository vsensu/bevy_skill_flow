use bevy::prelude::*;
use bevy::window::WindowResolution;
use bevy_egui::{EguiContexts, EguiPlugin, EguiPrimaryContextPass, egui};
use bevy_skill_dsl::editor::{
    SkillEditorConfig, SkillEditorPlugin, SkillEditorState, is_skill_file,
};
use bevy_skill_dsl::{
    PendingSkillExecutions, SkillAction, SkillArgs, SkillContext, SkillDslPlugin, SkillError,
    SkillId, SkillLibrary, SkillRegistry, SkillResult, SkillRuntimeEvent, SkillValue, StatModifier,
    StatOp,
};
use std::fs;
use std::path::Path;

const ARENA_HALF: Vec2 = Vec2::new(430.0, 280.0);
const PLAYER_SPEED: f32 = 280.0;
const ENEMY_SPEED: f32 = 70.0;
const MAX_ENEMIES: usize = 10;

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "bevy_skill_dsl - skill editor".to_owned(),
                    resolution: WindowResolution::new(1380, 820),
                    ..default()
                }),
                ..default()
            }),
            EguiPlugin::default(),
            SkillDslPlugin,
            SkillEditorPlugin::new(SkillEditorConfig::default()),
        ))
        .init_resource::<AimWorld>()
        .init_resource::<CombatLog>()
        .init_resource::<BlastQueue>()
        .init_resource::<EditorCastRequest>()
        .insert_resource(PlayerStats { shield: 0.0 })
        .insert_resource(EnemySpawnTimer(Timer::from_seconds(
            1.2,
            TimerMode::Repeating,
        )))
        .insert_resource(SkillBar::default())
        .add_systems(Startup, (setup_scene, setup_editor_library))
        .add_systems(EguiPrimaryContextPass, editor_ui)
        .add_systems(
            Update,
            (
                update_aim,
                move_player,
                enemy_spawner,
                enemy_chase,
                handle_skill_input,
                tick_skill_runtime,
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
    recent_events: Vec<String>,
}

#[derive(Resource, Default)]
struct BlastQueue {
    positions: Vec<Vec2>,
}

#[derive(Resource, Default)]
struct EditorCastRequest(Option<SkillId>);

#[derive(Resource, Default)]
struct SkillBar {
    slots: Vec<SkillSlot>,
}

struct SkillSlot {
    id: SkillId,
    cooldown: f32,
    remaining: f32,
    key: KeyCode,
}

fn setup_scene(mut commands: Commands) {
    commands.spawn(Camera2d);
    commands.spawn((
        Sprite::from_color(Color::srgb(0.14, 0.17, 0.18), ARENA_HALF * 2.0),
        Transform::from_xyz(0.0, 0.0, -5.0),
    ));
    commands.spawn((
        Sprite::from_color(Color::srgb(0.25, 0.86, 0.72), Vec2::new(30.0, 30.0)),
        Transform::from_xyz(0.0, -80.0, 10.0),
        Player,
    ));
    for i in 0..5 {
        spawn_enemy(&mut commands, Vec2::new(-280.0 + i as f32 * 140.0, 205.0));
    }
    commands.spawn((
        Text::new(""),
        TextFont {
            font_size: 17.0,
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

fn setup_editor_library(
    config: Res<SkillEditorConfig>,
    mut registry: ResMut<SkillRegistry>,
    mut library: ResMut<SkillLibrary>,
    mut editor: ResMut<SkillEditorState>,
    mut skill_bar: ResMut<SkillBar>,
    mut log: ResMut<CombatLog>,
) {
    registry
        .register_skill_action("spawn_projectile", SpawnGameplayProjectile)
        .register_skill_action("spawn_zone", SpawnZoneAction)
        .register_skill_action("area_damage", AreaDamageAction)
        .register_skill_action("combat_log", CombatLogAction)
        .register_skill_action("mark_blast", MarkBlastAction)
        .register_skill_action("detonate_marked_blast", DetonateMarkedBlastAction)
        .register_skill_action("heal_or_shield", HealOrShieldAction)
        .register_skill_modifier(
            "fan_out",
            StatModifier::new(
                vec!["projectile".to_owned()],
                vec![
                    StatOp::Add("projectile_count".to_owned(), SkillValue::Number(4.0)),
                    StatOp::Set(
                        "projectile_spread_degrees".to_owned(),
                        SkillValue::Number(34.0),
                    ),
                    StatOp::Mul("projectile_damage".to_owned(), SkillValue::Number(0.68)),
                ],
            ),
        );

    if let Err(err) = seed_default_skills(&config.skills_dir) {
        log.lines.push(format!("seed skills failed: {err}"));
    }
    if let Err(err) = editor.load_dir(&config, &registry, &mut library) {
        log.lines.push(format!("load skills failed: {err}"));
    }
    refresh_skill_bar(&mut skill_bar, &editor);
}

fn editor_ui(
    mut contexts: EguiContexts,
    config: Res<SkillEditorConfig>,
    registry: Res<SkillRegistry>,
    mut library: ResMut<SkillLibrary>,
    mut editor: ResMut<SkillEditorState>,
    mut cast_request: ResMut<EditorCastRequest>,
    mut skill_bar: ResMut<SkillBar>,
    pending: Res<PendingSkillExecutions>,
    log: Res<CombatLog>,
) -> Result {
    let mut action = EditorAction::None;
    let selected = editor.current_file.clone();

    egui::SidePanel::left("skill_files")
        .resizable(true)
        .default_width(220.0)
        .show(contexts.ctx_mut()?, |ui| {
            ui.heading("Skills");
            ui.horizontal(|ui| {
                if ui.button("New").clicked() {
                    action = EditorAction::New;
                }
                if ui.button("Save").clicked() {
                    action = EditorAction::Save;
                }
                if ui.button("Reload").clicked() {
                    action = EditorAction::Reload;
                }
            });
            if ui.button("Delete").clicked() {
                action = EditorAction::Delete;
            }
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for file in &editor.files {
                    let is_selected = Some(&file.path) == selected.as_ref();
                    let status = if file.parse_error.is_some() {
                        "parse"
                    } else if file.compile_error.is_some() {
                        "compile"
                    } else {
                        "ok"
                    };
                    let label = format!("{}  {}", status, file.label());
                    if ui.selectable_label(is_selected, label).clicked() {
                        action = EditorAction::Open(file.path.clone());
                    }
                }
            });
        });

    egui::SidePanel::right("skill_inspector")
        .resizable(true)
        .default_width(280.0)
        .show(contexts.ctx_mut()?, |ui| {
            ui.heading("Inspector");
            if let Some(path) = editor.current_file.as_ref() {
                ui.label(path.display().to_string());
            }
            ui.separator();
            status_row(ui, "Dirty", if editor.dirty { "yes" } else { "no" });
            if let Some(def) = editor.current_def.as_ref() {
                status_row(ui, "Id", &def.id.0);
                status_row(ui, "Cast", &def.cast_model);
                status_row(ui, "Tags", &def.tags.join(", "));
            }
            ui.separator();
            if let Some(err) = editor.diagnostics.parse_error.as_ref() {
                ui.colored_label(egui::Color32::from_rgb(238, 112, 92), err);
            } else if let Some(err) = editor.diagnostics.compile_error.as_ref() {
                ui.colored_label(egui::Color32::from_rgb(238, 180, 80), err.to_string());
            } else {
                ui.colored_label(egui::Color32::from_rgb(124, 207, 154), "parse + compile ok");
            }
            ui.separator();
            if let Some(compiled) = editor.selected_compiled() {
                ui.label("Stats");
                for (key, value) in &compiled.plan.stats {
                    ui.monospace(format!("{key}: {value:?}"));
                }
            }
            ui.separator();
            status_row(ui, "Compiled", &library.compiled_len().to_string());
            status_row(ui, "Pending", &pending.len().to_string());
            ui.separator();
            ui.label("Events");
            for event in log.recent_events.iter().rev().take(8) {
                ui.monospace(event);
            }
        });

    egui::TopBottomPanel::bottom("skill_toolbar")
        .resizable(false)
        .show(contexts.ctx_mut()?, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Cast Selected").clicked()
                    && let Some(id) = editor.selected_skill_id()
                {
                    cast_request.0 = Some(id);
                }
                for (index, slot) in skill_bar.slots.iter().enumerate() {
                    let state = if slot.remaining > 0.0 {
                        format!("{:.1}s", slot.remaining)
                    } else {
                        "ready".to_owned()
                    };
                    ui.label(format!("{}: {} ({state})", index + 1, slot.id));
                }
            });
        });

    egui::CentralPanel::default().show(contexts.ctx_mut()?, |ui| {
        let mut source = editor.source.clone();
        let response = egui::TextEdit::multiline(&mut source)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .desired_rows(28)
            .lock_focus(true)
            .show(ui);
        if response.response.changed() {
            editor.edit_source(source, &registry, &mut library);
            if config.autosave_on_compile_success && editor.diagnostics.compile_error.is_none() {
                let _ = editor.save_current(&registry);
            }
            refresh_skill_bar(&mut skill_bar, &editor);
        }
    });

    match action {
        EditorAction::None => {}
        EditorAction::Open(path) => {
            if let Err(err) = editor.open_file(path, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            }
        }
        EditorAction::New => {
            if let Err(err) = editor.create_new(&config, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            }
        }
        EditorAction::Save => {
            if let Err(err) = editor.save_current(&registry) {
                editor.last_io_error = Some(err.to_string());
            }
        }
        EditorAction::Reload => {
            if let Err(err) = editor.load_dir(&config, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            }
        }
        EditorAction::Delete => {
            if let Err(err) = editor.delete_current(&config, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            }
        }
    }
    refresh_skill_bar(&mut skill_bar, &editor);
    Ok(())
}

#[derive(Clone)]
enum EditorAction {
    None,
    Open(std::path::PathBuf),
    New,
    Save,
    Reload,
    Delete,
}

fn status_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.monospace(value);
    });
}

fn refresh_skill_bar(skill_bar: &mut SkillBar, editor: &SkillEditorState) {
    let previous = skill_bar
        .slots
        .iter()
        .map(|slot| (slot.id.clone(), slot.remaining))
        .collect::<std::collections::HashMap<_, _>>();
    skill_bar.slots = editor
        .compiled_cache
        .keys()
        .take(4)
        .enumerate()
        .map(|(index, id)| SkillSlot {
            id: id.clone(),
            cooldown: 0.45 + index as f32 * 0.2,
            remaining: previous.get(id).copied().unwrap_or(0.0),
            key: match index {
                0 => KeyCode::Digit1,
                1 => KeyCode::Digit2,
                2 => KeyCode::Digit3,
                _ => KeyCode::Digit4,
            },
        })
        .collect();
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
        transform.translation.x = (transform.translation.x + delta.x).clamp(-410.0, 410.0);
        transform.translation.y = (transform.translation.y + delta.y).clamp(-260.0, 260.0);
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
    let wave = (t * 1.67).sin();
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
            stats.shield = (stats.shield - 10.0 * time.delta_secs()).max(0.0);
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

    let requested_from_ui = world.resource_mut::<EditorCastRequest>().0.take();
    let requested_from_key = {
        let keyboard = world.resource::<ButtonInput<KeyCode>>();
        let skill_bar = world.resource::<SkillBar>();
        skill_bar
            .slots
            .iter()
            .position(|slot| slot.remaining <= 0.0 && keyboard.just_pressed(slot.key))
            .map(|index| skill_bar.slots[index].id.clone())
    };
    let Some(skill_id) = requested_from_ui.or(requested_from_key) else {
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

    let Some(compiled) = world.resource::<SkillLibrary>().get(&skill_id).cloned() else {
        push_log(world, format!("missing compiled skill `{skill_id}`"));
        return;
    };
    let registry = world.resource::<SkillRegistry>().clone();
    let mut pending = world
        .remove_resource::<PendingSkillExecutions>()
        .unwrap_or_default();
    match pending.cast(&compiled, caster, world, &registry) {
        Ok(_) => {
            let mut skill_bar = world.resource_mut::<SkillBar>();
            if let Some(slot) = skill_bar.slots.iter_mut().find(|slot| slot.id == skill_id) {
                slot.remaining = slot.cooldown;
            }
        }
        Err(err) => push_log(world, format!("skill error: {err}")),
    }
    world.insert_resource(pending);
}

fn tick_skill_runtime(world: &mut World) {
    let delta = world.resource::<Time>().delta_secs_f64();
    let registry = world.resource::<SkillRegistry>().clone();
    let mut pending = world
        .remove_resource::<PendingSkillExecutions>()
        .unwrap_or_default();
    if let Err(err) = pending.tick(delta, world, &registry) {
        push_log(world, format!("runtime tick error: {err}"));
    }
    world.insert_resource(pending);
}

fn update_projectiles(world: &mut World) {
    let delta = world.resource::<Time>().delta_secs();
    let registry = world.resource::<SkillRegistry>().clone();
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
            spawn_flash(world, position, Color::srgba(1.0, 0.52, 0.22, 0.85), 34.0);
            despawn.push(projectile_entity);
            if died {
                despawn.push(enemy_entity);
                push_log(world, "enemy defeated".to_owned());
            }
            if let Some(name) = hit_event {
                hits.push(SkillRuntimeEvent::new(
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
        trigger_runtime_event(world, &registry, event);
    }
}

fn update_zones(world: &mut World) {
    let delta = world.resource::<Time>().delta();
    let registry = world.resource::<SkillRegistry>().clone();
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
                SkillRuntimeEvent::new(
                    name,
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
        trigger_runtime_event(world, &registry, event);
    }
    for entity in expired {
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }
}

fn trigger_runtime_event(world: &mut World, registry: &SkillRegistry, event: SkillRuntimeEvent) {
    let mut pending = world
        .remove_resource::<PendingSkillExecutions>()
        .unwrap_or_default();
    if let Err(err) = pending.trigger_event(event, world, registry) {
        push_log(world, format!("event trigger error: {err}"));
    }
    world.insert_resource(pending);
}

fn drain_skill_events(world: &mut World) {
    let mut pending = world
        .remove_resource::<PendingSkillExecutions>()
        .unwrap_or_default();
    let events = pending.drain_emitted_events().collect::<Vec<_>>();
    world.insert_resource(pending);

    for event in events {
        let label = text_arg(&event.payload, "label").unwrap_or_else(|| event.name.clone());
        {
            let mut log = world.resource_mut::<CombatLog>();
            log.recent_events.push(format!("{}: {label}", event.name));
            if log.recent_events.len() > 16 {
                log.recent_events.remove(0);
            }
        }
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
    mut sprites: Query<(Entity, &mut Transform, Option<&mut Sprite>, &mut Lifetime)>,
) {
    for (entity, mut transform, sprite, mut lifetime) in &mut sprites {
        lifetime.timer.tick(time.delta());
        let left = lifetime.timer.fraction_remaining();
        if lifetime.timer.is_finished() {
            commands.entity(entity).despawn();
            continue;
        }
        if let Some(mut sprite) = sprite {
            sprite.color = sprite.color.with_alpha(left.clamp(0.0, 1.0));
            transform.scale = Vec3::splat(1.0 + (1.0 - left) * 0.7);
        }
    }
}

fn update_hud(
    skill_bar: Res<SkillBar>,
    stats: Res<PlayerStats>,
    pending: Res<PendingSkillExecutions>,
    enemies: Query<&Enemy>,
    log: Res<CombatLog>,
    mut hud: Single<&mut Text, With<HudText>>,
) {
    let mut text = String::from("WASD move | Mouse aim | 1-4 cast\n");
    text.push_str(&format!(
        "Shield: {:>4.0} | Enemies: {} | Pending runtime nodes: {}\n\n",
        stats.shield,
        enemies.iter().count(),
        pending.len()
    ));
    for (index, slot) in skill_bar.slots.iter().enumerate() {
        let state = if slot.remaining > 0.0 {
            format!("{:.1}s", slot.remaining)
        } else {
            "ready".to_owned()
        };
        text.push_str(&format!("{}  {:<20} {}\n", index + 1, slot.id, state));
    }
    text.push('\n');
    for line in log.lines.iter().rev().take(6) {
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

#[derive(Clone, Debug, Default)]
struct SpawnGameplayProjectile;

impl SkillAction for SpawnGameplayProjectile {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
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
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct SpawnZoneAction;

impl SkillAction for SpawnZoneAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
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
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct AreaDamageAction;

impl SkillAction for AreaDamageAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let fallback = world.resource::<CastIntent>().target;
        let center = Vec2::new(
            number_arg(args, "x", fallback.x),
            number_arg(args, "y", fallback.y),
        );
        let radius = number_arg(args, "radius", 80.0);
        let amount = number_arg(args, "amount", 20.0);
        let kind = text_arg(args, "kind").unwrap_or_else(|| "hit".to_owned());
        let mut dead = Vec::new();
        let mut hit_count = 0;

        let mut enemy_query = world.query::<(Entity, &mut Enemy, &Transform)>();
        for (entity, mut enemy, transform) in enemy_query.iter_mut(world) {
            if transform.translation.xy().distance_squared(center) <= radius.powi(2) {
                enemy.hp -= amount;
                hit_count += 1;
                if enemy.hp <= 0.0 {
                    dead.push(entity);
                }
            }
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
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct CombatLogAction;

impl SkillAction for CombatLogAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let message = text_arg(args, "message").unwrap_or_else(|| "skill event".to_owned());
        push_log(world, message);
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct MarkBlastAction;

impl SkillAction for MarkBlastAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
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
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
struct DetonateMarkedBlastAction;

impl SkillAction for DetonateMarkedBlastAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let Some(position) = world.resource_mut::<BlastQueue>().positions.pop() else {
            return Ok(());
        };
        let mut area_args = args.clone();
        area_args.insert("x".to_owned(), SkillValue::Number(position.x as f64));
        area_args.insert("y".to_owned(), SkillValue::Number(position.y as f64));
        area_args.insert("kind".to_owned(), SkillValue::String("blast".to_owned()));
        AreaDamageAction.execute(world, ctx, &area_args)
    }
}

#[derive(Clone, Debug, Default)]
struct HealOrShieldAction;

impl SkillAction for HealOrShieldAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn execute(&self, world: &mut World, _ctx: &mut SkillContext, args: &SkillArgs) -> SkillResult {
        let amount = number_arg(args, "amount", 5.0);
        let mode = text_arg(args, "mode").unwrap_or_else(|| "shield".to_owned());
        let mut stats = world.resource_mut::<PlayerStats>();
        stats.shield = (stats.shield + amount).min(100.0);
        push_log(world, format!("{mode} +{amount:.0}"));
        Ok(())
    }
}

fn spawn_flash(world: &mut World, position: Vec2, color: Color, size: f32) {
    world.spawn((
        Sprite::from_color(color, Vec2::splat(size)),
        Transform::from_xyz(position.x, position.y, 20.0),
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

fn event_coord(event: &SkillRuntimeEvent) -> Vec2 {
    Vec2::new(
        number_arg(&event.payload, "x", 0.0),
        number_arg(&event.payload, "y", 0.0),
    )
}

fn seed_default_skills(skills_dir: &Path) -> std::io::Result<()> {
    fs::create_dir_all(skills_dir)?;
    let has_skill = fs::read_dir(skills_dir)?
        .filter_map(Result::ok)
        .any(|entry| is_skill_file(&entry.path()));
    if has_skill {
        return Ok(());
    }
    for (name, source) in DEFAULT_SKILLS {
        fs::write(skills_dir.join(name), source)?;
    }
    Ok(())
}

const DEFAULT_SKILLS: &[(&str, &str)] = &[
    (
        "split_fireball.skill.ron",
        r#"Skill(
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
)
"#,
    ),
    (
        "delayed_blast.skill.ron",
        r#"Skill(
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
)
"#,
    ),
    (
        "arc_trap.skill.ron",
        r#"Skill(
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
)
"#,
    ),
    (
        "burst_shot.skill.ron",
        r#"Skill(
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
)
"#,
    ),
];
