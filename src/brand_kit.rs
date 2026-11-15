//! Brand kit: a folder of reusable assets shared by every project.
//!
//! ```text
//! <kit folder>/
//!   Brand Kit/          logos and other brand images, plus brand.json
//!     brand.json        { name, handle, colors: [{name, hex}], fonts: [..], logo }
//!   Assets/             reusable clips and pictures; each subfolder is a group
//!     Intros/ …  End Cards/ …  Images/ …
//! ```
//! Folders whose names start with `_` or `.` are ignored. The chosen kit folder is
//! remembered in `brand_kit.json` in the per-user config folder.

use crate::model::{Clip, Id, MediaKind, Project};
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const BRAND_DIR: &str = "Brand Kit";
pub const ASSETS_DIR: &str = "Assets";
pub const BRAND_FILE: &str = "brand.json";

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BrandColor {
    pub name: String,
    pub hex: String,
}

impl BrandColor {
    /// `#RRGGBB` (or `RRGGBB`) as bytes.
    pub fn rgb(&self) -> Option<[u8; 3]> {
        parse_hex(&self.hex)
    }
}

pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 || !s.is_ascii() {
        return None;
    }
    let b = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
    Some([b(0)?, b(2)?, b(4)?])
}

/// What `Brand Kit/brand.json` holds. Every field is optional.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BrandInfo {
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub handle: String,
    #[serde(default)]
    pub colors: Vec<BrandColor>,
    #[serde(default)]
    pub fonts: Vec<String>,
    /// The main logo, relative to the `Brand Kit` folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrandAsset {
    pub path: PathBuf,
    pub name: String,
    pub kind: MediaKind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AssetGroup {
    pub name: String,
    pub items: Vec<BrandAsset>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct BrandKit {
    pub root: PathBuf,
    pub info: BrandInfo,
    /// Pictures and clips in `Brand Kit/` (logos first: the main logo leads).
    pub logos: Vec<BrandAsset>,
    /// `Assets/` grouped by subfolder; loose files form a group named "Assets".
    pub groups: Vec<AssetGroup>,
    /// Problems worth showing (a bad brand.json, say). The kit still loads.
    pub warnings: Vec<String>,
}

impl BrandKit {
    pub fn display_name(&self) -> String {
        if !self.info.name.trim().is_empty() {
            return self.info.name.clone();
        }
        self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Brand kit".into())
    }

    pub fn main_logo(&self) -> Option<&BrandAsset> {
        self.logos.iter().find(|a| a.kind == MediaKind::Image)
    }

    pub fn all_assets(&self) -> impl Iterator<Item = &BrandAsset> {
        self.logos.iter().chain(self.groups.iter().flat_map(|g| g.items.iter()))
    }

    /// An asset by file name, stem or relative path (case-insensitive), e.g. "intro-1" or "Intros/ready-rock-media-intro-1.mp4".
    pub fn find(&self, query: &str) -> Option<&BrandAsset> {
        let q = query.trim().replace('\\', "/").to_lowercase();
        let rel = |a: &BrandAsset| a.path.strip_prefix(&self.root).unwrap_or(&a.path).to_string_lossy().replace('\\', "/").to_lowercase();
        let stem = |a: &BrandAsset| a.path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        self.all_assets()
            .find(|a| a.path.to_string_lossy().replace('\\', "/").to_lowercase() == q || a.name.to_lowercase() == q || stem(a) == q)
            .or_else(|| self.all_assets().find(|a| rel(a).ends_with(&q)))
            .or_else(|| self.all_assets().find(|a| stem(a).contains(&q)))
    }

    pub fn color(&self, name: &str) -> Option<[u8; 3]> {
        self.info.colors.iter().find(|c| c.name.eq_ignore_ascii_case(name.trim())).and_then(BrandColor::rgb)
    }
}

fn skip(name: &str) -> bool {
    name.starts_with('_') || name.starts_with('.')
}

pub fn kind_of(path: &Path) -> Option<MediaKind> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    if !crate::media::ffmpeg::MEDIA_EXTS.contains(&ext.as_str()) {
        return None;
    }
    Some(match ext.as_str() {
        "png" | "jpg" | "jpeg" | "bmp" | "webp" | "tif" | "tiff" => MediaKind::Image,
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "wma" | "aif" | "aiff" => MediaKind::Audio,
        _ => MediaKind::Video,
    })
}

/// Media files under `dir` (recursively, skipping `_`/`.` folders), sorted by path.
fn media_in(dir: &Path) -> Vec<BrandAsset> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if skip(&name) {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
            } else if let Some(kind) = kind_of(&p) {
                out.push(BrandAsset { path: p, name, kind });
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Reads a kit folder. Missing `Brand Kit`/`Assets` folders just mean empty sections.
pub fn load(root: &Path) -> Result<BrandKit> {
    if !root.is_dir() {
        return Err(anyhow!("brand kit folder not found: {}", root.display()));
    }
    let brand = root.join(BRAND_DIR);
    let mut warnings = Vec::new();
    let info = match std::fs::read_to_string(brand.join(BRAND_FILE)) {
        Ok(s) => serde_json::from_str::<BrandInfo>(&s).unwrap_or_else(|e| {
            warnings.push(format!("{BRAND_DIR}/{BRAND_FILE} couldn't be read: {e}"));
            BrandInfo::default()
        }),
        Err(_) => BrandInfo::default(),
    };
    for c in &info.colors {
        if c.rgb().is_none() {
            warnings.push(format!("colour \"{}\" has a bad hex value \"{}\" (use #RRGGBB)", c.name, c.hex));
        }
    }
    let mut logos = media_in(&brand);
    if let Some(main) = info.logo.as_ref().map(|l| brand.join(l)) {
        if let Some(i) = logos.iter().position(|a| a.path == main) {
            let a = logos.remove(i);
            logos.insert(0, a);
        } else {
            warnings.push(format!("logo \"{}\" isn't in the {BRAND_DIR} folder", info.logo.as_deref().unwrap_or_default()));
        }
    }

    let assets = root.join(ASSETS_DIR);
    let mut groups = Vec::new();
    let mut loose = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&assets) {
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if skip(&name) {
                continue;
            }
            if p.is_dir() {
                let items = media_in(&p);
                if !items.is_empty() {
                    groups.push(AssetGroup { name, items });
                }
            } else if let Some(kind) = kind_of(&p) {
                loose.push(BrandAsset { path: p, name, kind });
            }
        }
    }
    if !loose.is_empty() {
        groups.insert(0, AssetGroup { name: ASSETS_DIR.into(), items: loose });
    }
    Ok(BrandKit { root: root.to_path_buf(), info, logos, groups, warnings })
}

/// Makes the `Brand Kit` and `Assets` folders (and a starter brand.json) if missing.
pub fn init(root: &Path) -> Result<()> {
    let brand = root.join(BRAND_DIR);
    std::fs::create_dir_all(&brand).with_context(|| format!("creating {}", brand.display()))?;
    std::fs::create_dir_all(root.join(ASSETS_DIR))?;
    let file = brand.join(BRAND_FILE);
    if !file.exists() {
        let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let info = BrandInfo {
            name,
            colors: vec![
                BrandColor { name: "Primary".into(), hex: "#E8B54A".into() },
                BrandColor { name: "Dark".into(), hex: "#0B0B0B".into() },
                BrandColor { name: "Light".into(), hex: "#FFFFFF".into() },
            ],
            fonts: vec!["Montserrat".into()],
            ..Default::default()
        };
        std::fs::write(&file, serde_json::to_string_pretty(&info)?)?;
    }
    Ok(())
}

// ── the remembered folder ────────────────────────────────────────────────────

#[derive(Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    folder: Option<PathBuf>,
}

fn settings_file() -> PathBuf {
    crate::util::config_dir().join("brand_kit.json")
}

/// The kit folder: the one chosen last, else `Videos/RustCut AI/Movie Brand Kit` if it exists.
pub fn saved_folder() -> Option<PathBuf> {
    let s: Settings = std::fs::read_to_string(settings_file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    s.folder.or_else(|| {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        let d = PathBuf::from(home).join("Videos").join("RustCut AI").join("Movie Brand Kit");
        d.is_dir().then_some(d)
    })
}

pub fn save_folder(folder: &Path) -> Result<()> {
    let dir = crate::util::config_dir();
    std::fs::create_dir_all(&dir)?;
    let s = Settings { folder: Some(folder.to_path_buf()) };
    std::fs::write(settings_file(), serde_json::to_string_pretty(&s)?)?;
    Ok(())
}

// ── putting assets in a project ──────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Where the media tab would put it: the main track if free, else above it.
    Timeline,
    /// Full frame on a track above the footage.
    Overlay,
    /// Small in the top-right corner on a track above the footage (a logo bug).
    Corner,
    /// Only into the media library.
    LibraryOnly,
}

impl Placement {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_lowercase().as_str() {
            "timeline" | "main" => Self::Timeline,
            "overlay" | "full" => Self::Overlay,
            "corner" | "logo" | "bug" | "watermark" => Self::Corner,
            "library" | "none" => Self::LibraryOnly,
            _ => return None,
        })
    }
}

/// The project's media item for `path`, if already imported.
pub fn media_for(project: &Project, path: &Path) -> Option<Id> {
    project.media.iter().find(|m| m.path == path).map(|m| m.id)
}

/// Places media already in the project at `start`. Returns the clip id (None for LibraryOnly).
pub fn place(project: &mut Project, media: Id, start: f64, how: Placement) -> Option<Id> {
    let kind = project.media(media)?.kind;
    let mut clip: Clip = project.new_media_clip(media, start.max(0.0))?;
    let id = clip.id;
    match (how, kind) {
        (Placement::LibraryOnly, _) => return None,
        (_, MediaKind::Audio) | (Placement::Timeline, _) => {
            let main = project.main_track();
            if kind != MediaKind::Audio && project.tracks[main].clips.is_empty() {
                clip.start = 0.0;
            }
            project.place_clip(clip, (kind != MediaKind::Audio).then_some(main));
        }
        (Placement::Overlay, _) => {
            crate::motion_graphics::place_overlay(project, clip);
        }
        (Placement::Corner, _) => {
            clip.transform.scale = 0.22;
            clip.transform.x = 0.38;
            clip.transform.y = -0.36;
            crate::motion_graphics::place_overlay(project, clip);
        }
    }
    Some(id)
}

/// Imports (if needed, synchronously) and places an asset. Returns (media id, clip id).
pub fn add_to_project(project: &mut Project, path: &Path, start: f64, how: Placement) -> Result<(Id, Option<Id>)> {
    let media = match media_for(project, path) {
        Some(m) => m,
        None => {
            let info = crate::media::ffmpeg::probe(path)?;
            project.add_media(info.into_media_item(path.to_path_buf()))
        }
    };
    Ok((media, place(project, media, start, how)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(p: &Path) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, b"x").unwrap();
    }

    #[test]
    fn loads_brand_and_asset_folders() {
        let root = std::env::temp_dir().join(format!("rustcut-brandkit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        init(&root).unwrap();
        let k = load(&root).unwrap();
        assert!(k.logos.is_empty() && k.groups.is_empty());
        assert_eq!(k.info.colors.len(), 3);

        touch(&root.join("Brand Kit/logos/a-logo.jpg"));
        touch(&root.join("Brand Kit/logos/main.png"));
        touch(&root.join("Assets/Intros/intro-1.mp4"));
        touch(&root.join("Assets/Images/card.jpg"));
        touch(&root.join("Assets/Images/notes.txt"));
        touch(&root.join("Assets/loose.mp3"));
        touch(&root.join("Assets/_duplicates/dup.jpg"));
        std::fs::write(
            root.join("Brand Kit/brand.json"),
            r##"{"name":"Ready Rock","colors":[{"name":"Gold","hex":"#D9A441"},{"name":"Bad","hex":"nope"}],"fonts":["Montserrat"],"logo":"logos/main.png"}"##,
        )
        .unwrap();

        let k = load(&root).unwrap();
        assert_eq!(k.display_name(), "Ready Rock");
        assert_eq!(k.main_logo().unwrap().name, "main.png");
        assert_eq!(k.logos.len(), 2);
        let names: Vec<_> = k.groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Assets", "Images", "Intros"]);
        assert_eq!(k.groups[0].items[0].kind, MediaKind::Audio);
        assert_eq!(k.groups[1].items.len(), 1, "non-media files are skipped");
        assert!(k.all_assets().all(|a| !a.path.to_string_lossy().contains("_duplicates")));
        assert_eq!(k.color("gold"), Some([0xD9, 0xA4, 0x41]));
        assert_eq!(k.warnings.len(), 1, "{:?}", k.warnings);
        assert_eq!(k.find("intro-1").unwrap().name, "intro-1.mp4");
        assert_eq!(k.find("Images/card.jpg").unwrap().name, "card.jpg");
        assert!(k.find("missing").is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `BRAND_KIT="<folder>" cargo test --lib brand_kit_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn brand_kit_live() {
        let root = std::env::var("BRAND_KIT").map(PathBuf::from).ok().or_else(saved_folder).expect("set BRAND_KIT");
        let k = load(&root).unwrap();
        println!("{} {} warnings={:?}", k.display_name(), k.info.handle, k.warnings);
        println!("colours: {:?}", k.info.colors.iter().map(|c| format!("{} {}", c.name, c.hex)).collect::<Vec<_>>());
        for a in &k.logos {
            println!("logo  {:?} {}", a.kind, a.name);
        }
        for g in &k.groups {
            println!("{}: {:?}", g.name, g.items.iter().map(|a| a.name.as_str()).collect::<Vec<_>>());
        }
        let mut p = Project::default();
        let intro = k.find("intro-1").unwrap().path.clone();
        let (_, c1) = add_to_project(&mut p, &intro, 0.0, Placement::Timeline).unwrap();
        let logo = k.main_logo().unwrap().path.clone();
        let (_, c2) = add_to_project(&mut p, &logo, 2.0, Placement::Corner).unwrap();
        let again = add_to_project(&mut p, &logo, 6.0, Placement::Corner).unwrap();
        assert_eq!(p.media.len(), 2, "the logo is imported once");
        for c in [c1, c2, again.1] {
            let c = p.clip(c.unwrap()).unwrap();
            println!("clip {} start {:.1} dur {:.2} track {:?}", c.id, c.start, c.duration, p.track_kind_for(c));
        }
        println!("tracks: {}", p.tracks.len());
    }

    #[test]
    fn corner_placement_goes_small_above_the_footage() {
        use crate::model::{MediaItem, TrackKind};
        let mut p = Project::default();
        let item = |kind, width, height, duration| MediaItem {
            id: 0,
            path: PathBuf::from("x"),
            name: "x".into(),
            kind,
            duration,
            width,
            height,
            fps: 30.0,
            has_audio: false,
        };
        let media = p.add_media(item(MediaKind::Image, 400, 400, 0.0));
        let footage = p.add_media(item(MediaKind::Video, 1920, 1080, 10.0));
        place(&mut p, footage, 0.0, Placement::Timeline).unwrap();
        let clip = place(&mut p, media, 1.0, Placement::Corner).unwrap();
        let main = p.main_track();
        let (ti, c) = p.tracks.iter().enumerate().find_map(|(i, t)| t.clips.iter().find(|c| c.id == clip).map(|c| (i, c.clone()))).unwrap();
        assert!(ti < main, "above the main track");
        assert_eq!(p.tracks[ti].kind, TrackKind::Video);
        assert!(c.transform.scale < 0.5 && c.transform.x > 0.0 && c.transform.y < 0.0);
        assert_eq!(p.tracks[main].clips.len(), 1, "footage stays alone on the main track");
        assert_eq!(place(&mut p, media, 0.0, Placement::LibraryOnly), None);
    }
}
