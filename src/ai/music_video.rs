//! AI music video from the song on the timeline: shots are timed to bars (faster in
//! loud sections), a text model writes one shot per unique slot from the theme, the
//! song's sections and its lyrics, the storyboard flow draws and animates approved
//! shots, and assembly cuts them exactly on the bar lines. Repeated sections (the
//! second chorus…) reuse earlier shots, as music videos do, which also keeps cost down.
//! Also: find the best 30 seconds for a vertical clip.

use super::openrouter::Client;
use super::storyboard::{AssembleReport, Shot, Slot, Storyboard};
use super::video_gen::Generated;
use crate::model::{Id, Project, TrackKind};
use crate::music::{self, MusicAnalysis};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

/// Longest slot we aim for (most image-to-video models do up to 5–8 s).
pub const MAX_SLOT: f64 = 6.0;

#[derive(Clone, Debug, PartialEq)]
pub struct SlotDraft {
    pub start: f64,
    pub end: f64,
    pub section: String,
    /// Index of the section instance (0 = first intro, 1 = first verse, …).
    pub section_index: usize,
    pub energy: f32,
    pub lyric: String,
}

/// Slots over `[from, to]` (timeline) cut on downbeats: 2 bars per shot in loud
/// sections, 4 elsewhere, never longer than [`MAX_SLOT`] when a bar allows it.
pub fn plan_slots(project: &Project, from: f64, to: f64) -> Vec<SlotDraft> {
    let beats = music::timeline_beats(project);
    let sections = music::timeline_sections(project);
    let lyrics = crate::lyrics::timeline_lines(project);
    let lyric_at = |a: f64, b: f64| -> String {
        lyrics.iter().filter(|l| l.end > a && l.start < b).map(|l| l.text.as_str()).collect::<Vec<_>>().join(" / ")
    };
    let downbeats: Vec<f64> = beats.iter().filter(|b| b.downbeat && b.time >= from - 1e-6 && b.time < to - 0.5).map(|b| b.time).collect();
    let mut out = Vec::new();
    if downbeats.len() < 2 {
        // No beat grid: plain 4-second slots.
        let mut t = from;
        while t < to - 1.0 {
            let e = (t + 4.0).min(to);
            out.push(SlotDraft { start: t, end: e, section: "verse".into(), section_index: 0, energy: 0.6, lyric: lyric_at(t, e) });
            t = e;
        }
        return out;
    }
    let bar = (downbeats[1] - downbeats[0]).max(0.2);
    let section_of = |t: f64| sections.iter().enumerate().find(|(_, s)| t >= s.start - 1e-6 && t < s.end - 1e-6);
    // Lead-in before the first downbeat joins the first slot.
    let mut k = 0;
    let mut start = from;
    while k < downbeats.len() {
        let (si, label, energy) = section_of(downbeats[k]).map_or((0, "verse".to_string(), 0.6), |(i, s)| (i, s.label.clone(), s.energy));
        let mut bars = if energy >= 0.8 { 2 } else { 4 };
        while bars > 1 && bars as f64 * bar > MAX_SLOT {
            bars /= 2;
        }
        // Don't run past the section's end.
        let section_end = sections.get(si).map_or(to, |s| s.end).min(to);
        let mut j = k + bars;
        let mut end = downbeats.get(j).copied().unwrap_or(to).min(section_end.max(downbeats[k] + bar * 0.5));
        while j > k + 1 && end > section_end + 1e-6 {
            j -= 1;
            end = downbeats[j];
        }
        if downbeats.get(j).is_none() {
            end = to;
        }
        // A short tail (< 1 bar) at the end of the range joins this slot.
        if to - end < bar * 0.9 {
            end = to;
        }
        if end - start >= 0.5 {
            out.push(SlotDraft { start, end, section: label, section_index: si, energy, lyric: lyric_at(start, end) });
        }
        start = end;
        if end >= to - 1e-6 {
            break;
        }
        k = downbeats.iter().position(|d| *d >= end - 1e-6).unwrap_or(downbeats.len());
    }
    out
}

/// Gives each slot a shot. Each kind of section (intro, verse, chorus…) gets its own
/// shots, in proportion to its first appearance's length, within `max_shots`; cuts
/// cycle through them, and later appearances (the second chorus…) reuse them.
pub fn assign_shots(drafts: &[SlotDraft], max_shots: usize) -> (Vec<usize>, usize) {
    let max_shots = max_shots.max(1);
    // Labels in order of first appearance, and how many cuts they have across the whole song
    // (a short first chorus must not leave the long later choruses with one or two shots).
    let mut labels: Vec<(String, usize, usize)> = Vec::new(); // (label, first section index, cuts)
    for d in drafts {
        match labels.iter_mut().find(|l| l.0 == d.section) {
            Some(l) => l.2 += 1,
            None => labels.push((d.section.clone(), d.section_index, 1)),
        }
    }
    let need: usize = labels.iter().map(|l| l.2).sum();
    let mut alloc: Vec<usize> = if need <= max_shots {
        labels.iter().map(|l| l.2).collect()
    } else {
        labels.iter().map(|l| ((max_shots as f64 * l.2 as f64 / need as f64).round() as usize).clamp(1, l.2)).collect()
    };
    while alloc.iter().sum::<usize>() > max_shots {
        let Some((i, _)) = alloc.iter().enumerate().filter(|(_, a)| **a > 1).max_by_key(|(_, a)| **a) else { break };
        alloc[i] -= 1;
    }
    while alloc.iter().sum::<usize>() < max_shots.min(need) {
        // Spare budget (from rounding) goes to the label with the most cuts per shot.
        let Some((i, _)) = labels.iter().enumerate().filter(|(i, l)| alloc[*i] < l.2).max_by(|a, b| {
            (a.1 .2 as f64 / alloc[a.0] as f64).total_cmp(&(b.1 .2 as f64 / alloc[b.0] as f64))
        }) else { break };
        alloc[i] += 1;
    }
    let mut first_shot = Vec::with_capacity(alloc.len());
    let mut next = 0;
    for a in &alloc {
        first_shot.push(next);
        next += a;
    }
    let mut cursor = vec![0usize; labels.len()];
    let out = drafts
        .iter()
        .map(|d| {
            let li = labels.iter().position(|l| l.0 == d.section).unwrap_or(0);
            let shot = first_shot[li] + cursor[li] % alloc[li].max(1);
            cursor[li] += 1;
            shot
        })
        .collect();
    (out, next.max(1))
}

fn schema(n: usize) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["title", "style", "concept", "shots"],
        "properties": {
            "title": { "type": "string" },
            "style": { "type": "string", "description": "One sentence visual style shared by every shot: palette, lighting, lens, film look" },
            "concept": { "type": "string", "description": "Two or three sentences: the video's idea and arc" },
            "shots": {
                "type": "array",
                "minItems": n,
                "maxItems": n,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["description", "motion"],
                    "properties": {
                        "description": { "type": "string", "description": "A single still frame: subject, setting, composition, light. No text in the image." },
                        "motion": { "type": "string", "description": "Camera and subject motion, matched to the energy of the music" }
                    }
                }
            }
        }
    })
}

/// Everything needed before calling the model; also used for previews and tests.
pub struct Outline {
    pub song_clip: Id,
    pub drafts: Vec<SlotDraft>,
    pub shot_of: Vec<usize>,
    pub shots: usize,
    pub bpm: f64,
    pub song_name: String,
}

pub fn outline(project: &Project, song_clip: Id, range: (f64, f64), max_shots: usize) -> Result<Outline> {
    let clip = project.clip(song_clip).ok_or_else(|| anyhow!("the song clip was removed"))?;
    let (from, to) = (range.0.max(clip.start), range.1.min(clip.end()));
    if to - from < 2.0 {
        bail!("the chosen range is too short");
    }
    let drafts = plan_slots(project, from, to);
    if drafts.is_empty() {
        bail!("could not split the song into shots");
    }
    let (shot_of, shots) = assign_shots(&drafts, max_shots);
    // Back-to-back cuts of the same shot become one longer cut (up to 8 s).
    let mut merged: Vec<(SlotDraft, usize)> = Vec::new();
    for (d, s) in drafts.into_iter().zip(shot_of) {
        if let Some((last, ls)) = merged.last_mut() {
            if *ls == s && d.end - last.start <= 8.0 + 1e-6 {
                last.end = d.end;
                if last.lyric.is_empty() {
                    last.lyric = d.lyric;
                } else if !d.lyric.is_empty() && !last.lyric.contains(&d.lyric) {
                    last.lyric = format!("{} / {}", last.lyric, d.lyric);
                }
                continue;
            }
        }
        merged.push((d, s));
    }
    let (drafts, shot_of): (Vec<SlotDraft>, Vec<usize>) = merged.into_iter().unzip();
    let media = clip.media_id().and_then(|m| project.media(m));
    let bpm = clip.media_id().and_then(|m| project.music.get(&m)).map_or(0.0, |a| a.bpm);
    Ok(Outline { song_clip, drafts, shot_of, shots, bpm, song_name: media.map(|m| m.name.clone()).unwrap_or_default() })
}

pub fn plan_messages(theme: &str, o: &Outline, aspect: crate::model::Aspect) -> Value {
    plan_messages_cast(theme, o, aspect, &[])
}

/// With a cast, the model must name each person in every shot they appear in (that's how
/// frames get the right reference photos) and must not describe their faces.
pub fn plan_messages_cast(theme: &str, o: &Outline, aspect: crate::model::Aspect, cast: &[String]) -> Value {
    let mut lines = String::new();
    for i in 0..o.shots {
        let slots: Vec<&SlotDraft> = o.drafts.iter().zip(&o.shot_of).filter(|(_, s)| **s == i).map(|(d, _)| d).collect();
        let Some(first) = slots.first() else { continue };
        let lyric = slots.iter().map(|d| d.lyric.as_str()).find(|l| !l.is_empty()).unwrap_or("");
        lines += &format!(
            "Shot {}: {} section, {:.1}s, energy {:.0}%, used {}×{}\n",
            i + 1,
            first.section,
            first.end - first.start,
            first.energy * 100.0,
            slots.len(),
            if lyric.is_empty() { String::new() } else { format!(", lyrics: \"{lyric}\"") }
        );
    }
    json!([
        { "role": "system", "content": "You are a music video director. Design a visually striking, coherent music video: a clear concept, \
consistent characters/wardrobe/locations, and shots that match each section's energy (calm, wide, slow moves in quiet parts; \
dynamic, close, fast moves in loud ones). Each shot is ONE continuous moment an image-to-video model can animate; no text in frames. \
Echo the lyrics visually where they're given, without being literal." },
        { "role": "user", "content": format!(
            "Song: {} ({:.0} BPM). Canvas: {}.\nTheme / idea: {theme}\n{}\nWrite exactly {} shots, in order:\n{lines}",
            o.song_name, o.bpm, aspect.short_label(),
            if cast.is_empty() { String::new() } else { format!(
                "Recurring characters: {}. Name them explicitly (by first name) in every shot description where they appear; never describe their faces, \
                 hair or skin (reference photos are supplied); describe wardrobe, action and setting instead. Other people are unnamed extras.\n",
                cast.join(", ")) },
            o.shots
        ) }
    ])
}

/// Plans the music video: outline → model → storyboard (frames not drawn yet).
pub fn plan(client: &Client, model: &str, theme: &str, project: &Project, song_clip: Id, range: (f64, f64), max_shots: usize, out_root: &Path) -> Result<Storyboard> {
    plan_cast(client, model, theme, project, song_clip, range, max_shots, out_root, &[])
}

/// [`plan`] with a cast: named people with photos, kept on the storyboard.
#[allow(clippy::too_many_arguments)]
pub fn plan_cast(
    client: &Client,
    model: &str,
    theme: &str,
    project: &Project,
    song_clip: Id,
    range: (f64, f64),
    max_shots: usize,
    out_root: &Path,
    cast: &[super::storyboard::CastMember],
) -> Result<Storyboard> {
    let mut project = project.clone();
    // Make sure the song has a beat grid (analysed here if needed).
    let media = project.clip(song_clip).and_then(|c| c.media_id()).ok_or_else(|| anyhow!("the song clip has no media"))?;
    let mut fresh: Option<MusicAnalysis> = None;
    if !project.music.contains_key(&media) {
        let path = project.media(media).map(|m| m.path.clone()).ok_or_else(|| anyhow!("no song media"))?;
        let a = music::analyze(&path)?;
        project.music.insert(media, a.clone());
        fresh = Some(a);
    }
    let o = outline(&project, song_clip, range, max_shots)?;
    let aspect = project.aspect;
    let names: Vec<String> = cast.iter().map(|c| c.name.clone()).collect();
    let (v, usage) = client.chat_json(model, plan_messages_cast(theme, &o, aspect, &names), "music_video", schema(o.shots))?;
    let mut shots: Vec<Shot> = v["shots"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|x| Shot {
            description: x["description"].as_str().unwrap_or("").trim().to_string(),
            motion: x["motion"].as_str().unwrap_or("").trim().to_string(),
            caption: String::new(),
            seconds: 0.0,
            frame: None,
            approved: false,
            video: None,
            no_lead: false,
        })
        .filter(|s| !s.description.is_empty())
        .collect();
    if shots.is_empty() {
        bail!("the model returned no shots; try rewording the idea");
    }
    while shots.len() < o.shots {
        let s = shots[shots.len() % shots.len().max(1)].clone();
        shots.push(s);
    }
    shots.truncate(o.shots);
    let slots: Vec<Slot> = o
        .drafts
        .iter()
        .zip(&o.shot_of)
        .map(|(d, &shot)| Slot { start: d.start, end: d.end, shot, section: d.section.clone(), lyric: d.lyric.clone() })
        .collect();
    for (i, shot) in shots.iter_mut().enumerate() {
        // Long enough for every slot it fills; the first lyric it carries as a caption hint.
        shot.seconds = slots.iter().filter(|s| s.shot == i).map(|s| s.end - s.start).fold(0.0, f64::max).max(1.0);
        shot.caption = slots.iter().filter(|s| s.shot == i).map(|s| s.lyric.clone()).find(|l| !l.is_empty()).unwrap_or_default();
    }
    let s = |k: &str| v[k].as_str().unwrap_or("").trim().to_string();
    let title = if s("title").is_empty() { format!("{} music video", o.song_name) } else { s("title") };
    let mut dir = out_root.join(format!("music-video-{}", super::video_gen::slug(&title)));
    let mut n = 2;
    while dir.exists() {
        dir = out_root.join(format!("music-video-{}-{n}", super::video_gen::slug(&title)));
        n += 1;
    }
    let sb = Storyboard {
        brief: theme.trim().to_string(),
        aspect,
        title,
        style: s("style"),
        script: s("concept"),
        music: String::new(),
        shots,
        dir,
        spent: usage.cost,
        slots,
        song_clip: Some(song_clip),
        fresh_analysis: fresh.map(|a| (media, a)),
        character: None,
        cast: cast.to_vec(),
    };
    sb.save()?;
    Ok(sb)
}

/// Cuts the rendered shots onto the timeline at their slot times (hard cuts on the
/// bars). Slots whose shot failed borrow another shot from the same section.
pub fn assemble(project: &mut Project, sb: &Storyboard, rendered: &[(usize, Generated)]) -> AssembleReport {
    let mut report = AssembleReport::default();
    if rendered.is_empty() || sb.slots.is_empty() {
        return report;
    }
    project.aspect = sb.aspect;
    let mut media_of: HashMap<usize, (Id, f64)> = HashMap::new();
    for (i, g) in rendered {
        let m = project.add_media(g.info.clone().into_media_item(g.path.clone()));
        media_of.insert(*i, (m, g.info.duration));
    }
    let (from, to) = (sb.slots[0].start, sb.slots.last().unwrap().end);
    // Re-assembling replaces this storyboard's earlier clips (media from its folder) instead of stacking a copy.
    let ours = |project: &Project, c: &crate::model::Clip| c.media_id().and_then(|m| project.media(m)).is_some_and(|m| !sb.dir.as_os_str().is_empty() && m.path.starts_with(&sb.dir));
    for ti in 0..project.tracks.len() {
        let keep: Vec<crate::model::Clip> = project.tracks[ti].clips.iter().filter(|c| !(ours(project, c) && c.start >= from - 1e-6 && c.end() <= to + 1e-6)).cloned().collect();
        project.tracks[ti].clips = keep;
    }
    project.cleanup_tracks();
    let main = project.main_track();
    let ti = if project.fits(main, from, to, None) { main } else { project.insert_track(TrackKind::Video, Some(main)) };
    let mut uses: HashMap<usize, usize> = HashMap::new();
    for slot in &sb.slots {
        let shot = if media_of.contains_key(&slot.shot) {
            slot.shot
        } else {
            // Borrow: same section first, then any rendered shot.
            let same = sb.slots.iter().filter(|s| s.section == slot.section).map(|s| s.shot).find(|s| media_of.contains_key(s));
            match same.or_else(|| media_of.keys().min().copied()) {
                Some(s) => s,
                None => continue,
            }
        };
        let (m, gen_len) = media_of[&shot];
        let len = slot.end - slot.start;
        let Some(mut clip) = project.new_media_clip(m, slot.start) else { continue };
        clip.start = slot.start;
        clip.duration = len;
        clip.volume = 0.0;
        if gen_len + 1e-6 < len {
            clip.speed = (gen_len / len).max(0.5);
            clip.duration = len.min(gen_len / clip.speed);
        } else {
            // Reused shots start at a different point each time, for variety.
            let n = uses.entry(shot).or_insert(0);
            let spare = gen_len - len;
            clip.source_in = if *n % 2 == 1 { spare } else { 0.0 };
            *n += 1;
        }
        report.clips.push(clip.id);
        project.tracks[ti].clips.push(clip);
    }
    project.sort_track(ti);
    report.duration = to - from;
    project.audio.loudness = Some(super::storyboard::LOUDNESS);
    report
}

// ── best 30 seconds ──────────────────────────────────────────────────────────

/// The most energetic window of `seconds` that starts on a downbeat (a chorus start
/// gets a bonus). Timeline times.
pub fn best_window(project: &Project, seconds: f64) -> Option<(f64, f64)> {
    let beats = music::timeline_beats(project);
    let clip = music::beat_clip(project)?;
    let (s0, e0) = (clip.start, clip.end());
    if e0 - s0 <= seconds + 0.5 {
        return Some((s0, e0));
    }
    let sections = music::timeline_sections(project);
    let starts: Vec<f64> = beats.iter().filter(|b| b.downbeat && b.time + seconds <= e0 + 1e-6).map(|b| b.time).collect();
    let mut best: Option<(f64, f32)> = None;
    for &s in &starts {
        let inside: Vec<f32> = beats.iter().filter(|b| b.time >= s && b.time < s + seconds).map(|b| b.energy).collect();
        if inside.is_empty() {
            continue;
        }
        let mut score = inside.iter().sum::<f32>() / inside.len() as f32;
        if sections.iter().any(|x| x.label == "chorus" && (x.start - s).abs() < 0.3) {
            score += 0.15;
        }
        if best.is_none_or(|b| score > b.1) {
            best = Some((s, score));
        }
    }
    let s = best.map_or(s0, |b| b.0);
    // End on a downbeat near the target length when there is one.
    let target = s + seconds;
    let e = beats.iter().filter(|b| b.downbeat && (b.time - target).abs() <= 1.5).map(|b| b.time).min_by(|a, b| (a - target).abs().total_cmp(&(b - target).abs())).unwrap_or(target).min(e0);
    Some((s, e))
}

/// Ripple-trims the project to `[a, b]` and fades the song in and out. Returns the new length.
pub fn apply_window(project: &mut Project, a: f64, b: f64) -> f64 {
    project.keep_ranges(&[(a, b)]);
    let len = project.duration();
    for c in project.tracks.iter_mut().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| t.clips.iter_mut()) {
        c.fade_in = c.fade_in.max(0.3_f64.min(c.duration / 4.0));
        c.fade_out = c.fade_out.max(1.0_f64.min(c.duration / 4.0));
    }
    len
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ffmpeg::ProbeInfo;
    use crate::model::{MediaItem, MediaKind};
    use crate::music::Section;

    /// 120 BPM song (bar = 2 s) placed at 0: intro 0–16 (quiet), verse 16–32, chorus
    /// 32–48 (loud), verse 48–64, chorus 64–80.
    fn song_project() -> (Project, Id) {
        let mut p = Project::default();
        let song = p.add_media(MediaItem {
            id: 0, path: "song.mp3".into(), name: "Song".into(), kind: MediaKind::Audio,
            duration: 80.0, width: 0, height: 0, fps: 0.0, has_audio: true,
        });
        let beats: Vec<f64> = (0..160).map(|i| i as f64 * 0.5).collect();
        let energy: Vec<f32> = beats.iter().map(|&t| if (32.0..48.0).contains(&t) || t >= 64.0 { 1.0 } else if t < 16.0 { 0.3 } else { 0.6 }).collect();
        let sec = |a: f64, b: f64, l: &str, e: f32| Section { start: a, end: b, label: l.into(), energy: e };
        p.music.insert(song, MusicAnalysis {
            bpm: 120.0,
            beats,
            first_downbeat: 0,
            beats_per_bar: 4,
            sections: vec![sec(0.0, 16.0, "intro", 0.3), sec(16.0, 32.0, "verse", 0.6), sec(32.0, 48.0, "chorus", 1.0), sec(48.0, 64.0, "verse", 0.6), sec(64.0, 80.0, "chorus", 1.0)],
            beat_energy: energy,
        });
        let c = p.new_media_clip(song, 0.0).unwrap();
        let id = c.id;
        p.place_clip(c, None);
        (p, id)
    }


    /// Manual: `RUSTCUT_SONG=path cargo test --lib real_song_outline -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_song_outline() {
        let Ok(song) = std::env::var("RUSTCUT_SONG") else { return };
        let mut p = Project::default();
        let m = p.add_media(crate::media::ffmpeg::probe(Path::new(&song)).unwrap().into_media_item(song.clone().into()));
        p.music.insert(m, music::analyze(Path::new(&song)).unwrap());
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.place_clip(c, None);
        for (label, range) in [("whole", (0.0, 1e9)), ("best30", best_window(&p, 30.0).unwrap())] {
            let o = outline(&p, id, range, 10).unwrap();
            let lens: Vec<String> = o.drafts.iter().map(|d| format!("{:.1}", d.end - d.start)).collect();
            println!("{label}: range {:.1}–{:.1}, {} cuts, {} unique shots", range.0, range.1.min(999.0), o.drafts.len(), o.shots);
            println!("  shot per cut: {:?}", o.shot_of.iter().map(|s| s + 1).collect::<Vec<_>>());
            println!("  cut lengths: {}", lens.join(" "));
        }
    }


    /// PAID, manual: `RUSTCUT_LIVE=1 RUSTCUT_SONG=path RUSTCUT_THEME="…" cargo test --lib live_music_video -- --ignored --nocapture`
    /// Plans the best 30 s and draws the storyboard frames (no video generation).
    #[test]
    #[ignore]
    fn live_music_video() {
        let (Ok(_), Ok(song), Ok(theme)) = (std::env::var("RUSTCUT_LIVE"), std::env::var("RUSTCUT_SONG"), std::env::var("RUSTCUT_THEME")) else { return };
        let settings = crate::ai::openrouter::AiSettings::load();
        let client = crate::ai::openrouter::Client::from_settings(&settings).expect("OpenRouter key in RustCut settings");
        let mut p = Project::default();
        p.aspect = crate::model::Aspect::Portrait;
        let m = p.add_media(crate::media::ffmpeg::probe(Path::new(&song)).unwrap().into_media_item(song.clone().into()));
        p.music.insert(m, music::analyze(Path::new(&song)).unwrap());
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.place_clip(c, None);
        let range = best_window(&p, 30.0).unwrap();
        let root = dirs_videos().join("RustCut AI");
        let mut sb = plan(&client, &settings.edit_model, &theme, &p, id, range, 10, &root).unwrap();
        println!("planned \"{}\" ({} shots, {} cuts, {:.1}–{:.1}s) for ${:.4}", sb.title, sb.shots.len(), sb.slots.len(), range.0, range.1, sb.spent);
        println!("style: {}
concept: {}", sb.style, sb.script);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        crate::ai::storyboard::generate_missing_frames(&client, &mut sb, &settings.image_model, &cancel, &mut |_, s| println!("  {s}")).unwrap();
        for (i, s) in sb.shots.iter().enumerate() {
            println!("shot {} [{}×]: {} | motion: {} | frame: {}", i + 1, sb.uses(i), s.description, s.motion, s.frame.as_ref().map_or("—".into(), |f| f.display().to_string()));
        }
        println!("folder: {}
total spent: ${:.3}", sb.dir.display(), sb.spent);
    }

    fn dirs_videos() -> std::path::PathBuf {
        std::env::var("USERPROFILE").map(|h| Path::new(&h).join("Videos")).unwrap_or_else(|_| std::env::temp_dir())
    }


    /// Manual, staged music-video pipeline (stages plan/produce are PAID):
    /// `MV_DIR=folder MV_SONG=song MV_STAGE=prepare|plan|produce [MV_LYRICS=lyrics.txt MV_THEME=… MV_CHARACTER=photo MV_SHOTS=16 MV_VIDEO_MODEL=…]
    ///  cargo test --lib pipeline_music_video -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn pipeline_music_video() {
        use crate::ai::openrouter::{AiSettings, Client};
        let (Ok(dir), Ok(stage)) = (std::env::var("MV_DIR"), std::env::var("MV_STAGE")) else { return };
        let dir = std::path::PathBuf::from(dir);
        let project_path = dir.join(format!("{}.rcproj", std::env::var("MV_PROJECT").unwrap_or_else(|_| "Checklist".into())));
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let settings = AiSettings::load();
        match stage.as_str() {
            "prepare" => {
                let song = std::env::var("MV_SONG").expect("MV_SONG");
                let mut p = Project::default();
                p.name = std::env::var("MV_PROJECT").unwrap_or_else(|_| "Checklist".into());
                p.aspect = match std::env::var("MV_ASPECT").as_deref() {
                    Ok("9:16") => crate::model::Aspect::Portrait,
                    Ok("1:1") => crate::model::Aspect::Square,
                    _ => crate::model::Aspect::Landscape,
                };
                let m = p.add_media(crate::media::ffmpeg::probe(Path::new(&song)).unwrap().into_media_item(song.clone().into()));
                let mut c = p.new_media_clip(m, 0.0).unwrap();
                c.start = 0.0;
                let song_clip = c.id;
                p.place_clip(c, None);
                let a = music::analyze(Path::new(&song)).unwrap();
                println!("beats: {:.1} BPM, {} sections", a.bpm, a.sections.len());
                p.music.insert(m, a);
                if let Ok(lyr) = std::env::var("MV_LYRICS") {
                    let text = std::fs::read_to_string(lyr).unwrap();
                    let t = std::time::Instant::now();
                    let lines = crate::lyrics::timed_lines(&p, song_clip, &crate::lyrics::Input::Text(text), "en", &cancel, &mut |_, _| {}).unwrap().unwrap();
                    let style = std::env::var("MV_LYRIC_STYLE").ok().and_then(|s| crate::lyrics::LyricStyle::parse(&s)).unwrap_or(crate::lyrics::LyricStyle::Karaoke);
                    let n = crate::lyrics::apply_lyrics(&mut p, &lines, style, 0.36, false);
                    println!("lyrics: {} lines → {n} clips in {:?}", lines.len(), t.elapsed());
                    for l in lines.iter().take(6).chain(lines.iter().rev().take(3)) {
                        println!("  [{:6.1}-{:6.1}] {}", l.start, l.end, l.text);
                    }
                }
                p.save(&project_path).unwrap();
                println!("saved {}", project_path.display());
            }
            "plan" => {
                let client = Client::from_settings(&settings).expect("OpenRouter key");
                let p = Project::load(&project_path).unwrap();
                let song = crate::multicam::song_clip(&p).unwrap();
                let clip = p.clip(song).unwrap().clone();
                let theme = std::env::var("MV_THEME").expect("MV_THEME");
                let shots: usize = std::env::var("MV_SHOTS").ok().and_then(|s| s.parse().ok()).unwrap_or(16);
                // MV_CAST="Aidan=C:/…/sheet.png;Aishyla=C:/…/photo.png"
                let cast: Vec<crate::ai::storyboard::CastMember> = std::env::var("MV_CAST").unwrap_or_default().split(';').filter_map(|e| {
                    let (n, p) = e.split_once('=')?;
                    Some(crate::ai::storyboard::CastMember { name: n.trim().into(), photo: p.trim().into() })
                }).collect();
                let mut sb = plan_cast(&client, &settings.edit_model, &theme, &p, song, (clip.start, clip.end()), shots, &dir, &cast).unwrap();
                sb.character = std::env::var("MV_CHARACTER").ok().map(Into::into);
                sb.save().unwrap();
                println!("planned \"{}\": {} shots, {} cuts, ${:.4}\nstyle: {}\nconcept: {}", sb.title, sb.shots.len(), sb.slots.len(), sb.spent, sb.style, sb.script);
                crate::ai::storyboard::generate_missing_frames(&client, &mut sb, &settings.image_model, &cancel, &mut |_, s| println!("  {s}")).unwrap();
                for (i, s) in sb.shots.iter().enumerate() {
                    let who: Vec<&str> = sb.cast_in(i).iter().map(|c| c.name.as_str()).collect();
                    println!("shot {:2} [{}×, {:.1}s] {{{}}} {} | motion: {}", i + 1, sb.uses(i), s.seconds, who.join("+"), s.description, s.motion);
                }
                println!("storyboard: {}\ntotal spent: ${:.3}", sb.dir.join("storyboard.json").display(), sb.spent);
            }
            "sheet" => {
                // A character reference sheet from a description: MV_SHEET_NAME="Saivon" MV_SHEET_PROMPT="…"
                let client = Client::from_settings(&settings).expect("OpenRouter key");
                let name = std::env::var("MV_SHEET_NAME").expect("MV_SHEET_NAME");
                let look = std::env::var("MV_SHEET_PROMPT").expect("MV_SHEET_PROMPT");
                let prompt = format!(
                    "Photorealistic character reference sheet of one person, {name}: {look}. Same person shown four times on a plain light-grey studio background: \
                     full-body front view, full-body three-quarter view, full-body side profile, and a head-and-shoulders close-up of the face. \
                     Consistent face, hairstyle, skin tone, outfit and accessories in every view. Soft even studio lighting, sharp detail, no text, no labels, no watermark."
                );
                let req = crate::ai::image_gen::ImageRequest { model: settings.image_model.clone(), prompt, aspect_ratio: Some("16:9".into()), resolution: None, transparent: false, references: vec![] };
                let img = crate::ai::image_gen::generate(&client, &req).unwrap();
                let path = crate::ai::image_gen::save(&img, &dir, &format!("{}-character-sheet", crate::ai::video_gen::slug(&name))).unwrap();
                println!("sheet: {} (${:.3})", path.display(), img.cost.unwrap_or(0.0));
            }
            "redraw" => {
                // MV_REDRAW="3,12"  MV_NO_LEAD="3,12"  MV_DESC_3="…"  MV_MOTION_3="…"
                let client = Client::from_settings(&settings).expect("OpenRouter key");
                let sb_path = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir() && p.join("storyboard.json").is_file()).max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok()).expect("a planned storyboard").join("storyboard.json");
                let mut sb = crate::ai::storyboard::Storyboard::load(&sb_path).unwrap();
                let list = |k: &str| -> Vec<usize> { std::env::var(k).unwrap_or_default().split(',').filter_map(|x| x.trim().parse::<usize>().ok()).filter(|&n| n >= 1).map(|n| n - 1).collect() };
                for i in list("MV_NO_LEAD") {
                    sb.shots[i].no_lead = true;
                }
                let redraw = list("MV_REDRAW");
                for &i in &redraw {
                    if let Ok(d) = std::env::var(format!("MV_DESC_{}", i + 1)) {
                        sb.shots[i].description = d;
                    }
                    if let Ok(m) = std::env::var(format!("MV_MOTION_{}", i + 1)) {
                        sb.shots[i].motion = m;
                    }
                    sb.shots[i].frame = None;
                    sb.shots[i].video = None;
                }
                let before = sb.spent;
                crate::ai::storyboard::generate_missing_frames(&client, &mut sb, &settings.image_model, &cancel, &mut |_, s| println!("  {s}")).unwrap();
                sb.save().unwrap();
                for &i in &redraw {
                    println!("shot {} → {}", i + 1, sb.shots[i].frame.as_ref().map_or("FAILED".into(), |f| f.display().to_string()));
                }
                println!("redraw cost ${:.3}, storyboard total ${:.3}", sb.spent - before, sb.spent);
            }
            "produce" => {
                let client = Client::from_settings(&settings).expect("OpenRouter key");
                let sb_path = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir() && p.join("storyboard.json").is_file()).max_by_key(|p| p.metadata().and_then(|m| m.modified()).ok()).expect("a planned storyboard").join("storyboard.json");
                let mut sb = crate::ai::storyboard::Storyboard::load(&sb_path).unwrap();
                for s in sb.shots.iter_mut().filter(|s| s.frame.is_some()) {
                    s.approved = true;
                }
                let model_id = std::env::var("MV_VIDEO_MODEL").unwrap_or_else(|_| "x-ai/grok-imagine-video-1.5-lite".into());
                let m = crate::ai::video_gen::list_models(&client).unwrap().into_iter().find(|m| m.id == model_id).expect("video model");
                let approved = sb.approved();
                let before = sb.spent;
                let (rendered, errors, reused) = crate::ai::storyboard::produce_shots(&client, &mut sb, &approved, &m, false, &cancel, &mut |p, s| {
                    if (p * 100.0) as i32 % 10 == 0 { println!("  {s}") }
                });
                sb.spent += rendered.iter().filter_map(|(_, g)| g.cost).sum::<f64>();
                sb.save().unwrap();
                println!("animated {} (reused {reused}), errors: {errors:?}, video cost ${:.3}, storyboard total ${:.3}", rendered.len(), sb.spent - before, sb.spent);
                let mut p = Project::load(&project_path).unwrap();
                // Replace any earlier assembly on the main video track.
                let main = p.main_track();
                p.tracks[main].clips.clear();
                let report = crate::ai::storyboard::assemble_any(&mut p, &sb, &rendered, None, &|_| None);
                p.save(&project_path).unwrap();
                println!("assembled {} clips over {:.1}s", report.clips.len(), report.duration);
                let out = dir.join(format!("{} - music video (preview 720p).mp4", std::env::var("MV_PROJECT").unwrap_or_else(|_| "Checklist".into())));
                let es = crate::export::ExportSettings { path: out.clone(), short_side: 720, fps: 30, crf: 23, preset: "veryfast".into() };
                let t = std::time::Instant::now();
                crate::export::run(&p, &es, &cancel, |_| {}).unwrap();
                println!("exported {} in {:?}", out.display(), t.elapsed());
            }
            other => panic!("unknown stage {other}"),
        }
    }

    #[test]
    fn slots_follow_bars_and_energy() {
        let (p, _) = song_project();
        let d = plan_slots(&p, 0.0, 80.0);
        // Quiet/verse: 4 bars would be 8 s > MAX_SLOT → 2 bars (4 s); chorus: 2 bars (4 s).
        assert!(d.iter().all(|s| (s.end - s.start - 4.0).abs() < 1e-9), "{:?}", d.iter().map(|s| s.end - s.start).collect::<Vec<_>>());
        assert_eq!(d.len(), 20);
        assert!(d.windows(2).all(|w| (w[0].end - w[1].start).abs() < 1e-9), "back to back");
        assert_eq!(d[8].section, "chorus");
        assert_eq!(d[8].start, 32.0);
    }

    #[test]
    fn repeated_sections_reuse_shots_within_budget() {
        let (p, song) = song_project();
        let o = outline(&p, song, (0.0, 80.0), 10).unwrap();
        assert!(o.shots <= 10);
        // Second chorus (64–80) reuses first-chorus shots.
        let chorus1: Vec<usize> = o.drafts.iter().zip(&o.shot_of).filter(|(d, _)| d.start >= 32.0 && d.start < 48.0).map(|(_, s)| *s).collect();
        let chorus2: Vec<usize> = o.drafts.iter().zip(&o.shot_of).filter(|(d, _)| d.start >= 64.0).map(|(_, s)| *s).collect();
        assert!(chorus2.iter().all(|s| chorus1.contains(s)), "{chorus1:?} vs {chorus2:?}");
        // Every shot index is used at least once.
        assert!((0..o.shots).all(|i| o.shot_of.contains(&i)));
        let msgs = plan_messages("neon city", &o, crate::model::Aspect::Portrait);
        assert!(msgs[1]["content"].as_str().unwrap().contains("Shot 1: intro section"));
    }

    #[test]
    fn assembles_on_slot_times_with_reuse_and_stretch() {
        let (mut p, song) = song_project();
        let o = outline(&p, song, (32.0, 48.0), 2).unwrap();
        assert_eq!(o.drafts.len(), 4);
        let shots = (0..o.shots).map(|_| Shot { description: "x".into(), motion: String::new(), caption: String::new(), seconds: 4.0, frame: None, approved: true, video: None, no_lead: false }).collect();
        let slots = o.drafts.iter().zip(&o.shot_of).map(|(d, &shot)| Slot { start: d.start, end: d.end, shot, section: d.section.clone(), lyric: String::new() }).collect();
        let sb = Storyboard {
            brief: String::new(), aspect: crate::model::Aspect::Portrait, title: "t".into(), style: String::new(), script: String::new(),
            music: String::new(), shots, dir: std::env::temp_dir(), spent: 0.0, slots, song_clip: Some(song), fresh_analysis: None, character: None, cast: Vec::new(),
        };
        let gen = |path: &str, d: f64| Generated { path: path.into(), info: ProbeInfo { kind: MediaKind::Video, duration: d, width: 720, height: 1280, fps: 24.0, has_audio: false }, cost: None, job_id: String::new() };
        // Shot 0 is 6 s (room to vary), shot 1 only 3 s (must be slowed to fill 4 s).
        let r = assemble(&mut p, &sb, &[(0, gen("a.mp4", 6.0)), (1, gen("b.mp4", 3.0))]);
        assert_eq!(r.clips.len(), 4);
        let clips: Vec<_> = r.clips.iter().map(|c| p.clip(*c).unwrap().clone()).collect();
        assert_eq!(clips.iter().map(|c| c.start).collect::<Vec<_>>(), vec![32.0, 36.0, 40.0, 44.0]);
        assert!(clips.iter().all(|c| c.volume == 0.0));
        let slow = clips.iter().find(|c| p.media(c.media_id().unwrap()).unwrap().path.ends_with("b.mp4")).unwrap();
        assert!((slow.speed - 0.75).abs() < 1e-9 && (slow.duration - 4.0).abs() < 1e-9);
        let reused: Vec<_> = clips.iter().filter(|c| p.media(c.media_id().unwrap()).unwrap().path.ends_with("a.mp4")).collect();
        if reused.len() == 2 {
            assert_ne!(reused[0].source_in, reused[1].source_in, "reuse starts elsewhere");
        }
        assert_eq!(p.audio.loudness, Some(-14.0));
    }

    #[test]
    fn reassembling_replaces_the_previous_version() {
        let (mut p, song) = song_project();
        let o = outline(&p, song, (32.0, 48.0), 2).unwrap();
        let dir = std::path::PathBuf::from("C:/mv");
        let shots = (0..o.shots).map(|_| Shot { description: "x".into(), motion: String::new(), caption: String::new(), seconds: 4.0, frame: None, approved: true, video: None, no_lead: false }).collect();
        let slots = o.drafts.iter().zip(&o.shot_of).map(|(d, &shot)| Slot { start: d.start, end: d.end, shot, section: d.section.clone(), lyric: String::new() }).collect();
        let sb = Storyboard {
            brief: String::new(), aspect: crate::model::Aspect::Landscape, title: "t".into(), style: String::new(), script: String::new(),
            music: String::new(), shots, dir: dir.clone(), spent: 0.0, slots, song_clip: Some(song), fresh_analysis: None, character: None, cast: Vec::new(),
        };
        let gen = |name: &str| Generated { path: dir.join(name), info: ProbeInfo { kind: MediaKind::Video, duration: 5.0, width: 1280, height: 720, fps: 24.0, has_audio: false }, cost: None, job_id: String::new() };
        assemble(&mut p, &sb, &[(0, gen("a.mp4")), (1, gen("b.mp4"))]);
        let first = p.clip_count();
        assemble(&mut p, &sb, &[(0, gen("a-2.mp4")), (1, gen("b.mp4"))]);
        assert_eq!(p.clip_count(), first, "replaced, not stacked");
        assert_eq!(p.tracks.iter().filter(|t| t.kind == TrackKind::Video).count(), 1);
    }

    #[test]
    fn best_30_seconds_prefers_the_chorus() {
        let (mut p, _) = song_project();
        let (a, b) = best_window(&p, 16.0).unwrap();
        assert!(a == 32.0 || a == 64.0, "starts on a chorus: {a}");
        assert!((b - a - 16.0).abs() < 1e-9);
        let len = apply_window(&mut p, a, b);
        assert!((len - 16.0).abs() < 1e-9);
        let song = p.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap().clips[0].clone();
        assert!(song.fade_out > 0.5);
    }
}
