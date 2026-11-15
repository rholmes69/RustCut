//! Left panel "Brand" tab: the brand kit (logos, colours, fonts) and the reusable
//! asset folders, ready to drop into any project.

use crate::app::EditorApp;
use crate::brand_kit::{self as bk, BrandAsset, BrandKit, Placement};
use crate::media::ffmpeg;
use crate::model::MediaKind;
use crate::ui::{theme, widgets};
use egui::{pos2, vec2, Align2, Button, Color32, ColorImage, FontId, Rect, RichText, Sense, Stroke, TextureHandle, TextureOptions, Ui};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

type ThumbMsg = (PathBuf, Option<(u32, u32, Vec<u8>, f64)>);

pub struct BrandKitUi {
    pub kit: Option<BrandKit>,
    pub error: Option<String>,
    loaded: bool,
    thumbs: HashMap<PathBuf, TextureHandle>,
    /// Seconds for clips (shown on the card).
    durations: HashMap<PathBuf, f64>,
    requested: HashSet<PathBuf>,
    tx: Sender<ThumbMsg>,
    rx: Receiver<ThumbMsg>,
}

impl Default for BrandKitUi {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            kit: None,
            error: None,
            loaded: false,
            thumbs: HashMap::new(),
            durations: HashMap::new(),
            requested: HashSet::new(),
            tx,
            rx,
        }
    }
}

impl BrandKitUi {
    /// Loads the remembered kit the first time it's needed.
    pub fn ensure_loaded(&mut self) {
        if !self.loaded {
            self.loaded = true;
            if let Some(dir) = bk::saved_folder() {
                self.load(&dir);
            }
        }
    }

    pub fn load(&mut self, dir: &Path) {
        match bk::load(dir) {
            Ok(k) => {
                self.kit = Some(k);
                self.error = None;
            }
            Err(e) => {
                self.kit = None;
                self.error = Some(format!("{e:#}"));
            }
        }
        // Files may have changed on disk: start thumbnails afresh.
        self.thumbs.clear();
        self.durations.clear();
        self.requested.clear();
    }

    fn thumb(&mut self, ctx: &egui::Context, a: &BrandAsset) -> Option<&TextureHandle> {
        while let Ok((path, res)) = self.rx.try_recv() {
            if let Some((w, h, rgba, dur)) = res {
                let img = ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                let tex = ctx.load_texture(format!("brand-{}", path.display()), img, TextureOptions::LINEAR);
                self.thumbs.insert(path.clone(), tex);
                if dur > 0.0 {
                    self.durations.insert(path, dur);
                }
            }
        }
        if a.kind != MediaKind::Audio && self.requested.insert(a.path.clone()) {
            let (tx, path, ctx) = (self.tx.clone(), a.path.clone(), ctx.clone());
            std::thread::spawn(move || {
                let res = ffmpeg::probe(&path).ok().and_then(|info| {
                    let (w, h) = ffmpeg::fit_within(info.width, info.height, 256, 144);
                    let t = if info.duration > 0.0 { (info.duration * 0.3).min(3.0) } else { 0.0 };
                    ffmpeg::extract_frame(&path, t, w, h).ok().map(|rgba| (w, h, rgba, info.duration))
                });
                let _ = tx.send((path, res));
                ctx.request_repaint();
            });
        }
        self.thumbs.get(&a.path)
    }
}

enum Action {
    Add(PathBuf, Placement),
    Reveal(PathBuf),
    Color(String, [u8; 3]),
}

pub fn tab(ui: &mut Ui, app: &mut EditorApp) {
    app.brand_kit.ensure_loaded();
    widgets::section_title(ui, "Brand kit");

    let mut choose = false;
    let mut create = false;
    let mut rescan = false;
    let Some(kit) = app.brand_kit.kit.clone() else {
        ui.label(widgets::dim("Keep logos, colours, fonts and reusable clips (intros, end cards, b-roll) in one folder and use them in every project.").small());
        if let Some(e) = &app.brand_kit.error {
            ui.label(RichText::new(e).color(theme::DANGER).small());
        }
        ui.add_space(6.0);
        if ui.add_sized([ui.available_width(), 34.0], Button::new("📂 Choose brand kit folder…").fill(theme::SURFACE)).clicked() {
            choose = true;
        }
        if ui.add_sized([ui.available_width(), 30.0], Button::new("➕ Create a new brand kit…")).clicked() {
            create = true;
        }
        finish(app, choose, create, false);
        return;
    };

    ui.horizontal(|ui| {
        ui.label(RichText::new(kit.display_name()).strong().size(15.0));
        if !kit.info.handle.is_empty() {
            ui.label(widgets::dim(&kit.info.handle));
        }
    });
    ui.horizontal(|ui| {
        if ui.small_button("📂 Open folder").on_hover_text(kit.root.display().to_string()).clicked() {
            crate::ui::library::reveal(&kit.root.join(bk::BRAND_DIR));
        }
        if ui.small_button("🔄 Rescan").on_hover_text("Pick up files added to the folder").clicked() {
            rescan = true;
        }
        if ui.small_button("Change…").clicked() {
            choose = true;
        }
    });
    for w in &kit.warnings {
        ui.label(RichText::new(format!("⚠ {w}")).color(theme::DANGER).small());
    }

    let mut action = None;
    if !kit.info.colors.is_empty() {
        ui.add_space(8.0);
        ui.label(RichText::new("Colours").strong());
        ui.horizontal_wrapped(|ui| {
            for c in &kit.info.colors {
                let Some(rgb) = c.rgb() else { continue };
                let (rect, resp) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::click());
                ui.painter().rect_filled(rect, 6.0, Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                ui.painter().rect_stroke(rect, 6.0, Stroke::new(1.0_f32, if resp.hovered() { theme::ACCENT } else { theme::BORDER }));
                if resp.on_hover_text(format!("{} {}\nClick: copy the hex and use it for the next motion graphic", c.name, c.hex)).clicked() {
                    action = Some(Action::Color(c.hex.clone(), rgb));
                }
            }
        });
    }
    if !kit.info.fonts.is_empty() {
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Fonts").strong());
            for f in &kit.info.fonts {
                ui.label(RichText::new(f).font(FontId::new(13.0, theme::text_family(f, false))));
                ui.label(widgets::dim("·"));
            }
        });
    }

    ui.add_space(8.0);
    ui.label(widgets::dim("Double-click to add at the playhead · right-click for more").small());
    if !kit.logos.is_empty() {
        ui.add_space(6.0);
        ui.label(RichText::new("Logos").strong());
        grid(ui, app, &kit.logos, true, &mut action);
    }
    for g in &kit.groups {
        ui.add_space(8.0);
        ui.label(RichText::new(&g.name).strong());
        grid(ui, app, &g.items, false, &mut action);
    }
    if kit.logos.is_empty() && kit.groups.is_empty() {
        ui.add_space(10.0);
        ui.label(widgets::dim(format!(
            "The kit is empty. Put logos in \"{}\" and reusable clips in subfolders of \"{}\" (e.g. Intros, End Cards), then click Rescan.",
            bk::BRAND_DIR,
            bk::ASSETS_DIR
        )));
    }

    match action {
        Some(Action::Add(path, how)) => {
            let start = app.playhead;
            match bk::add_to_project(&mut app.project, &path, start, how) {
                Ok((media, clip)) => {
                    app.selected_media = Some(media);
                    if let Some(c) = clip {
                        app.selection = Some(c);
                        if let Some(s) = app.project.clip(c).map(|c| c.start) {
                            app.reveal(s);
                        }
                    } else {
                        app.notify("Added to the media library");
                    }
                }
                Err(e) => app.notify_error(format!("Couldn't add {}: {e:#}", path.display())),
            }
        }
        Some(Action::Reveal(p)) => crate::ui::library::reveal(&p),
        Some(Action::Color(hex, rgb)) => {
            ui.ctx().copy_text(hex.clone());
            app.motion_graphics.set_color(rgb);
            app.notify(format!("Copied {hex} — the next motion graphic will use it"));
        }
        None => {}
    }
    finish(app, choose, create, rescan);
}

fn finish(app: &mut EditorApp, choose: bool, create: bool, rescan: bool) {
    if rescan {
        if let Some(root) = app.brand_kit.kit.as_ref().map(|k| k.root.clone()) {
            app.brand_kit.load(&root);
            app.notify("Brand kit rescanned");
        }
    }
    if !(choose || create) {
        return;
    }
    let title = if create { "Choose or make a folder for the new brand kit" } else { "Choose the brand kit folder" };
    let mut dlg = rfd::FileDialog::new().set_title(title);
    if let Some(d) = app.brand_kit.kit.as_ref().map(|k| k.root.clone()).or_else(bk::saved_folder) {
        dlg = dlg.set_directory(d.parent().unwrap_or(&d));
    }
    let Some(dir) = dlg.pick_folder() else { return };
    // A kit folder picked by its "Brand Kit" or "Assets" subfolder means its parent.
    let dir = match dir.file_name().map(|n| n.to_string_lossy().to_lowercase()) {
        Some(n) if n == bk::BRAND_DIR.to_lowercase() || n == bk::ASSETS_DIR.to_lowercase() => dir.parent().map(Path::to_path_buf).unwrap_or(dir),
        _ => dir,
    };
    if create {
        if let Err(e) = bk::init(&dir) {
            app.notify_error(format!("Couldn't create the brand kit: {e:#}"));
            return;
        }
    }
    if let Err(e) = bk::save_folder(&dir) {
        app.notify_error(format!("Couldn't remember the folder: {e:#}"));
    }
    app.brand_kit.load(&dir);
    if create {
        app.notify("Brand kit created — edit Brand Kit/brand.json for colours and fonts");
    }
}

fn grid(ui: &mut Ui, app: &mut EditorApp, items: &[BrandAsset], logos: bool, action: &mut Option<Action>) {
    let avail = ui.available_width();
    let cols = ((avail + 8.0) / 120.0).floor().max(2.0);
    let w = ((avail - 8.0 * (cols - 1.0)) / cols).floor();
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
        for a in items {
            if let Some(act) = card(ui, app, a, w, logos) {
                *action = Some(act);
            }
        }
    });
}

fn card(ui: &mut Ui, app: &mut EditorApp, a: &BrandAsset, w: f32, logo: bool) -> Option<Action> {
    let th = (w * 9.0 / 16.0).round();
    let (rect, resp) = ui.allocate_exact_size(vec2(w, th + 22.0), Sense::click());
    let thumb = Rect::from_min_size(rect.min, vec2(w, th));
    let painter = ui.painter_at(rect);
    painter.rect_filled(thumb, 6.0, theme::SURFACE);
    let tex = app.brand_kit.thumb(ui.ctx(), a).map(|t| (t.id(), t.size_vec2()));
    match tex {
        Some((id, ts)) => {
            // Fit inside the card (logos aren't cropped).
            let s = (thumb.width() / ts.x).min(thumb.height() / ts.y);
            let r = Rect::from_center_size(thumb.center(), ts * s);
            painter.image(id, r.shrink(1.0), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => {
            let icon = match a.kind {
                MediaKind::Audio => "🎵",
                MediaKind::Image => "🖼",
                MediaKind::Video => "🎬",
            };
            painter.text(thumb.center(), Align2::CENTER_CENTER, icon, FontId::proportional(24.0), theme::TEXT_DIM);
        }
    }
    if let Some(d) = app.brand_kit.durations.get(&a.path) {
        let g = painter.layout_no_wrap(widgets::format_short(*d), FontId::proportional(10.5), Color32::WHITE);
        let r = Rect::from_min_size(thumb.right_bottom() - g.size() - vec2(10.0, 6.0), g.size() + vec2(6.0, 2.0));
        painter.rect_filled(r, 3.0, Color32::from_black_alpha(170));
        painter.galley(r.min + vec2(3.0, 1.0), g, Color32::WHITE);
    }
    let in_project = bk::media_for(&app.project, &a.path).is_some();
    let name_rect = Rect::from_min_max(pos2(rect.left(), thumb.bottom() + 3.0), rect.right_bottom());
    painter.with_clip_rect(name_rect).text(
        name_rect.left_top(),
        Align2::LEFT_TOP,
        &a.name,
        FontId::proportional(11.5),
        if in_project { theme::TEXT } else { theme::TEXT_DIM },
    );
    if resp.hovered() {
        painter.rect_stroke(thumb, 6.0, Stroke::new(1.5_f32, theme::BORDER));
        let plus = thumb.right_top() + vec2(-14.0, 14.0);
        painter.circle_filled(plus, 10.0, theme::ACCENT);
        painter.text(plus, Align2::CENTER_CENTER, "+", FontId::proportional(16.0), Color32::BLACK);
    }
    // Logos default to a corner bug, other pictures and clips to the timeline.
    let default = if logo && a.kind == MediaKind::Image { Placement::Corner } else { Placement::Timeline };
    let mut action = None;
    let resp = resp.on_hover_text(format!(
        "{}\n{}",
        a.name,
        if default == Placement::Corner { "Double-click: logo in the top-right corner" } else { "Double-click: add at the playhead" }
    ));
    let plus_hit = resp.interact_pointer_pos().is_some_and(|p| Rect::from_center_size(thumb.right_top() + vec2(-14.0, 14.0), vec2(20.0, 20.0)).contains(p));
    if resp.double_clicked() || (resp.clicked() && plus_hit) {
        action = Some(Action::Add(a.path.clone(), default));
    }
    resp.context_menu(|ui| {
        let mut item = |ui: &mut Ui, label: &str, how: Placement| {
            if ui.button(label).clicked() {
                action = Some(Action::Add(a.path.clone(), how));
                ui.close_menu();
            }
        };
        if a.kind == MediaKind::Audio {
            item(ui, "➕ Add at playhead", Placement::Timeline);
        } else {
            item(ui, "➕ Add at playhead", Placement::Timeline);
            item(ui, "⬆ Add as full-frame overlay", Placement::Overlay);
            item(ui, "↗ Add as corner logo", Placement::Corner);
        }
        item(ui, "📥 Import to media library only", Placement::LibraryOnly);
        ui.separator();
        if ui.button("📂 Show in folder").clicked() {
            action = Some(Action::Reveal(a.path.clone()));
            ui.close_menu();
        }
    });
    action
}
