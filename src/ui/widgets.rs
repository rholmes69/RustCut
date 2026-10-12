//! Small shared UI helpers.

use crate::media::ffmpeg::PEAKS_PER_SEC;
use crate::ui::theme;
use egui::{pos2, Color32, Painter, Rect, Response, RichText, Stroke, Ui};

/// `mm:ss.cc` (or `h:mm:ss.cc`).
pub fn format_time(t: f64) -> String {
    let total_cs = (t.max(0.0) * 100.0).round() as u64;
    let cs = total_cs % 100;
    let s = (total_cs / 100) % 60;
    let m = (total_cs / 6000) % 60;
    let h = total_cs / 360_000;
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{cs:02}")
    } else {
        format!("{m:02}:{s:02}.{cs:02}")
    }
}

/// `mm:ss` (or `h:mm:ss`).
pub fn format_short(t: f64) -> String {
    let s = t.max(0.0).round() as u64;
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m:02}:{s:02}")
    }
}

pub fn accent_button(ui: &mut Ui, text: &str) -> Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::BLACK).strong())
            .fill(theme::ACCENT)
            .rounding(6.0),
    )
}

pub fn section_title(ui: &mut Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).strong().size(13.0).color(theme::TEXT));
    ui.add_space(2.0);
}

pub fn dim(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).color(theme::TEXT_DIM)
}

pub fn with_alpha(c: [u8; 4], alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], (c[3] as f32 * alpha.clamp(0.0, 1.0)) as u8)
}

/// Draws waveform peaks into `area`; `time_at(x)` maps a screen x to source seconds.
pub fn draw_waveform(painter: &Painter, area: Rect, peaks: &[f32], color: Color32, time_at: impl Fn(f32) -> f64) {
    if peaks.is_empty() || area.width() <= 0.0 {
        return;
    }
    let cy = area.center().y;
    let half = area.height() * 0.48;
    let mut x = area.left();
    while x < area.right() {
        let t0 = time_at(x).max(0.0);
        let t1 = time_at(x + 2.0).max(t0);
        let i0 = (t0 * PEAKS_PER_SEC) as usize;
        if i0 < peaks.len() {
            let i1 = ((t1 * PEAKS_PER_SEC) as usize).max(i0 + 1).min(peaks.len());
            let p = peaks[i0..i1].iter().cloned().fold(0.0f32, f32::max);
            if p > 0.002 {
                let h = (p * half).max(0.5);
                painter.line_segment([pos2(x + 1.0, cy - h), pos2(x + 1.0, cy + h)], Stroke::new(1.5_f32, color));
            }
        }
        x += 2.0;
    }
}
