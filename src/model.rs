//! Project data model: the media pool, tracks and clips.
//!
//! Everything here is plain data (serde-serialisable and `PartialEq`) so the whole
//! project can be snapshotted for undo/redo and saved as JSON.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub type Id = u64;

pub const MIN_CLIP_LEN: f64 = 0.1;
pub const DEFAULT_STILL_LEN: f64 = 5.0;
pub const DEFAULT_TEXT_LEN: f64 = 3.0;
const EPS: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    Video,
    Image,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MediaItem {
    pub id: Id,
    pub path: PathBuf,
    pub name: String,
    pub kind: MediaKind,
    /// Seconds. Zero for still images.
    pub duration: f64,
    /// Display size (rotation metadata already applied).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
}

impl MediaItem {
    pub fn has_video(&self) -> bool {
        self.kind != MediaKind::Audio
    }

    pub fn has_sound(&self) -> bool {
        self.has_audio && self.kind != MediaKind::Image
    }

    /// Length of the source, `None` for stills which can be stretched indefinitely.
    pub fn source_len(&self) -> Option<f64> {
        match self.kind {
            MediaKind::Image => None,
            _ => Some(self.duration),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Aspect {
    #[default]
    Landscape,
    Portrait,
    Square,
    Classic,
    Social,
}

impl Aspect {
    pub const ALL: [Aspect; 5] = [
        Aspect::Landscape,
        Aspect::Portrait,
        Aspect::Square,
        Aspect::Classic,
        Aspect::Social,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Aspect::Landscape => "16:9  YouTube",
            Aspect::Portrait => "9:16  TikTok / Reels / Shorts",
            Aspect::Square => "1:1  Square",
            Aspect::Classic => "4:3  Classic",
            Aspect::Social => "4:5  Instagram post",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Aspect::Landscape => "16:9",
            Aspect::Portrait => "9:16",
            Aspect::Square => "1:1",
            Aspect::Classic => "4:3",
            Aspect::Social => "4:5",
        }
    }

    pub fn ratio(self) -> (u32, u32) {
        match self {
            Aspect::Landscape => (16, 9),
            Aspect::Portrait => (9, 16),
            Aspect::Square => (1, 1),
            Aspect::Classic => (4, 3),
            Aspect::Social => (4, 5),
        }
    }

    /// Canvas size in pixels for a given short side (e.g. 1080 → 1920×1080 for 16:9).
    pub fn size_for(self, short_side: u32) -> (u32, u32) {
        let (a, b) = self.ratio();
        let long = |num: u32, den: u32| {
            let v = (short_side as f64 * num as f64 / den as f64).round() as u32;
            v + v % 2
        };
        if a >= b {
            (long(a, b), short_side)
        } else {
            (short_side, long(b, a))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum FilterPreset {
    #[default]
    None,
    BlackWhite,
    Vintage,
    Warm,
    Cool,
    Vivid,
    Matte,
}

impl FilterPreset {
    pub const ALL: [FilterPreset; 7] = [
        FilterPreset::None,
        FilterPreset::BlackWhite,
        FilterPreset::Vintage,
        FilterPreset::Warm,
        FilterPreset::Cool,
        FilterPreset::Vivid,
        FilterPreset::Matte,
    ];

    pub fn label(self) -> &'static str {
        match self {
            FilterPreset::None => "None",
            FilterPreset::BlackWhite => "B&W",
            FilterPreset::Vintage => "Vintage",
            FilterPreset::Warm => "Warm",
            FilterPreset::Cool => "Cool",
            FilterPreset::Vivid => "Vivid",
            FilterPreset::Matte => "Matte",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Adjust {
    /// -1..1, added to every channel.
    pub brightness: f32,
    /// Multiplier around mid-grey.
    pub contrast: f32,
    /// Multiplier on chroma; 0 = greyscale.
    pub saturation: f32,
}

impl Default for Adjust {
    fn default() -> Self {
        Self { brightness: 0.0, contrast: 1.0, saturation: 1.0 }
    }
}

impl Adjust {
    pub fn is_identity(&self) -> bool {
        self.brightness.abs() < 1e-3
            && (self.contrast - 1.0).abs() < 1e-3
            && (self.saturation - 1.0).abs() < 1e-3
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    /// Offset of the layer centre from the canvas centre, as a fraction of canvas width.
    pub x: f32,
    /// Offset of the layer centre from the canvas centre, as a fraction of canvas height.
    pub y: f32,
    /// Multiplier on the "contain" fit inside the canvas.
    pub scale: f32,
    /// Degrees, clockwise.
    pub rotation: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self { x: 0.0, y: 0.0, scale: 1.0, rotation: 0.0 }
    }
}

/// A property that can be animated with keyframes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Prop {
    X,
    Y,
    Scale,
    Rotation,
    Opacity,
    Volume,
    /// Gaussian blur radius in pixels (at a 1080 px short side).
    Blur,
    /// Glow 0..1: brighter, more saturated picture.
    Glow,
}

impl Prop {
    pub const ALL: [Prop; 8] = [Prop::X, Prop::Y, Prop::Scale, Prop::Rotation, Prop::Opacity, Prop::Volume, Prop::Blur, Prop::Glow];

    pub fn name(self) -> &'static str {
        match self {
            Prop::X => "x",
            Prop::Y => "y",
            Prop::Scale => "scale",
            Prop::Rotation => "rotation",
            Prop::Opacity => "opacity",
            Prop::Volume => "volume",
            Prop::Blur => "blur",
            Prop::Glow => "glow",
        }
    }

    pub fn parse(s: &str) -> Option<Prop> {
        Prop::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(s.trim()))
    }

    /// The static (un-animated) value on a clip.
    pub fn base(self, c: &Clip) -> f32 {
        match self {
            Prop::X => c.transform.x,
            Prop::Y => c.transform.y,
            Prop::Scale => c.transform.scale,
            Prop::Rotation => c.transform.rotation,
            Prop::Opacity => c.opacity,
            Prop::Volume => c.volume,
            Prop::Blur => c.blur,
            Prop::Glow => c.glow,
        }
    }

    pub fn set_base(self, c: &mut Clip, v: f32) {
        match self {
            Prop::X => c.transform.x = v,
            Prop::Y => c.transform.y = v,
            Prop::Scale => c.transform.scale = v.max(0.01),
            Prop::Rotation => c.transform.rotation = v,
            Prop::Opacity => c.opacity = v.clamp(0.0, 1.0),
            Prop::Volume => c.volume = v.max(0.0),
            Prop::Blur => c.blur = v.clamp(0.0, 100.0),
            Prop::Glow => c.glow = v.clamp(0.0, 1.0),
        }
    }
}

/// How a value travels from one keyframe to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Ease {
    #[default]
    Linear,
    Smooth,
    EaseIn,
    EaseOut,
    Hold,
}

impl Ease {
    pub const ALL: [Ease; 5] = [Ease::Linear, Ease::Smooth, Ease::EaseIn, Ease::EaseOut, Ease::Hold];

    pub fn label(self) -> &'static str {
        match self {
            Ease::Linear => "Linear",
            Ease::Smooth => "Smooth",
            Ease::EaseIn => "Ease in",
            Ease::EaseOut => "Ease out",
            Ease::Hold => "Hold",
        }
    }

    pub fn parse(s: &str) -> Option<Ease> {
        let k: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        Some(match k.as_str() {
            "linear" => Ease::Linear,
            "smooth" | "easeinout" => Ease::Smooth,
            "easein" | "in" => Ease::EaseIn,
            "easeout" | "out" => Ease::EaseOut,
            "hold" | "step" => Ease::Hold,
            _ => return None,
        })
    }

    /// Maps progress 0..1 to eased progress 0..1. Mirrored by the exporter's expressions.
    pub fn apply(self, p: f64) -> f64 {
        match self {
            Ease::Linear => p,
            Ease::Smooth => p * p * (3.0 - 2.0 * p),
            Ease::EaseIn => p * p,
            Ease::EaseOut => p * (2.0 - p),
            Ease::Hold => 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Seconds from the clip start.
    pub time: f64,
    pub value: f32,
    /// Easing of the segment that starts at this keyframe.
    #[serde(default)]
    pub ease: Ease,
}

/// What a clip's sound is, for auto-ducking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AudioRole {
    /// Imported audio files count as music; everything else as voice.
    #[default]
    Auto,
    /// Speech: music is lowered while it plays.
    Voice,
    /// Lowered under voice.
    Music,
    /// Neither triggers nor receives ducking.
    Effect,
}

impl AudioRole {
    pub const ALL: [AudioRole; 4] = [AudioRole::Auto, AudioRole::Voice, AudioRole::Music, AudioRole::Effect];

    pub fn name(self) -> &'static str {
        match self {
            AudioRole::Auto => "auto",
            AudioRole::Voice => "voice",
            AudioRole::Music => "music",
            AudioRole::Effect => "effect",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AudioRole::Auto => "Auto",
            AudioRole::Voice => "Voice",
            AudioRole::Music => "Music",
            AudioRole::Effect => "Sound effect",
        }
    }

    pub fn parse(s: &str) -> Option<AudioRole> {
        AudioRole::ALL.into_iter().find(|r| r.name().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoiseLevel {
    Light,
    Medium,
    Strong,
}

impl NoiseLevel {
    pub const ALL: [NoiseLevel; 3] = [NoiseLevel::Light, NoiseLevel::Medium, NoiseLevel::Strong];

    pub fn name(self) -> &'static str {
        match self {
            NoiseLevel::Light => "light",
            NoiseLevel::Medium => "medium",
            NoiseLevel::Strong => "strong",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NoiseLevel::Light => "Light",
            NoiseLevel::Medium => "Medium",
            NoiseLevel::Strong => "Strong",
        }
    }

    /// Noise reduction amount in dB for FFmpeg's `afftdn`.
    pub fn reduction_db(self) -> u32 {
        match self {
            NoiseLevel::Light => 12,
            NoiseLevel::Medium => 24,
            NoiseLevel::Strong => 36,
        }
    }

    pub fn parse(s: &str) -> Option<NoiseLevel> {
        NoiseLevel::ALL.into_iter().find(|n| n.name().eq_ignore_ascii_case(s.trim()))
    }
}

/// Noise removal settings for a clip; `floor_db` is measured from the source.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Denoise {
    pub level: NoiseLevel,
    pub floor_db: f32,
}

/// Project-wide audio settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioMix {
    /// Integrated loudness target for the export (LUFS), or `None` for no normalisation.
    pub loudness: Option<f32>,
    /// How far music is lowered under voice (dB, negative).
    pub duck_db: f32,
    /// Voice detection threshold on normalised waveform peaks (0..1).
    pub duck_sensitivity: f32,
    /// Gain computed by the exporter to hit `loudness` (not saved).
    #[serde(skip)]
    pub master_gain_db: Option<f32>,
}

impl Default for AudioMix {
    fn default() -> Self {
        Self { loudness: None, duck_db: -12.0, duck_sensitivity: 0.12, master_gain_db: None }
    }
}

pub const LOUDNESS_TARGETS: [(f32, &str); 3] =
    [(-14.0, "−14 LUFS · YouTube, TikTok, Spotify"), (-16.0, "−16 LUFS · Podcasts, Apple"), (-23.0, "−23 LUFS · Broadcast (EBU R128)")];

/// Transition styles. Each maps to an FFmpeg `xfade` transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Dissolve,
    DipBlack,
    DipWhite,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    ZoomIn,
}

impl TransitionKind {
    pub const ALL: [TransitionKind; 12] = [
        TransitionKind::Dissolve,
        TransitionKind::DipBlack,
        TransitionKind::DipWhite,
        TransitionKind::SlideLeft,
        TransitionKind::SlideRight,
        TransitionKind::SlideUp,
        TransitionKind::SlideDown,
        TransitionKind::WipeLeft,
        TransitionKind::WipeRight,
        TransitionKind::WipeUp,
        TransitionKind::WipeDown,
        TransitionKind::ZoomIn,
    ];

    pub fn name(self) -> &'static str {
        match self {
            TransitionKind::Dissolve => "dissolve",
            TransitionKind::DipBlack => "dip_black",
            TransitionKind::DipWhite => "dip_white",
            TransitionKind::SlideLeft => "slide_left",
            TransitionKind::SlideRight => "slide_right",
            TransitionKind::SlideUp => "slide_up",
            TransitionKind::SlideDown => "slide_down",
            TransitionKind::WipeLeft => "wipe_left",
            TransitionKind::WipeRight => "wipe_right",
            TransitionKind::WipeUp => "wipe_up",
            TransitionKind::WipeDown => "wipe_down",
            TransitionKind::ZoomIn => "zoom_in",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            TransitionKind::Dissolve => "Cross-dissolve",
            TransitionKind::DipBlack => "Dip to black",
            TransitionKind::DipWhite => "Dip to white",
            TransitionKind::SlideLeft => "Slide left",
            TransitionKind::SlideRight => "Slide right",
            TransitionKind::SlideUp => "Slide up",
            TransitionKind::SlideDown => "Slide down",
            TransitionKind::WipeLeft => "Wipe left",
            TransitionKind::WipeRight => "Wipe right",
            TransitionKind::WipeUp => "Wipe up",
            TransitionKind::WipeDown => "Wipe down",
            TransitionKind::ZoomIn => "Zoom in",
        }
    }

    /// The FFmpeg `xfade` transition name.
    pub fn xfade(self) -> &'static str {
        match self {
            TransitionKind::Dissolve => "fade",
            TransitionKind::DipBlack => "fadeblack",
            TransitionKind::DipWhite => "fadewhite",
            TransitionKind::SlideLeft => "slideleft",
            TransitionKind::SlideRight => "slideright",
            TransitionKind::SlideUp => "slideup",
            TransitionKind::SlideDown => "slidedown",
            TransitionKind::WipeLeft => "wipeleft",
            TransitionKind::WipeRight => "wiperight",
            TransitionKind::WipeUp => "wipeup",
            TransitionKind::WipeDown => "wipedown",
            TransitionKind::ZoomIn => "zoomin",
        }
    }

    pub fn parse(s: &str) -> Option<TransitionKind> {
        let k: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
        let k = match k.as_str() {
            "crossdissolve" | "crossfade" | "fade" => "dissolve".to_string(),
            "fadeblack" | "fadetoblack" => "dipblack".into(),
            "fadewhite" | "fadetowhite" => "dipwhite".into(),
            other => other.to_string(),
        };
        TransitionKind::ALL.into_iter().find(|t| t.name().replace('_', "") == k)
    }
}

/// A transition into a clip from the clip right before it on the same track.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    /// Requested length in seconds, centred on the cut.
    pub duration: f64,
}

pub const MAX_TRANSITION: f64 = 3.0;

/// A transition that applies on the timeline right now (both clips adjacent).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveTransition {
    pub kind: TransitionKind,
    pub from: Id,
    pub to: Id,
    /// Timeline time of the cut.
    pub cut: f64,
    /// Effective length: shortened so it never covers more than half of either clip.
    pub duration: f64,
}

impl ActiveTransition {
    pub fn start(&self) -> f64 {
        self.cut - self.duration / 2.0
    }

    pub fn end(&self) -> f64 {
        self.cut + self.duration / 2.0
    }

    pub fn contains(&self, t: f64) -> bool {
        t >= self.start() && t < self.end()
    }

    /// 0 at the start of the transition, 1 at the end.
    pub fn progress(&self, t: f64) -> f32 {
        (((t - self.start()) / self.duration.max(1e-9)) as f32).clamp(0.0, 1.0)
    }
}

/// One-click motion built from keyframes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MotionPreset {
    KenBurns,
    PunchIn,
    SlideInLeft,
    PopIn,
    SpinIn,
}

impl MotionPreset {
    pub const ALL: [MotionPreset; 5] =
        [MotionPreset::KenBurns, MotionPreset::PunchIn, MotionPreset::SlideInLeft, MotionPreset::PopIn, MotionPreset::SpinIn];

    pub fn name(self) -> &'static str {
        match self {
            MotionPreset::KenBurns => "ken_burns",
            MotionPreset::PunchIn => "punch_in",
            MotionPreset::SlideInLeft => "slide_in_left",
            MotionPreset::PopIn => "pop_in",
            MotionPreset::SpinIn => "spin_in",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            MotionPreset::KenBurns => "Ken Burns — slow zoom & drift",
            MotionPreset::PunchIn => "Punch-in zoom",
            MotionPreset::SlideInLeft => "Slide in from left",
            MotionPreset::PopIn => "Pop in",
            MotionPreset::SpinIn => "Spin in",
        }
    }

    pub fn parse(s: &str) -> Option<MotionPreset> {
        MotionPreset::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(s.trim()))
    }

    /// Whether the preset animates rotation (not available for text).
    pub fn uses_rotation(self) -> bool {
        self == MotionPreset::SpinIn
    }

    /// Replaces the keyframes of the properties this preset animates.
    pub fn apply(self, c: &mut Clip) {
        let d = c.duration.max(MIN_CLIP_LEN);
        let at = |c: &Clip, p: Prop| c.value_at(p, c.start);
        let (x0, s0, r0, o0) = (at(c, Prop::X), at(c, Prop::Scale), at(c, Prop::Rotation), at(c, Prop::Opacity));
        let k = |time: f64, value: f32, ease: Ease| Keyframe { time: time.min(d), value, ease };
        let set = |c: &mut Clip, p: Prop, keys: Vec<Keyframe>| {
            c.keyframes.insert(p, keys);
        };
        match self {
            MotionPreset::KenBurns => {
                set(c, Prop::Scale, vec![k(0.0, s0, Ease::Smooth), k(d, s0 * 1.15, Ease::Linear)]);
                set(c, Prop::X, vec![k(0.0, x0 - 0.02, Ease::Smooth), k(d, x0 + 0.02, Ease::Linear)]);
            }
            MotionPreset::PunchIn => {
                set(c, Prop::Scale, vec![k(0.0, s0, Ease::EaseOut), k(0.35, s0 * 1.25, Ease::Linear)]);
            }
            MotionPreset::SlideInLeft => {
                set(c, Prop::X, vec![k(0.0, x0 - 1.0, Ease::EaseOut), k(0.6, x0, Ease::Linear)]);
            }
            MotionPreset::PopIn => {
                set(c, Prop::Scale, vec![k(0.0, s0 * 0.5, Ease::EaseOut), k(0.18, s0 * 1.08, Ease::Smooth), k(0.3, s0, Ease::Linear)]);
                set(c, Prop::Opacity, vec![k(0.0, 0.0, Ease::Linear), k(0.15, o0, Ease::Linear)]);
            }
            MotionPreset::SpinIn => {
                set(c, Prop::Rotation, vec![k(0.0, r0 - 180.0, Ease::EaseOut), k(0.6, r0, Ease::Linear)]);
                set(c, Prop::Scale, vec![k(0.0, s0 * 0.3, Ease::EaseOut), k(0.6, s0, Ease::Linear)]);
            }
        }
        // Very short clips can collapse two keyframes onto the same time.
        for keys in c.keyframes.values_mut() {
            keys.dedup_by(|b, a| (b.time - a.time).abs() <= KEY_TOLERANCE);
        }
    }
}

/// Keyframes closer than this (seconds) are treated as the same keyframe.
pub const KEY_TOLERANCE: f64 = 1.0 / 60.0;

/// Value of sorted keyframes at clip-local time `local`.
pub fn interpolate(keys: &[Keyframe], local: f64) -> f32 {
    let Some(first) = keys.first() else { return 0.0 };
    if local <= first.time {
        return first.value;
    }
    for w in keys.windows(2) {
        let (a, b) = (w[0], w[1]);
        if local < b.time {
            let p = ((local - a.time) / (b.time - a.time).max(1e-9)).clamp(0.0, 1.0);
            return a.value + (b.value - a.value) * a.ease.apply(p) as f32;
        }
    }
    keys[keys.len() - 1].value
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub content: String,
    /// Pixels relative to a 1080 px short side.
    pub font_size: f32,
    pub bold: bool,
    pub color: [u8; 4],
    pub background: Option<[u8; 4]>,
    pub outline: Option<[u8; 4]>,
    /// Left-align this text where it would start if this full line were centred
    /// (karaoke: the highlighted part sits exactly over the white line).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align_to: Option<String>,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            content: "Add text".into(),
            font_size: 72.0,
            bold: true,
            color: [255, 255, 255, 255],
            background: None,
            outline: Some([0, 0, 0, 255]),
            align_to: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipSource {
    Media(Id),
    Text(TextStyle),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: Id,
    pub source: ClipSource,
    /// Timeline position, seconds.
    pub start: f64,
    /// Timeline length, seconds.
    pub duration: f64,
    /// Offset into the source, seconds.
    pub source_in: f64,
    pub speed: f64,
    pub transform: Transform,
    pub opacity: f32,
    pub volume: f32,
    pub fade_in: f64,
    pub fade_out: f64,
    pub filter: FilterPreset,
    pub adjust: Adjust,
    /// A video file used only for its sound (detached audio).
    #[serde(default)]
    pub audio_only: bool,
    /// Animated properties; keyframe times are seconds from the clip start.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keyframes: BTreeMap<Prop, Vec<Keyframe>>,
    /// Transition from the previous clip on the track (applies only when adjacent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_in: Option<Transition>,
    #[serde(default)]
    pub audio_role: AudioRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denoise: Option<Denoise>,
    /// Image (media id) drawn behind this clip, filling the canvas (AI background fill).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_plate: Option<Id>,
    /// Blur radius in pixels at a 1080 px short side (0 = sharp). Video/image clips.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blur: f32,
    /// Glow 0..1 (brighter, more saturated). Video/image clips.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub glow: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Clip {
    pub fn new(id: Id, source: ClipSource, start: f64, duration: f64) -> Self {
        Self {
            id,
            source,
            start,
            duration,
            source_in: 0.0,
            speed: 1.0,
            transform: Transform::default(),
            opacity: 1.0,
            volume: 1.0,
            fade_in: 0.0,
            fade_out: 0.0,
            filter: FilterPreset::None,
            adjust: Adjust::default(),
            audio_only: false,
            keyframes: BTreeMap::new(),
            transition_in: None,
            audio_role: AudioRole::Auto,
            denoise: None,
            background_plate: None,
            blur: 0.0,
            glow: 0.0,
        }
    }

    pub fn end(&self) -> f64 {
        self.start + self.duration
    }

    /// Time remaining in the clip after timeline time `t`.
    pub fn end_at(&self, t: f64) -> f64 {
        self.end() - t
    }

    pub fn is_active(&self, t: f64) -> bool {
        t >= self.start && t < self.end()
    }

    pub fn source_time(&self, t: f64) -> f64 {
        self.source_in + (t - self.start).max(0.0) * self.speed
    }

    /// Fades clamped so they never exceed the clip.
    pub fn fades(&self) -> (f64, f64) {
        let fi = self.fade_in.clamp(0.0, self.duration);
        let fo = self.fade_out.clamp(0.0, self.duration - fi);
        (fi, fo)
    }

    /// Fade multiplier (0..1) at timeline time `t`.
    pub fn fade_gain(&self, t: f64) -> f32 {
        let (fi, fo) = self.fades();
        let mut g: f64 = 1.0;
        if fi > 0.0 {
            g = g.min((t - self.start) / fi);
        }
        if fo > 0.0 {
            g = g.min((self.end() - t) / fo);
        }
        g.clamp(0.0, 1.0) as f32
    }

    pub fn is_animated(&self, p: Prop) -> bool {
        self.keyframes.get(&p).is_some_and(|k| !k.is_empty())
    }

    pub fn has_keyframes(&self) -> bool {
        self.keyframes.values().any(|k| !k.is_empty())
    }

    /// Value of a property at timeline time `t` (keyframed or static).
    pub fn value_at(&self, p: Prop, t: f64) -> f32 {
        match self.keyframes.get(&p) {
            Some(keys) if !keys.is_empty() => interpolate(keys, t - self.start),
            _ => p.base(self),
        }
    }

    pub fn transform_at(&self, t: f64) -> Transform {
        Transform {
            x: self.value_at(Prop::X, t),
            y: self.value_at(Prop::Y, t),
            scale: self.value_at(Prop::Scale, t).max(0.01),
            rotation: self.value_at(Prop::Rotation, t),
        }
    }

    pub fn opacity_at(&self, t: f64) -> f32 {
        self.value_at(Prop::Opacity, t).clamp(0.0, 1.0)
    }

    pub fn volume_at(&self, t: f64) -> f32 {
        self.value_at(Prop::Volume, t).max(0.0)
    }

    /// Index of the keyframe of `p` at clip-local time `local`, if any.
    pub fn key_index(&self, p: Prop, local: f64) -> Option<usize> {
        self.keyframes.get(&p)?.iter().position(|k| (k.time - local).abs() <= KEY_TOLERANCE)
    }

    /// Adds a keyframe, or updates the value of the one already at that time.
    pub fn set_key(&mut self, p: Prop, local: f64, value: f32) {
        let keys = self.keyframes.entry(p).or_default();
        match keys.iter_mut().find(|k| (k.time - local).abs() <= KEY_TOLERANCE) {
            Some(k) => k.value = value,
            None => {
                let ease = keys.iter().rev().find(|k| k.time < local).map_or(Ease::Linear, |k| k.ease);
                keys.push(Keyframe { time: local, value, ease });
                keys.sort_by(|a, b| a.time.total_cmp(&b.time));
            }
        }
    }

    /// Removes the keyframe at `local`. Removing the last one keeps its value.
    pub fn remove_key(&mut self, p: Prop, local: f64) -> bool {
        let Some(i) = self.key_index(p, local) else { return false };
        let keys = self.keyframes.get_mut(&p).expect("checked");
        let removed = keys.remove(i);
        if keys.is_empty() {
            self.keyframes.remove(&p);
            p.set_base(self, removed.value);
        }
        true
    }

    /// Removes every keyframe of `p`, keeping the value it had at timeline time `t`.
    pub fn clear_keys(&mut self, p: Prop, t: f64) {
        let v = self.value_at(p, t);
        if self.keyframes.remove(&p).is_some() {
            p.set_base(self, v);
        }
    }

    /// Sets a property at timeline time `t`: keyframes it if it is animated, else
    /// changes the static value.
    pub fn set_value_at(&mut self, p: Prop, t: f64, v: f32) {
        if self.is_animated(p) {
            let local = (t - self.start).clamp(0.0, self.duration);
            self.set_key(p, local, v);
        } else {
            p.set_base(self, v);
        }
    }

    pub fn shift_keyframes(&mut self, delta: f64) {
        self.keyframes.values_mut().flatten().for_each(|k| k.time += delta);
    }

    pub fn scale_keyframe_times(&mut self, factor: f64) {
        self.keyframes.values_mut().flatten().for_each(|k| k.time *= factor);
    }

    /// Distinct keyframe times (clip-local), sorted.
    pub fn key_times(&self) -> Vec<f64> {
        let mut t: Vec<f64> = self.keyframes.values().flatten().map(|k| k.time).collect();
        t.sort_by(f64::total_cmp);
        t.dedup_by(|a, b| (*a - *b).abs() <= KEY_TOLERANCE);
        t
    }

    pub fn media_id(&self) -> Option<Id> {
        match self.source {
            ClipSource::Media(id) => Some(id),
            ClipSource::Text(_) => None,
        }
    }

    pub fn text(&self) -> Option<&TextStyle> {
        match &self.source {
            ClipSource::Text(t) => Some(t),
            ClipSource::Media(_) => None,
        }
    }

    pub fn text_mut(&mut self) -> Option<&mut TextStyle> {
        match &mut self.source {
            ClipSource::Text(t) => Some(t),
            ClipSource::Media(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Text,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: Id,
    pub kind: TrackKind,
    pub name: String,
    pub muted: bool,
    pub hidden: bool,
    pub locked: bool,
    /// Holds auto-generated captions.
    #[serde(default)]
    pub captions: bool,
    /// Multi-cam angle number for a synced take's track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub angle: Option<u32>,
    /// The track holding the multi-cam edit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multicam: bool,
    /// Sorted by start time.
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn new(id: Id, kind: TrackKind, name: impl Into<String>) -> Self {
        Self { id, kind, name: name.into(), muted: false, hidden: false, locked: false, captions: false, angle: None, multicam: false, clips: Vec::new() }
    }

    pub fn end(&self) -> f64 {
        self.clips.iter().map(Clip::end).fold(0.0, f64::max)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub aspect: Aspect,
    pub fps: u32,
    pub media: Vec<MediaItem>,
    /// Ordered top → bottom as displayed; upper tracks draw on top.
    pub tracks: Vec<Track>,
    pub next_id: Id,
    #[serde(default)]
    pub audio: AudioMix,
    /// Thumbnail image for the video (saved next to exports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<PathBuf>,
    /// Beat/section analysis per media id (music videos).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub music: std::collections::BTreeMap<Id, crate::music::MusicAnalysis>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            name: "Untitled project".into(),
            aspect: Aspect::Landscape,
            fps: 30,
            media: Vec::new(),
            tracks: vec![Track::new(1, TrackKind::Video, "Main")],
            next_id: 2,
            audio: AudioMix::default(),
            thumbnail: None,
            music: Default::default(),
        }
    }
}

impl Project {
    pub fn new_id(&mut self) -> Id {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn media(&self, id: Id) -> Option<&MediaItem> {
        self.media.iter().find(|m| m.id == id)
    }

    pub fn add_media(&mut self, mut item: MediaItem) -> Id {
        item.id = self.new_id();
        let id = item.id;
        self.media.push(item);
        id
    }

    /// Removes a media item and every clip that uses it.
    pub fn remove_media(&mut self, id: Id) {
        for track in &mut self.tracks {
            track.clips.retain(|c| c.media_id() != Some(id));
        }
        self.media.retain(|m| m.id != id);
        self.cleanup_tracks();
    }

    pub fn duration(&self) -> f64 {
        self.tracks.iter().map(Track::end).fold(0.0, f64::max)
    }

    pub fn clip_count(&self) -> usize {
        self.tracks.iter().map(|t| t.clips.len()).sum()
    }

    pub fn locate(&self, clip_id: Id) -> Option<(usize, usize)> {
        self.tracks.iter().enumerate().find_map(|(ti, t)| {
            t.clips.iter().position(|c| c.id == clip_id).map(|ci| (ti, ci))
        })
    }

    pub fn clip(&self, id: Id) -> Option<&Clip> {
        self.locate(id).map(|(t, c)| &self.tracks[t].clips[c])
    }

    pub fn clip_mut(&mut self, id: Id) -> Option<&mut Clip> {
        self.locate(id).map(move |(t, c)| &mut self.tracks[t].clips[c])
    }

    /// Which kind of track a clip belongs on.
    pub fn track_kind_for(&self, clip: &Clip) -> TrackKind {
        if clip.audio_only {
            return TrackKind::Audio;
        }
        match &clip.source {
            ClipSource::Text(_) => TrackKind::Text,
            ClipSource::Media(m) => match self.media(*m).map(|m| m.kind) {
                Some(MediaKind::Audio) => TrackKind::Audio,
                _ => TrackKind::Video,
            },
        }
    }

    /// The main (bottom-most) video track, created if missing.
    pub fn main_track(&mut self) -> usize {
        match self.tracks.iter().rposition(|t| t.kind == TrackKind::Video) {
            Some(i) => i,
            None => self.insert_track(TrackKind::Video, None),
        }
    }

    pub fn main_track_id(&self) -> Option<Id> {
        self.tracks.iter().rev().find(|t| t.kind == TrackKind::Video).map(|t| t.id)
    }

    /// Longest timeline length this clip may have given its source.
    pub fn max_duration(&self, clip: &Clip) -> Option<f64> {
        let media = self.media(clip.media_id()?)?;
        let len = media.source_len()?;
        Some(((len - clip.source_in) / clip.speed).max(MIN_CLIP_LEN))
    }

    pub fn fits(&self, track: usize, start: f64, end: f64, ignore: Option<Id>) -> bool {
        self.tracks[track]
            .clips
            .iter()
            .filter(|c| Some(c.id) != ignore)
            .all(|c| end <= c.start + EPS || start >= c.end() - EPS)
    }

    /// End of the previous clip and start of the next clip on the same track.
    pub fn neighbor_bounds(&self, id: Id) -> (f64, f64) {
        let Some((ti, ci)) = self.locate(id) else { return (0.0, f64::INFINITY) };
        let me = &self.tracks[ti].clips[ci];
        let mut prev_end: f64 = 0.0;
        let mut next_start = f64::INFINITY;
        for c in self.tracks[ti].clips.iter().filter(|c| c.id != id) {
            if c.end() <= me.start + EPS {
                prev_end = prev_end.max(c.end());
            } else if c.start >= me.end() - EPS {
                next_start = next_start.min(c.start);
            }
        }
        (prev_end, next_start)
    }

    pub fn sort_track(&mut self, ti: usize) {
        self.tracks[ti].clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    }

    /// Inserts a new track of `kind`, near `near` when given. Returns its index.
    pub fn insert_track(&mut self, kind: TrackKind, near: Option<usize>) -> usize {
        let count = self.tracks.iter().filter(|t| t.kind == kind).count();
        let name = match kind {
            TrackKind::Video => format!("Video {}", count + 1),
            TrackKind::Text => format!("Text {}", count + 1),
            TrackKind::Audio => format!("Audio {}", count + 1),
        };
        let near = near.filter(|&i| i < self.tracks.len() && self.tracks[i].kind == kind);
        let idx = match kind {
            TrackKind::Text => near.unwrap_or(0),
            TrackKind::Video => near
                .or_else(|| self.tracks.iter().position(|t| t.kind == TrackKind::Video))
                .unwrap_or_else(|| {
                    self.tracks.iter().rposition(|t| t.kind == TrackKind::Text).map_or(0, |i| i + 1)
                }),
            TrackKind::Audio => near.map_or(self.tracks.len(), |i| i + 1),
        };
        let id = self.new_id();
        self.tracks.insert(idx, Track::new(id, kind, name));
        idx
    }

    fn default_anchor(&self, kind: TrackKind) -> Option<usize> {
        match kind {
            TrackKind::Video => self.tracks.iter().rposition(|t| t.kind == kind),
            _ => self.tracks.iter().position(|t| t.kind == kind),
        }
    }

    /// Puts a clip on the preferred track if it fits there, otherwise on the nearest
    /// compatible track with room, otherwise on a brand-new track. Returns the track index.
    pub fn place_clip(&mut self, clip: Clip, preferred: Option<usize>) -> usize {
        let kind = self.track_kind_for(&clip);
        let (s, e) = (clip.start, clip.end());
        let preferred = preferred
            .filter(|&i| i < self.tracks.len() && self.tracks[i].kind == kind)
            .or_else(|| self.default_anchor(kind));
        let mut order: Vec<usize> = (0..self.tracks.len()).collect();
        if let Some(p) = preferred {
            order.sort_by_key(|&i| (i as i64 - p as i64).abs());
        }
        let target = order.into_iter().find(|&i| {
            let t = &self.tracks[i];
            t.kind == kind && !t.locked && self.fits(i, s, e, Some(clip.id))
        });
        let ti = match target {
            Some(i) => i,
            None => self.insert_track(kind, preferred),
        };
        self.tracks[ti].clips.push(clip);
        self.sort_track(ti);
        ti
    }

    /// Appends a clip to the end of the main track.
    pub fn append_to_main(&mut self, mut clip: Clip) {
        let ti = self.main_track();
        clip.start = self.tracks[ti].end();
        self.tracks[ti].clips.push(clip);
        self.sort_track(ti);
    }

    pub fn new_media_clip(&mut self, media_id: Id, start: f64) -> Option<Clip> {
        let media = self.media(media_id)?;
        let duration = media.source_len().unwrap_or(DEFAULT_STILL_LEN).max(MIN_CLIP_LEN);
        let id = self.new_id();
        Some(Clip::new(id, ClipSource::Media(media_id), start.max(0.0), duration))
    }

    pub fn new_text_clip(&mut self, style: TextStyle, start: f64) -> Clip {
        let id = self.new_id();
        Clip::new(id, ClipSource::Text(style), start.max(0.0), DEFAULT_TEXT_LEN)
    }

    /// Splits a clip at timeline time `t`. Returns the id of the new right-hand part.
    pub fn split_clip(&mut self, id: Id, t: f64) -> Option<Id> {
        let (ti, ci) = self.locate(id)?;
        {
            let c = &self.tracks[ti].clips[ci];
            if t <= c.start + MIN_CLIP_LEN || t >= c.end() - MIN_CLIP_LEN {
                return None;
            }
        }
        let new_id = self.new_id();
        let c = &mut self.tracks[ti].clips[ci];
        let offset = t - c.start;
        let mut right = c.clone();
        right.id = new_id;
        right.start = t;
        right.duration = c.duration - offset;
        right.source_in = c.source_in + offset * c.speed;
        right.fade_in = 0.0;
        right.shift_keyframes(-offset);
        right.transition_in = None;
        c.duration = offset;
        c.fade_out = 0.0;
        self.tracks[ti].clips.insert(ci + 1, right);
        Some(new_id)
    }

    pub fn remove_clip(&mut self, id: Id) -> Option<Clip> {
        let (ti, ci) = self.locate(id)?;
        Some(self.tracks[ti].clips.remove(ci))
    }

    /// Duplicates a clip right after itself. Returns the new id.
    pub fn duplicate_clip(&mut self, id: Id) -> Option<Id> {
        let (ti, ci) = self.locate(id)?;
        let mut copy = self.tracks[ti].clips[ci].clone();
        copy.id = self.new_id();
        copy.start = copy.end();
        copy.transition_in = None;
        let new_id = copy.id;
        self.place_clip(copy, Some(ti));
        Some(new_id)
    }

    /// Moves a clip to `track` at `start` without overlap checks (used while dragging).
    pub fn move_clip_live(&mut self, id: Id, track: usize, start: f64) {
        let Some((ti, ci)) = self.locate(id) else { return };
        if ti != track && track < self.tracks.len() {
            let mut clip = self.tracks[ti].clips.remove(ci);
            clip.start = start.max(0.0);
            self.tracks[track].clips.push(clip);
            self.sort_track(track);
        } else {
            self.tracks[ti].clips[ci].start = start.max(0.0);
            self.sort_track(ti);
        }
    }

    /// Removes the time range `a..b` from every track and closes the gap (a ripple
    /// cut through the whole timeline). Clips crossing the edges are trimmed; a clip
    /// spanning the whole range is split around it. Keyframes stay on their frames.
    pub fn cut_range(&mut self, a: f64, b: f64) {
        if b - a < 1e-6 {
            return;
        }
        let gap = b - a;
        for ti in 0..self.tracks.len() {
            let mut out: Vec<Clip> = Vec::with_capacity(self.tracks[ti].clips.len() + 1);
            let mut new_ids = Vec::new();
            for mut c in std::mem::take(&mut self.tracks[ti].clips) {
                let (s, e) = (c.start, c.end());
                if e <= a + EPS {
                    out.push(c); // entirely before
                } else if s >= b - EPS {
                    c.start -= gap; // entirely after: shift left
                    out.push(c);
                } else if s >= a - EPS && e <= b + EPS {
                    // entirely inside: dropped
                } else if s < a && e > b {
                    // spans the cut: keep the part before, and a copy for the part after
                    let mut right = c.clone();
                    let off = b - s;
                    right.start = a;
                    right.duration = e - b;
                    right.source_in += off * c.speed;
                    right.shift_keyframes(-off);
                    right.fade_in = 0.0;
                    right.transition_in = None;
                    new_ids.push(right.id);
                    c.duration = a - s;
                    c.fade_out = 0.0;
                    out.push(c);
                    out.push(right);
                } else if s < a {
                    // crosses the start of the cut: trim its tail
                    c.duration = a - s;
                    c.fade_out = c.fade_out.min(c.duration);
                    if c.duration >= MIN_CLIP_LEN {
                        out.push(c);
                    }
                } else {
                    // crosses the end of the cut: trim its head, then shift
                    let off = b - s;
                    c.duration = e - b;
                    c.source_in += off * c.speed;
                    c.shift_keyframes(-off);
                    c.fade_in = c.fade_in.min(c.duration);
                    c.start = a;
                    if c.duration >= MIN_CLIP_LEN {
                        out.push(c);
                    }
                }
            }
            self.tracks[ti].clips = out;
            self.sort_track(ti);
            // Copies need their own ids.
            for old in new_ids {
                let fresh = self.new_id();
                if let Some(c) = self.tracks[ti].clips.iter_mut().rev().find(|c| c.id == old && c.start == a) {
                    c.id = fresh;
                }
            }
        }
        self.cleanup_tracks();
    }

    /// Keeps only `keep` ranges (sorted, non-overlapping) of the timeline, cutting
    /// everything else and closing the gaps. Returns the seconds removed.
    /// Stretches of the timeline (≥ `min` seconds) where no visible video or image
    /// clip is present — only audio (or text on black) plays there.
    pub fn picture_gaps(&self, min: f64) -> Vec<(f64, f64)> {
        let mut spans: Vec<(f64, f64)> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video && !t.hidden)
            .flat_map(|t| &t.clips)
            .filter(|c| c.media_id().and_then(|m| self.media(m)).is_some_and(|m| m.has_video()))
            .map(|c| (c.start, c.end()))
            .collect();
        spans.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps = Vec::new();
        let mut cursor = 0.0;
        for (s, e) in spans {
            if s - cursor >= min {
                gaps.push((cursor, s));
            }
            cursor = cursor.max(e);
        }
        let total = self.duration();
        if total - cursor >= min {
            gaps.push((cursor, total));
        }
        gaps
    }

    pub fn keep_ranges(&mut self, keep: &[(f64, f64)]) -> f64 {
        let total = self.duration();
        let mut removed = Vec::new();
        let mut cursor = 0.0;
        for &(s, e) in keep {
            if s > cursor + 1e-6 {
                removed.push((cursor, s));
            }
            cursor = cursor.max(e);
        }
        if total > cursor + 1e-6 {
            removed.push((cursor, total));
        }
        let mut secs = 0.0;
        for &(a, b) in removed.iter().rev() {
            self.cut_range(a, b);
            secs += b - a;
        }
        secs
    }

    /// Inserts `clip` at `at` on track `ti`, pushing every later clip on that track
    /// right by the clip's length. Returns the clip id.
    pub fn ripple_insert(&mut self, ti: usize, at: f64, mut clip: Clip) -> Id {
        let id = clip.id;
        for c in self.tracks[ti].clips.iter_mut().filter(|c| c.start >= at - EPS) {
            c.start += clip.duration;
        }
        clip.start = at;
        self.tracks[ti].clips.push(clip);
        self.sort_track(ti);
        id
    }

    /// If a clip overlaps a neighbour, move it to another track (creating one if needed).
    pub fn resolve_overlap(&mut self, id: Id) {
        let Some((ti, ci)) = self.locate(id) else { return };
        let c = &self.tracks[ti].clips[ci];
        if !self.fits(ti, c.start, c.end(), Some(id)) {
            let clip = self.tracks[ti].clips.remove(ci);
            self.place_clip(clip, Some(ti));
        }
    }

    /// Changes speed, keeping the same source range (clamped to the space available).
    pub fn set_clip_speed(&mut self, id: Id, speed: f64) {
        let speed = speed.clamp(0.1, 10.0);
        let (_, next_start) = self.neighbor_bounds(id);
        let Some(c) = self.clip_mut(id) else { return };
        let src_len = c.duration * c.speed;
        let old_speed = c.speed;
        c.speed = speed;
        c.scale_keyframe_times(old_speed / speed);
        c.duration = (src_len / speed).min(next_start - c.start).max(MIN_CLIP_LEN);
    }

    /// The clip's audio role with `Auto` resolved: imported audio files are music,
    /// a video's own sound (including detached audio) is voice.
    pub fn audio_role(&self, clip: &Clip) -> AudioRole {
        match clip.audio_role {
            AudioRole::Auto => match clip.media_id().and_then(|m| self.media(m)) {
                Some(m) if m.kind == MediaKind::Audio => AudioRole::Music,
                Some(_) => AudioRole::Voice,
                None => AudioRole::Effect,
            },
            role => role,
        }
    }

    /// Transitions that currently apply on track `ti`, in timeline order.
    pub fn track_transitions(&self, ti: usize) -> Vec<ActiveTransition> {
        let track = &self.tracks[ti];
        if track.kind != TrackKind::Video {
            return Vec::new();
        }
        track
            .clips
            .windows(2)
            .filter_map(|w| {
                let (a, b) = (&w[0], &w[1]);
                let tr = b.transition_in?;
                if (a.end() - b.start).abs() > 1e-3 {
                    return None;
                }
                let duration = tr.duration.min(MAX_TRANSITION).min(a.duration).min(b.duration);
                (duration >= 0.05).then_some(ActiveTransition { kind: tr.kind, from: a.id, to: b.id, cut: b.start, duration })
            })
            .collect()
    }

    /// The clip right before `id` on its track, if it ends exactly where `id` starts.
    pub fn previous_adjacent(&self, id: Id) -> Option<Id> {
        let (ti, ci) = self.locate(id)?;
        let clips = &self.tracks[ti].clips;
        (ci > 0 && (clips[ci - 1].end() - clips[ci].start).abs() <= 1e-3).then(|| clips[ci - 1].id)
    }

    /// The clip right after `id` on its track, if it starts exactly where `id` ends.
    pub fn next_adjacent(&self, id: Id) -> Option<Id> {
        let (ti, ci) = self.locate(id)?;
        let clips = &self.tracks[ti].clips;
        clips.get(ci + 1).filter(|n| (n.start - clips[ci].end()).abs() <= 1e-3).map(|n| n.id)
    }

    /// Sets (or clears) the transition at the cut before `to`. Fails if there is no
    /// adjacent previous clip on a video track.
    pub fn set_transition(&mut self, to: Id, transition: Option<Transition>) -> Result<(), &'static str> {
        let (ti, _) = self.locate(to).ok_or("no such clip")?;
        if self.tracks[ti].kind != TrackKind::Video {
            return Err("transitions work between clips on video tracks");
        }
        if transition.is_some() && self.previous_adjacent(to).is_none() {
            return Err("there is no clip directly before this one on its track");
        }
        let clip = self.clip_mut(to).ok_or("no such clip")?;
        clip.transition_in = transition.map(|t| Transition { duration: t.duration.clamp(0.1, MAX_TRANSITION), ..t });
        Ok(())
    }

    /// Visible time range of a clip, extended into neighbouring transitions.
    pub fn visible_range(&self, id: Id) -> Option<(f64, f64)> {
        let (ti, ci) = self.locate(id)?;
        let c = &self.tracks[ti].clips[ci];
        let (mut lo, mut hi) = (c.start, c.end());
        for tr in self.track_transitions(ti) {
            if tr.to == id {
                lo = lo.min(tr.start());
            }
            if tr.from == id {
                hi = hi.max(tr.end());
            }
        }
        Some((lo, hi))
    }

    /// Removes empty tracks, always keeping the main track.
    pub fn cleanup_tracks(&mut self) {
        let main = self.main_track_id();
        self.tracks.retain(|t| !t.clips.is_empty() || Some(t.id) == main);
        if main.is_none() {
            self.main_track();
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let mut p: Project = serde_json::from_str(&text).context("not a valid RustCut project")?;
        p.main_track();
        Ok(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(p: &mut Project, dur: f64) -> Id {
        p.add_media(MediaItem {
            id: 0,
            path: "clip.mp4".into(),
            name: "clip.mp4".into(),
            kind: MediaKind::Video,
            duration: dur,
            width: 1920,
            height: 1080,
            fps: 30.0,
            has_audio: true,
        })
    }

    #[test]
    fn aspect_sizes() {
        assert_eq!(Aspect::Landscape.size_for(1080), (1920, 1080));
        assert_eq!(Aspect::Portrait.size_for(1080), (1080, 1920));
        assert_eq!(Aspect::Social.size_for(1080), (1080, 1350));
        assert_eq!(Aspect::Classic.size_for(720), (960, 720));
    }

    #[test]
    fn append_and_split() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let a = p.new_media_clip(m, 0.0).unwrap();
        let a_id = a.id;
        p.append_to_main(a);
        let b = p.new_media_clip(m, 0.0).unwrap();
        p.append_to_main(b);
        assert_eq!(p.duration(), 20.0);

        let right = p.split_clip(a_id, 4.0).unwrap();
        let r = p.clip(right).unwrap();
        assert_eq!(r.start, 4.0);
        assert_eq!(r.duration, 6.0);
        assert_eq!(r.source_in, 4.0);
        assert_eq!(p.clip(a_id).unwrap().duration, 4.0);
        assert_eq!(p.duration(), 20.0);
        // Splitting at the very edge is refused.
        assert!(p.split_clip(a_id, 0.01).is_none());
    }

    #[test]
    fn split_respects_speed() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.append_to_main(c);
        p.set_clip_speed(id, 2.0);
        assert_eq!(p.clip(id).unwrap().duration, 5.0);
        let right = p.split_clip(id, 1.0).unwrap();
        assert_eq!(p.clip(right).unwrap().source_in, 2.0);
    }

    #[test]
    fn overlapping_clip_goes_to_new_track() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let a = p.new_media_clip(m, 0.0).unwrap();
        p.place_clip(a, None);
        let b = p.new_media_clip(m, 5.0).unwrap();
        let b_id = b.id;
        let ti = p.place_clip(b, Some(0));
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.locate(b_id).unwrap().0, ti);
        // The new overlay track sits above the main track.
        assert_eq!(p.tracks[ti].kind, TrackKind::Video);
        assert_eq!(p.tracks.last().unwrap().name, "Main");
    }

    #[test]
    fn text_and_audio_track_ordering() {
        let mut p = Project::default();
        let t = p.new_text_clip(TextStyle::default(), 0.0);
        p.place_clip(t, None);
        let a = p.add_media(MediaItem {
            id: 0,
            path: "song.mp3".into(),
            name: "song.mp3".into(),
            kind: MediaKind::Audio,
            duration: 30.0,
            width: 0,
            height: 0,
            fps: 0.0,
            has_audio: true,
        });
        let c = p.new_media_clip(a, 0.0).unwrap();
        p.place_clip(c, None);
        let kinds: Vec<_> = p.tracks.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![TrackKind::Text, TrackKind::Video, TrackKind::Audio]);
    }

    #[test]
    fn duplicate_and_cleanup() {
        let mut p = Project::default();
        let m = video(&mut p, 3.0);
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.append_to_main(c);
        let d = p.duplicate_clip(id).unwrap();
        assert_eq!(p.clip(d).unwrap().start, 3.0);
        assert_eq!(p.tracks.len(), 1);

        // Moving onto the same spot then resolving pushes it to a new track.
        p.move_clip_live(d, 0, 1.0);
        p.resolve_overlap(d);
        assert_eq!(p.tracks.len(), 2);
        p.remove_clip(d);
        p.cleanup_tracks();
        assert_eq!(p.tracks.len(), 1);
    }

    #[test]
    fn keyframe_interpolation() {
        let keys = [
            Keyframe { time: 1.0, value: 0.0, ease: Ease::Linear },
            Keyframe { time: 3.0, value: 10.0, ease: Ease::Smooth },
            Keyframe { time: 5.0, value: 20.0, ease: Ease::Hold },
            Keyframe { time: 6.0, value: 0.0, ease: Ease::Linear },
        ];
        assert_eq!(interpolate(&keys, 0.0), 0.0);
        assert_eq!(interpolate(&keys, 2.0), 5.0);
        assert_eq!(interpolate(&keys, 4.0), 15.0); // smoothstep(0.5) = 0.5
        assert!(interpolate(&keys, 3.5) < 12.5); // eased start is slower than linear
        assert_eq!(interpolate(&keys, 5.9), 20.0); // hold
        assert_eq!(interpolate(&keys, 9.0), 0.0);
        assert_eq!(Ease::EaseOut.apply(0.5), 0.75);
    }

    #[test]
    fn clip_keyframes_follow_edits() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let c = p.new_media_clip(m, 2.0).unwrap();
        let id = c.id;
        p.place_clip(c, None);
        {
            let c = p.clip_mut(id).unwrap();
            c.set_key(Prop::Scale, 0.0, 1.0);
            c.set_key(Prop::Scale, 4.0, 2.0);
            c.set_key(Prop::Scale, 4.005, 3.0); // same keyframe (within tolerance)
            assert_eq!(c.keyframes[&Prop::Scale].len(), 2);
            assert_eq!(c.value_at(Prop::Scale, 4.0), 2.0); // timeline 4.0 = local 2.0, halfway
            assert_eq!(c.transform_at(6.0).scale, 3.0);
            c.set_value_at(Prop::X, 3.0, 0.25); // not animated: static value
            assert_eq!(c.transform.x, 0.25);
            assert!(!c.is_animated(Prop::X));
        }
        // Splitting keeps the animation continuous across the cut.
        let before = p.clip(id).unwrap().value_at(Prop::Scale, 5.0);
        let right = p.split_clip(id, 5.0).unwrap();
        assert_eq!(p.clip(right).unwrap().value_at(Prop::Scale, 5.0), before);
        assert_eq!(p.clip(right).unwrap().value_at(Prop::Scale, 7.0), p.clip(id).unwrap().keyframes[&Prop::Scale][1].value);
        // Doubling speed halves keyframe times.
        p.set_clip_speed(right, 2.0);
        assert_eq!(p.clip(right).unwrap().keyframes[&Prop::Scale][1].time, 0.5);
        // Removing the last keyframe keeps its value.
        let c = p.clip_mut(id).unwrap();
        assert!(c.remove_key(Prop::Scale, 0.0));
        assert!(c.remove_key(Prop::Scale, 4.0));
        assert!(!c.is_animated(Prop::Scale));
        assert_eq!(c.transform.scale, 3.0);
    }

    #[test]
    fn transitions_between_adjacent_clips() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let mut ids = Vec::new();
        for d in [4.0, 1.0, 4.0] {
            let mut c = p.new_media_clip(m, 0.0).unwrap();
            c.duration = d;
            ids.push(c.id);
            p.append_to_main(c);
        }
        let main = p.main_track();
        assert!(p.set_transition(ids[0], Some(Transition { kind: TransitionKind::Dissolve, duration: 1.0 })).is_err());
        p.set_transition(ids[1], Some(Transition { kind: TransitionKind::WipeLeft, duration: 2.0 })).unwrap();
        p.set_transition(ids[2], Some(Transition { kind: TransitionKind::Dissolve, duration: 9.0 })).unwrap();
        assert_eq!(p.clip(ids[2]).unwrap().transition_in.unwrap().duration, MAX_TRANSITION);
        let trs = p.track_transitions(main);
        assert_eq!(trs.len(), 2);
        // Clamped to the 1 s middle clip; centred on the cut at 4 s.
        assert_eq!((trs[0].duration, trs[0].start(), trs[0].end()), (1.0, 3.5, 4.5));
        assert_eq!(trs[0].progress(4.0), 0.5);
        assert_eq!(p.visible_range(ids[1]), Some((3.5, 5.5)));
        // Moving a clip away breaks adjacency, so the transition stops applying.
        p.move_clip_live(ids[2], main, 7.0);
        assert_eq!(p.track_transitions(main).len(), 1);
        // A split creates a seamless cut without a transition.
        let right = p.split_clip(ids[0], 2.0).unwrap();
        assert!(p.clip(right).unwrap().transition_in.is_none());
        assert_eq!(TransitionKind::parse("Cross-dissolve"), Some(TransitionKind::Dissolve));
        assert_eq!(TransitionKind::parse("wipe_up"), Some(TransitionKind::WipeUp));
    }

    #[test]
    fn motion_presets() {
        let mut c = Clip::new(1, ClipSource::Media(1), 10.0, 4.0);
        c.transform.scale = 2.0;
        MotionPreset::KenBurns.apply(&mut c);
        assert_eq!(c.value_at(Prop::Scale, 10.0), 2.0);
        assert!((c.value_at(Prop::Scale, 14.0) - 2.3).abs() < 1e-5);
        MotionPreset::PopIn.apply(&mut c);
        assert_eq!(c.opacity_at(10.0), 0.0);
        assert_eq!(c.opacity_at(11.0), 1.0);
        // Tiny clip: keyframes clamp to the clip and never duplicate.
        let mut short = Clip::new(2, ClipSource::Media(1), 0.0, 0.1);
        MotionPreset::SlideInLeft.apply(&mut short);
        assert_eq!(short.keyframes[&Prop::X].len(), 2);
        MotionPreset::PunchIn.apply(&mut short);
        assert!(short.keyframes[&Prop::Scale].iter().all(|k| k.time <= 0.1));
    }

    #[test]
    fn keyframes_serialize_compactly() {
        let mut c = Clip::new(1, ClipSource::Media(1), 0.0, 1.0);
        let plain = serde_json::to_string(&c).unwrap();
        assert!(!plain.contains("keyframes"));
        c.set_key(Prop::Opacity, 0.5, 0.2);
        let json = serde_json::to_string(&c).unwrap();
        assert!(json.contains(r#""keyframes":{"opacity":[{"time":0.5,"value":0.2,"ease":"linear"}]}"#), "{json}");
        let back: Clip = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn cut_range_ripples_every_track() {
        let mut p = Project::default();
        let m = video(&mut p, 30.0);
        let main = p.main_track();
        let mut a = p.new_media_clip(m, 0.0).unwrap();
        a.duration = 10.0;
        a.set_key(Prop::X, 8.0, 0.5);
        let aid = a.id;
        p.append_to_main(a);
        let mut b = p.new_media_clip(m, 0.0).unwrap();
        b.duration = 5.0;
        let bid = b.id;
        p.append_to_main(b); // 10–15
        let mut t = p.new_text_clip(TextStyle::default(), 3.0);
        t.duration = 2.0; // 3–5: entirely inside the cut below → removed
        let tid = t.id;
        p.place_clip(t, None);
        let mut t2 = p.new_text_clip(TextStyle::default(), 12.0);
        t2.duration = 1.0;
        let t2id = t2.id;
        p.place_clip(t2, None);

        p.cut_range(2.0, 6.0); // cut 4 s out of clip A (which spans it)
        let main = p.tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap_or(main);
        let clips: Vec<(f64, f64, f64)> = p.tracks[main].clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();
        assert_eq!(clips, vec![(0.0, 2.0, 0.0), (2.0, 4.0, 6.0), (6.0, 5.0, 0.0)]);
        assert_eq!(p.clip(bid).unwrap().start, 6.0);
        assert!(p.clip(tid).is_none());
        assert_eq!(p.clip(t2id).unwrap().start, 8.0);
        // The right-hand copy has a fresh id and its keyframe stays on the same frame.
        let right = &p.tracks[main].clips[1];
        assert_ne!(right.id, aid);
        assert_eq!(right.keyframes[&Prop::X][0].time, 2.0); // was local 8 s; 6 s trimmed off
        assert_eq!(p.duration(), 11.0);
    }

    #[test]
    fn picture_gaps_find_audio_only_stretches() {
        let mut p = Project::default();
        let m = video(&mut p, 30.0);
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.duration = 35.0;
        p.append_to_main(c);
        let song = p.add_media(MediaItem {
            id: 0, path: "song.mp3".into(), name: "song.mp3".into(), kind: MediaKind::Audio,
            duration: 332.0, width: 0, height: 0, fps: 0.0, has_audio: true,
        });
        let mut a = p.new_media_clip(song, 0.0).unwrap();
        a.duration = 332.0;
        p.place_clip(a, None);
        assert_eq!(p.picture_gaps(1.0), vec![(35.0, 332.0)]);
    }

    #[test]
    fn keep_ranges_removes_everything_else() {
        let mut p = Project::default();
        let m = video(&mut p, 30.0);
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.duration = 20.0;
        p.append_to_main(c);
        let removed = p.keep_ranges(&[(2.0, 5.0), (10.0, 12.0)]);
        assert_eq!(removed, 15.0);
        assert_eq!(p.duration(), 5.0);
        let main = p.main_track();
        let src: Vec<(f64, f64, f64)> = p.tracks[main].clips.iter().map(|c| (c.start, c.duration, c.source_in)).collect();
        assert_eq!(src, vec![(0.0, 3.0, 2.0), (3.0, 2.0, 10.0)]);
    }

    #[test]
    fn ripple_insert_pushes_later_clips() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut c = p.new_media_clip(m, 0.0).unwrap();
            c.duration = 3.0;
            ids.push(c.id);
            p.append_to_main(c);
        }
        let mut ins = p.new_media_clip(m, 0.0).unwrap();
        ins.duration = 2.0;
        let main = p.main_track();
        let id = p.ripple_insert(main, 3.0, ins);
        assert_eq!(p.clip(id).unwrap().start, 3.0);
        assert_eq!(p.clip(ids[0]).unwrap().start, 0.0);
        assert_eq!(p.clip(ids[1]).unwrap().start, 5.0);
        assert_eq!(p.duration(), 8.0);
    }

    #[test]
    fn fade_gain() {
        let mut c = Clip::new(1, ClipSource::Media(1), 10.0, 4.0);
        c.fade_in = 1.0;
        c.fade_out = 2.0;
        assert_eq!(c.fade_gain(10.0), 0.0);
        assert!((c.fade_gain(10.5) - 0.5).abs() < 1e-6);
        assert_eq!(c.fade_gain(11.5), 1.0);
        assert!((c.fade_gain(13.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn max_duration_and_neighbors() {
        let mut p = Project::default();
        let m = video(&mut p, 10.0);
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.source_in = 4.0;
        c.duration = 2.0;
        assert_eq!(p.max_duration(&c), Some(6.0));
        let id = c.id;
        p.place_clip(c, None);
        let mut d = p.new_media_clip(m, 5.0).unwrap();
        d.duration = 1.0;
        p.place_clip(d, None);
        assert_eq!(p.neighbor_bounds(id), (0.0, 5.0));
    }
}
