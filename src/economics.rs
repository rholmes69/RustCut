//! Production economics: a SQLite database (`economics.sqlite` in the config folder)
//! that records what RustCut actually spent on AI models (per output file), holds the
//! prices of commercial tools, and builds side-by-side cost reports for a project at the
//! clip and audio level: RustCut vs commercial tool stacks.
//!
//! Prices are seeded from public, mostly third-party sources (dated, with links) and can
//! be edited in the database or with [`set_rate`] / [`set_subscription`].

use crate::model::{ClipSource, MediaKind, Project, TrackKind};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const PRICES_AS_OF: &str = "2026-10";

/// The project the editor (or MCP server) is working on, so chat-model spend that has no
/// output file can be attributed to it.
static CURRENT_PROJECT: Mutex<Option<String>> = Mutex::new(None);

pub fn set_current_project(path: Option<&Path>) {
    if let Ok(mut p) = CURRENT_PROJECT.lock() {
        *p = path.map(|p| norm(&p.to_string_lossy()));
    }
}

fn current_project() -> Option<String> {
    CURRENT_PROJECT.lock().ok().and_then(|p| p.clone())
}

pub fn db_path() -> PathBuf {
    crate::util::config_dir().join("economics.sqlite")
}

/// Normalised path key: forward slashes, lower case on Windows.
pub fn norm(p: &str) -> String {
    let s = p.replace('\\', "/");
    if cfg!(windows) { s.to_lowercase() } else { s }
}

pub fn open() -> Result<Connection> {
    let path = db_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
    migrate(&conn)?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         CREATE TABLE IF NOT EXISTS tools(
             id INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL, vendor TEXT NOT NULL, kind TEXT NOT NULL,
             monthly_usd REAL NOT NULL DEFAULT 0, notes TEXT NOT NULL DEFAULT '', source_url TEXT NOT NULL DEFAULT '', as_of TEXT NOT NULL DEFAULT '');
         CREATE TABLE IF NOT EXISTS rates(
             tool_id INTEGER NOT NULL REFERENCES tools(id), task TEXT NOT NULL, unit TEXT NOT NULL,
             usd_per_unit REAL NOT NULL, notes TEXT NOT NULL DEFAULT '', PRIMARY KEY(tool_id, task));
         CREATE TABLE IF NOT EXISTS stacks(id INTEGER PRIMARY KEY, name TEXT UNIQUE NOT NULL, description TEXT NOT NULL DEFAULT '');
         CREATE TABLE IF NOT EXISTS stack_tools(stack_id INTEGER NOT NULL, tool_id INTEGER NOT NULL, PRIMARY KEY(stack_id, tool_id));
         CREATE TABLE IF NOT EXISTS spend(
             id INTEGER PRIMARY KEY, at TEXT NOT NULL DEFAULT (datetime('now')), kind TEXT NOT NULL, model TEXT NOT NULL,
             usd REAL NOT NULL, units REAL NOT NULL DEFAULT 0, unit TEXT NOT NULL DEFAULT '',
             output_path TEXT, project TEXT, source TEXT NOT NULL DEFAULT 'live', note TEXT NOT NULL DEFAULT '');
         CREATE INDEX IF NOT EXISTS spend_output ON spend(output_path);
         CREATE TABLE IF NOT EXISTS lineage(child TEXT NOT NULL, parent TEXT NOT NULL, PRIMARY KEY(child, parent));
         CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE IF NOT EXISTS reports(
             id INTEGER PRIMARY KEY, at TEXT NOT NULL DEFAULT (datetime('now')), project TEXT NOT NULL,
             rustcut_usd REAL NOT NULL, summary TEXT NOT NULL, json TEXT NOT NULL);",
    )?;
    let seeded: i64 = conn.query_row("SELECT COUNT(*) FROM tools", [], |r| r.get(0))?;
    if seeded == 0 {
        seed(conn)?;
    }
    Ok(())
}

// ── seed prices ──────────────────────────────────────────────────────────────

/// Tasks a clip or audio track can need.
pub mod task {
    pub const AI_VIDEO_SECOND: &str = "ai_video_second";
    pub const AI_IMAGE: &str = "ai_image";
    pub const TRANSCRIPTION_MINUTE: &str = "transcription_minute";
    pub const NOISE_REMOVAL_MINUTE: &str = "noise_removal_minute";
    pub const MASTERING_TRACK: &str = "mastering_track";
    pub const BEAT_DETECTION: &str = "beat_detection";
    pub const VISUALIZER_RENDER: &str = "visualizer_render";
    pub const MULTICAM_SYNC: &str = "multicam_sync";
}

struct SeedTool {
    name: &'static str,
    vendor: &'static str,
    kind: &'static str,
    monthly: f64,
    notes: &'static str,
    url: &'static str,
    rates: &'static [(&'static str, &'static str, f64, &'static str)],
}

const SEED: &[SeedTool] = &[
    SeedTool { name: "Adobe Premiere Pro", vendor: "Adobe", kind: "editor", monthly: 22.99, notes: "Single app; third-party listings range $20.99–34.49/mo depending on billing.",
        url: "https://checkthat.ai/brands/adobe-premiere/pricing",
        rates: &[("transcription_minute", "minute", 0.0, "Speech to Text included"), ("noise_removal_minute", "minute", 0.0, "Enhance Speech included"),
                 ("mastering_track", "track", 0.0, "Loudness match included"), ("multicam_sync", "project", 0.0, "Audio sync included")] },
    SeedTool { name: "Adobe Firefly Pro", vendor: "Adobe", kind: "ai_generation", monthly: 19.99, notes: "4,000 credits ≈ 40 five-second video clips per month.",
        url: "https://saascrmreview.com/adobe-firefly-pricing/",
        rates: &[("ai_video_second", "second", 0.10, "$19.99 ÷ ~200 s of video per month"), ("ai_image", "image", 0.02, "estimated from credit allowance")] },
    SeedTool { name: "CapCut Pro", vendor: "ByteDance", kind: "editor", monthly: 19.99, notes: "US benchmark; $7.99–19.99/mo depending on region and store.",
        url: "https://eesel.ai/blog/capcut-pricing",
        rates: &[("transcription_minute", "minute", 0.0, "Auto captions included"), ("noise_removal_minute", "minute", 0.0, "Reduce noise included"),
                 ("beat_detection", "project", 0.0, "Auto beat included"), ("mastering_track", "track", 0.0, "Loudness normalise included"),
                 ("visualizer_render", "render", 0.0, "Audio visualizer effects included")] },
    SeedTool { name: "Runway Pro", vendor: "Runway", kind: "ai_generation", monthly: 35.0, notes: "2,250 credits/mo. Gen-4 Turbo 5 credits/s (≈$0.08/s); Gen-4.5 12 credits/s (≈$0.31/s).",
        url: "https://eesel.ai/blog/runway-ai-pricing",
        rates: &[("ai_video_second", "second", 0.08, "Gen-4 Turbo, the tier comparable to RustCut's default video model")] },
    SeedTool { name: "Midjourney Basic", vendor: "Midjourney", kind: "ai_generation", monthly: 10.0, notes: "~200 Fast minutes/mo; Standard $30 adds unlimited Relax.",
        url: "https://eesel.ai/blog/midjourney-pricing",
        rates: &[("ai_image", "image", 0.05, "≈ $10 ÷ 200 fast jobs")] },
    SeedTool { name: "Descript Creator", vendor: "Descript", kind: "editor", monthly: 35.0, notes: "Monthly billing ($24 annual); ~10 transcription hours per month.",
        url: "https://sonix.ai/resources/descript-pricing/",
        rates: &[("transcription_minute", "minute", 0.0, "included up to ~10 h/month"), ("noise_removal_minute", "minute", 0.0, "Studio Sound included")] },
    SeedTool { name: "eMastered", vendor: "eMastered", kind: "audio", monthly: 19.0, notes: "Yearly plan billed monthly; unlimited masters.",
        url: "https://help.emastered.com/hc/en-us/articles/5615662953101",
        rates: &[("mastering_track", "track", 0.0, "unlimited with subscription")] },
    SeedTool { name: "Google Veo 3.1 Lite (API)", vendor: "Google", kind: "api", monthly: 0.0, notes: "Pay as you go, 720p. Fast $0.10/s, standard $0.40/s.",
        url: "https://the-decoder.com/googles-veo-3-1-lite-cuts-video-generation-costs-by-more-than-half/",
        rates: &[("ai_video_second", "second", 0.05, "720p")] },
    SeedTool { name: "Rev AI transcription", vendor: "Rev", kind: "api", monthly: 0.0, notes: "Consumer AI transcription; human transcription $1.99/min.",
        url: "https://sonix.ai/resources/rev-pricing/",
        rates: &[("transcription_minute", "minute", 0.25, "AI transcription")] },
    SeedTool { name: "Generic image API", vendor: "various", kind: "api", monthly: 0.0, notes: "Typical pay-per-image price for 1K images.",
        url: "", rates: &[("ai_image", "image", 0.04, "estimate")] },
];

const STACKS: &[(&str, &str, &[&str])] = &[
    ("Adobe", "Premiere Pro + Firefly Pro", &["Adobe Premiere Pro", "Adobe Firefly Pro"]),
    ("CapCut Pro", "CapCut Pro, with pay-as-you-go APIs where CapCut has no equivalent", &["CapCut Pro", "Google Veo 3.1 Lite (API)", "Generic image API"]),
    ("Runway + Descript", "Runway Pro + Midjourney Basic + Descript Creator + eMastered", &["Runway Pro", "Midjourney Basic", "Descript Creator", "eMastered"]),
    ("Pay-as-you-go APIs", "Veo 3.1 Lite + image API + Rev AI transcription (no subscriptions)", &["Google Veo 3.1 Lite (API)", "Generic image API", "Rev AI transcription"]),
];

fn seed(conn: &Connection) -> Result<()> {
    for t in SEED {
        conn.execute(
            "INSERT INTO tools(name, vendor, kind, monthly_usd, notes, source_url, as_of) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![t.name, t.vendor, t.kind, t.monthly, t.notes, t.url, PRICES_AS_OF],
        )?;
        let id = conn.last_insert_rowid();
        for (task, unit, usd, note) in t.rates {
            conn.execute("INSERT INTO rates(tool_id, task, unit, usd_per_unit, notes) VALUES (?1,?2,?3,?4,?5)", params![id, task, unit, usd, note])?;
        }
    }
    for (name, desc, tools) in STACKS {
        conn.execute("INSERT INTO stacks(name, description) VALUES (?1,?2)", params![name, desc])?;
        let sid = conn.last_insert_rowid();
        for t in *tools {
            conn.execute("INSERT INTO stack_tools(stack_id, tool_id) SELECT ?1, id FROM tools WHERE name = ?2", params![sid, t])?;
        }
    }
    conn.execute("INSERT OR IGNORE INTO settings(key, value) VALUES ('videos_per_month', '4')", [])?;
    Ok(())
}

// ── ledger ───────────────────────────────────────────────────────────────────

/// One paid model call.
#[derive(Clone, Debug, Default)]
pub struct Spend<'a> {
    pub kind: &'a str,
    pub model: &'a str,
    pub usd: f64,
    pub units: f64,
    pub unit: &'a str,
    pub output: Option<&'a Path>,
    pub source: &'a str,
    pub note: &'a str,
}

fn insert_spend(conn: &Connection, s: &Spend, project: Option<String>) -> Result<()> {
    conn.execute(
        "INSERT INTO spend(kind, model, usd, units, unit, output_path, project, source, note) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![s.kind, s.model, s.usd, s.units, s.unit, s.output.map(|p| norm(&p.to_string_lossy())), project, if s.source.is_empty() { "live" } else { s.source }, s.note],
    )?;
    Ok(())
}

/// Records a paid call. Never fails the caller: problems go to the error log.
pub fn record(s: Spend) {
    let r = open().and_then(|c| insert_spend(&c, &s, current_project()));
    if let Err(e) = r {
        crate::app::log_error(&format!("economics: could not record spend: {e:#}"));
    }
}

/// Records that `child` was made from `parent` (a shot's video from its frame).
pub fn link(child: &Path, parent: &Path) {
    let r = open().and_then(|c| {
        c.execute("INSERT OR IGNORE INTO lineage(child, parent) VALUES (?1,?2)", params![norm(&child.to_string_lossy()), norm(&parent.to_string_lossy())])?;
        Ok(())
    });
    if let Err(e) = r {
        crate::app::log_error(&format!("economics: could not record lineage: {e:#}"));
    }
}

/// Estimates spend for a storyboard made before the ledger existed (marked `estimate`).
/// Frames at `image_usd` each, planning at `plan_usd`, the rest shared by the shot videos.
/// Returns the number of rows written (0 if it was already imported).
pub fn backfill_storyboard(conn: &Connection, sb_json: &Path, image_usd: f64, plan_usd: f64) -> Result<usize> {
    let sb: crate::ai::storyboard::Storyboard = crate::ai::storyboard::Storyboard::load(sb_json)?;
    let dir = sb.dir.clone();
    let key = norm(&dir.to_string_lossy());
    let already: i64 = conn.query_row("SELECT COUNT(*) FROM spend WHERE output_path LIKE ?1 || '%'", params![key], |r| r.get(0))?;
    if already > 0 {
        return Ok(0);
    }
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok().map(|e| e.path())).collect();
    let is = |p: &PathBuf, ext: &str| p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext));
    let name = |p: &PathBuf| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let frames: Vec<&PathBuf> = files.iter().filter(|p| is(p, "png") && name(p).starts_with("shot-") && !name(p).contains("own")).collect();
    let videos: Vec<&PathBuf> = files.iter().filter(|p| is(p, "mp4") && name(p).starts_with("shot-")).collect();
    let video_usd = ((sb.spent - plan_usd - image_usd * frames.len() as f64) / videos.len().max(1) as f64).max(0.0);
    let mut n = 0;
    let note = "estimated from the storyboard total";
    insert_spend(conn, &Spend { kind: "llm", model: "storyboard planning", usd: plan_usd, units: 1.0, unit: "call", output: Some(&dir.join("storyboard.json")), source: "estimate", note }, None)?;
    n += 1;
    for f in &frames {
        insert_spend(conn, &Spend { kind: "image", model: "storyboard frame", usd: image_usd, units: 1.0, unit: "image", output: Some(f), source: "estimate", note }, None)?;
        n += 1;
    }
    for v in &videos {
        let secs = crate::media::ffmpeg::probe(v).map(|i| i.duration).unwrap_or(0.0);
        insert_spend(conn, &Spend { kind: "video", model: "shot video", usd: video_usd, units: secs, unit: "second", output: Some(v), source: "estimate", note }, None)?;
        n += 1;
    }
    // Lineage: each shot's current video was made from its current frame.
    for s in &sb.shots {
        if let (Some(f), Some(v)) = (&s.frame, &s.video) {
            conn.execute("INSERT OR IGNORE INTO lineage(child, parent) VALUES (?1,?2)", params![norm(&v.to_string_lossy()), norm(&f.to_string_lossy())])?;
        }
    }
    Ok(n)
}

// ── prices ───────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Stack {
    pub name: String,
    pub description: String,
    /// (tool, monthly USD)
    pub subscriptions: Vec<(String, f64)>,
    /// task → (USD per unit, tool, note)
    pub rates: HashMap<String, (f64, String, String)>,
}

impl Stack {
    pub fn monthly(&self) -> f64 {
        self.subscriptions.iter().map(|s| s.1).sum()
    }
}

pub fn stacks(conn: &Connection) -> Result<Vec<Stack>> {
    let mut out = Vec::new();
    let mut st = conn.prepare("SELECT id, name, description FROM stacks ORDER BY id")?;
    let rows: Vec<(i64, String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
    for (id, name, description) in rows {
        let mut subs = Vec::new();
        let mut q = conn.prepare("SELECT t.name, t.monthly_usd FROM tools t JOIN stack_tools s ON s.tool_id = t.id WHERE s.stack_id = ?1 AND t.monthly_usd > 0 ORDER BY t.id")?;
        for r in q.query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))? {
            subs.push(r?);
        }
        let mut rates: HashMap<String, (f64, String, String)> = HashMap::new();
        let mut q = conn.prepare("SELECT r.task, r.usd_per_unit, t.name, r.notes FROM rates r JOIN tools t ON t.id = r.tool_id JOIN stack_tools s ON s.tool_id = t.id WHERE s.stack_id = ?1")?;
        for r in q.query_map(params![id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?)))? {
            let (task, usd, tool, note) = r?;
            // The cheapest tool in the stack for each task.
            if rates.get(&task).is_none_or(|e| usd < e.0) {
                rates.insert(task, (usd, tool, note));
            }
        }
        out.push(Stack { name, description, subscriptions: subs, rates });
    }
    Ok(out)
}

pub fn videos_per_month(conn: &Connection) -> f64 {
    conn.query_row("SELECT value FROM settings WHERE key = 'videos_per_month'", [], |r| r.get::<_, String>(0))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4.0f64)
        .max(1.0)
}

pub fn set_videos_per_month(conn: &Connection, n: f64) -> Result<()> {
    conn.execute("INSERT INTO settings(key, value) VALUES ('videos_per_month', ?1) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![format!("{}", n.max(1.0))])?;
    Ok(())
}

/// Changes (or adds) a tool's price for a task.
pub fn set_rate(conn: &Connection, tool: &str, task: &str, unit: &str, usd: f64, note: &str) -> Result<()> {
    let id: i64 = conn.query_row("SELECT id FROM tools WHERE name = ?1", params![tool], |r| r.get(0)).optional()?.ok_or_else(|| anyhow::anyhow!("no tool named {tool}"))?;
    conn.execute(
        "INSERT INTO rates(tool_id, task, unit, usd_per_unit, notes) VALUES (?1,?2,?3,?4,?5)
         ON CONFLICT(tool_id, task) DO UPDATE SET unit = excluded.unit, usd_per_unit = excluded.usd_per_unit, notes = excluded.notes",
        params![id, task, unit, usd, note],
    )?;
    Ok(())
}

pub fn set_subscription(conn: &Connection, tool: &str, monthly_usd: f64) -> Result<()> {
    let n = conn.execute("UPDATE tools SET monthly_usd = ?2, as_of = date('now') WHERE name = ?1", params![tool, monthly_usd])?;
    anyhow::ensure!(n == 1, "no tool named {tool}");
    Ok(())
}

// ── reports ──────────────────────────────────────────────────────────────────

/// A cost for one stack: dollars, or not offered.
#[derive(Clone, Debug, PartialEq)]
pub enum Cost {
    Usd(f64, String),
    NotOffered,
}

impl Cost {
    pub fn usd(&self) -> f64 {
        match self {
            Cost::Usd(v, _) => *v,
            Cost::NotOffered => 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Item {
    pub task: String,
    pub qty: f64,
    pub unit: String,
    pub rustcut: f64,
    pub rustcut_note: String,
    pub stacks: Vec<Cost>,
}

#[derive(Clone, Debug)]
pub struct Row {
    /// "Video", "Audio" or "Project".
    pub scope: String,
    pub track: String,
    pub start: f64,
    pub duration: f64,
    pub label: String,
    pub items: Vec<Item>,
}

impl Row {
    pub fn rustcut(&self) -> f64 {
        self.items.iter().map(|i| i.rustcut).sum()
    }
    pub fn stack(&self, k: usize) -> f64 {
        self.items.iter().map(|i| i.stacks[k].usd()).sum()
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub project: String,
    pub duration: f64,
    pub stacks: Vec<Stack>,
    pub videos_per_month: f64,
    pub rows: Vec<Row>,
    pub estimated: bool,
}

impl Report {
    pub fn rustcut_total(&self) -> f64 {
        self.rows.iter().map(Row::rustcut).sum()
    }
    pub fn stack_total(&self, k: usize) -> f64 {
        self.rows.iter().map(|r| r.stack(k)).sum()
    }
}

struct Ledger {
    /// path → (usd, kind, units, estimated)
    direct: HashMap<String, (f64, String, f64, bool)>,
    parents: HashMap<String, Vec<String>>,
}

fn load_ledger(conn: &Connection) -> Result<Ledger> {
    let mut direct: HashMap<String, (f64, String, f64, bool)> = HashMap::new();
    let mut st = conn.prepare("SELECT output_path, usd, kind, units, source FROM spend WHERE output_path IS NOT NULL")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?, r.get::<_, String>(2)?, r.get::<_, f64>(3)?, r.get::<_, String>(4)?)))? {
        let (p, usd, kind, units, source) = r?;
        let e = direct.entry(p).or_insert((0.0, kind, 0.0, false));
        e.0 += usd;
        e.2 = e.2.max(units);
        e.3 |= source != "live";
    }
    let mut parents: HashMap<String, Vec<String>> = HashMap::new();
    let mut st = conn.prepare("SELECT child, parent FROM lineage")?;
    for r in st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (c, p) = r?;
        parents.entry(c).or_default().push(p);
    }
    Ok(Ledger { direct, parents })
}

fn stack_cost(stack: &Stack, task: &str, qty: f64) -> Cost {
    match stack.rates.get(task) {
        Some((usd, tool, note)) => {
            let label = if *usd == 0.0 { format!("{tool}: {note}") } else { format!("{tool} at ${usd:.3}/unit") };
            Cost::Usd(usd * qty, label)
        }
        None => Cost::NotOffered,
    }
}

/// Builds the side-by-side report for a project. `project_path` attributes chat-model
/// spend (rough cut, planning) recorded while that project was open.
pub fn build_report(conn: &Connection, project: &Project, project_path: Option<&Path>) -> Result<Report> {
    let stacks = stacks(conn)?;
    let vpm = videos_per_month(conn);
    let ledger = load_ledger(conn)?;
    let n = stacks.len();
    let mut estimated = false;
    let total = project.duration().max(0.001);
    let media_by_id: HashMap<_, _> = project.media.iter().map(|m| (m.id, m)).collect();
    // Visible clips per media, for sharing a generated file's cost between the clips that use it.
    let mut uses: HashMap<u64, f64> = HashMap::new();
    for t in project.tracks.iter().filter(|t| !t.hidden) {
        for c in &t.clips {
            if let ClipSource::Media(m) = c.source {
                *uses.entry(m).or_default() += c.duration;
            }
        }
    }
    let has_lyrics = project.tracks.iter().any(|t| t.captions && !t.clips.is_empty());
    let mut used_paths: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    // Cost of producing a media file: its own spend plus its parents' (a video's frame).
    let produce = |path: &str| -> (f64, Option<(String, f64)>, Vec<(String, f64)>, bool) {
        let mut est = false;
        let own = ledger.direct.get(path).cloned();
        let mut total = own.as_ref().map_or(0.0, |o| o.0);
        est |= own.as_ref().is_some_and(|o| o.3);
        let mut parents = Vec::new();
        for p in ledger.parents.get(path).into_iter().flatten() {
            if let Some(d) = ledger.direct.get(p) {
                total += d.0;
                est |= d.3;
                parents.push((d.1.clone(), d.0));
            }
        }
        (total, own.map(|o| (o.1, o.2)), parents, est)
    };
    for t in project.tracks.iter().filter(|t| !t.hidden) {
        for c in &t.clips {
            let ClipSource::Media(mid) = c.source else { continue };
            let Some(m) = media_by_id.get(&mid) else { continue };
            let key = norm(&m.path.to_string_lossy());
            let share = c.duration / uses.get(&mid).copied().unwrap_or(c.duration).max(0.001);
            let mut items = Vec::new();
            let audio = t.kind == TrackKind::Audio || m.kind == MediaKind::Audio;
            if !audio {
                let (spent, own, parents, est) = produce(&key);
                estimated |= est;
                used_paths.push(key.clone());
                used_paths.extend(ledger.parents.get(&key).cloned().unwrap_or_default());
                match own.as_ref().map(|o| o.0.as_str()) {
                    Some("video") => {
                        let secs = own.as_ref().map_or(m.duration, |o| if o.1 > 0.0 { o.1 } else { m.duration });
                        let video_spend = spent - parents.iter().map(|p| p.1).sum::<f64>();
                        items.push(Item {
                            task: "AI video generation".into(),
                            qty: secs * share,
                            unit: "s".into(),
                            rustcut: video_spend * share,
                            rustcut_note: if share < 0.999 { format!("{:.0}% of a reused shot", share * 100.0) } else { String::new() },
                            stacks: stacks.iter().map(|s| scale(stack_cost(s, task::AI_VIDEO_SECOND, secs), share)).collect(),
                        });
                        for (kind, usd) in parents.iter().filter(|p| p.0 == "image") {
                            let _ = kind;
                            items.push(Item {
                                task: "Storyboard frame (AI image)".into(),
                                qty: share,
                                unit: "image".into(),
                                rustcut: usd * share,
                                rustcut_note: String::new(),
                                stacks: stacks.iter().map(|s| scale(stack_cost(s, task::AI_IMAGE, 1.0), share)).collect(),
                            });
                        }
                    }
                    Some("image") => items.push(Item {
                        task: "AI image".into(),
                        qty: share,
                        unit: "image".into(),
                        rustcut: spent * share,
                        rustcut_note: String::new(),
                        stacks: stacks.iter().map(|s| scale(stack_cost(s, task::AI_IMAGE, 1.0), share)).collect(),
                    }),
                    _ => {}
                }
                if let Some(pm) = c.background_plate.and_then(|p| media_by_id.get(&p)) {
                    let pk = norm(&pm.path.to_string_lossy());
                    let (spent, _, _, est) = produce(&pk);
                    estimated |= est;
                    used_paths.push(pk);
                    items.push(Item { task: "Background fill (AI image)".into(), qty: 1.0, unit: "image".into(), rustcut: spent, rustcut_note: String::new(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::AI_IMAGE, 1.0)).collect() });
                }
                if m.path.file_name().is_some_and(|n| n.to_string_lossy().starts_with("visualizer-")) {
                    items.push(Item { task: "Audio visualizer".into(), qty: 1.0, unit: "render".into(), rustcut: 0.0, rustcut_note: "local FFmpeg".into(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::VISUALIZER_RENDER, 1.0)).collect() });
                }
                if t.angle.is_some() {
                    items.push(Item { task: "Multi-cam audio sync".into(), qty: 1.0, unit: "take".into(), rustcut: 0.0, rustcut_note: "local".into(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::MULTICAM_SYNC, 0.0)).collect() });
                }
            } else {
                let minutes = c.duration / 60.0;
                if has_lyrics {
                    items.push(Item { task: "Transcription / lyric timing".into(), qty: minutes, unit: "min".into(), rustcut: 0.0, rustcut_note: "local whisper".into(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::TRANSCRIPTION_MINUTE, minutes)).collect() });
                }
                if c.denoise.is_some() {
                    items.push(Item { task: "Noise removal".into(), qty: minutes, unit: "min".into(), rustcut: 0.0, rustcut_note: "local".into(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::NOISE_REMOVAL_MINUTE, minutes)).collect() });
                }
                if project.music.contains_key(&mid) {
                    items.push(Item { task: "Beat & section detection".into(), qty: 1.0, unit: "song".into(), rustcut: 0.0, rustcut_note: "local".into(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::BEAT_DETECTION, 1.0)).collect() });
                }
                if project.audio.loudness.is_some() && project.audio_role(c) == crate::model::AudioRole::Music {
                    items.push(Item { task: "Loudness mastering".into(), qty: 1.0, unit: "track".into(), rustcut: 0.0, rustcut_note: "local".into(),
                        stacks: stacks.iter().map(|s| stack_cost(s, task::MASTERING_TRACK, 1.0)).collect() });
                }
            }
            // Each clip carries its share (by duration) of the stack's monthly subscriptions for one video.
            let weight = c.duration / total / project.tracks.iter().filter(|t| !t.hidden && !t.clips.is_empty() && t.kind != TrackKind::Text).count().max(1) as f64;
            items.push(Item {
                task: "Subscriptions (share of this video)".into(),
                qty: weight,
                unit: "share".into(),
                rustcut: 0.0,
                rustcut_note: "RustCut is free".into(),
                stacks: stacks.iter().map(|s| Cost::Usd(s.monthly() / vpm * weight, format!("${:.2}/mo ÷ {vpm} videos", s.monthly()))).collect(),
            });
            rows.push(Row {
                scope: if audio { "Audio".into() } else { "Video".into() },
                track: t.name.clone(),
                start: c.start,
                duration: c.duration,
                label: m.name.clone(),
                items,
            });
        }
    }
    // Project level: chat models (planning, rough cut, reframe) and thumbnails.
    let mut project_items = Vec::new();
    if let Some(p) = project_path {
        let key = norm(&p.to_string_lossy());
        let (usd, calls): (f64, i64) = conn.query_row("SELECT COALESCE(SUM(usd),0), COUNT(*) FROM spend WHERE project = ?1 AND output_path IS NULL", params![key], |r| Ok((r.get(0)?, r.get(1)?)))?;
        if calls > 0 {
            project_items.push(Item { task: "AI editing (rough cut, planning, reframe)".into(), qty: calls as f64, unit: "calls".into(), rustcut: usd, rustcut_note: String::new(),
                stacks: (0..n).map(|_| Cost::NotOffered).collect() });
        }
    }
    // Storyboard planning and unused AI outputs in the folders this project's AI media came from.
    let mut dirs: Vec<String> = used_paths.iter().filter_map(|p| p.rsplit_once('/').map(|(d, _)| d.to_string())).collect();
    dirs.sort();
    dirs.dedup();
    for d in &dirs {
        let mut st = conn.prepare("SELECT output_path, usd, kind, source FROM spend WHERE output_path LIKE ?1 || '/%'")?;
        let rows_d: Vec<(String, f64, String, String)> = st.query_map(params![d], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<Result<_, _>>()?;
        let mut planning = 0.0;
        let mut unused = (0.0, 0usize);
        for (p, usd, kind, source) in rows_d {
            estimated |= source != "live";
            if used_paths.contains(&p) {
                continue;
            }
            if kind == "llm" {
                planning += usd;
            } else {
                unused.0 += usd;
                unused.1 += 1;
            }
        }
        if planning > 0.0 {
            project_items.push(Item { task: "Storyboard planning (AI)".into(), qty: 1.0, unit: "plan".into(), rustcut: planning, rustcut_note: String::new(), stacks: (0..n).map(|_| Cost::NotOffered).collect() });
        }
        if unused.1 > 0 {
            project_items.push(Item {
                task: "Redraws and replaced shots (not in the video)".into(),
                qty: unused.1 as f64,
                unit: "files".into(),
                rustcut: unused.0,
                rustcut_note: "paid, then replaced".into(),
                stacks: (0..n).map(|_| Cost::NotOffered).collect(),
            });
        }
    }
    if let Some(t) = &project.thumbnail {
        let (spent, _, _, est) = produce(&norm(&t.to_string_lossy()));
        estimated |= est;
        project_items.push(Item { task: "Thumbnail (AI image)".into(), qty: 1.0, unit: "image".into(), rustcut: spent, rustcut_note: String::new(),
            stacks: stacks.iter().map(|s| stack_cost(s, task::AI_IMAGE, 1.0)).collect() });
    }
    if !project_items.is_empty() {
        rows.push(Row { scope: "Project".into(), track: String::new(), start: 0.0, duration: project.duration(), label: "Whole project".into(), items: project_items });
    }
    Ok(Report {
        project: project_path.map_or_else(|| project.name.clone(), |p| p.display().to_string()),
        duration: project.duration(),
        stacks,
        videos_per_month: vpm,
        rows,
        estimated,
    })
}

fn scale(c: Cost, k: f64) -> Cost {
    match c {
        Cost::Usd(v, l) => Cost::Usd(v * k, l),
        Cost::NotOffered => Cost::NotOffered,
    }
}

pub fn save_report(conn: &Connection, r: &Report) -> Result<()> {
    let summary = r.stacks.iter().enumerate().map(|(k, s)| format!("{}: ${:.2}", s.name, r.stack_total(k))).collect::<Vec<_>>().join("; ");
    let json = serde_json::json!({
        "project": r.project,
        "rustcut_usd": r.rustcut_total(),
        "stacks": r.stacks.iter().enumerate().map(|(k, s)| serde_json::json!({ "name": s.name, "usd": r.stack_total(k), "monthly": s.monthly() })).collect::<Vec<_>>(),
        "rows": r.rows.len(),
    });
    conn.execute("INSERT INTO reports(project, rustcut_usd, summary, json) VALUES (?1,?2,?3,?4)", params![r.project, r.rustcut_total(), summary, json.to_string()])?;
    Ok(())
}

fn money(v: f64) -> String {
    if v == 0.0 { "$0".into() } else if v < 0.01 { "<$0.01".into() } else { format!("${v:.2}") }
}

fn cell(c: &Cost) -> String {
    match c {
        Cost::Usd(v, _) => money(*v),
        Cost::NotOffered => "—".into(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn clock(t: f64) -> String {
    format!("{}:{:04.1}", (t / 60.0).floor() as u64, t % 60.0)
}

/// CSV with one line per item.
pub fn to_csv(r: &Report) -> String {
    let mut out = String::from("scope,track,start,duration,clip,task,qty,unit,rustcut_usd");
    for s in &r.stacks {
        out += &format!(",\"{} usd\"", s.name);
    }
    out += "\n";
    for row in &r.rows {
        for i in &row.items {
            out += &format!("{},\"{}\",{:.2},{:.2},\"{}\",\"{}\",{:.3},{},{:.4}", row.scope, row.track.replace('"', "'"), row.start, row.duration, row.label.replace('"', "'"), i.task, i.qty, i.unit, i.rustcut);
            for c in &i.stacks {
                out += &match c {
                    Cost::Usd(v, _) => format!(",{v:.4}"),
                    Cost::NotOffered => ",".into(),
                };
            }
            out += "\n";
        }
    }
    out
}

/// A self-contained HTML report (summary + clip and audio detail), light and dark aware.
pub fn to_html(r: &Report) -> String {
    let name = Path::new(&r.project).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| r.project.clone());
    let mut h = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{} cost report</title><style>
:root{{--bg:#f4f6f9;--card:#fff;--ink:#141a24;--muted:#5d6779;--rule:#dde2ea;--accent:#0b7f8b;--gold:#a9781c;--save:#1e7a43}}
@media (prefers-color-scheme:dark){{:root{{--bg:#10141b;--card:#181e28;--ink:#e7ebf1;--muted:#9aa4b5;--rule:#2b3342;--accent:#3cc3cf;--gold:#e0b04a;--save:#5fd08f}}}}
body{{background:var(--bg);color:var(--ink);font:14.5px/1.55 'Segoe UI',system-ui,sans-serif;margin:0;padding:32px 16px}}
main{{max-width:1180px;margin:0 auto;display:grid;gap:26px}} h1{{font-size:26px;margin:0}} h2{{font-size:17px;margin:0 0 8px}}
.muted{{color:var(--muted)}} .cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:12px}}
.card{{background:var(--card);border:1px solid var(--rule);border-radius:10px;padding:14px 16px}} .big{{font-size:24px;font-weight:700;font-variant-numeric:tabular-nums}}
.rc .big{{color:var(--accent)}} .save{{color:var(--save);font-weight:600}}
.wrap{{overflow-x:auto;background:var(--card);border:1px solid var(--rule);border-radius:10px}}
table{{border-collapse:collapse;width:100%;min-width:900px}} th,td{{padding:7px 10px;border-bottom:1px solid var(--rule);text-align:left;vertical-align:top}}
th{{font-size:11.5px;text-transform:uppercase;letter-spacing:.05em;color:var(--muted)}} td.n,th.n{{text-align:right;font-variant-numeric:tabular-nums;white-space:nowrap}}
tr.clip td{{border-top:2px solid var(--rule);font-weight:600}} td.rc{{color:var(--accent)}} tr.total td{{font-weight:700;background:color-mix(in srgb,var(--accent) 10%,transparent)}}
.tag{{font-size:11px;padding:1px 6px;border:1px solid var(--rule);border-radius:99px;color:var(--muted)}}
</style></head><body><main><header><h1>{} — production cost report</h1><p class=\"muted\">{} · {} long · {} stacks compared · subscriptions spread over {} videos a month · prices as of {}{}</p></header>",
        esc(&name), esc(&name), esc(&r.project), clock(r.duration), r.stacks.len(), r.videos_per_month, PRICES_AS_OF,
        if r.estimated { " · some RustCut amounts are estimates (spent before the ledger existed)" } else { "" }
    );
    // Summary cards.
    h += "<section class=\"cards\">";
    h += &format!("<div class=\"card rc\"><div class=\"muted\">RustCut (actual AI spend)</div><div class=\"big\">{}</div><div class=\"muted\">no subscription</div></div>", money(r.rustcut_total()));
    for (k, s) in r.stacks.iter().enumerate() {
        let t = r.stack_total(k);
        let diff = t - r.rustcut_total();
        h += &format!(
            "<div class=\"card\"><div class=\"muted\">{}</div><div class=\"big\">{}</div><div class=\"muted\">{} · {}</div><div class=\"{}\">{}</div></div>",
            esc(&s.name), money(t), esc(&s.description), if s.monthly() > 0.0 { format!("${:.2}/mo in subscriptions", s.monthly()) } else { "pay as you go".into() },
            if diff >= 0.0 { "save" } else { "muted" },
            if diff >= 0.0 { format!("RustCut saves {}", money(diff)) } else { format!("RustCut costs {} more", money(-diff)) }
        );
    }
    h += "</section>";
    // Per scope summary.
    for scope in ["Video", "Audio", "Project"] {
        let rows: Vec<&Row> = r.rows.iter().filter(|x| x.scope == scope).collect();
        if rows.is_empty() {
            continue;
        }
        h += &format!("<section><h2>{} — {}</h2><div class=\"wrap\"><table><thead><tr><th>Time</th><th>Clip / task</th><th class=\"n\">Qty</th><th class=\"n\">RustCut</th>",
            if scope == "Project" { "Project level" } else if scope == "Audio" { "Audio, clip by clip" } else { "Video clips, clip by clip" },
            if scope == "Project" { "not tied to one clip" } else { "each clip and its tasks" });
        for s in &r.stacks {
            h += &format!("<th class=\"n\">{}</th>", esc(&s.name));
        }
        h += "</tr></thead><tbody>";
        let mut tot = vec![0.0; r.stacks.len()];
        let mut tot_rc = 0.0;
        for row in rows {
            h += &format!("<tr class=\"clip\"><td>{}</td><td>{} <span class=\"tag\">{}</span></td><td class=\"n\">{:.1}s</td><td class=\"n rc\">{}</td>",
                clock(row.start), esc(&row.label), esc(&row.track), row.duration, money(row.rustcut()));
            for k in 0..r.stacks.len() {
                h += &format!("<td class=\"n\">{}</td>", money(row.stack(k)));
                tot[k] += row.stack(k);
            }
            tot_rc += row.rustcut();
            h += "</tr>";
            for i in &row.items {
                h += &format!("<tr><td></td><td class=\"muted\">{}{}</td><td class=\"n muted\">{:.2} {}</td><td class=\"n rc\">{}</td>",
                    esc(&i.task), if i.rustcut_note.is_empty() { String::new() } else { format!(" <span class=\"tag\">{}</span>", esc(&i.rustcut_note)) }, i.qty, esc(&i.unit), money(i.rustcut));
                for c in &i.stacks {
                    let title = match c { Cost::Usd(_, l) => l.clone(), Cost::NotOffered => "no equivalent in this stack".into() };
                    h += &format!("<td class=\"n muted\" title=\"{}\">{}</td>", esc(&title), cell(c));
                }
                h += "</tr>";
            }
        }
        h += &format!("<tr class=\"total\"><td></td><td>Total ({scope})</td><td></td><td class=\"n rc\">{}</td>", money(tot_rc));
        for t in tot {
            h += &format!("<td class=\"n\">{}</td>", money(t));
        }
        h += "</tr></tbody></table></div></section>";
    }
    // Price sources.
    h += "<section><h2>Prices used</h2><div class=\"wrap\"><table><thead><tr><th>Stack</th><th>Subscriptions</th><th>Per-use rates</th></tr></thead><tbody>";
    for s in &r.stacks {
        let subs = s.subscriptions.iter().map(|(t, m)| format!("{} ${m:.2}/mo", esc(t))).collect::<Vec<_>>().join("<br>");
        let mut rates: Vec<String> = s.rates.iter().map(|(task, (usd, tool, note))| format!("{task}: ${usd:.3} ({} — {})", esc(tool), esc(note))).collect();
        rates.sort();
        h += &format!("<tr><td>{}</td><td>{}</td><td class=\"muted\">{}</td></tr>", esc(&s.name), if subs.is_empty() { "none".into() } else { subs }, rates.join("<br>"));
    }
    h += "</tbody></table></div><p class=\"muted\">Commercial prices come from public, mostly third-party sources and vary by region and billing period; edit them in economics.sqlite. \"—\" means the stack has no equivalent feature. RustCut's local features (transcription, beat detection, mastering, visualizers, multi-cam sync, local AI) cost $0.</p></section></main></body></html>";
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};

    /// Manual: estimate a pre-ledger storyboard's spend into the REAL database and write a report.
    /// `ECON_PROJECT=project.rcproj ECON_OUT=report.html cargo test --lib econ_report -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn econ_report() {
        let (Ok(project), Ok(out)) = (std::env::var("ECON_PROJECT"), std::env::var("ECON_OUT")) else { return };
        let conn = open().unwrap();
        let p = Project::load(Path::new(&project)).unwrap();
        let mut dirs: Vec<PathBuf> = p.media.iter().filter_map(|m| m.path.parent().map(Path::to_path_buf)).filter(|d| d.join("storyboard.json").is_file()).collect();
        dirs.sort();
        dirs.dedup();
        for d in dirs {
            let n = backfill_storyboard(&conn, &d.join("storyboard.json"), 0.069, 0.011).unwrap();
            println!("backfilled {n} rows for {}", d.display());
        }
        let r = build_report(&conn, &p, Some(Path::new(&project))).unwrap();
        save_report(&conn, &r).unwrap();
        std::fs::write(&out, to_html(&r)).unwrap();
        println!("RustCut ${:.3}", r.rustcut_total());
        for (k, s) in r.stacks.iter().enumerate() {
            println!("{:<22} ${:.2}  (subs ${:.2}/mo)", s.name, r.stack_total(k), s.monthly());
        }
        println!("rows: {} video, {} audio, {} project", r.rows.iter().filter(|x| x.scope == "Video").count(), r.rows.iter().filter(|x| x.scope == "Audio").count(), r.rows.iter().filter(|x| x.scope == "Project").count());
    }

    #[test]
    fn seeded_prices_and_stacks() {
        let c = open_in_memory().unwrap();
        let s = stacks(&c).unwrap();
        assert_eq!(s.len(), 4);
        let adobe = &s[0];
        assert!((adobe.monthly() - 42.98).abs() < 1e-9);
        assert_eq!(adobe.rates[task::AI_VIDEO_SECOND].0, 0.10);
        assert_eq!(s[2].rates[task::AI_IMAGE].1, "Midjourney Basic");
        assert!(s[3].subscriptions.is_empty(), "pay-as-you-go has no subscriptions");
        set_rate(&c, "Runway Pro", task::AI_VIDEO_SECOND, "second", 0.31, "Gen-4.5").unwrap();
        assert_eq!(stacks(&c).unwrap()[2].rates[task::AI_VIDEO_SECOND].0, 0.31);
        set_subscription(&c, "CapCut Pro", 9.99).unwrap();
        assert!(set_subscription(&c, "Nope", 1.0).is_err());
    }

    #[test]
    fn report_splits_reused_shots_and_counts_audio_tasks() {
        let c = open_in_memory().unwrap();
        let mut p = Project::default();
        let song = p.add_media(MediaItem { id: 0, path: "C:/m/song.mp3".into(), name: "song.mp3".into(), kind: MediaKind::Audio, duration: 60.0, width: 0, height: 0, fps: 0.0, has_audio: true });
        let vid = p.add_media(MediaItem { id: 0, path: "C:/mv/shot-01-video.mp4".into(), name: "shot-01-video.mp4".into(), kind: MediaKind::Video, duration: 4.0, width: 1280, height: 720, fps: 24.0, has_audio: false });
        // The video cost $0.12 and its frame $0.07; the shot is used twice.
        insert_spend(&c, &Spend { kind: "video", model: "grok", usd: 0.12, units: 4.0, unit: "second", output: Some(Path::new("C:/mv/shot-01-video.mp4")), source: "live", note: "" }, None).unwrap();
        insert_spend(&c, &Spend { kind: "image", model: "gemini", usd: 0.07, units: 1.0, unit: "image", output: Some(Path::new("C:/mv/shot-01.png")), source: "live", note: "" }, None).unwrap();
        insert_spend(&c, &Spend { kind: "image", model: "gemini", usd: 0.07, units: 1.0, unit: "image", output: Some(Path::new("C:/mv/shot-01-old.png")), source: "live", note: "" }, None).unwrap();
        c.execute("INSERT INTO lineage VALUES (?1, ?2)", params![norm("C:/mv/shot-01-video.mp4"), norm("C:/mv/shot-01.png")]).unwrap();
        for start in [0.0, 4.0] {
            let mut clip = p.new_media_clip(vid, start).unwrap();
            clip.start = start;
            p.append_to_main(clip);
        }
        let sc = p.new_media_clip(song, 0.0).unwrap();
        p.place_clip(sc, None);
        p.audio.loudness = Some(-14.0);
        p.music.insert(song, crate::music::MusicAnalysis { bpm: 120.0, beats: vec![0.0, 0.5], first_downbeat: 0, beats_per_bar: 4, sections: vec![], beat_energy: vec![] });
        let r = build_report(&c, &p, None).unwrap();
        let video_rows: Vec<&Row> = r.rows.iter().filter(|x| x.scope == "Video").collect();
        assert_eq!(video_rows.len(), 2);
        // Each use carries half of video + frame.
        assert!((video_rows[0].rustcut() - 0.095).abs() < 1e-9, "{}", video_rows[0].rustcut());
        // Adobe: 4 s × $0.10 + 1 image × $0.02, halved, plus subscription share.
        let adobe_video = video_rows[0].items.iter().find(|i| i.task == "AI video generation").unwrap().stacks[0].usd();
        assert!((adobe_video - 0.2).abs() < 1e-9);
        let audio = r.rows.iter().find(|x| x.scope == "Audio").unwrap();
        assert!(audio.items.iter().any(|i| i.task == "Beat & section detection"));
        assert!(audio.items.iter().any(|i| i.task == "Loudness mastering"));
        // The old frame is shown as paid-but-unused at project level.
        let proj = r.rows.iter().find(|x| x.scope == "Project").unwrap();
        let unused = proj.items.iter().find(|i| i.task.starts_with("Redraws")).unwrap();
        assert!((unused.rustcut - 0.07).abs() < 1e-9);
        // Totals: RustCut = 0.12 + 0.07 + 0.07 (unused); subscriptions make the stacks more expensive.
        assert!((r.rustcut_total() - 0.26).abs() < 1e-9, "{}", r.rustcut_total());
        assert!(r.stack_total(0) > r.rustcut_total());
        let html = to_html(&r);
        assert!(html.contains("RustCut saves") && html.contains("Adobe"));
        assert!(to_csv(&r).lines().count() > 6);
        save_report(&c, &r).unwrap();
    }
}
