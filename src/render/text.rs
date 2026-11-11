//! Text metrics and fonts shared by preview and export.

use crate::model::{TextAnim, TextStyle};
use std::path::PathBuf;

/// Text sizes are authored against a 1080 px short side.
pub const REFERENCE_SHORT_SIDE: f64 = 1080.0;

/// A font bundled with RustCut (assets/fonts, SIL OFL / Apache 2.0), so text looks the
/// same in the preview and the export on every machine. Projects store its `id`.
pub struct Font {
    pub id: &'static str,
    pub name: &'static str,
    pub cursive: bool,
    regular: &'static [u8],
    bold: Option<&'static [u8]>,
}

macro_rules! font {
    ($id:literal, $name:literal, $cursive:literal) => {
        Font { id: $id, name: $name, cursive: $cursive, regular: include_bytes!(concat!("../../assets/fonts/", $id, ".ttf")), bold: None }
    };
    ($id:literal, $name:literal, $cursive:literal, bold) => {
        Font {
            id: $id,
            name: $name,
            cursive: $cursive,
            regular: include_bytes!(concat!("../../assets/fonts/", $id, ".ttf")),
            bold: Some(include_bytes!(concat!("../../assets/fonts/", $id, "-bold.ttf"))),
        }
    };
}

/// Bundled fonts besides the default (Arial / the system sans). Fonts without a bold
/// cut ignore the Bold switch.
pub const FONTS: &[Font] = &[
    font!("great-vibes", "Great Vibes", true),
    font!("pacifico", "Pacifico", true),
    font!("dancing-script", "Dancing Script", true, bold),
    font!("lobster", "Lobster", true),
    font!("satisfy", "Satisfy", true),
    font!("sacramento", "Sacramento", true),
    font!("caveat", "Caveat", true, bold),
    font!("bebas-neue", "Bebas Neue", false),
    font!("anton", "Anton", false),
    font!("montserrat", "Montserrat", false, bold),
    font!("poppins", "Poppins", false, bold),
    font!("oswald", "Oswald", false, bold),
    font!("playfair-display", "Playfair Display", false, bold),
    font!("permanent-marker", "Permanent Marker", false),
    font!("bangers", "Bangers", false),
];

/// The bundled font with this id; `None` for "" (the default) and unknown ids.
pub fn find_font(id: &str) -> Option<&'static Font> {
    FONTS.iter().find(|f| f.id == id)
}

/// Display name of a font id ("Default" for the system sans).
pub fn font_name(id: &str) -> &'static str {
    find_font(id).map_or("Default", |f| f.name)
}

/// Accepts a font id or its display name, any case ("Great Vibes", "great-vibes");
/// "" / "default" mean the system sans.
pub fn parse_font(s: &str) -> Option<&'static str> {
    let key: String = s.trim().to_lowercase().replace([' ', '_'], "-");
    if key.is_empty() || key == "default" {
        return Some("");
    }
    FONTS.iter().find(|f| f.id == key || f.name.eq_ignore_ascii_case(s.trim())).map(|f| f.id)
}

/// The TTF bytes for a bundled font, or `None` for the system default.
fn bundled_bytes(font: &str, bold: bool) -> Option<&'static [u8]> {
    find_font(font).map(|f| if bold { f.bold.unwrap_or(f.regular) } else { f.regular })
}

/// A TrueType font file for `font` (a bundled id, or "" for the system sans), used both by
/// the preview (loaded into egui) and by FFmpeg's `drawtext` during export. Bundled fonts
/// are written once to a cache folder so FFmpeg can read them.
pub fn font_path(font: &str, bold: bool) -> Option<PathBuf> {
    let Some(bytes) = bundled_bytes(font, bold) else { return system_font_path(bold) };
    let has_bold = find_font(font).is_some_and(|f| f.bold.is_some());
    let file = format!("{font}{}.ttf", if bold && has_bold { "-bold" } else { "" });
    let path = std::env::temp_dir().join("rustcut-fonts").join(file);
    if std::fs::metadata(&path).map(|m| m.len()).ok() != Some(bytes.len() as u64) {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, bytes).ok()?;
    }
    Some(path)
}

/// The TTF bytes the preview loads for `font`.
pub fn font_bytes(font: &str, bold: bool) -> Option<Vec<u8>> {
    match bundled_bytes(font, bold) {
        Some(b) => Some(b.to_vec()),
        None => system_font_path(bold).and_then(|p| std::fs::read(p).ok()),
    }
}

/// The system sans (Arial on Windows/macOS, DejaVu Sans on Linux).
pub fn system_font_path(bold: bool) -> Option<PathBuf> {
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

fn load_font(font: &str, bold: bool) -> Option<&'static ab_glyph::FontVec> {
    type Cache = std::sync::Mutex<std::collections::HashMap<(String, bool), Option<&'static ab_glyph::FontVec>>>;
    static FONTS: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let mut fonts = FONTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
    *fonts.entry((font.to_string(), bold)).or_insert_with(|| {
        let f = font_bytes(font, bold).and_then(|d| ab_glyph::FontVec::try_from_vec(d).ok())?;
        Some(Box::leak(Box::new(f)))
    })
}

/// Width in pixels of one line of `text` at `font_px` (em size, as FreeType/drawtext
/// use it), with kerning. `None` if the font can't be loaded.
pub fn line_width_px(text: &str, font: &str, bold: bool, font_px: f64) -> Option<f64> {
    use ab_glyph::Font;
    let font = load_font(font, bold)?;
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

/// A text style made for one platform, with where it sits on screen.
pub struct SocialPreset {
    pub name: &'static str,
    pub platform: &'static str,
    pub style: TextStyle,
    /// Vertical centre offset as a fraction of canvas height. Captions sit a little
    /// below centre, clear of TikTok/Reels/Shorts buttons and descriptions.
    pub y: f32,
}

/// Platform groups in the order the library shows them.
pub const PLATFORMS: &[&str] = &["YouTube", "TikTok", "Shorts & Reels", "Instagram", "Any platform"];

/// Text styles for social video: YouTube, TikTok, Shorts, Reels and Instagram.
pub fn social_presets() -> Vec<SocialPreset> {
    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];
    let s = |content: &str, font: &str, size: f32, bold: bool, color: [u8; 4], background: Option<[u8; 4]>, outline: Option<[u8; 4]>| TextStyle {
        content: content.into(),
        font_size: size,
        font: font.into(),
        bold,
        color,
        background,
        outline,
        align_to: None,
    };
    let p = |name, platform, style, y| SocialPreset { name, platform, style, y };
    vec![
        p("YouTube title", "YouTube", s("THIS CHANGED\nEVERYTHING", "anton", 120.0, false, WHITE, None, Some(BLACK)), -0.28),
        p("Subscribe", "YouTube", s("SUBSCRIBE", "montserrat", 56.0, true, WHITE, Some([229, 32, 27, 255]), None), 0.36),
        p("Chapter title", "YouTube", s("Chapter 1 — The Beginning", "oswald", 60.0, true, WHITE, Some([0, 0, 0, 185]), None), 0.36),
        p("TikTok caption", "TikTok", s("wait for the end", "montserrat", 64.0, true, WHITE, None, Some(BLACK)), 0.12),
        p("TikTok label", "TikTok", s("POV: you finally did it", "poppins", 64.0, true, BLACK, Some(WHITE), None), -0.25),
        p("Comment reply", "TikTok", s("Reply to @username\nhow did you do this?", "poppins", 52.0, true, [22, 22, 22, 255], Some(WHITE), None), -0.28),
        p("Viral caption", "Shorts & Reels", s("NO WAY\nTHIS WORKED", "montserrat", 92.0, true, [255, 228, 0, 255], None, Some(BLACK)), 0.12),
        p("Hook", "Shorts & Reels", s("WATCH TILL THE END", "anton", 110.0, false, WHITE, Some([229, 32, 27, 255]), None), -0.3),
        p("Word pop", "Shorts & Reels", s("CRAZY", "bangers", 150.0, false, [80, 255, 120, 255], None, Some(BLACK)), 0.1),
        p("Reels caption", "Instagram", s("golden hour hits different", "poppins", 58.0, true, WHITE, Some([0, 0, 0, 140]), None), 0.18),
        p("Story script", "Instagram", s("good vibes only", "pacifico", 84.0, false, WHITE, None, Some([255, 64, 160, 255])), -0.2),
        p("Story strong", "Instagram", s("NEW DROP", "bebas-neue", 120.0, false, BLACK, Some([255, 214, 0, 255]), None), -0.2),
        p("Handle", "Any platform", s("@yourhandle", "poppins", 50.0, true, WHITE, Some([0, 0, 0, 150]), None), 0.4),
        p("Meme", "Any platform", s("TOP TEXT", "anton", 100.0, false, WHITE, None, Some(BLACK)), -0.38),
        p("Handwritten note", "Any platform", s("you've got this", "permanent-marker", 72.0, false, WHITE, None, Some(BLACK)), 0.0),
    ]
}

/// An animated social style: a social preset's look and position plus a motion.
pub struct AnimatedPreset {
    pub name: &'static str,
    pub platform: &'static str,
    pub style: TextStyle,
    pub y: f32,
    pub anim: TextAnim,
    /// What the animation does, for the library tooltip.
    pub about: &'static str,
}

/// Animated text styles for social video, built on the social looks.
pub fn animated_presets() -> Vec<AnimatedPreset> {
    let social = social_presets();
    let base = |name: &str| social.iter().find(|p| p.name == name).expect("social preset");
    let a = |name, platform, from: &str, content: &str, anim, about| {
        let b = base(from);
        AnimatedPreset { name, platform, style: TextStyle { content: content.into(), ..b.style.clone() }, y: b.y, anim, about }
    };
    vec![
        a("Drop-in title", "YouTube", "YouTube title", "THIS CHANGED\nEVERYTHING", TextAnim::DropIn, "Falls in from above and settles with a bounce"),
        a("Pulsing subscribe", "YouTube", "Subscribe", "SUBSCRIBE", TextAnim::Pulse, "Gently grows and shrinks to draw the eye"),
        a("Slide-up chapter", "YouTube", "Chapter title", "Chapter 1 — The Beginning", TextAnim::SlideUp, "Rises and fades in"),
        a("Word-by-word caption", "TikTok", "TikTok caption", "wait for the end of this", TextAnim::WordByWord, "Words appear one after another, like auto captions"),
        a("One-word caption", "TikTok", "Viral caption", "THIS IS INSANE", TextAnim::OneWord, "One big word at a time, each popping in"),
        a("Bouncy label", "TikTok", "TikTok label", "POV: you finally did it", TextAnim::BounceUp, "Bounces up into place"),
        a("Pop caption", "Shorts & Reels", "Viral caption", "NO WAY\nTHIS WORKED", TextAnim::Pop, "Pops in with a little overshoot"),
        a("Slam hook", "Shorts & Reels", "Hook", "WATCH TILL THE END", TextAnim::Slam, "Slams down from huge and jolts on impact"),
        a("Shaking word", "Shorts & Reels", "Word pop", "CRAZY", TextAnim::Shake, "Shakes side to side, then settles"),
        a("Neon flicker", "Instagram", "Story script", "good vibes only", TextAnim::Flicker, "Flickers on like a neon sign"),
        a("Typewriter note", "Instagram", "Reels caption", "golden hour hits different", TextAnim::Typewriter, "Types out letter by letter"),
        a("Typewriter handle", "Any platform", "Handle", "@yourhandle", TextAnim::Typewriter, "Types out letter by letter"),
    ]
}

/// A preset found by name: its look, its screen position (social styles) and its
/// animation (animated styles).
pub struct PresetHit {
    pub style: TextStyle,
    pub y: Option<f32>,
    pub anim: Option<TextAnim>,
}

/// A preset by name (case and spacing don't matter): the classic styles first, then
/// the social ones, then the animated ones.
pub fn find_preset(name: &str) -> Option<PresetHit> {
    let key = |s: &str| s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
    let want = key(name);
    presets()
        .into_iter()
        .find(|(n, _)| key(n) == want)
        .map(|(_, style)| PresetHit { style, y: None, anim: None })
        .or_else(|| social_presets().into_iter().find(|p| key(p.name) == want).map(|p| PresetHit { style: p.style, y: Some(p.y), anim: None }))
        .or_else(|| animated_presets().into_iter().find(|p| key(p.name) == want).map(|p| PresetHit { style: p.style, y: Some(p.y), anim: Some(p.anim) }))
}

/// Every preset name, for tool descriptions and error messages.
pub fn preset_names() -> Vec<&'static str> {
    presets()
        .into_iter()
        .map(|(n, _)| n)
        .chain(social_presets().into_iter().map(|p| p.name))
        .chain(animated_presets().into_iter().map(|p| p.name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_fonts_load_and_measure() {
        assert_eq!(FONTS.len(), 15);
        for f in FONTS {
            for bold in [false, true] {
                let w = line_width_px("Hold on", f.id, bold, 72.0).unwrap_or_else(|| panic!("{} did not load", f.id));
                assert!(w > 50.0 && w < 600.0, "{} width {w}", f.id);
            }
            let path = font_path(f.id, false).unwrap();
            assert_eq!(std::fs::metadata(path).unwrap().len(), f.regular.len() as u64);
        }
    }

    #[test]
    fn social_presets_use_bundled_fonts_and_resolve() {
        for p in social_presets() {
            assert!(find_font(&p.style.font).is_some(), "{}: unknown font {}", p.name, p.style.font);
            assert!(PLATFORMS.contains(&p.platform), "{}: platform {}", p.name, p.platform);
            assert!(p.y.abs() <= 0.42, "{} sits off screen", p.name);
            assert_eq!(find_preset(p.name).and_then(|h| h.y), Some(p.y));
        }
        for p in animated_presets() {
            assert!(PLATFORMS.contains(&p.platform), "{}: platform {}", p.name, p.platform);
            assert_eq!(find_preset(p.name).and_then(|h| h.anim), Some(p.anim));
        }
        assert_eq!(find_preset("tiktok_caption").map(|h| h.style.font), Some("montserrat".to_string()));
        assert!(find_preset("title").is_some_and(|h| h.y.is_none() && h.anim.is_none()));
        assert!(find_preset("nope").is_none());
    }

    #[test]
    fn parses_ids_and_names() {
        assert_eq!(parse_font("Great Vibes"), Some("great-vibes"));
        assert_eq!(parse_font("dancing_script"), Some("dancing-script"));
        assert_eq!(parse_font("default"), Some(""));
        assert_eq!(parse_font(""), Some(""));
        assert_eq!(parse_font("comic sans"), None);
        assert_eq!(font_name("nope"), "Default");
    }
}
