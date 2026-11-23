//! Uploads to the user's YouTube channel with everything YouTube asks for: title, description,
//! tags, category, language, privacy or a scheduled publish time, "made for kids", the altered or
//! synthetic content disclosure, licence, embedding, subscriber notification and a custom
//! thumbnail. [`suggest`] fills the details in from the project for free (name, lyrics, song
//! sections as chapters, AI shots → disclosure); [`write_with_ai`] drafts them with the editor
//! model (≈ 1¢). Uses the `youtube.upload` scope; sign-in and the upload protocol are in
//! `crate::google`.

use crate::google::{self, enc, Resumable, SCOPE_YOUTUBE};
use crate::localtime;
use crate::model::Project;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const UPLOAD_API: &str = "https://www.googleapis.com/upload/youtube/v3";

pub const TITLE_MAX: usize = 100;
pub const DESCRIPTION_MAX_BYTES: usize = 5000;
pub const TAGS_MAX: usize = 500;
pub const THUMBNAIL_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// YouTube's assignable video categories (the same in most regions).
pub const CATEGORIES: &[(&str, &str)] = &[
    ("1", "Film & Animation"),
    ("2", "Autos & Vehicles"),
    ("10", "Music"),
    ("15", "Pets & Animals"),
    ("17", "Sports"),
    ("19", "Travel & Events"),
    ("20", "Gaming"),
    ("22", "People & Blogs"),
    ("23", "Comedy"),
    ("24", "Entertainment"),
    ("25", "News & Politics"),
    ("26", "Howto & Style"),
    ("27", "Education"),
    ("28", "Science & Technology"),
    ("29", "Nonprofits & Activism"),
];

pub const LANGUAGES: &[(&str, &str)] = &[
    ("", "Not set"),
    ("en", "English"),
    ("en-US", "English (United States)"),
    ("en-GB", "English (United Kingdom)"),
    ("es", "Spanish"),
    ("fr", "French"),
    ("de", "German"),
    ("it", "Italian"),
    ("pt", "Portuguese"),
    ("ja", "Japanese"),
    ("ko", "Korean"),
    ("zh-Hans", "Chinese (Simplified)"),
    ("hi", "Hindi"),
    ("ar", "Arabic"),
    ("yo", "Yoruba"),
    ("sw", "Swahili"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Private,
    Unlisted,
    Public,
    /// Private until the publish time, then public.
    Scheduled,
}

impl Visibility {
    pub const ALL: [Visibility; 4] = [Visibility::Private, Visibility::Unlisted, Visibility::Public, Visibility::Scheduled];
    pub fn label(self) -> &'static str {
        match self {
            Visibility::Private => "Private",
            Visibility::Unlisted => "Unlisted",
            Visibility::Public => "Public",
            Visibility::Scheduled => "Scheduled",
        }
    }
}

/// Everything YouTube's upload form asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Details {
    pub title: String,
    pub description: String,
    /// Comma-separated in the form; sent as a list.
    pub tags: String,
    pub category: String,
    /// The title and description's language, and the audio's.
    pub language: String,
    pub visibility: Visibility,
    /// Local time for `Scheduled`, as typed ("2026-11-25 18:00").
    pub publish_at: String,
    /// YouTube requires an answer; `None` until the user chooses.
    pub made_for_kids: Option<bool>,
    /// Realistic altered or synthetic content (YouTube's AI disclosure).
    pub synthetic: bool,
    pub creative_commons: bool,
    pub embeddable: bool,
    pub notify_subscribers: bool,
    pub show_likes: bool,
    pub thumbnail: Option<PathBuf>,
}

impl Default for Details {
    fn default() -> Self {
        Self {
            title: String::new(),
            description: String::new(),
            tags: String::new(),
            category: "24".into(),
            language: String::new(),
            visibility: Visibility::Private,
            publish_at: String::new(),
            made_for_kids: None,
            synthetic: false,
            creative_commons: false,
            embeddable: true,
            notify_subscribers: true,
            show_likes: true,
            thumbnail: None,
        }
    }
}

pub fn tag_list(tags: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags.split(',').map(|t| t.trim().trim_start_matches('#').trim()).filter(|t| !t.is_empty()) {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(t)) {
            out.push(t.to_string());
        }
    }
    out
}

/// YouTube's count for the 500-character tag limit: tags with spaces count their quotes, and
/// tags are separated by commas.
pub fn tags_length(tags: &[String]) -> usize {
    tags.iter().map(|t| t.chars().count() + if t.contains(' ') { 2 } else { 0 }).sum::<usize>() + tags.len().saturating_sub(1)
}

/// Problems that would make YouTube refuse the upload, in the order the form shows the fields.
pub fn validate(d: &Details, video: Option<&Path>, now: i64) -> Vec<String> {
    let mut e = Vec::new();
    match video {
        None => e.push("Choose the video file to upload.".to_string()),
        Some(p) if !p.is_file() => e.push(format!("{} doesn't exist.", p.display())),
        _ => {}
    }
    let title = d.title.trim();
    if title.is_empty() {
        e.push("Add a title.".into());
    } else if title.chars().count() > TITLE_MAX {
        e.push(format!("The title is {} characters; YouTube allows {TITLE_MAX}.", title.chars().count()));
    }
    if title.contains(['<', '>']) {
        e.push("The title can't contain < or >.".into());
    }
    if d.description.len() > DESCRIPTION_MAX_BYTES {
        e.push(format!("The description is {} bytes; YouTube allows {DESCRIPTION_MAX_BYTES}.", d.description.len()));
    }
    if d.description.contains(['<', '>']) {
        e.push("The description can't contain < or >.".into());
    }
    let tags = tag_list(&d.tags);
    if tags_length(&tags) > TAGS_MAX {
        e.push(format!("The tags add up to {} characters; YouTube allows {TAGS_MAX}.", tags_length(&tags)));
    }
    if tags.iter().any(|t| t.contains(['<', '>'])) {
        e.push("Tags can't contain < or >.".into());
    }
    if d.made_for_kids.is_none() {
        e.push("Say whether the video is made for kids (YouTube requires it).".into());
    }
    if d.visibility == Visibility::Scheduled {
        match localtime::LocalTime::parse(&d.publish_at) {
            Err(err) => e.push(format!("Publish time: {err}.")),
            Ok(t) if t.to_unix() < now + 15 * 60 => e.push("The publish time must be at least 15 minutes from now.".into()),
            Ok(_) => {}
        }
    }
    if let Some(t) = &d.thumbnail {
        match std::fs::metadata(t) {
            Err(_) => e.push(format!("The thumbnail {} doesn't exist.", t.display())),
            Ok(m) if m.len() > THUMBNAIL_MAX_BYTES => e.push(format!("The thumbnail is {}; YouTube allows 2 MB.", google::size_label(m.len()))),
            Ok(_) => {}
        }
        let ext = t.extension().and_then(|x| x.to_str()).unwrap_or("").to_ascii_lowercase();
        if !matches!(ext.as_str(), "jpg" | "jpeg" | "png") {
            e.push("The thumbnail must be a JPG or PNG.".into());
        }
    }
    e
}

/// The `videos.insert` body for these details.
pub fn resource(d: &Details) -> Result<Value> {
    let mut snippet = json!({
        "title": d.title.trim(),
        "description": d.description,
        "tags": tag_list(&d.tags),
        "categoryId": d.category,
    });
    if !d.language.is_empty() {
        snippet["defaultLanguage"] = json!(d.language);
        snippet["defaultAudioLanguage"] = json!(d.language);
    }
    let mut status = json!({
        "privacyStatus": match d.visibility {
            Visibility::Private | Visibility::Scheduled => "private",
            Visibility::Unlisted => "unlisted",
            Visibility::Public => "public",
        },
        "selfDeclaredMadeForKids": d.made_for_kids.unwrap_or(false),
        "containsSyntheticMedia": d.synthetic,
        "license": if d.creative_commons { "creativeCommon" } else { "youtube" },
        "embeddable": d.embeddable,
        "publicStatsViewable": d.show_likes,
    });
    if d.visibility == Visibility::Scheduled {
        let t = localtime::LocalTime::parse(&d.publish_at)?;
        status["publishAt"] = json!(localtime::rfc3339_utc(t.to_unix()));
    }
    Ok(json!({ "snippet": snippet, "status": status }))
}

// ── filling the form in from the project ─────────────────────────────────────

/// The lyric lines on the timeline, in order (from text tracks named Lyrics or caption tracks).
pub fn lyric_lines(project: &Project) -> Vec<String> {
    let mut lines: Vec<(f64, String)> = Vec::new();
    for t in &project.tracks {
        let lyric_track = t.captions || t.name.to_ascii_lowercase().contains("lyric");
        if !lyric_track {
            continue;
        }
        for c in &t.clips {
            if let Some(s) = c.text() {
                let line = s.content.split_whitespace().collect::<Vec<_>>().join(" ");
                if !line.is_empty() {
                    lines.push((c.start, line));
                }
            }
        }
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines.dedup_by(|a, b| a.1 == b.1);
    lines.into_iter().map(|(_, l)| l).collect()
}

/// "1:05" / "1:02:03".
pub fn timestamp(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Chapters from the song sections, following YouTube's rules: the first at 0:00, at least
/// three, each at least 10 s long (shorter ones are merged into the one before).
pub fn chapters(project: &Project) -> Vec<(f64, String)> {
    let marks = crate::music::timeline_sections(project);
    let names = crate::music::section_names(&marks);
    let mut out: Vec<(f64, f64, String)> = Vec::new();
    for (m, name) in marks.iter().zip(names) {
        match out.last_mut() {
            Some(last) if m.start - last.0 < 10.0 => last.1 = m.end,
            _ => out.push((m.start, m.end, name)),
        }
    }
    if let Some(first) = out.first_mut() {
        first.0 = 0.0;
    }
    // A short last chapter joins the one before it.
    while out.len() >= 2 && out.last().is_some_and(|l| l.1 - l.0 < 10.0) {
        let l = out.pop().unwrap();
        out.last_mut().unwrap().1 = l.1;
    }
    if out.len() < 3 {
        return Vec::new();
    }
    out.into_iter().map(|(s, _, n)| (s, n)).collect()
}

/// True when the project uses footage or stills RustCut's AI tools made.
pub fn uses_ai_media(project: &Project) -> bool {
    project.media.iter().any(|m| {
        let p = m.path.to_string_lossy().to_ascii_lowercase().replace('\\', "/");
        p.contains("/rustcut ai/") || p.contains("/music-video-") || p.contains("/reshoots/") || p.contains("/new shots/")
    })
}

/// A title without RustCut's own words ("… music video" stays; file-ish suffixes go).
fn clean_title(name: &str) -> String {
    let t = name.trim();
    let t = t.strip_suffix(".rcproj").unwrap_or(t);
    let t = t.replace(['_'], " ");
    t.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Free first draft of the details from the project: title from its name, a description with
/// chapters and lyrics, tags from the title, Music when it has a song, the AI disclosure
/// when it uses AI footage, and its thumbnail. The user's choices (kids, visibility) are kept.
pub fn suggest(project: &Project, keep: &Details) -> Details {
    let title: String = clean_title(&project.name).chars().take(TITLE_MAX).collect();
    let mut description = String::new();
    description.push_str(&title);
    let ch = chapters(project);
    if !ch.is_empty() {
        description.push_str("\n\n");
        for (s, n) in &ch {
            description.push_str(&format!("{} {}\n", timestamp(*s), n));
        }
    }
    let lyrics = lyric_lines(project);
    if !lyrics.is_empty() {
        description.push_str(if ch.is_empty() { "\n\nLyrics:\n" } else { "\nLyrics:\n" });
        for l in &lyrics {
            if description.len() + l.len() + 1 > DESCRIPTION_MAX_BYTES - 40 {
                description.push_str("…\n");
                break;
            }
            description.push_str(l);
            description.push('\n');
        }
    }
    let lower = title.to_ascii_lowercase();
    let mut tags: Vec<String> = Vec::new();
    for part in title.split([' ', '-', '–', '|', '(', ')']).map(str::trim) {
        if part.chars().count() > 2 && !["the", "and", "video", "music", "lyric", "official"].contains(&part.to_ascii_lowercase().as_str()) {
            tags.push(part.to_string());
        }
    }
    let has_song = !project.music.is_empty();
    if lower.contains("lyric") {
        tags.push("lyric video".into());
        tags.push("lyrics".into());
    } else if has_song {
        tags.push("music video".into());
    }
    if has_song {
        tags.push("new music".into());
    }
    let mut tag_str = tag_list(&tags.join(", ")).join(", ");
    while tags_length(&tag_list(&tag_str)) > TAGS_MAX {
        let mut l = tag_list(&tag_str);
        l.pop();
        tag_str = l.join(", ");
    }
    Details {
        title,
        description: description.trim_end().to_string(),
        tags: tag_str,
        category: if has_song { "10".into() } else { keep.category.clone() },
        synthetic: uses_ai_media(project) || keep.synthetic,
        thumbnail: project.thumbnail.clone().filter(|t| t.is_file() && validate_thumb(t)).or_else(|| keep.thumbnail.clone()),
        ..keep.clone()
    }
}

fn validate_thumb(t: &Path) -> bool {
    let ok_ext = matches!(t.extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase).as_deref(), Some("jpg" | "jpeg" | "png"));
    ok_ext && std::fs::metadata(t).map(|m| m.len() <= THUMBNAIL_MAX_BYTES).unwrap_or(false)
}

/// Drafts the title, description and tags with the editor model (≈ 1¢, recorded in the cost
/// report), starting from [`suggest`]'s draft so chapters and lyrics stay accurate.
pub fn write_with_ai(project: &Project, draft: &Details) -> Result<Details> {
    let settings = crate::ai::openrouter::AiSettings::load();
    let client = crate::ai::openrouter::Client::from_settings(&settings)?;
    let schema = json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "description": { "type": "string" },
            "tags": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["title", "description", "tags"],
        "additionalProperties": false
    });
    let facts = json!({
        "project_name": project.name,
        "duration_seconds": project.duration().round(),
        "draft_title": draft.title,
        "draft_description": draft.description,
        "draft_tags": draft.tags,
        "lyrics": lyric_lines(project),
        "uses_ai_visuals": uses_ai_media(project),
    });
    let messages = json!([
        { "role": "system", "content": "You write YouTube upload details for a video creator. Return JSON with title, description and tags.\n\
- Title: under 70 characters, compelling and accurate, the artist and song name first when known (e.g. \"Artist - Song (Official Music Video)\"). No clickbait, no emoji spam, never use < or >.\n\
- Description: 2–3 short opening lines that hook the viewer and say what the video is, then keep the draft's chapter list (timestamps exactly as given, first 0:00) and lyrics unchanged if present. Add 3–5 relevant hashtags on the last line. Under 4500 characters. Never use < or >. Don't invent facts (release dates, labels, links, credits) that aren't in the input.\n\
- Tags: 10–20 search phrases people would type, most specific first, together under 450 characters.\n\
If uses_ai_visuals is true, you may say the visuals were created with AI." },
        { "role": "user", "content": facts.to_string() }
    ]);
    let (v, _usage) = client.chat_json(&settings.edit_model, messages, "youtube_details", schema)?;
    let title: String = v["title"].as_str().unwrap_or(&draft.title).replace(['<', '>'], "").chars().take(TITLE_MAX).collect();
    let mut description = v["description"].as_str().unwrap_or(&draft.description).replace(['<', '>'], "");
    while description.len() > DESCRIPTION_MAX_BYTES {
        description.pop();
    }
    let tags: Vec<String> = v["tags"].as_array().map(|a| a.iter().filter_map(|t| t.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let mut tags = tag_list(&tags.join(", "));
    while tags_length(&tags) > TAGS_MAX {
        tags.pop();
    }
    Ok(Details { title, description, tags: tags.join(", "), ..draft.clone() })
}

// ── uploading ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Published {
    pub id: String,
    pub title: String,
    /// What YouTube says about the upload ("uploaded", "processed", …).
    pub status: String,
    /// Shown after the upload, e.g. a thumbnail YouTube refused.
    pub warnings: Vec<String>,
}

impl Published {
    pub fn watch_url(&self) -> String {
        format!("https://youtu.be/{}", self.id)
    }
    pub fn studio_url(&self) -> String {
        format!("https://studio.youtube.com/video/{}/edit", self.id)
    }
}

/// True when the saved account may upload to YouTube.
pub fn ready() -> bool {
    google::load().can(SCOPE_YOUTUBE)
}

/// Uploads `video` with `details`, then sets the thumbnail. `Ok(None)` when cancelled.
pub fn upload(video: &Path, details: &Details, cancel: &AtomicBool, report: &mut dyn FnMut(f32, &str)) -> Result<Option<Published>> {
    let problems = validate(details, Some(video), localtime::now_unix());
    if !problems.is_empty() {
        bail!("{}", problems.join(" "));
    }
    let account = google::load();
    if !account.can(SCOPE_YOUTUBE) {
        bail!("connect YouTube first (File > ▶ Upload to YouTube…)");
    }
    let size = std::fs::metadata(video).with_context(|| format!("reading {}", video.display()))?.len();
    report(0.0, "Connecting to YouTube…");
    let token = google::access_token(&account, SCOPE_YOUTUBE)?;
    let body = resource(details)?;
    let url = format!(
        "{UPLOAD_API}/videos?uploadType=resumable&part=snippet,status&notifySubscribers={}",
        details.notify_subscribers && details.visibility == Visibility::Public
    );
    let mut progress = |n: u64| report(if size > 0 { 0.97 * n as f32 / size as f32 } else { 0.97 }, &format!("Uploading {} of {}", google::size_label(n), google::size_label(size)));
    let mut up = Resumable::start(&account, SCOPE_YOUTUBE, token, &url, &body, crate::gdrive::mime_for(video), Some(size), cancel, &mut progress)?;
    let mut f = std::fs::File::open(video)?;
    if let Err(e) = std::io::copy(&mut f, &mut up) {
        up.abort();
        return if cancel.load(Ordering::Relaxed) { Ok(None) } else { Err(e.into()) };
    }
    let token = up.token().to_string();
    let v = match up.finish() {
        Ok(v) => v,
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(None),
        Err(e) => return Err(e),
    };
    let id = v["id"].as_str().ok_or_else(|| anyhow!("YouTube didn't return the video's id"))?.to_string();
    let mut warnings = Vec::new();
    if let Some(thumb) = &details.thumbnail {
        report(0.98, "Setting the thumbnail…");
        if let Err(e) = set_thumbnail(&token, &id, thumb) {
            warnings.push(format!("The video is up, but YouTube didn't take the thumbnail: {e:#}. Custom thumbnails need a verified channel (youtube.com/verify); you can add it in YouTube Studio."));
        }
    }
    let wanted_public = matches!(details.visibility, Visibility::Public | Visibility::Unlisted | Visibility::Scheduled);
    let got = v["status"]["privacyStatus"].as_str().unwrap_or("");
    if wanted_public && got == "private" && details.visibility != Visibility::Scheduled {
        warnings.push("YouTube kept the video private. Videos uploaded through a Google Cloud project that hasn't passed YouTube's API audit stay private; change it in YouTube Studio if it lets you, or apply for the audit.".into());
    }
    report(1.0, "Done");
    Ok(Some(Published {
        id,
        title: v["snippet"]["title"].as_str().unwrap_or(&details.title).to_string(),
        status: v["status"]["uploadStatus"].as_str().unwrap_or("uploaded").to_string(),
        warnings,
    }))
}

fn set_thumbnail(token: &str, video_id: &str, path: &Path) -> Result<()> {
    let bytes = std::fs::read(path)?;
    google::agent()
        .post(&format!("{UPLOAD_API}/thumbnails/set?videoId={}&uploadType=media", enc(video_id)))
        .set("Authorization", &format!("Bearer {token}"))
        .set("Content-Type", crate::gdrive::mime_for(path))
        .send_bytes(&bytes)
        .map_err(google::api_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};

    fn ok_details() -> Details {
        Details { title: "Falling Deeper (Lyric Video)".into(), made_for_kids: Some(false), ..Default::default() }
    }

    #[test]
    fn validation() {
        let dir = std::env::temp_dir().join(format!("rc-yt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let video = dir.join("v.mp4");
        std::fs::write(&video, b"x").unwrap();
        let now = 1_795_000_000;
        assert!(validate(&ok_details(), Some(&video), now).is_empty());
        assert_eq!(validate(&ok_details(), None, now).len(), 1);

        let mut d = ok_details();
        d.made_for_kids = None;
        assert!(validate(&d, Some(&video), now)[0].contains("made for kids"));
        d = ok_details();
        d.title = "x".repeat(101);
        assert!(validate(&d, Some(&video), now)[0].contains("101"));
        d = ok_details();
        d.description = "a <b> c".into();
        assert!(validate(&d, Some(&video), now)[0].contains("< or >"));
        d = ok_details();
        d.tags = (0..60).map(|i| format!("tag number {i}")).collect::<Vec<_>>().join(",");
        assert!(validate(&d, Some(&video), now)[0].contains("tags"));
        d = ok_details();
        d.visibility = Visibility::Scheduled;
        d.publish_at = "2020-01-01 10:00".into();
        assert!(validate(&d, Some(&video), now)[0].contains("15 minutes"));
        d.publish_at = "next week".into();
        assert!(validate(&d, Some(&video), now)[0].starts_with("Publish time"));
        d.publish_at = localtime::LocalTime::from_unix(now + 86400).to_input();
        assert!(validate(&d, Some(&video), now).is_empty());
        d.thumbnail = Some(dir.join("thumb.gif"));
        assert_eq!(validate(&d, Some(&video), now).len(), 2, "missing and not JPG/PNG");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tags_count_like_youtube() {
        let t = tag_list(" #lyrics, Falling Deeper ,lyrics, r&b ,, ");
        assert_eq!(t, vec!["lyrics", "Falling Deeper", "r&b"]);
        // 6 + (14 + 2 quotes) + 3 + 2 commas
        assert_eq!(tags_length(&t), 27);
    }

    #[test]
    fn resource_for_scheduled_upload() {
        let mut d = ok_details();
        d.tags = "a, b".into();
        d.category = "10".into();
        d.language = "en".into();
        d.synthetic = true;
        d.visibility = Visibility::Scheduled;
        d.publish_at = "2030-06-01 18:00".into();
        let r = resource(&d).unwrap();
        assert_eq!(r["snippet"]["tags"], json!(["a", "b"]));
        assert_eq!(r["snippet"]["defaultAudioLanguage"], "en");
        assert_eq!(r["status"]["privacyStatus"], "private");
        assert_eq!(r["status"]["containsSyntheticMedia"], true);
        assert_eq!(r["status"]["selfDeclaredMadeForKids"], false);
        let at = r["status"]["publishAt"].as_str().unwrap();
        assert_eq!(localtime::parse_rfc3339(at), Some(localtime::LocalTime::parse("2030-06-01 18:00").unwrap().to_unix()));
        d.visibility = Visibility::Unlisted;
        let r = resource(&d).unwrap();
        assert_eq!(r["status"]["privacyStatus"], "unlisted");
        assert!(r["status"].get("publishAt").is_none());
    }

    #[test]
    fn suggestion_from_project() {
        let mut p = Project::default();
        p.name = "Falling Deeper - lyric video".into();
        p.media.push(MediaItem {
            id: 1,
            path: PathBuf::from("C:/Videos/RustCut AI/Falling Deeper music video/music-video-x/shot-01.png"),
            name: "shot-01.png".into(),
            kind: MediaKind::Image,
            duration: 0.0,
            width: 1,
            height: 1,
            fps: 0.0,
            has_audio: false,
        });
        let keep = Details { made_for_kids: Some(false), visibility: Visibility::Unlisted, ..Default::default() };
        let d = suggest(&p, &keep);
        assert_eq!(d.title, "Falling Deeper - lyric video");
        assert!(d.synthetic, "AI stills > disclosure");
        assert_eq!(d.made_for_kids, Some(false));
        assert_eq!(d.visibility, Visibility::Unlisted);
        let tags = tag_list(&d.tags);
        assert!(tags.contains(&"Falling".to_string()) && tags.contains(&"lyric video".to_string()), "{tags:?}");
        assert_eq!(timestamp(65.0), "1:05");
        assert_eq!(timestamp(3723.0), "1:02:03");
    }

    /// Prints the free draft for a real project: `YT_PROJECT=<.rcproj> cargo test --lib youtube_suggest_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn youtube_suggest_live() {
        let p = Project::load(Path::new(&std::env::var("YT_PROJECT").expect("YT_PROJECT=<.rcproj>"))).unwrap();
        let d = suggest(&p, &Details::default());
        eprintln!("TITLE: {}
CATEGORY: {}  SYNTHETIC: {}  THUMB: {:?}
TAGS: {}
--- DESCRIPTION ---
{}", d.title, d.category, d.synthetic, d.thumbnail, d.tags, d.description);
    }
}
