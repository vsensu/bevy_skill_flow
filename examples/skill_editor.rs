use bevy::camera::{RenderTarget, ScalingMode, visibility::RenderLayers};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, TextureDescriptor, TextureDimension, TextureFormat, TextureUsages,
};
use bevy::window::WindowResolution;
use bevy_egui::{
    EguiContexts, EguiGlobalSettings, EguiPlugin, EguiPrimaryContextPass, EguiTextureHandle,
    EguiUserTextures, PrimaryEguiContext, egui,
};
use bevy_skill_flow::editor::{
    SkillEditorConfig, SkillEditorPlugin, SkillEditorState, is_skill_file,
};
use bevy_skill_flow::{
    ActiveSkill, SkillAction, SkillActionOutput, SkillArgs, SkillCastRequest, SkillContext,
    SkillDef, SkillDslPlugin, SkillError, SkillExpr, SkillId, SkillIntent, SkillLibrary, SkillNode,
    SkillRegistry, SkillResult, SkillRuntimeSignal, SkillSpecialValue, SkillValue, StatModifier,
    StatOp,
};
use indexmap::IndexSet;
use std::fs;
use std::path::Path;

const ARENA_HALF: Vec2 = Vec2::new(430.0, 280.0);
const PREVIEW_WORLD_SIZE: Vec2 = Vec2::new(980.0, 680.0);
const PREVIEW_TEXTURE_SIZE: Extent3d = Extent3d {
    width: 980,
    height: 680,
    depth_or_array_layers: 1,
};
const PLAYER_SPEED: f32 = 280.0;
const ENEMY_SPEED: f32 = 70.0;
const MAX_ENEMIES: usize = 10;

fn main() {
    App::new()
        .add_plugins((
            DefaultPlugins.set(WindowPlugin {
                primary_window: Some(Window {
                    title: "bevy_skill_flow - skill editor".to_owned(),
                    resolution: WindowResolution::new(1380, 820),
                    ..default()
                }),
                ..default()
            }),
            EguiPlugin::default(),
            SkillDslPlugin,
            SkillEditorPlugin::new(SkillEditorConfig {
                skills_dir: Path::new("examples/assets/skills").to_path_buf(),
                ..default()
            }),
        ))
        .init_resource::<AimWorld>()
        .init_resource::<PreviewArea>()
        .init_resource::<CombatLog>()
        .init_resource::<BlastQueue>()
        .init_resource::<EditorCastRequest>()
        .init_resource::<EditorUiState>()
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
struct HudText;

#[derive(Component)]
struct PreviewCamera;

#[derive(Resource, Deref)]
struct PreviewImage(Handle<Image>);

#[derive(Resource, Default)]
struct AimWorld(Vec2);

#[derive(Resource, Default)]
struct PreviewArea {
    logical_rect: Option<Rect>,
}

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
struct EditorUiState {
    pending_dirty_action: Option<EditorAction>,
}

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

fn setup_scene(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut egui_user_textures: ResMut<EguiUserTextures>,
    mut egui_global_settings: ResMut<EguiGlobalSettings>,
) {
    egui_global_settings.auto_create_primary_context = false;

    let mut preview_image = Image {
        texture_descriptor: TextureDescriptor {
            label: Some("skill_editor_preview"),
            size: PREVIEW_TEXTURE_SIZE,
            dimension: TextureDimension::D2,
            format: TextureFormat::Bgra8UnormSrgb,
            mip_level_count: 1,
            sample_count: 1,
            usage: TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        },
        ..default()
    };
    preview_image.resize(PREVIEW_TEXTURE_SIZE);
    let preview_image = images.add(preview_image);
    egui_user_textures.add_image(EguiTextureHandle::Strong(preview_image.clone()));
    commands.insert_resource(PreviewImage(preview_image.clone()));

    commands.spawn((
        Camera2d,
        Camera {
            order: -1,
            clear_color: ClearColorConfig::Custom(Color::srgb(0.10, 0.12, 0.13)),
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed {
                width: PREVIEW_WORLD_SIZE.x,
                height: PREVIEW_WORLD_SIZE.y,
            },
            ..OrthographicProjection::default_2d()
        }),
        RenderTarget::Image(preview_image.into()),
        PreviewCamera,
    ));
    commands.spawn((
        PrimaryEguiContext,
        Camera2d,
        Camera {
            order: 10,
            clear_color: ClearColorConfig::Custom(Color::srgb(0.08, 0.085, 0.09)),
            ..default()
        },
        RenderLayers::layer(31),
    ));
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
    mut editor_ui_state: ResMut<EditorUiState>,
    mut skill_bar: ResMut<SkillBar>,
    mut preview_area: ResMut<PreviewArea>,
    active_skills: Query<&ActiveSkill>,
    log: Res<CombatLog>,
    preview_image: Res<PreviewImage>,
) -> Result {
    let mut action = EditorAction::None;
    let selected = editor.current_file.clone();
    let mut pending_def_update: Option<SkillDef> = None;
    let mut pending_source_update: Option<String> = None;

    egui::SidePanel::right("skill_inspector")
        .resizable(true)
        .default_width(280.0)
        .show(contexts.ctx_mut()?, |ui| {
            ui.heading("Inspector");
            if let Some(path) = editor.current_file.as_ref() {
                ui.label(path.display().to_string());
            }
            if let Some(err) = editor.last_io_error.as_ref() {
                ui.colored_label(egui::Color32::from_rgb(238, 112, 92), err);
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
            if editor.preview_is_stale()
                && let Some(id) = editor.selected_preview_skill_id()
            {
                ui.colored_label(
                    egui::Color32::from_rgb(238, 180, 80),
                    format!("Previewing last compiled `{id}`"),
                );
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
            status_row(ui, "Pending", &active_skills.iter().count().to_string());
            ui.separator();
            ui.label("Log");
            for line in log.lines.iter().rev().take(6) {
                ui.monospace(line);
            }
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
                let cast_enabled = editor.selected_compiled().is_some();
                let cast_response =
                    ui.add_enabled(cast_enabled, egui::Button::new("Cast Selected"));
                if cast_response.clicked()
                    && let Some(id) = editor.selected_preview_skill_id()
                {
                    cast_request.0 = Some(id);
                }
                if !cast_enabled {
                    ui.colored_label(
                        egui::Color32::from_rgb(238, 112, 92),
                        cast_disabled_reason(&editor),
                    );
                } else if editor.preview_is_stale() {
                    ui.colored_label(
                        egui::Color32::from_rgb(238, 180, 80),
                        "current source failed; casting last compiled",
                    );
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

    preview_area.logical_rect = None;
    let preview_texture_id = contexts.image_id(&**preview_image);
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(contexts.ctx_mut()?, |ui| {
            let available = ui.available_rect_before_wrap();
            let gap = 10.0;
            let editor_width = if available.width() >= 760.0 {
                (available.width() * 0.32)
                    .clamp(340.0, 460.0)
                    .min(available.width() - gap - 360.0)
            } else {
                (available.width() * 0.42).clamp(280.0, 340.0)
            };
            let editor_rect = egui::Rect::from_min_size(
                available.min,
                egui::vec2(editor_width, available.height()),
            );
            let preview_bounds = egui::Rect::from_min_max(
                egui::pos2(editor_rect.max.x + gap, available.min.y),
                available.max,
            );
            let preview_rect =
                fit_rect_to_aspect(preview_bounds, PREVIEW_WORLD_SIZE.x / PREVIEW_WORLD_SIZE.y);

            ui.scope_builder(egui::UiBuilder::new().max_rect(editor_rect), |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(29, 32, 34))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_min_size(editor_rect.shrink(8.0).size());
                        ui.heading("Skills");
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("New").clicked() {
                                request_editor_action(
                                    EditorAction::New,
                                    editor.dirty,
                                    &mut editor_ui_state,
                                    &mut action,
                                );
                            }
                            if ui.button("Save").clicked() {
                                action = EditorAction::Save;
                            }
                            if ui.button("Reload").clicked() {
                                request_editor_action(
                                    EditorAction::Reload,
                                    editor.dirty,
                                    &mut editor_ui_state,
                                    &mut action,
                                );
                            }
                            if ui.button("Delete").clicked() {
                                request_editor_action(
                                    EditorAction::Delete,
                                    editor.dirty,
                                    &mut editor_ui_state,
                                    &mut action,
                                );
                            }
                        });
                        ui.separator();
                        let files_height = (ui.available_height() * 0.23).clamp(96.0, 170.0);
                        egui::ScrollArea::vertical()
                            .id_salt("skill_file_list_scroll")
                            .max_height(files_height)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
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
                                        request_editor_action(
                                            EditorAction::Open(file.path.clone()),
                                            editor.dirty && !is_selected,
                                            &mut editor_ui_state,
                                            &mut action,
                                        );
                                    }
                                }
                            });
                        ui.separator();
                        egui::ScrollArea::vertical()
                            .id_salt("skill_structured_editor_scroll")
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                if let Some(mut def) = editor.current_def.clone() {
                                    if edit_skill_def_ui(ui, &mut def) {
                                        pending_def_update = Some(def);
                                    }
                                } else {
                                    ui.colored_label(
                                        egui::Color32::from_rgb(238, 112, 92),
                                        "Source must parse before structured editing is available.",
                                    );
                                }
                                ui.separator();
                                egui::CollapsingHeader::new("Source")
                                    .id_salt("skill_source_header")
                                    .show(ui, |ui| {
                                        let mut source = editor.source.clone();
                                        let response = ui.add_sized(
                                            egui::vec2(ui.available_width(), 260.0),
                                            egui::TextEdit::multiline(&mut source)
                                                .id_salt("skill_source_editor")
                                                .font(egui::TextStyle::Monospace)
                                                .desired_width(f32::INFINITY)
                                                .lock_focus(true),
                                        );
                                        if response.changed() {
                                            pending_source_update = Some(source);
                                        }
                                    });
                            });
                    });
            });

            let response = if let Some(texture_id) = preview_texture_id {
                ui.put(
                    preview_rect,
                    egui::Image::new(egui::load::SizedTexture::new(
                        texture_id,
                        preview_rect.size(),
                    ))
                    .sense(egui::Sense::hover()),
                )
            } else {
                ui.allocate_rect(preview_rect, egui::Sense::hover())
            };
            let rect = response.rect;
            preview_area.logical_rect = Some(Rect {
                min: Vec2::new(rect.min.x, rect.min.y),
                max: Vec2::new(rect.max.x, rect.max.y),
            });
            ui.painter().rect_stroke(
                rect,
                6,
                egui::Stroke::new(1.0, egui::Color32::from_rgb(78, 92, 96)),
                egui::StrokeKind::Inside,
            );
            egui::Area::new("preview_label".into())
                .fixed_pos(rect.min + egui::vec2(10.0, 10.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::new()
                        .fill(egui::Color32::from_black_alpha(150))
                        .corner_radius(6)
                        .inner_margin(egui::Margin::symmetric(8, 4))
                        .show(ui, |ui| {
                            ui.label("Preview");
                        });
                });
            if editor.selected_compiled().is_none() {
                egui::Area::new("preview_error".into())
                    .fixed_pos(rect.center() - egui::vec2(130.0, 20.0))
                    .show(ui.ctx(), |ui| {
                        egui::Frame::new()
                            .fill(egui::Color32::from_black_alpha(180))
                            .corner_radius(6)
                            .inner_margin(egui::Margin::symmetric(10, 6))
                            .show(ui, |ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(238, 112, 92),
                                    cast_disabled_reason(&editor),
                                );
                            });
                    });
            }
        });

    if let Some(source) = pending_source_update {
        editor.edit_source(source, &registry, &mut library);
        if config.autosave_on_compile_success
            && editor.diagnostics.parse_error.is_none()
            && editor.diagnostics.compile_error.is_none()
        {
            let _ = editor.save_current_with_library(&registry, &mut library);
        }
        refresh_skill_bar(&mut skill_bar, &editor);
    } else if let Some(def) = pending_def_update {
        if let Err(err) = editor.edit_def(def, &registry, &mut library) {
            editor.last_io_error = Some(err.to_string());
        } else if config.autosave_on_compile_success
            && editor.diagnostics.parse_error.is_none()
            && editor.diagnostics.compile_error.is_none()
        {
            let _ = editor.save_current_with_library(&registry, &mut library);
        }
        refresh_skill_bar(&mut skill_bar, &editor);
    }

    show_dirty_confirmation(contexts.ctx_mut()?, &mut editor_ui_state, &mut action);

    match action {
        EditorAction::None => {}
        EditorAction::Open(path) => {
            if let Err(err) = editor.open_file(path, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            } else {
                editor.last_io_error = None;
            }
        }
        EditorAction::New => {
            if let Err(err) = editor.create_new(&config, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            } else {
                editor.last_io_error = None;
            }
        }
        EditorAction::Save => {
            if let Err(err) = editor.save_current_with_library(&registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            } else {
                editor.last_io_error = None;
            }
        }
        EditorAction::Reload => {
            if let Err(err) = editor.load_dir(&config, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            } else {
                editor.last_io_error = None;
            }
        }
        EditorAction::Delete => {
            if let Err(err) = editor.delete_current(&config, &registry, &mut library) {
                editor.last_io_error = Some(err.to_string());
            } else {
                editor.last_io_error = None;
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

fn request_editor_action(
    requested: EditorAction,
    dirty: bool,
    ui_state: &mut EditorUiState,
    action: &mut EditorAction,
) {
    if dirty && requested_discards_source(&requested) {
        ui_state.pending_dirty_action = Some(requested);
    } else {
        *action = requested;
    }
}

fn requested_discards_source(action: &EditorAction) -> bool {
    matches!(
        action,
        EditorAction::Open(_) | EditorAction::New | EditorAction::Reload | EditorAction::Delete
    )
}

fn show_dirty_confirmation(
    ctx: &egui::Context,
    ui_state: &mut EditorUiState,
    action: &mut EditorAction,
) {
    if ui_state.pending_dirty_action.is_none() {
        return;
    }
    egui::Window::new("Unsaved changes")
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label("Discard the current unsaved skill edits?");
            ui.horizontal(|ui| {
                if ui.button("Discard").clicked() {
                    if let Some(pending) = ui_state.pending_dirty_action.take() {
                        *action = pending;
                    }
                }
                if ui.button("Cancel").clicked() {
                    ui_state.pending_dirty_action = None;
                }
            });
        });
}

fn edit_skill_def_ui(ui: &mut egui::Ui, def: &mut SkillDef) -> bool {
    let mut changed = false;
    ui.label("Definition");
    ui.horizontal(|ui| {
        ui.label("Id");
        changed |= text_edit_singleline(ui, "skill_def_id", &mut def.id.0);
    });
    ui.horizontal(|ui| {
        ui.label("Cast");
        changed |= text_edit_singleline(ui, "skill_def_cast_model", &mut def.cast_model);
    });
    changed |= edit_string_list(ui, "Tags", &mut def.tags, "tag");
    changed |= edit_string_list(ui, "Modifiers", &mut def.modifiers, "modifier");
    changed |= edit_args_map(ui, "Params", &mut def.params, "params");
    ui.separator();
    ui.label("Body");
    changed |= edit_skill_node(ui, &mut def.body, "body");
    changed
}

fn edit_string_list(
    ui: &mut egui::Ui,
    label: &str,
    values: &mut Vec<String>,
    fallback: &str,
) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(label)
        .id_salt(format!("string_list_{label}"))
        .show(ui, |ui| {
            let mut remove = None;
            let mut move_op = None;
            for index in 0..values.len() {
                ui.horizontal(|ui| {
                    ui.label(index.to_string());
                    changed |= text_edit_singleline(
                        ui,
                        format!("{label}_{index}_string"),
                        &mut values[index],
                    );
                    if ui.button("Up").clicked() && index > 0 {
                        move_op = Some((index, index - 1));
                    }
                    if ui.button("Down").clicked() && index + 1 < values.len() {
                        move_op = Some((index, index + 1));
                    }
                    if ui.button("Del").clicked() {
                        remove = Some(index);
                    }
                });
            }
            if let Some((from, to)) = move_op {
                values.swap(from, to);
                changed = true;
            }
            if let Some(index) = remove {
                values.remove(index);
                changed = true;
            }
            if ui.button(format!("Add {fallback}")).clicked() {
                values.push(make_unique_name(
                    fallback,
                    values.iter().map(String::as_str),
                ));
                changed = true;
            }
            let original_len = values.len();
            values.retain(|value| !value.trim().is_empty());
            changed |= values.len() != original_len;
        });
    changed
}

fn edit_args_map(ui: &mut egui::Ui, label: &str, args: &mut SkillArgs, id: &str) -> bool {
    let mut changed = false;
    egui::CollapsingHeader::new(label)
        .id_salt(format!("{id}_args_header"))
        .show(ui, |ui| {
            let mut rows = Vec::new();
            let mut seen = IndexSet::new();
            let entries = args
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<Vec<_>>();

            for (index, (old_key, old_value)) in entries.into_iter().enumerate() {
                let mut key = old_key.clone();
                let mut value = old_value.clone();
                let mut remove = false;
                ui.horizontal(|ui| {
                    ui.label("Key");
                    changed |= text_edit_singleline(ui, format!("{id}_{index}_key"), &mut key);
                    if ui.button("Del").clicked() {
                        remove = true;
                        changed = true;
                    }
                });
                if key.trim().is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(238, 112, 92),
                        "Empty keys are ignored until renamed.",
                    );
                    changed = true;
                    continue;
                }
                if !remove {
                    ui.indent(format!("{id}_{index}_value"), |ui| {
                        changed |= edit_skill_value(ui, &mut value, &format!("{id}_{index}"));
                    });
                    if seen.insert(key.clone()) {
                        rows.push((key, value));
                    } else {
                        ui.colored_label(
                            egui::Color32::from_rgb(238, 180, 80),
                            "Duplicate key ignored.",
                        );
                        changed = true;
                    }
                }
            }

            if ui.button("Add field").clicked() {
                rows.push((
                    make_unique_name("field", args.keys().map(String::as_str)),
                    SkillValue::Number(0.0),
                ));
                changed = true;
            }

            if changed {
                args.clear();
                for (key, value) in rows {
                    args.insert(key, value);
                }
            }
        });
    changed
}

fn edit_skill_value(ui: &mut egui::Ui, value: &mut SkillValue, id: &str) -> bool {
    let mut changed = false;
    let mut kind = value_kind(value);
    ui.horizontal(|ui| {
        ui.label("Type");
        egui::ComboBox::from_id_salt(format!("{id}_value_kind"))
            .selected_text(value_kind_label(kind))
            .show_ui(ui, |ui| {
                for candidate in VALUE_KINDS {
                    ui.selectable_value(&mut kind, candidate, value_kind_label(candidate));
                }
            });
    });
    if kind != value_kind(value) {
        *value = default_value(kind);
        changed = true;
    }

    match value {
        SkillValue::Special(SkillSpecialValue::Expr(expr))
        | SkillValue::Special(SkillSpecialValue::Ref(expr))
        | SkillValue::Special(SkillSpecialValue::Tag(expr))
        | SkillValue::Special(SkillSpecialValue::Stat(expr))
        | SkillValue::String(expr) => {
            changed |= text_edit_singleline(ui, format!("{id}_value_text"), expr);
        }
        SkillValue::Map(map) => {
            changed |= edit_args_map(ui, "Map", map, &format!("{id}_map"));
        }
        SkillValue::List(list) => {
            changed |= edit_value_list(ui, list, &format!("{id}_list"));
        }
        SkillValue::Number(number) => {
            changed |= ui.add(egui::DragValue::new(number).speed(0.1)).changed();
        }
        SkillValue::Bool(flag) => {
            changed |= ui.checkbox(flag, "Value").changed();
        }
        SkillValue::Node(node) => {
            changed |= edit_skill_node(ui, node, &format!("{id}_node"));
        }
        SkillValue::Null => {
            ui.label("null");
        }
    }
    changed
}

fn edit_value_list(ui: &mut egui::Ui, values: &mut Vec<SkillValue>, id: &str) -> bool {
    let mut changed = false;
    let mut remove = None;
    let mut move_op = None;
    for index in 0..values.len() {
        ui.horizontal(|ui| {
            ui.label(format!("Item {index}"));
            if ui.button("Up").clicked() && index > 0 {
                move_op = Some((index, index - 1));
            }
            if ui.button("Down").clicked() && index + 1 < values.len() {
                move_op = Some((index, index + 1));
            }
            if ui.button("Del").clicked() {
                remove = Some(index);
            }
        });
        ui.indent(format!("{id}_{index}"), |ui| {
            changed |= edit_skill_value(ui, &mut values[index], &format!("{id}_{index}"));
        });
    }
    if let Some((from, to)) = move_op {
        values.swap(from, to);
        changed = true;
    }
    if let Some(index) = remove {
        values.remove(index);
        changed = true;
    }
    if ui.button("Add item").clicked() {
        values.push(SkillValue::Number(0.0));
        changed = true;
    }
    changed
}

fn edit_skill_node(ui: &mut egui::Ui, node: &mut SkillNode, id: &str) -> bool {
    let mut changed = false;
    let mut kind = node_kind(node);
    ui.horizontal(|ui| {
        ui.label("Node");
        egui::ComboBox::from_id_salt(format!("{id}_node_kind"))
            .selected_text(node_kind_label(kind))
            .show_ui(ui, |ui| {
                for candidate in NODE_KINDS {
                    ui.selectable_value(&mut kind, candidate, node_kind_label(candidate));
                }
            });
    });
    if kind != node_kind(node) {
        *node = default_node(kind);
        changed = true;
    }

    match node {
        SkillNode::Sequence(nodes) => {
            changed |= edit_node_list(ui, "Sequence", nodes, &format!("{id}_sequence"));
        }
        SkillNode::Parallel(nodes) => {
            changed |= edit_node_list(ui, "Parallel", nodes, &format!("{id}_parallel"));
        }
        SkillNode::Delay(expr, child) => {
            changed |= edit_expr(ui, "Seconds", expr, &format!("{id}_delay_seconds"));
            ui.indent(format!("{id}_delay_child"), |ui| {
                changed |= edit_skill_node(ui, child, &format!("{id}_delay_child"));
            });
        }
        SkillNode::Repeat {
            times,
            duration,
            interval,
            node,
        } => {
            changed |= edit_optional_expr(ui, "Times", times, "1", &format!("{id}_repeat_times"));
            changed |= edit_optional_expr(
                ui,
                "Duration",
                duration,
                "1.0",
                &format!("{id}_repeat_duration"),
            );
            changed |= edit_optional_expr(
                ui,
                "Interval",
                interval,
                "0.2",
                &format!("{id}_repeat_interval"),
            );
            ui.indent(format!("{id}_repeat_child"), |ui| {
                changed |= edit_skill_node(ui, node, &format!("{id}_repeat_child"));
            });
        }
        SkillNode::If {
            condition,
            then_node,
            else_node,
        } => {
            changed |= edit_expr(ui, "Condition", condition, &format!("{id}_if_condition"));
            ui.label("Then");
            ui.indent(format!("{id}_then"), |ui| {
                changed |= edit_skill_node(ui, then_node, &format!("{id}_then"));
            });
            let mut has_else = else_node.is_some();
            if ui.checkbox(&mut has_else, "Else").changed() {
                *else_node = if has_else {
                    Some(Box::new(default_node(SkillNodeKind::Action)))
                } else {
                    None
                };
                changed = true;
            }
            if let Some(else_child) = else_node {
                ui.indent(format!("{id}_else"), |ui| {
                    changed |= edit_skill_node(ui, else_child, &format!("{id}_else"));
                });
            }
        }
        SkillNode::Let(name, value, child) => {
            ui.horizontal(|ui| {
                ui.label("Name");
                changed |= text_edit_singleline(ui, format!("{id}_let_name"), name);
            });
            changed |= edit_skill_value(ui, value, &format!("{id}_let_value"));
            ui.indent(format!("{id}_let_child"), |ui| {
                changed |= edit_skill_node(ui, child, &format!("{id}_let_child"));
            });
        }
        SkillNode::On(event, child) => {
            ui.horizontal(|ui| {
                ui.label("Event");
                changed |= text_edit_singleline(ui, format!("{id}_on_event"), event);
            });
            ui.indent(format!("{id}_on_child"), |ui| {
                changed |= edit_skill_node(ui, child, &format!("{id}_on_child"));
            });
        }
        SkillNode::Emit(event, args) => {
            ui.horizontal(|ui| {
                ui.label("Event");
                changed |= text_edit_singleline(ui, format!("{id}_emit_event"), event);
            });
            changed |= edit_args_map(ui, "Payload", args, &format!("{id}_emit_args"));
        }
        SkillNode::Action(action, args) => {
            ui.horizontal(|ui| {
                ui.label("Action");
                changed |= text_edit_singleline(ui, format!("{id}_action_id"), action);
            });
            changed |= edit_args_map(ui, "Args", args, &format!("{id}_action_args"));
        }
        SkillNode::Deck(nodes) => {
            changed |= edit_node_list(ui, "Deck", nodes, &format!("{id}_deck"));
        }
        SkillNode::Spell(spell, args) => {
            ui.horizontal(|ui| {
                ui.label("Spell");
                changed |= text_edit_singleline(ui, format!("{id}_spell_id"), spell);
            });
            changed |= edit_args_map(ui, "Args", args, &format!("{id}_spell_args"));
        }
        SkillNode::Modifier(modifier, args) => {
            ui.horizontal(|ui| {
                ui.label("Modifier");
                changed |= text_edit_singleline(ui, format!("{id}_modifier_id"), modifier);
            });
            changed |= edit_args_map(ui, "Args", args, &format!("{id}_modifier_args"));
        }
    }
    changed
}

fn edit_node_list(ui: &mut egui::Ui, label: &str, nodes: &mut Vec<SkillNode>, id: &str) -> bool {
    let mut changed = false;
    ui.label(label);
    let mut remove = None;
    let mut move_op = None;
    for index in 0..nodes.len() {
        ui.horizontal(|ui| {
            ui.label(format!("Child {index}"));
            if ui.button("Up").clicked() && index > 0 {
                move_op = Some((index, index - 1));
            }
            if ui.button("Down").clicked() && index + 1 < nodes.len() {
                move_op = Some((index, index + 1));
            }
            if ui.button("Del").clicked() {
                remove = Some(index);
            }
        });
        ui.indent(format!("{id}_{index}"), |ui| {
            changed |= edit_skill_node(ui, &mut nodes[index], &format!("{id}_{index}"));
        });
    }
    if let Some((from, to)) = move_op {
        nodes.swap(from, to);
        changed = true;
    }
    if let Some(index) = remove {
        nodes.remove(index);
        changed = true;
    }
    if ui.button("Add child").clicked() {
        nodes.push(default_node(SkillNodeKind::Action));
        changed = true;
    }
    changed
}

fn edit_expr(ui: &mut egui::Ui, label: &str, expr: &mut SkillExpr, id: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.label(label);
        changed |= text_edit_singleline(ui, id, &mut expr.0);
    });
    changed
}

fn edit_optional_expr(
    ui: &mut egui::Ui,
    label: &str,
    expr: &mut Option<SkillExpr>,
    fallback: &str,
    id: &str,
) -> bool {
    let mut changed = false;
    let mut enabled = expr.is_some();
    if ui.checkbox(&mut enabled, label).changed() {
        *expr = if enabled {
            Some(SkillExpr::new(fallback))
        } else {
            None
        };
        changed = true;
    }
    if let Some(expr) = expr {
        changed |= edit_expr(ui, "Expr", expr, &format!("{id}_expr"));
    }
    changed
}

fn text_edit_singleline(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    value: &mut String,
) -> bool {
    ui.add(
        egui::TextEdit::singleline(value)
            .id_salt(id_salt)
            .desired_width(f32::INFINITY),
    )
    .changed()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillNodeKind {
    Sequence,
    Parallel,
    Delay,
    Repeat,
    If,
    Let,
    On,
    Emit,
    Action,
    Deck,
    Spell,
    Modifier,
}

const NODE_KINDS: [SkillNodeKind; 12] = [
    SkillNodeKind::Sequence,
    SkillNodeKind::Parallel,
    SkillNodeKind::Delay,
    SkillNodeKind::Repeat,
    SkillNodeKind::If,
    SkillNodeKind::Let,
    SkillNodeKind::On,
    SkillNodeKind::Emit,
    SkillNodeKind::Action,
    SkillNodeKind::Deck,
    SkillNodeKind::Spell,
    SkillNodeKind::Modifier,
];

fn node_kind(node: &SkillNode) -> SkillNodeKind {
    match node {
        SkillNode::Sequence(_) => SkillNodeKind::Sequence,
        SkillNode::Parallel(_) => SkillNodeKind::Parallel,
        SkillNode::Delay(_, _) => SkillNodeKind::Delay,
        SkillNode::Repeat { .. } => SkillNodeKind::Repeat,
        SkillNode::If { .. } => SkillNodeKind::If,
        SkillNode::Let(_, _, _) => SkillNodeKind::Let,
        SkillNode::On(_, _) => SkillNodeKind::On,
        SkillNode::Emit(_, _) => SkillNodeKind::Emit,
        SkillNode::Action(_, _) => SkillNodeKind::Action,
        SkillNode::Deck(_) => SkillNodeKind::Deck,
        SkillNode::Spell(_, _) => SkillNodeKind::Spell,
        SkillNode::Modifier(_, _) => SkillNodeKind::Modifier,
    }
}

fn node_kind_label(kind: SkillNodeKind) -> &'static str {
    match kind {
        SkillNodeKind::Sequence => "Sequence",
        SkillNodeKind::Parallel => "Parallel",
        SkillNodeKind::Delay => "Delay",
        SkillNodeKind::Repeat => "Repeat",
        SkillNodeKind::If => "If",
        SkillNodeKind::Let => "Let",
        SkillNodeKind::On => "On",
        SkillNodeKind::Emit => "Emit",
        SkillNodeKind::Action => "Action",
        SkillNodeKind::Deck => "Deck",
        SkillNodeKind::Spell => "Spell",
        SkillNodeKind::Modifier => "Modifier",
    }
}

fn default_node(kind: SkillNodeKind) -> SkillNode {
    match kind {
        SkillNodeKind::Sequence => SkillNode::Sequence(vec![default_node(SkillNodeKind::Action)]),
        SkillNodeKind::Parallel => SkillNode::Parallel(vec![default_node(SkillNodeKind::Action)]),
        SkillNodeKind::Delay => SkillNode::Delay(
            SkillExpr::new("0.25"),
            Box::new(default_node(SkillNodeKind::Action)),
        ),
        SkillNodeKind::Repeat => SkillNode::Repeat {
            times: Some(SkillExpr::new("3")),
            duration: None,
            interval: Some(SkillExpr::new("0.2")),
            node: Box::new(default_node(SkillNodeKind::Action)),
        },
        SkillNodeKind::If => SkillNode::If {
            condition: SkillExpr::new("true"),
            then_node: Box::new(default_node(SkillNodeKind::Action)),
            else_node: None,
        },
        SkillNodeKind::Let => SkillNode::Let(
            "value".to_owned(),
            SkillValue::Number(0.0),
            Box::new(default_node(SkillNodeKind::Action)),
        ),
        SkillNodeKind::On => SkillNode::On(
            "hit".to_owned(),
            Box::new(default_node(SkillNodeKind::Action)),
        ),
        SkillNodeKind::Emit => SkillNode::Emit("skill_event".to_owned(), SkillArgs::new()),
        SkillNodeKind::Action => SkillNode::Action("trace".to_owned(), SkillArgs::new()),
        SkillNodeKind::Deck => SkillNode::Deck(vec![default_node(SkillNodeKind::Spell)]),
        SkillNodeKind::Spell => SkillNode::Spell("spell".to_owned(), SkillArgs::new()),
        SkillNodeKind::Modifier => SkillNode::Modifier("modifier".to_owned(), SkillArgs::new()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SkillValueKind {
    Number,
    Bool,
    String,
    Null,
    Expr,
    Ref,
    Tag,
    Stat,
    Map,
    List,
    Node,
}

const VALUE_KINDS: [SkillValueKind; 11] = [
    SkillValueKind::Number,
    SkillValueKind::Bool,
    SkillValueKind::String,
    SkillValueKind::Null,
    SkillValueKind::Expr,
    SkillValueKind::Ref,
    SkillValueKind::Tag,
    SkillValueKind::Stat,
    SkillValueKind::Map,
    SkillValueKind::List,
    SkillValueKind::Node,
];

fn value_kind(value: &SkillValue) -> SkillValueKind {
    match value {
        SkillValue::Special(SkillSpecialValue::Expr(_)) => SkillValueKind::Expr,
        SkillValue::Special(SkillSpecialValue::Ref(_)) => SkillValueKind::Ref,
        SkillValue::Special(SkillSpecialValue::Tag(_)) => SkillValueKind::Tag,
        SkillValue::Special(SkillSpecialValue::Stat(_)) => SkillValueKind::Stat,
        SkillValue::Map(_) => SkillValueKind::Map,
        SkillValue::List(_) => SkillValueKind::List,
        SkillValue::Number(_) => SkillValueKind::Number,
        SkillValue::Bool(_) => SkillValueKind::Bool,
        SkillValue::String(_) => SkillValueKind::String,
        SkillValue::Node(_) => SkillValueKind::Node,
        SkillValue::Null => SkillValueKind::Null,
    }
}

fn value_kind_label(kind: SkillValueKind) -> &'static str {
    match kind {
        SkillValueKind::Number => "Number",
        SkillValueKind::Bool => "Bool",
        SkillValueKind::String => "String",
        SkillValueKind::Null => "Null",
        SkillValueKind::Expr => "Expr",
        SkillValueKind::Ref => "Ref",
        SkillValueKind::Tag => "Tag",
        SkillValueKind::Stat => "Stat",
        SkillValueKind::Map => "Map",
        SkillValueKind::List => "List",
        SkillValueKind::Node => "Node",
    }
}

fn default_value(kind: SkillValueKind) -> SkillValue {
    match kind {
        SkillValueKind::Number => SkillValue::Number(0.0),
        SkillValueKind::Bool => SkillValue::Bool(false),
        SkillValueKind::String => SkillValue::String(String::new()),
        SkillValueKind::Null => SkillValue::Null,
        SkillValueKind::Expr => SkillValue::Special(SkillSpecialValue::Expr("stat.damage".into())),
        SkillValueKind::Ref => SkillValue::Special(SkillSpecialValue::Ref("target".into())),
        SkillValueKind::Tag => SkillValue::Special(SkillSpecialValue::Tag("spell".into())),
        SkillValueKind::Stat => SkillValue::Special(SkillSpecialValue::Stat("damage".into())),
        SkillValueKind::Map => SkillValue::Map(SkillArgs::new()),
        SkillValueKind::List => SkillValue::List(Vec::new()),
        SkillValueKind::Node => SkillValue::Node(Box::new(default_node(SkillNodeKind::Action))),
    }
}

fn make_unique_name<'a>(prefix: &str, existing: impl IntoIterator<Item = &'a str>) -> String {
    let existing = existing.into_iter().collect::<IndexSet<_>>();
    if !existing.contains(prefix) {
        return prefix.to_owned();
    }
    for index in 2.. {
        let candidate = format!("{prefix}_{index}");
        if !existing.contains(candidate.as_str()) {
            return candidate;
        }
    }
    unreachable!("unbounded suffix search should always find a free name")
}

fn status_row(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.monospace(value);
    });
}

fn cast_disabled_reason(editor: &SkillEditorState) -> &'static str {
    if editor.diagnostics.parse_error.is_some() {
        "parse failed; no compiled preview"
    } else if editor.diagnostics.compile_error.is_some() {
        "compile failed; no previous compiled version"
    } else {
        "no compiled skill selected"
    }
}

fn fit_rect_to_aspect(bounds: egui::Rect, aspect: f32) -> egui::Rect {
    let width = bounds.width().max(1.0);
    let height = bounds.height().max(1.0);
    let bounds_aspect = width / height;
    let size = if bounds_aspect > aspect {
        egui::vec2(height * aspect, height)
    } else {
        egui::vec2(width, width / aspect)
    };
    egui::Rect::from_center_size(bounds.center(), size)
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

fn update_aim(mut aim: ResMut<AimWorld>, preview_area: Res<PreviewArea>, window: Single<&Window>) {
    if let Some(cursor_position) = window.cursor_position()
        && let Some(rect) = preview_area.logical_rect
        && rect.contains(cursor_position)
    {
        let uv = (cursor_position - rect.min) / rect.size();
        aim.0 = Vec2::new(
            (uv.x - 0.5) * PREVIEW_WORLD_SIZE.x,
            (0.5 - uv.y) * PREVIEW_WORLD_SIZE.y,
        );
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

    if world.resource::<SkillLibrary>().get(&skill_id).is_none() {
        push_log(world, format!("missing compiled skill `{skill_id}`"));
        return;
    }
    world.write_message(SkillCastRequest {
        skill: skill_id.clone(),
        caster,
        target: None,
    });
    let mut skill_bar = world.resource_mut::<SkillBar>();
    if let Some(slot) = skill_bar.slots.iter_mut().find(|slot| slot.id == skill_id) {
        slot.remaining = slot.cooldown;
    }
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
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "spawn_projectile", args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct SpawnZoneAction;

impl SkillAction for SpawnZoneAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "spawn_zone", args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct AreaDamageAction;

impl SkillAction for AreaDamageAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "area_damage", args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct CombatLogAction;

impl SkillAction for CombatLogAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "combat_log", args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct MarkBlastAction;

impl SkillAction for MarkBlastAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "mark_blast", args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct DetonateMarkedBlastAction;

impl SkillAction for DetonateMarkedBlastAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "detonate_marked_blast", args.clone())
    }
}

#[derive(Clone, Debug, Default)]
struct HealOrShieldAction;

impl SkillAction for HealOrShieldAction {
    fn validate(&self, _args: &SkillArgs, _registry: &SkillRegistry) -> Result<(), SkillError> {
        Ok(())
    }

    fn emit(
        &self,
        ctx: &SkillContext,
        args: &SkillArgs,
        out: &mut SkillActionOutput,
    ) -> SkillResult {
        out.emit_intent(ctx, "heal_or_shield", args.clone())
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

fn event_coord(event: &SkillRuntimeSignal) -> Vec2 {
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
