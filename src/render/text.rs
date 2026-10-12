//! Text metrics and fonts shared by preview and export.

use crate::model::TextStyle;
use std::path::PathBuf;

/// Text sizes are authored against a 1080 px short side.
pub const REFERENCE_SHORT_SIDE: f64 = 1080.0;

/// A TrueType font available on this system, used both by the preview (loaded into
/// egui) and by FFmpeg's `drawtext` during export.
pub fn font_path(bold: bool) -> Option<PathBuf> {
    let windir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    let candidates: Vec<String> = if cfg!(windows) {
        let f = |n: &str| format!("{windir}\\Fonts\\{n}");
        if bold {
            vec![f("arialbd.ttf"), f("segoeuib.ttf")]
        } else {
            vec![f("arial.ttf"), f("segoeui.ttf")]
        }
    } else if cfg!(target_os = "macos") {
        if bold {
            vec!["/System/Library/Fonts/Supplemental/Arial Bold.ttf".into()]
        } else {
            vec!["/System/Library/Fonts/Supplemental/Arial.ttf".into()]
        }
    } else if bold {
        vec![
            "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf".into(),
            "/usr/share/fonts/TTF/DejaVuSans-Bold.ttf".into(),
        ]
    } else {
        vec![
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".into(),
            "/usr/share/fonts/TTF/DejaVuSans.ttf".into(),
        ]
    };
    candidates.into_iter().map(PathBuf::from).find(|p| p.exists())
}

fn load_font(bold: bool) -> Option<&'static ab_glyph::FontVec> {
    static FONTS: std::sync::OnceLock<[Option<ab_glyph::FontVec>; 2]> = std::sync::OnceLock::new();
    let fonts = FONTS.get_or_init(|| {
        let load = |b: bool| font_path(b).and_then(|p| std::fs::read(p).ok()).and_then(|d| ab_glyph::FontVec::try_from_vec(d).ok());
        [load(false), load(true)]
    });
    fonts[usize::from(bold)].as_ref()
}

/// Width in pixels of one line of `text` at `font_px` (em size, as FreeType/drawtext
/// use it), with kerning. `None` if the font can't be loaded.
pub fn line_width_px(text: &str, bold: bool, font_px: f64) -> Option<f64> {
    use ab_glyph::Font;
    let font = load_font(bold)?;
    let upem = font.units_per_em()? as f64;
    let mut w = 0.0f64;
    let mut prev: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        let g = font.glyph_id(ch);
        if let Some(p) = prev {
            w += font.kern_unscaled(p, g) as f64;
        }
        w += font.h_advance_unscaled(g) as f64;
        prev = Some(g);
    }
    Some(w * font_px / upem)
}

/// Font size in output pixels.
pub fn font_px(style_size: f32, scale: f32, short_side: f64) -> f64 {
    style_size as f64 * scale as f64 * short_side / REFERENCE_SHORT_SIDE
}

pub fn box_padding(font_px: f64) -> f64 {
    font_px * 0.25
}

pub fn outline_width(font_px: f64) -> f64 {
    (font_px * 0.06).max(1.0)
}

pub fn presets() -> Vec<(&'static str, TextStyle)> {
    let base = TextStyle::default();
    vec![
        ("Default", TextStyle { content: "Add text".into(), ..base.clone() }),
        (
            "Title",
            TextStyle { content: "Big Title".into(), font_size: 120.0, ..base.clone() },
        ),
        (
            "Subtitle",
            TextStyle {
                content: "Subtitle goes here".into(),
                font_size: 48.0,
                bold: false,
                outline: None,
                background: Some([0, 0, 0, 170]),
                ..base.clone()
            },
        ),
        (
            "Caption",
            TextStyle {
                content: "CAPTION".into(),
                font_size: 64.0,
                color: [255, 222, 40, 255],
                ..base.clone()
            },
        ),
        (
            "Neon",
            TextStyle {
                content: "Neon vibes".into(),
                font_size: 90.0,
                color: [120, 255, 250, 255],
                outline: Some([255, 0, 170, 255]),
                ..base.clone()
            },
        ),
        (
            "Lower third",
            TextStyle {
                content: "Jane Doe — Producer".into(),
                font_size: 44.0,
                outline: None,
                background: Some([0, 170, 190, 230]),
                ..base
            },
        ),
    ]
}
