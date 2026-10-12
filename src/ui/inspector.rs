//! Right panel: properties of the selected clip, or project settings.

use crate::app::EditorApp;
use crate::model::{AudioRole, Clip, Denoise, Ease, FilterPreset, Id, MediaKind, NoiseLevel, Prop, TrackKind, Transform, Transition, TransitionKind, KEY_TOLERANCE};
use crate::render::fill_scale;
use crate::ui::{theme, widgets};
use egui::{
    vec2, Button, CollapsingHeader, Color32, Context, DragValue, Frame, Grid, Margin, Response, RichText, ScrollArea,
    Sense, Slider, Stroke, TextEdit, Ui,
};

pub fn show(ctx: &Context, app: &mut EditorApp) {
    egui::SidePanel::right("inspector")
        .default_width(320.0)
        .width_range(280.0..=480.0)
        .resizable(true)
        .frame(Frame::none().fill(theme::PANEL).inner_margin(Margin::same(12.0)))
        .show(ctx, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let clip = app.selection.and_then(|id| app.project.clip(id).cloned());
                match clip {
                    Some(c) => clip_panel(ui, app, c),
                    None => project_panel(ui, app),
                }
            });
        });
}

enum Action {
    Split,
    Duplicate,
    Delete,
    ExtractAudio,
    Fit,
    Fill,
    Seek(f64),
    SetTransition(Id, Option<Transition>),
    Denoise(NoiseLevel),
}

fn percent(v: f64, _: std::ops::RangeInclusive<usize>) -> String {
    format!("{:.0}%", v * 100.0)
}

fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    CollapsingHeader::new(RichText::new(title).strong())
        .default_open(true)
        .show(ui, |ui| {
            ui.add_space(2.0);
            add(ui);
            ui.add_space(4.0);
        });
}

/// The ◆ keyframe toggle for `props` at timeline time `t`.
fn key_toggle(ui: &mut Ui, c: &mut Clip, props: &[Prop], t: f64) {
    let local = t - c.start;
    let in_clip = (-1e-6..=c.duration + 1e-6).contains(&local);
    let has_key = props.iter().any(|p| c.key_index(*p, local).is_some());
    let animated = props.iter().any(|p| c.is_animated(*p));
    let (rect, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
    let base = if has_key || animated { theme::ACCENT } else { theme::TEXT_DIM };
    let stroke = if resp.hovered() && in_clip { theme::TEXT } else { base };
    let fill = if has_key { theme::ACCENT } else { Color32::TRANSPARENT };
    let stroke = if in_clip { stroke } else { stroke.gamma_multiply(0.4) };
    ui.painter().add(crate::ui::timeline::diamond(rect.center(), 5.5, fill, Stroke::new(1.5_f32, stroke)));
    let tip = if !in_clip {
        "Move the playhead over this clip to add keyframes"
    } else if has_key {
        "Remove keyframe at playhead"
    } else {
        "Add keyframe at playhead"
    };
    if resp.on_hover_text(tip).clicked() && in_clip {
        let local = local.clamp(0.0, c.duration);
        for p in props {
            if has_key {
                c.remove_key(*p, local);
            } else {
                let v = c.value_at(*p, t);
                c.set_key(*p, local, v);
            }
        }
    }
}

/// A keyframe toggle plus a value widget showing the property at the playhead.
fn prop_row(ui: &mut Ui, c: &mut Clip, p: Prop, t: f64, add: impl FnOnce(&mut Ui, &mut f32) -> Response) {
    ui.horizontal(|ui| {
        key_toggle(ui, c, &[p], t);
        let mut v = c.value_at(p, t);
        if add(ui, &mut v).changed() {
            c.set_value_at(p, t, v);
        }
    });
}

/// Picker for one cut's transition. Returns true when changed.
fn transition_editor(ui: &mut Ui, label: &str, value: &mut Option<Transition>, effective: Option<f64>, id: &str) -> bool {
    let mut changed = false;
    ui.label(RichText::new(label).strong());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text(value.map_or("None", |t| t.kind.label()))
            .width(150.0)
            .show_ui(ui, |ui| {
                if ui.selectable_label(value.is_none(), "None").clicked() {
                    *value = None;
                    changed = true;
                }
                for k in TransitionKind::ALL {
                    if ui.selectable_label(value.is_some_and(|t| t.kind == k), k.label()).clicked() {
                        *value = Some(Transition { kind: k, duration: value.map_or(0.5, |t| t.duration) });
                        changed = true;
                    }
                }
            });
        if let Some(t) = value {
            let mut d = t.duration;
            if ui.add(Slider::new(&mut d, 0.1..=3.0).suffix(" s").fixed_decimals(1)).changed() {
                t.duration = d;
                changed = true;
            }
        }
    });
    if let (Some(t), Some(e)) = (value.as_ref(), effective) {
        if e + 1e-6 < t.duration {
            ui.label(widgets::dim(format!("Shortened to {e:.2} s to fit the clips")).small());
        }
    }
    changed
}

fn color_toggle(ui: &mut Ui, value: &mut Option<[u8; 4]>, default: [u8; 4]) {
    ui.horizontal(|ui| {
        let mut on = value.is_some();
        if ui.checkbox(&mut on, "").changed() {
            *value = on.then_some(default);
        }
        if let Some(c) = value {
            ui.color_edit_button_srgba_unmultiplied(c);
        }
    });
}

fn clip_panel(ui: &mut Ui, app: &mut EditorApp, clip: Clip) {
    let media = clip.media_id().and_then(|m| app.project.media(m).cloned());
    let kind = app.project.track_kind_for(&clip);
    let is_text = clip.text().is_some();
    let is_visual = kind != TrackKind::Audio;
    let has_sound = media.as_ref().is_some_and(|m| m.has_sound());
    let timed = media.as_ref().is_some_and(|m| m.kind != MediaKind::Image);
    let mut c = clip.clone();
    let mut action = None;
    let t = app.playhead;
    let auto_role = {
        let mut probe = clip.clone();
        probe.audio_role = AudioRole::Auto;
        app.project.audio_role(&probe)
    };
    let measuring_noise = app.smart.measuring(clip.id);

    // Header
    let (icon, title) = match (&media, is_text) {
        (_, true) => ("T", "Text".to_string()),
        (Some(m), _) if kind == TrackKind::Audio => ("🎵", m.name.clone()),
        (Some(m), _) if m.kind == MediaKind::Image => ("🖼", m.name.clone()),
        (Some(m), _) => ("🎬", m.name.clone()),
        (None, _) => ("?", "Clip".into()),
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).size(18.0).color(theme::ACCENT));
        ui.add(egui::Label::new(RichText::new(title).strong().size(15.0)).truncate());
    });
    ui.label(widgets::dim(format!(
        "Start {}   ·   Length {}",
        widgets::format_time(c.start),
        widgets::format_time(c.duration)
    )));
    ui.add_space(6.0);

    if let Some(style) = c.text_mut() {
        section(ui, "Text", |ui| {
            ui.add(TextEdit::multiline(&mut style.content).desired_rows(3).desired_width(f32::INFINITY));
            ui.add_space(4.0);
            Grid::new("text-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Size");
                ui.add(Slider::new(&mut style.font_size, 12.0..=300.0).fixed_decimals(0));
                ui.end_row();
                ui.label("Bold");
                ui.checkbox(&mut style.bold, "");
                ui.end_row();
                ui.label("Color");
                ui.color_edit_button_srgba_unmultiplied(&mut style.color);
                ui.end_row();
                ui.label("Background");
                color_toggle(ui, &mut style.background, [0, 0, 0, 170]);
                ui.end_row();
                ui.label("Outline");
                color_toggle(ui, &mut style.outline, [0, 0, 0, 255]);
                ui.end_row();
            });
        });
    }

    if is_visual {
        section(ui, "Transform", |ui| {
            Grid::new("xf-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Position");
                ui.horizontal(|ui| {
                    key_toggle(ui, &mut c, &[Prop::X, Prop::Y], t);
                    let (mut x, mut y) = (c.value_at(Prop::X, t), c.value_at(Prop::Y, t));
                    if ui.add(DragValue::new(&mut x).speed(0.002).range(-2.0..=2.0).prefix("X ").fixed_decimals(3)).changed() {
                        c.set_value_at(Prop::X, t, x);
                    }
                    if ui.add(DragValue::new(&mut y).speed(0.002).range(-2.0..=2.0).prefix("Y ").fixed_decimals(3)).changed() {
                        c.set_value_at(Prop::Y, t, y);
                    }
                });
                ui.end_row();
                ui.label("Scale");
                prop_row(ui, &mut c, Prop::Scale, t, |ui, v| {
                    ui.add(Slider::new(v, 0.05..=5.0).logarithmic(true).custom_formatter(percent))
                });
                ui.end_row();
                if !is_text {
                    ui.label("Rotation");
                    prop_row(ui, &mut c, Prop::Rotation, t, |ui, v| {
                        ui.add(Slider::new(v, -360.0..=360.0).suffix("°").fixed_decimals(0))
                    });
                    ui.end_row();
                }
                ui.label("Opacity");
                prop_row(ui, &mut c, Prop::Opacity, t, |ui, v| ui.add(Slider::new(v, 0.0..=1.0).custom_formatter(percent)));
                ui.end_row();
                if !is_text {
                    ui.label("Blur");
                    prop_row(ui, &mut c, Prop::Blur, t, |ui, v| ui.add(Slider::new(v, 0.0..=40.0).suffix(" px").fixed_decimals(1)));
                    ui.end_row();
                    ui.label("Glow");
                    prop_row(ui, &mut c, Prop::Glow, t, |ui, v| ui.add(Slider::new(v, 0.0..=1.0).custom_formatter(percent)));
                    ui.end_row();
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Reset").on_hover_text("Reset transform and remove its keyframes").clicked() {
                    for p in [Prop::X, Prop::Y, Prop::Scale, Prop::Rotation, Prop::Opacity, Prop::Blur, Prop::Glow] {
                        c.keyframes.remove(&p);
                    }
                    c.transform = Transform::default();
                    c.opacity = 1.0;
                    c.blur = 0.0;
                    c.glow = 0.0;
                }
                if !is_text {
                    if ui.button("Fit").on_hover_text("Show the whole frame").clicked() {
                        action = Some(Action::Fit);
                    }
                    if ui.button("Fill").on_hover_text("Cover the whole canvas").clicked() {
                        action = Some(Action::Fill);
                    }
                }
            });
            ui.label(widgets::dim("Tip: drag on the preview to move, scroll to scale.").small());
        });
    }

    if has_sound {
        section(ui, "Audio", |ui| {
            Grid::new("audio-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Volume");
                ui.horizontal(|ui| {
                    prop_row(ui, &mut c, Prop::Volume, t, |ui, v| ui.add(Slider::new(v, 0.0..=2.0).custom_formatter(percent)));
                    let muted = c.value_at(Prop::Volume, t) <= 0.001;
                    if ui.button(if muted { "🔇" } else { "🔊" }).on_hover_text("Mute / unmute").clicked() {
                        c.set_value_at(Prop::Volume, t, if muted { 1.0 } else { 0.0 });
                    }
                });
                ui.end_row();

                ui.label("Role").on_hover_text("Voice clips make music clips duck (Audio tab → Smart audio)");
                let role_label = |r: AudioRole| {
                    if r == AudioRole::Auto { format!("Auto ({})", auto_role.label()) } else { r.label().to_string() }
                };
                egui::ComboBox::from_id_salt("audio-role").selected_text(role_label(c.audio_role)).show_ui(ui, |ui| {
                    for r in AudioRole::ALL {
                        ui.selectable_value(&mut c.audio_role, r, role_label(r));
                    }
                });
                ui.end_row();

                ui.label("Noise removal");
                if measuring_noise {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Measuring noise…");
                    });
                } else {
                    let before = c.denoise.map(|d| d.level);
                    let mut sel = before;
                    egui::ComboBox::from_id_salt("denoise")
                        .selected_text(sel.map_or("Off", |l| l.label()))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut sel, None, "Off");
                            for l in NoiseLevel::ALL {
                                ui.selectable_value(&mut sel, Some(l), l.label());
                            }
                        })
                        .response
                        .on_hover_text("Removes steady background noise (hiss, hum, fans)");
                    if sel != before {
                        match (sel, c.denoise) {
                            (None, _) => c.denoise = None,
                            (Some(level), Some(d)) => c.denoise = Some(Denoise { level, ..d }),
                            (Some(level), None) => action = Some(Action::Denoise(level)),
                        }
                    }
                }
                ui.end_row();
            });
            if kind == TrackKind::Video
                && ui
                    .button("🎵  Extract audio")
                    .on_hover_text("Move this clip's sound to its own audio track")
                    .clicked()
            {
                action = Some(Action::ExtractAudio);
            }
        });
    }

    section(ui, "Keyframes", |ui| {
        let local = t - c.start;
        let times = c.key_times();
        if times.is_empty() {
            ui.label(widgets::dim(
                "Click the diamond button next to a property to animate it, then move the playhead and change the value to add more keyframes.",
            ));
            return;
        }
        ui.label(format!(
            "{} keyframes on {} properties",
            c.keyframes.values().map(Vec::len).sum::<usize>(),
            c.keyframes.len()
        ));
        ui.horizontal(|ui| {
            let prev = times.iter().rev().find(|k| **k < local - KEY_TOLERANCE).copied();
            let next = times.iter().find(|k| **k > local + KEY_TOLERANCE).copied();
            if ui.add_enabled(prev.is_some(), Button::new("◀ Previous")).clicked() {
                action = prev.map(|k| Action::Seek(c.start + k));
            }
            if ui.add_enabled(next.is_some(), Button::new("Next ▶")).clicked() {
                action = next.map(|k| Action::Seek(c.start + k));
            }
        });
        // Easing of the keyframes at the playhead (applies to the segment after them).
        let here: Vec<Prop> = Prop::ALL.into_iter().filter(|p| c.key_index(*p, local).is_some()).collect();
        if let Some(first) = here.first() {
            let current = c.keyframes[first][c.key_index(*first, local).expect("present")].ease;
            let mut ease = current;
            ui.horizontal(|ui| {
                ui.label("Easing");
                egui::ComboBox::from_id_salt("kf-ease").selected_text(ease.label()).show_ui(ui, |ui| {
                    for e in Ease::ALL {
                        ui.selectable_value(&mut ease, e, e.label());
                    }
                });
            });
            if ease != current {
                for p in &here {
                    if let Some(i) = c.key_index(*p, local) {
                        if let Some(keys) = c.keyframes.get_mut(p) {
                            keys[i].ease = ease;
                        }
                    }
                }
            }
        } else {
            ui.label(widgets::dim("Move the playhead onto a keyframe to change its easing.").small());
        }
        if ui.button(RichText::new("Clear all keyframes").color(theme::DANGER)).clicked() {
            for p in Prop::ALL {
                c.clear_keys(p, t);
            }
        }
    });

    if timed {
        section(ui, "Speed", |ui| {
            ui.add(Slider::new(&mut c.speed, 0.25..=4.0).logarithmic(true).custom_formatter(|v, _| format!("{v:.2}×")));
            ui.horizontal_wrapped(|ui| {
                for s in [0.5, 1.0, 1.5, 2.0, 3.0] {
                    let selected = (c.speed - s).abs() < 1e-3;
                    if ui.selectable_label(selected, format!("{s}×")).clicked() {
                        c.speed = s;
                    }
                }
            });
        });
    }

    if kind == TrackKind::Video {
        let prev = app.project.previous_adjacent(clip.id);
        let next = app.project.next_adjacent(clip.id);
        let effective = |to: Id| {
            let (ti, _) = app.project.locate(to)?;
            app.project.track_transitions(ti).into_iter().find(|t| t.to == to).map(|t| t.duration)
        };
        let eff_in = effective(clip.id);
        let next_tr = next.and_then(|n| app.project.clip(n)).and_then(|n| n.transition_in);
        let eff_out = next.and_then(effective);
        section(ui, "Transitions", |ui| {
            if prev.is_none() && next.is_none() {
                ui.label(widgets::dim("Butt this clip against another clip on the same track to add a transition."));
                return;
            }
            if prev.is_some() {
                transition_editor(ui, "From previous clip", &mut c.transition_in, eff_in, "tr-in");
            }
            if let Some(n) = next {
                let mut t = next_tr;
                if transition_editor(ui, "Into next clip", &mut t, eff_out, "tr-out") {
                    action = Some(Action::SetTransition(n, t));
                }
            }
        });
    }

    if kind == TrackKind::Video && media.as_ref().is_some_and(|m| m.kind != MediaKind::Audio) {
        section(ui, "Auto-reframe (AI)", |ui| crate::ui::ai::reframe_section(ui, app, clip.id));
        section(ui, "Extend shot (AI)", |ui| crate::ui::video_gen::extend_section(ui, app, clip.id));
        section(ui, "Background fill (AI)", |ui| crate::ui::image_gen::plate_section(ui, app, clip.id));
    }

    section(ui, "Animation", |ui| {
        let max = (c.duration / 2.0).min(5.0);
        Grid::new("anim-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            ui.label("Fade in");
            ui.add(Slider::new(&mut c.fade_in, 0.0..=max).suffix(" s").fixed_decimals(2));
            ui.end_row();
            ui.label("Fade out");
            ui.add(Slider::new(&mut c.fade_out, 0.0..=max).suffix(" s").fixed_decimals(2));
            ui.end_row();
        });
    });

    if is_visual && !is_text {
        section(ui, "Filter & adjust", |ui| {
            Grid::new("adjust-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("Filter");
                egui::ComboBox::from_id_salt("filter-combo").selected_text(c.filter.label()).show_ui(ui, |ui| {
                    for p in FilterPreset::ALL {
                        ui.selectable_value(&mut c.filter, p, p.label());
                    }
                });
                ui.end_row();
                ui.label("Brightness");
                ui.add(Slider::new(&mut c.adjust.brightness, -0.5..=0.5).fixed_decimals(2));
                ui.end_row();
                ui.label("Contrast");
                ui.add(Slider::new(&mut c.adjust.contrast, 0.5..=2.0).fixed_decimals(2));
                ui.end_row();
                ui.label("Saturation");
                ui.add(Slider::new(&mut c.adjust.saturation, 0.0..=2.0).fixed_decimals(2));
                ui.end_row();
            });
            if ui.button("Reset adjustments").clicked() {
                c.adjust = Default::default();
                c.filter = FilterPreset::None;
            }
        });
    }

    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        if ui.button("✂ Split").on_hover_text("Split at playhead (Ctrl+B)").clicked() {
            action = Some(Action::Split);
        }
        if ui.button("📋 Duplicate").on_hover_text("Ctrl+D").clicked() {
            action = Some(Action::Duplicate);
        }
        if ui
            .add(Button::new(RichText::new("🗑 Delete").color(theme::DANGER)))
            .on_hover_text("Delete / Backspace")
            .clicked()
        {
            action = Some(Action::Delete);
        }
    });

    // Apply edits made through the widgets.
    if c != clip {
        let id = clip.id;
        let new_speed = c.speed;
        c.speed = clip.speed;
        let (fi, fo) = (c.fade_in, c.fade_out);
        c.fade_out = fo.min(c.duration - fi.min(c.duration));
        if let Some(target) = app.project.clip_mut(id) {
            *target = c;
        }
        if (new_speed - clip.speed).abs() > 1e-9 {
            app.project.set_clip_speed(id, new_speed);
        }
    }

    match action {
        Some(Action::Split) => app.split_at_playhead(),
        Some(Action::Duplicate) => app.duplicate_selection(),
        Some(Action::Delete) => app.delete_selection(),
        Some(Action::ExtractAudio) => extract_audio(app, clip.id),
        Some(Action::Fit) | Some(Action::Fill) => {
            let fill = matches!(action, Some(Action::Fill));
            if let Some(m) = &media {
                let (cw, ch) = app.project.aspect.size_for(1080);
                let scale = if fill { fill_scale(m.width as f64, m.height as f64, cw as f64, ch as f64) } else { 1.0 };
                if let Some(c) = app.project.clip_mut(clip.id) {
                    c.set_value_at(Prop::Scale, t, scale as f32);
                    c.set_value_at(Prop::X, t, 0.0);
                    c.set_value_at(Prop::Y, t, 0.0);
                }
            }
        }
        Some(Action::Seek(time)) => app.seek(time),
        Some(Action::Denoise(level)) => {
            let ctx = ui.ctx().clone();
            crate::ui::smart_audio::request_denoise(app, &ctx, clip.id, level);
        }
        Some(Action::SetTransition(to, t)) => {
            if let Err(e) = app.project.set_transition(to, t) {
                app.notify_error(e);
            }
        }
        None => {}
    }
}

fn extract_audio(app: &mut EditorApp, id: crate::model::Id) {
    let Some(orig) = app.project.clip(id).cloned() else { return };
    let mut a = orig.clone();
    a.id = app.project.new_id();
    a.audio_only = true;
    a.transform = Transform::default();
    a.filter = FilterPreset::None;
    a.adjust = Default::default();
    a.opacity = 1.0;
    a.keyframes.retain(|p, _| *p == Prop::Volume);
    app.project.place_clip(a, None);
    if let Some(v) = app.project.clip_mut(id) {
        v.volume = 0.0;
        v.keyframes.remove(&Prop::Volume);
    }
    app.notify("Audio extracted to its own track");
}

fn project_panel(ui: &mut Ui, app: &mut EditorApp) {
    ui.label(RichText::new("Project").strong().size(15.0));
    ui.add_space(6.0);
    Grid::new("project-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
        ui.label("Name");
        ui.add(TextEdit::singleline(&mut app.project.name).desired_width(170.0));
        ui.end_row();
        ui.label("Ratio");
        egui::ComboBox::from_id_salt("proj-aspect")
            .selected_text(app.project.aspect.label())
            .width(170.0)
            .show_ui(ui, |ui| {
                for a in crate::model::Aspect::ALL {
                    ui.selectable_value(&mut app.project.aspect, a, a.label());
                }
            });
        ui.end_row();
        ui.label("Frame rate");
        egui::ComboBox::from_id_salt("proj-fps").selected_text(format!("{} fps", app.project.fps)).show_ui(ui, |ui| {
            for f in [24, 25, 30, 50, 60] {
                ui.selectable_value(&mut app.project.fps, f, format!("{f} fps"));
            }
        });
        ui.end_row();
        ui.label("Duration");
        ui.label(widgets::format_time(app.project.duration()));
        ui.end_row();
        ui.label("Media");
        ui.label(format!("{} items", app.project.media.len()));
        ui.end_row();
        ui.label("Clips");
        ui.label(format!("{} on {} tracks", app.project.clip_count(), app.project.tracks.len()));
        ui.end_row();
    });

    ui.add_space(16.0);
    widgets::section_title(ui, "Getting started");
    for (n, step) in [
        "Import videos, photos or music from the left panel.",
        "Double-click or drag them onto the timeline.",
        "Select a clip to edit it here: transform, speed, filters…",
        "Add titles from the Text tab.",
        "Export when you're done.",
    ]
    .iter()
    .enumerate()
    {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("{}", n + 1)).color(theme::ACCENT).strong());
            ui.label(widgets::dim(*step));
        });
    }

    ui.add_space(16.0);
    widgets::section_title(ui, "Shortcuts");
    Grid::new("shortcuts").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
        for (k, v) in [
            ("Space", "Play / pause"),
            ("Ctrl+B", "Split"),
            ("Del", "Delete clip"),
            ("Ctrl+D", "Duplicate"),
            ("Ctrl+Z / Ctrl+Y", "Undo / redo"),
            ("← →", "Step one frame"),
            ("Shift+← →", "Step one second"),
            ("Home / End", "Jump to start / end"),
            ("Ctrl+scroll", "Zoom timeline"),
            ("Ctrl+S / Ctrl+E", "Save / export"),
        ] {
            ui.label(RichText::new(k).monospace().color(Color32::from_gray(200)));
            ui.label(widgets::dim(v));
            ui.end_row();
        }
    });
    ui.add_space(vec2(0.0, 8.0).y);
}
