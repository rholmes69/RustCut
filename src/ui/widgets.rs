//! Small shared UI helpers.

use crate::media::ffmpeg::PEAKS_PER_SEC;
use crate::ui::theme;
use egui::{pos2, Color32, Painter, Rect, Response, RichText, Stroke, Ui};

/// `mm:ss.cc` (or `h:mm:ss.cc`).
/// Reads a time typed by a person: "158.9", "2:38.9", "1:02:03", "02:38.90" (as shown
/// on the timecode), "2m38s", "2m 38.5s", "1h2m", "95 seconds", "1 minute 30 seconds".
pub fn parse_time(s: &str) -> Option<f64> {
    let s = s.trim().trim_end_matches('.').to_ascii_lowercase();
    if s.is_empty() {
        return None;
    }
    let ok = |t: f64| (t.is_finite() && t >= 0.0).then_some(t);
    if s.contains(':') {
        let mut t = 0.0;
        let parts: Vec<&str> = s.split_whitespace().next()?.split(':').collect();
        if parts.len() > 3 {
            return None;
        }
        for (i, part) in parts.iter().enumerate() {
            let v: f64 = part.parse().ok()?;
            // Only the last field may have a fraction; earlier ones are whole minutes/hours.
            if (i + 1 < parts.len() && v.fract() != 0.0) || (i > 0 && v >= 60.0) {
                return None;
            }
            t = t * 60.0 + v;
        }
        return ok(t);
    }
    if let Ok(v) = s.parse::<f64>() {
        return ok(v);
    }
    // Numbers with units, written together ("2m38s") or apart ("2 minutes 38 seconds").
    let (mut t, mut num, mut any) = (0.0, String::new(), false);
    let mut chars = s.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() || c == '.' {
            num.push(c);
            chars.next();
        } else if c.is_alphabetic() {
            let mut unit = String::new();
            while let Some(&u) = chars.peek().filter(|u| u.is_alphabetic()) {
                unit.push(u);
                chars.next();
            }
            if unit == "and" {
                continue;
            }
            let n: f64 = std::mem::take(&mut num).parse().ok()?;
            t += n * match unit.as_str() {
                "h" | "hr" | "hrs" | "hour" | "hours" => 3600.0,
                "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
                "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
                _ => return None,
            };
            any = true;
        } else if c.is_whitespace() || c == ',' {
            chars.next();
        } else {
            return None;
        }
    }
    if !num.is_empty() {
        t += num.parse::<f64>().ok()?; // a trailing bare number counts as seconds: "2m 30"
        any = true;
    }
    if any {
        ok(t)
    } else {
        None
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_typed_times() {
        let t = |s: &str| parse_time(s);
        assert_eq!(t("158.9"), Some(158.9));
        assert_eq!(t("2:38.9"), Some(158.9));
        assert_eq!(t("02:38.90"), Some(158.9));
        assert_eq!(t("1:02:03"), Some(3723.0));
        assert_eq!(t("2m38s"), Some(158.0));
        assert_eq!(t("2m 38.5s"), Some(158.5));
        assert_eq!(t("2m 30"), Some(150.0));
        assert_eq!(t("1h2m"), Some(3720.0));
        assert_eq!(t("95 seconds"), Some(95.0));
        assert_eq!(t("1 minute and 30 seconds"), Some(90.0));
        assert_eq!(t("1:20 please"), Some(80.0));
        // Each time shown on the timecode reads back as itself.
        for v in [0.0, 5.25, 158.9, 3723.5] {
            assert!((t(&format_time(v)).unwrap() - v).abs() < 0.006, "{v}");
        }
        for bad in ["", "abc", "1:75", "1.5:20", "-3", "2 parsecs", "1:2:3:4"] {
            assert_eq!(t(bad), None, "{bad}");
        }
    }
}
