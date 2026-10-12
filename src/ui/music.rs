//! Music menu and windows: beat detection, beat montage, beat effects.

use crate::app::EditorApp;
use crate::model::{Id, MediaKind, TrackKind};
use crate::music::{self, BeatEffect, Every, MusicAnalysis};
use crate::task::{Task, TaskState};
use crate::ui::{theme, widgets};
use egui::{Align2, Button, Color32, Context, Grid, Key, RichText, ScrollArea, Slider, Ui};
use std::collections::BTreeSet;

pub struct MusicUi {
    analyzing: Vec<(Id, Task<MusicAnalysis>)>,
    pub montage_open: bool,
    every: Every,
    shuffle: bool,
    picked: BTreeSet<Id>,
    pub effects_open: bool,
    effect: BeatEffect,
    effect_every: Every,
    amount: f32,
    pub multicam_open: bool,
    takes: BTreeSet<Id>,
    sync_job: Option<(Id, Vec<Id>, Task<Vec<(f64, f32)>>)>,
    switch_every: Every,
    /// Live switching: recording number-key cuts while the song plays.
    recording: bool,
    live: Vec<(f64, u32)>,
    rec_range: (f64, f64),
    snap: bool,
    pub lyrics_open: bool,
    lyrics_text: String,
    lyric_style: crate::lyrics::LyricStyle,
    lyric_pos: f32,
    lyric_beat_snap: bool,
    lyrics_job: Option<Task<Vec<crate::lyrics::Line>>>,
    pub react_open: bool,
    viz: crate::reactive::Visualizer,
    viz_color: [u8; 3],
    viz_pos: f32,
    viz_job: Option<(crate::reactive::Visualizer, f64, f32, std::path::PathBuf, Task<crate::media::ffmpeg::ProbeInfo>)>,
    react: crate::reactive::React,
    band: crate::reactive::Band,
    react_amount: f32,
    react_job: Option<(Vec<Id>, Id, crate::reactive::React, f32, Task<Vec<f32>>)>,
}

impl Default for MusicUi {
    fn default() -> Self {
        Self {
            analyzing: Vec::new(),
            montage_open: false,
            every: Every::Auto,
            shuffle: false,
            picked: BTreeSet::new(),
            effects_open: false,
            effect: BeatEffect::Pulse,
            effect_every: Every::Beat,
            amount: 0.6,
            multicam_open: false,
            takes: BTreeSet::new(),
            sync_job: None,
            switch_every: Every::Bar,
            recording: false,
            live: Vec::new(),
            rec_range: (0.0, 0.0),
            snap: true,
            lyrics_open: false,
            lyrics_text: String::new(),
            lyric_style: crate::lyrics::LyricStyle::Karaoke,
            lyric_pos: 0.30,
            lyric_beat_snap: true,
            lyrics_job: None,
            react_open: false,
            viz: crate::reactive::Visualizer::Bars,
            viz_color: [0, 205, 222],
            viz_pos: 0.36,
            viz_job: None,
            react: crate::reactive::React::Throb,
            band: crate::reactive::Band::Bass,
            react_amount: 0.6,
            react_job: None,
        }
    }
}

impl MusicUi {
    pub fn busy(&self) -> bool {
        !self.analyzing.is_empty()
    }
}

/// Starts beat detection for a media item (runs locally, free).
pub fn detect(app: &mut EditorApp, ctx: &Context, media: Id) {
    if app.music.analyzing.iter().any(|(m, _)| *m == media) {
        return;
    }
    let Some(path) = app.project.media(media).map(|m| m.path.clone()) else { return };
    let task = Task::spawn(ctx.clone(), move |_, report| {
        report(0.1, "Listening for the beat…");
        music::analyze(&path).map(Some)
    });
    app.music.analyzing.push((media, task));
}

/// The song to analyse: the selected clip if it has sound, else the first clip on an audio track.
fn song_candidate(app: &EditorApp) -> Option<Id> {
    let sel = app.selection.and_then(|id| app.project.clip(id)).and_then(|c| c.media_id()).filter(|m| app.project.media(*m).is_some_and(|x| x.has_sound()));
    sel.or_else(|| {
        app.project
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .flat_map(|t| &t.clips)
            .filter_map(|c| c.media_id())
            .find(|m| app.project.media(*m).is_some_and(|x| x.has_sound()))
    })
}

pub fn poll(app: &mut EditorApp) {
    if let Some(state) = app.music.sync_job.as_ref().and_then(|j| j.2.take_finished()) {
        let (song, takes, _) = app.music.sync_job.take().unwrap();
        match state {
            TaskState::Done(offsets) => {
                let measured: Vec<(Id, f64, f32)> = takes.iter().zip(offsets).map(|(m, (o, c))| (*m, o, c)).collect();
                match crate::multicam::place_takes(&mut app.project, song, &measured) {
                    Ok(synced) => {
                        let shaky: Vec<String> = synced
                            .iter()
                            .filter(|s| s.confidence < 1.5)
                            .filter_map(|s| app.project.media(s.media).map(|m| format!("Cam {} ({})", s.angle, m.name)))
                            .collect();
                        app.notify(format!("🎥 {} take(s) synced to the song — now switch angles", synced.len()));
                        if !shaky.is_empty() {
                            app.notify_error(format!("Check the sync of {}: its sound did not match the song clearly", shaky.join(", ")));
                        }
                        app.music.takes.clear();
                    }
                    Err(e) => app.notify_error(format!("Multi-cam: {e}")),
                }
            }
            TaskState::Failed(e) => app.notify_error(format!("Multi-cam sync failed: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.music.lyrics_job.as_ref().and_then(Task::take_finished) {
        app.music.lyrics_job = None;
        match state {
            TaskState::Done(lines) => {
                let n = crate::lyrics::apply_lyrics(&mut app.project, &lines, app.music.lyric_style, app.music.lyric_pos, app.music.lyric_beat_snap);
                app.notify(format!("🎤 {} lyric lines added ({n} clips) on the Lyrics track — edit any of them like text", lines.len()));
            }
            TaskState::Failed(e) => app.notify_error(format!("Lyrics: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.music.viz_job.as_ref().and_then(|j| j.4.take_finished()) {
        let (kind, start, y, path, _) = app.music.viz_job.take().unwrap();
        match state {
            TaskState::Done(info) => {
                let (media, clip) = crate::reactive::place_visualizer(&mut app.project, kind, &path, info, start, y);
                if let Some(item) = app.project.media(media).cloned() {
                    app.assets.ensure(&item);
                }
                app.selection = clip;
                app.notify(format!("📊 {} added over the song — move or resize it like any clip", kind.label()));
            }
            TaskState::Failed(e) => app.notify_error(format!("Visualizer: {e}")),
            _ => {}
        }
    }
    if let Some(state) = app.music.react_job.as_ref().and_then(|j| j.4.take_finished()) {
        let (clips, song, effect, amount, _) = app.music.react_job.take().unwrap();
        match state {
            TaskState::Done(env) => {
                let n = crate::reactive::apply_reactive(&mut app.project, &clips, song, &env, effect, amount);
                app.notify(format!("🔊 {} on {} clip(s) ({n} keyframes)", effect.label(), clips.len()));
            }
            TaskState::Failed(e) => app.notify_error(format!("React to music: {e}")),
            _ => {}
        }
    }
    // Live switching ends when playback stops.
    if app.music.recording && !app.playing {
        finish_live(app);
    }
    let mut i = 0;
    while i < app.music.analyzing.len() {
        let Some(state) = app.music.analyzing[i].1.take_finished() else {
            i += 1;
            continue;
        };
        let (media, _) = app.music.analyzing.remove(i);
        let name = app.project.media(media).map(|m| m.name.clone()).unwrap_or_default();
        match state {
            TaskState::Done(a) => {
                let chorus = a.sections.iter().filter(|s| s.label == "chorus").count();
                app.notify(format!(
                    "🥁 {name}: {:.0} BPM, {} beats, {} sections{} — cuts now snap to the beat",
                    a.bpm,
                    a.beats.len(),
                    a.sections.len(),
                    if chorus > 0 { format!(" ({chorus} loud/chorus)") } else { String::new() }
                ));
                app.project.music.insert(media, a);
            }
            TaskState::Failed(e) => app.notify_error(format!("Beat detection failed for {name}: {e}")),
            _ => {}
        }
    }
}

pub fn menu(ui: &mut Ui, app: &mut EditorApp) {
    ui.set_min_width(260.0);
    let song = song_candidate(app);
    let has_grid = !music::timeline_beats(&app.project).is_empty();
    let detect_label = if app.music.busy() { "🥁 Detecting beats…" } else { "🥁 Detect beats in the song" };
    if ui
        .add_enabled(song.is_some() && !app.music.busy(), Button::new(detect_label))
        .on_hover_text("Select the song clip first, or put it on an audio track")
        .clicked()
    {
        if let Some(m) = song {
            let ctx = ui.ctx().clone();
            detect(app, &ctx, m);
        }
        ui.close_menu();
    }
    if let Some(a) = music::beat_clip(&app.project).and_then(|c| c.media_id()).and_then(|m| app.project.music.get(&m)) {
        ui.label(widgets::dim(format!("{:.0} BPM · {} bars · {} sections", a.bpm, a.beats.len() / a.beats_per_bar.max(1), a.sections.len())).small());
    }
    ui.separator();
    if ui.add_enabled(has_grid, Button::new("🎞 Beat montage…")).on_disabled_hover_text("Detect beats first").clicked() {
        app.music.montage_open = true;
        ui.close_menu();
    }
    if ui.add_enabled(has_grid, Button::new("💥 Beat effects on selected clips…")).on_disabled_hover_text("Detect beats first").clicked() {
        app.music.effects_open = true;
        ui.close_menu();
    }
    ui.separator();
    if ui.button(RichText::new("🎬 AI music video from this song…").strong()).clicked() {
        app.storyboard.open = true;
        app.storyboard.music_video = true;
        ui.close_menu();
    }
    if ui
        .add_enabled(has_grid, Button::new("🔥 Trim to the best 30 seconds"))
        .on_hover_text("Keeps the most energetic 30 s (a chorus when there is one) for TikTok / Reels. Undo to get the full song back.")
        .on_disabled_hover_text("Detect beats first")
        .clicked()
    {
        if let Some((a, b)) = crate::ai::music_video::best_window(&app.project, 30.0) {
            let len = crate::ai::music_video::apply_window(&mut app.project, a, b);
            app.seek(0.0);
            app.notify(format!("🔥 Trimmed to {} – {} of the song ({len:.0} s). Ctrl+Z to undo.", widgets::format_short(a), widgets::format_short(b)));
        }
        ui.close_menu();
    }
    ui.separator();
    if ui.button("🎤 Lyrics & karaoke captions…").clicked() {
        app.music.lyrics_open = true;
        ui.close_menu();
    }
    if ui.button("📊 Visualizer & react to the music…").clicked() {
        app.music.react_open = true;
        ui.close_menu();
    }
    if ui.button("🎥 Multi-cam: sync takes & switch angles…").clicked() {
        app.music.multicam_open = true;
        ui.close_menu();
    }
    ui.separator();
    if has_grid && ui.button("Clear beat grid").clicked() {
        app.project.music.clear();
        ui.close_menu();
    }
}

fn montage_window(ctx: &Context, app: &mut EditorApp) {
    if !app.music.montage_open {
        return;
    }
    let mut open = true;
    let mut go = false;
    let song = music::beat_clip(&app.project).map(|c| (c.start, c.end()));
    egui::Window::new(RichText::new("Beat montage").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(420.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(420.0);
            let Some((s, e)) = song else {
                ui.label(RichText::new("Detect beats in the song first (Music menu).").color(theme::DANGER));
                return;
            };
            ui.label(widgets::dim(format!(
                "Cuts your clips on the beat for the whole song ({} – {}). Their own sound is muted; the song plays over them.",
                widgets::format_short(s),
                widgets::format_short(e)
            )));
            ui.add_space(4.0);
            ui.label(RichText::new("Clips to use").strong());
            let visuals: Vec<(Id, String, MediaKind)> =
                app.project.media.iter().filter(|m| m.kind != MediaKind::Audio).map(|m| (m.id, m.name.clone(), m.kind)).collect();
            if visuals.is_empty() {
                ui.label(RichText::new("Import some video clips or photos first.").color(theme::DANGER));
                return;
            }
            if app.music.picked.is_empty() {
                app.music.picked = visuals.iter().map(|v| v.0).collect();
            }
            ScrollArea::vertical().max_height(180.0).show(ui, |ui| {
                for (id, name, kind) in &visuals {
                    let mut on = app.music.picked.contains(id);
                    let icon = if *kind == MediaKind::Image { "🖼" } else { "🎬" };
                    if ui.checkbox(&mut on, format!("{icon} {name}")).changed() {
                        if on {
                            app.music.picked.insert(*id);
                        } else {
                            app.music.picked.remove(id);
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                if ui.small_button("All").clicked() {
                    app.music.picked = visuals.iter().map(|v| v.0).collect();
                }
                if ui.small_button("None").clicked() {
                    app.music.picked.clear();
                }
            });
            ui.add_space(4.0);
            Grid::new("montage-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Cut");
                egui::ComboBox::from_id_salt("montage-every").selected_text(app.music.every.label()).show_ui(ui, |ui| {
                    for e in Every::ALL {
                        ui.selectable_value(&mut app.music.every, e, e.label());
                    }
                });
                ui.end_row();
                ui.label("Order");
                ui.checkbox(&mut app.music.shuffle, "Shuffle");
                ui.end_row();
            });
            ui.add_space(6.0);
            let n = music::cut_points(&app.project, s, e, app.music.every).len();
            let w = ui.available_width();
            let text = format!("🎞  Build montage (~{} cuts)", n.saturating_sub(1).max(1));
            if ui
                .add_enabled(!app.music.picked.is_empty(), Button::new(RichText::new(text).strong().color(Color32::BLACK)).fill(theme::ACCENT).min_size(egui::vec2(w, 30.0)))
                .clicked()
            {
                go = true;
            }
        });
    if go {
        if let Some((s, e)) = song {
            let picked: Vec<Id> = app.music.picked.iter().copied().collect();
            match music::beat_montage(&mut app.project, &picked, s, e, app.music.every, app.music.shuffle) {
                Ok(ids) => {
                    app.selection = ids.first().copied();
                    app.notify(format!("🎞 Montage built: {} cuts on the beat — try Music → Beat effects for extra punch", ids.len()));
                    app.music.montage_open = false;
                }
                Err(err) => app.notify_error(format!("Montage: {err}")),
            }
        }
    }
    if !open {
        app.music.montage_open = false;
    }
}

/// Clips the effect applies to: the selected clip, or every visual clip on its track when "whole track" is chosen.
fn effects_window(ctx: &Context, app: &mut EditorApp) {
    if !app.music.effects_open {
        return;
    }
    let mut open = true;
    let mut apply: Option<Vec<Id>> = None;
    let sel = app.selection.filter(|id| app.project.clip(*id).is_some_and(|c| app.project.track_kind_for(c) != TrackKind::Audio));
    egui::Window::new(RichText::new("Beat effects").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(360.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(widgets::dim("Adds keyframes on the beat. They're ordinary keyframes: tweak or remove them in the Animation tab."));
            let m = &mut app.music;
            Grid::new("fx-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Effect");
                egui::ComboBox::from_id_salt("fx-kind").selected_text(m.effect.label()).show_ui(ui, |ui| {
                    for e in BeatEffect::ALL {
                        ui.selectable_value(&mut m.effect, e, e.label());
                    }
                });
                ui.end_row();
                ui.label("On");
                egui::ComboBox::from_id_salt("fx-every").selected_text(m.effect_every.label()).show_ui(ui, |ui| {
                    for e in [Every::Beat, Every::TwoBeats, Every::Bar, Every::TwoBars] {
                        ui.selectable_value(&mut m.effect_every, e, e.label());
                    }
                });
                ui.end_row();
                ui.label("Strength");
                ui.add(Slider::new(&mut m.amount, 0.1..=1.0).show_value(false));
                ui.end_row();
            });
            ui.add_space(6.0);
            let Some(id) = sel else {
                ui.label(RichText::new("Select a video, image or text clip on the timeline.").color(theme::DANGER));
                return;
            };
            ui.horizontal(|ui| {
                if ui.add(Button::new(RichText::new("💥  Apply to selected clip").strong().color(Color32::BLACK)).fill(theme::ACCENT)).clicked() {
                    apply = Some(vec![id]);
                }
                if ui.button("Whole track").on_hover_text("Every clip on the selected clip's track").clicked() {
                    if let Some((ti, _)) = app.project.locate(id) {
                        apply = Some(app.project.tracks[ti].clips.iter().map(|c| c.id).collect());
                    }
                }
            });
        });
    if let Some(ids) = apply {
        let n = music::apply_beat_effect(&mut app.project, &ids, app.music.effect, app.music.effect_every, app.music.amount);
        if n == 0 {
            app.notify_error("No beats fall inside the selected clip(s)");
        } else {
            app.notify(format!("💥 {} on {} clip(s)", app.music.effect.label(), ids.len()));
        }
    }
    if !open {
        app.music.effects_open = false;
    }
}

fn finish_live(app: &mut EditorApp) {
    app.music.recording = false;
    let mut cuts = std::mem::take(&mut app.music.live);
    if cuts.is_empty() {
        app.notify("No angle switches recorded");
        return;
    }
    if app.music.snap {
        crate::multicam::snap_to_beats(&app.project, &mut cuts);
    }
    let (from, _) = app.music.rec_range;
    let to = app.playhead.max(cuts.last().map_or(from, |c| c.0) + 0.5);
    match crate::multicam::build_edit(&mut app.project, &cuts, from, to) {
        Ok(n) => app.notify(format!("🎥 Multi-cam edit: {n} shots from your live switching")),
        Err(e) => app.notify_error(format!("Multi-cam: {e}")),
    }
}

fn multicam_window(ctx: &Context, app: &mut EditorApp) {
    if !app.music.multicam_open {
        return;
    }
    // Number keys switch angles while recording.
    if app.music.recording {
        let keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        let angles: Vec<u32> = crate::multicam::angles(&app.project).iter().map(|a| a.0).collect();
        for (k, key) in keys.iter().enumerate() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, *key)) {
                if let Some(&a) = angles.get(k) {
                    let t = app.playhead;
                    app.music.live.push((t, a));
                }
            }
        }
        ctx.request_repaint();
    }
    let mut open = true;
    let song = crate::multicam::song_clip(&app.project);
    egui::Window::new(RichText::new("Multi-cam").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(430.0)
        .anchor(Align2::RIGHT_TOP, [-20.0, 60.0])
        .show(ctx, |ui| {
            ui.set_width(430.0);
            let Some(song_id) = song else {
                ui.label(RichText::new("Put the song on an audio track first; takes are synced to it.").color(theme::DANGER));
                return;
            };
            ui.label(RichText::new("1. Sync takes to the song").strong());
            ui.label(widgets::dim("Pick your performance takes (videos with sound). Each is lined up by its audio and put on its own track, muted."));
            let takes: Vec<(Id, String)> = app
                .project
                .media
                .iter()
                .filter(|m| m.kind == MediaKind::Video && m.has_audio)
                .map(|m| (m.id, m.name.clone()))
                .collect();
            let synced: BTreeSet<Id> = app
                .project
                .tracks
                .iter()
                .filter(|t| t.angle.is_some())
                .flat_map(|t| &t.clips)
                .filter_map(|c| c.media_id())
                .collect();
            if takes.is_empty() {
                ui.label(widgets::dim("Import your takes (video files) first."));
            }
            ScrollArea::vertical().id_salt("mc-takes").max_height(140.0).show(ui, |ui| {
                for (id, name) in &takes {
                    if synced.contains(id) {
                        ui.label(widgets::dim(format!("✔ {name} (synced)")));
                        continue;
                    }
                    let mut on = app.music.takes.contains(id);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            app.music.takes.insert(*id);
                        } else {
                            app.music.takes.remove(id);
                        }
                    }
                }
            });
            if let Some((_, _, job)) = &app.music.sync_job {
                let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
                ui.add(egui::ProgressBar::new(p).text(stage).animate(true));
            } else if ui.add_enabled(!app.music.takes.is_empty(), Button::new(format!("🎥  Sync {} take(s)", app.music.takes.len()))).clicked() {
                let picked: Vec<Id> = app.music.takes.iter().copied().collect();
                match crate::multicam::sync_inputs(&app.project, song_id, &picked) {
                    Ok((song_path, paths)) => {
                        let task = Task::spawn(ui.ctx().clone(), move |_, report| crate::multicam::measure_offsets(&song_path, &paths, report).map(Some));
                        app.music.sync_job = Some((song_id, picked, task));
                    }
                    Err(e) => app.notify_error(format!("Multi-cam: {e}")),
                }
            }

            let angles = crate::multicam::angles(&app.project);
            if angles.is_empty() {
                return;
            }
            ui.separator();
            ui.label(RichText::new("2. Switch angles").strong());
            for (k, (_, ti)) in angles.iter().enumerate() {
                ui.label(format!("  {}  →  {}", k + 1, app.project.tracks[*ti].name));
            }
            let (s, e) = app.project.clip(song_id).map_or((0.0, 0.0), |c| (c.start, c.end()));
            ui.add_space(4.0);
            if app.music.recording {
                ui.label(
                    RichText::new(format!("⏺ Recording: press 1–{} to switch. {} switches so far.", angles.len().min(9), app.music.live.len()))
                        .color(theme::DANGER)
                        .strong(),
                );
                if ui.button("⏹  Stop & build edit").clicked() {
                    app.pause();
                    finish_live(app);
                }
            } else {
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("⏺  Live switch").strong()).on_hover_text("Plays the song; press number keys to cut to that angle").clicked() {
                        app.music.live.clear();
                        let from = if app.playhead >= s && app.playhead < e - 1.0 { app.playhead } else { s };
                        app.music.rec_range = (from, e);
                        app.music.recording = true;
                        app.seek(from);
                        app.play();
                    }
                    ui.checkbox(&mut app.music.snap, "Snap to beats");
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("mc-every").selected_text(app.music.switch_every.label()).show_ui(ui, |ui| {
                        for ev in Every::ALL.into_iter().chain([Every::Section]) {
                            ui.selectable_value(&mut app.music.switch_every, ev, ev.label());
                        }
                    });
                    if ui.button("🎲  Auto-switch").on_hover_text("Cuts between angles on the beat grid (every 4 s without one)").clicked() {
                        let sw = crate::multicam::auto_switches(&app.project, s, e, app.music.switch_every);
                        match crate::multicam::build_edit(&mut app.project, &sw, s, e) {
                            Ok(n) => app.notify(format!("🎥 Multi-cam edit: {n} shots")),
                            Err(err) => app.notify_error(format!("Multi-cam: {err}")),
                        }
                    }
                });
                ui.label(widgets::dim("The edit goes on a Multi-cam edit track; the angle tracks are hidden (unhide them to compare). Run again to replace it.").small());
            }
        });
    if !open {
        app.music.multicam_open = false;
        if app.music.recording {
            app.pause();
            finish_live(app);
        }
    }
}

fn lyrics_window(ctx: &Context, app: &mut EditorApp) {
    if !app.music.lyrics_open {
        return;
    }
    let mut open = true;
    let song = crate::multicam::song_clip(&app.project);
    egui::Window::new(RichText::new("Lyrics").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(460.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(460.0);
            let Some(song_id) = song else {
                ui.label(RichText::new("Put the song on an audio track first.").color(theme::DANGER));
                return;
            };
            ui.label(widgets::dim(
                "Paste the lyrics (or open an .lrc file). Timed .lrc lines are used as they are; plain lyrics are lined up with the vocals by speech recognition. Leave it empty to transcribe the vocals.",
            ));
            ui.horizontal(|ui| {
                if ui.button("📂  Open .lrc / .txt…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Lyrics", &["lrc", "txt"]).pick_file() {
                        match std::fs::read_to_string(&p) {
                            Ok(t) => app.music.lyrics_text = t,
                            Err(e) => app.notify_error(format!("Could not read {}: {e}", p.display())),
                        }
                    }
                }
                let timed = crate::lyrics::parse_lrc(&app.music.lyrics_text).is_some();
                ui.label(widgets::dim(if app.music.lyrics_text.trim().is_empty() {
                    "Empty: the vocals will be transcribed"
                } else if timed {
                    "Timed (.lrc): no transcription needed"
                } else {
                    "Plain lyrics: will be aligned to the vocals"
                }));
            });
            ScrollArea::vertical().id_salt("lyrics-text").max_height(200.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut app.music.lyrics_text).desired_rows(8).desired_width(f32::INFINITY).hint_text("[Verse 1]\nFirst line of the song\n…"));
            });
            Grid::new("lyrics-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Style");
                egui::ComboBox::from_id_salt("lyric-style").selected_text(app.music.lyric_style.label()).show_ui(ui, |ui| {
                    for st in crate::lyrics::LyricStyle::ALL {
                        ui.selectable_value(&mut app.music.lyric_style, st, st.label());
                    }
                });
                ui.end_row();
                if app.music.lyric_style == crate::lyrics::LyricStyle::PopWords {
                    ui.label("");
                    let has_beats = !crate::music::timeline_beats(&app.project).is_empty();
                    ui.add_enabled(has_beats, egui::Checkbox::new(&mut app.music.lyric_beat_snap, "Pop on the beat"))
                        .on_disabled_hover_text("Detect beats first (Music menu)");
                    ui.end_row();
                }
                ui.label("Position");
                ui.add_enabled_ui(app.music.lyric_style != crate::lyrics::LyricStyle::BigCentered, |ui| ui.horizontal(|ui| {
                    ui.selectable_value(&mut app.music.lyric_pos, -0.30, "Top");
                    ui.selectable_value(&mut app.music.lyric_pos, 0.0, "Middle");
                    ui.selectable_value(&mut app.music.lyric_pos, 0.30, "Bottom");
                }));
                ui.end_row();
            });
            ui.add_space(6.0);
            if let Some(job) = &app.music.lyrics_job {
                let (p, stage) = job.progress().unwrap_or((1.0, "Finishing…".into()));
                ui.horizontal(|ui| {
                    ui.add(egui::ProgressBar::new(p).desired_width(320.0).text(stage).animate(true));
                    if ui.button("Stop").clicked() {
                        job.cancel();
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    if ui.add(Button::new(RichText::new("🎤  Add lyrics").strong().color(Color32::BLACK)).fill(theme::ACCENT)).clicked() {
                        let text = app.music.lyrics_text.clone();
                        let input = if text.trim().is_empty() { crate::lyrics::Input::Transcribe } else { crate::lyrics::Input::Text(text) };
                        let project = app.project.clone();
                        let task = Task::spawn(ui.ctx().clone(), move |cancel, report| crate::lyrics::timed_lines(&project, song_id, &input, "auto", cancel, report));
                        app.music.lyrics_job = Some(task);
                    }
                    let has = !crate::lyrics::timeline_lines(&app.project).is_empty();
                    if ui.add_enabled(has, Button::new("💾  Export .lrc…")).clicked() {
                        export_lrc(app, song_id);
                    }
                });
                ui.label(widgets::dim("Restyle them later in the Captions tab; .srt export works too.").small());
            }
        });
    if !open {
        app.music.lyrics_open = false;
    }
}

fn export_lrc(app: &mut EditorApp, song_id: Id) {
    let Some(clip) = app.project.clip(song_id).cloned() else { return };
    let mut lines = crate::lyrics::timeline_lines(&app.project);
    crate::lyrics::to_song_time(&mut lines, clip.start, clip.source_in, clip.speed);
    let name = clip.media_id().and_then(|m| app.project.media(m)).map(|m| m.name.clone()).unwrap_or_else(|| "lyrics".into());
    let stem = std::path::Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name.clone());
    if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{stem}.lrc")).add_filter("Lyrics", &["lrc"]).save_file() {
        match std::fs::write(&path, crate::lyrics::to_lrc(&lines, &stem)) {
            Ok(()) => app.notify(format!("Saved {}", path.display())),
            Err(e) => app.notify_error(format!("Could not save: {e}")),
        }
    }
}

fn react_window(ctx: &Context, app: &mut EditorApp) {
    use crate::reactive::{Band, React, Visualizer};
    if !app.music.react_open {
        return;
    }
    let mut open = true;
    let song = crate::multicam::song_clip(&app.project);
    egui::Window::new(RichText::new("Audio-reactive").strong())
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(400.0)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_width(400.0);
            let Some(song_id) = song else {
                ui.label(RichText::new("Put the song on an audio track first.").color(theme::DANGER));
                return;
            };
            ui.label(RichText::new("Visualizer").strong());
            ui.label(widgets::dim("Draws the song as a transparent overlay for its whole length."));
            Grid::new("viz-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Kind");
                egui::ComboBox::from_id_salt("viz-kind").selected_text(app.music.viz.label()).show_ui(ui, |ui| {
                    for v in Visualizer::ALL {
                        ui.selectable_value(&mut app.music.viz, v, v.label());
                    }
                });
                ui.end_row();
                ui.label("Colour");
                ui.color_edit_button_srgb(&mut app.music.viz_color);
                ui.end_row();
                ui.label("Position");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut app.music.viz_pos, -0.36, "Top");
                    ui.selectable_value(&mut app.music.viz_pos, 0.0, "Middle");
                    ui.selectable_value(&mut app.music.viz_pos, 0.36, "Bottom");
                });
                ui.end_row();
            });
            if let Some((.., job)) = &app.music.viz_job {
                let (p, stage) = job.progress().unwrap_or((1.0, "Drawing…".into()));
                ui.add(egui::ProgressBar::new(p).text(stage).animate(true));
            } else if ui.button("📊  Add visualizer").clicked() {
                match crate::reactive::song_range(&app.project, song_id) {
                    Ok((path, from, start, dur, speed)) => {
                        let kind = app.music.viz;
                        let (cw, ch) = app.project.aspect.size_for(1080);
                        let size = kind.size(cw, ch);
                        let fps = app.project.fps;
                        let color = app.music.viz_color;
                        let stem = std::path::Path::new(&path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        let dir = crate::ai::video_gen::output_dir(app.project_path.as_deref());
                        let mut out = dir.join(format!("visualizer-{}-{}.mov", kind.name(), crate::ai::video_gen::slug(&stem)));
                        let mut n = 2;
                        while out.exists() {
                            out = dir.join(format!("visualizer-{}-{}-{n}.mov", kind.name(), crate::ai::video_gen::slug(&stem)));
                            n += 1;
                        }
                        let o = out.clone();
                        let task = Task::spawn(ui.ctx().clone(), move |_, report| {
                            report(0.2, "Drawing the visualizer…");
                            crate::reactive::render_visualizer(&path, from, dur * speed, kind, size, fps, color, &o).map(Some)
                        });
                        app.music.viz_job = Some((kind, start, app.music.viz_pos, out, task));
                    }
                    Err(e) => app.notify_error(format!("Visualizer: {e}")),
                }
            }

            ui.separator();
            ui.label(RichText::new("React to the music").strong());
            ui.label(widgets::dim("Animates the selected clip with the song: big on the hits, calm in quiet parts. Written as keyframes."));
            let m = &mut app.music;
            Grid::new("react-grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
                ui.label("Effect");
                egui::ComboBox::from_id_salt("react-kind").selected_text(m.react.label()).show_ui(ui, |ui| {
                    for r in React::ALL {
                        ui.selectable_value(&mut m.react, r, r.label());
                    }
                });
                ui.end_row();
                ui.label("Follow");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut m.band, Band::Bass, "Bass / kick");
                    ui.selectable_value(&mut m.band, Band::Loudness, "Loudness");
                });
                ui.end_row();
                ui.label("Strength");
                ui.add(Slider::new(&mut m.react_amount, 0.1..=1.0).show_value(false));
                ui.end_row();
            });
            let sel = app.selection.filter(|id| app.project.clip(*id).is_some_and(|c| app.project.track_kind_for(c) != TrackKind::Audio));
            if let Some((.., job)) = &app.music.react_job {
                let (p, stage) = job.progress().unwrap_or((1.0, "Listening…".into()));
                ui.add(egui::ProgressBar::new(p).text(stage).animate(true));
            } else {
                ui.horizontal(|ui| {
                    let mut targets: Option<Vec<Id>> = None;
                    if ui.add_enabled(sel.is_some(), Button::new("🔊  Apply to selected clip")).on_disabled_hover_text("Select a video, image or text clip").clicked() {
                        targets = sel.map(|s| vec![s]);
                    }
                    if ui.add_enabled(sel.is_some(), Button::new("Whole track")).clicked() {
                        if let Some((ti, _)) = sel.and_then(|s| app.project.locate(s)) {
                            targets = Some(app.project.tracks[ti].clips.iter().map(|c| c.id).collect());
                        }
                    }
                    if let Some(clips) = targets {
                        match crate::reactive::song_range(&app.project, song_id) {
                            Ok((path, ..)) => {
                                let band = app.music.band;
                                let task = Task::spawn(ui.ctx().clone(), move |_, report| {
                                    report(0.3, "Listening to the song…");
                                    crate::reactive::envelope(&path, band).map(Some)
                                });
                                app.music.react_job = Some((clips, song_id, app.music.react, app.music.react_amount, task));
                            }
                            Err(e) => app.notify_error(format!("React to music: {e}")),
                        }
                    }
                });
            }
        });
    if !open {
        app.music.react_open = false;
    }
}

pub fn window(ctx: &Context, app: &mut EditorApp) {
    react_window(ctx, app);
    lyrics_window(ctx, app);
    multicam_window(ctx, app);
    montage_window(ctx, app);
    effects_window(ctx, app);
}
