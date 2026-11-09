//! Teaser / trailer: the best 15–60 s of the video, scored by the song (beat energy,
//! a chorus start) and the picture (how lively the cutting is, how many different
//! shots), trimmed into its own project with fades and an end card. Free and local.

use crate::model::{Project, TextStyle, TrackKind};
use crate::music;

#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
    pub score: f32,
    /// Why it was picked, for the UI.
    pub why: String,
}

/// Cuts and distinct media on the main track inside `[a, b)`.
fn picture_stats(p: &Project, a: f64, b: f64) -> (usize, usize) {
    let Some(main) = p.tracks.iter().rposition(|t| t.kind == TrackKind::Video) else { return (0, 0) };
    let clips: Vec<_> = p.tracks[main].clips.iter().filter(|c| c.end() > a && c.start < b).collect();
    let cuts = clips.iter().filter(|c| c.start > a + 0.05 && c.start < b - 0.05).count();
    let mut media: Vec<_> = clips.iter().filter_map(|c| c.media_id()).collect();
    media.sort();
    media.dedup();
    (cuts, media.len())
}

/// The best window of about `seconds`, starting on a downbeat when the song has beats.
pub fn best(project: &Project, seconds: f64) -> Option<Window> {
    let total = project.duration();
    if total <= 0.0 {
        return None;
    }
    if total <= seconds + 0.5 {
        return Some(Window { start: 0.0, end: total, score: 0.0, why: "the whole video is shorter than that".into() });
    }
    let beats = music::timeline_beats(project);
    let sections = music::timeline_sections(project);
    let mut starts: Vec<f64> = beats.iter().filter(|b| b.downbeat && b.time + seconds <= total).map(|b| b.time).collect();
    if starts.is_empty() {
        starts = (0..).map(|i| i as f64).take_while(|t| t + seconds <= total).collect();
    }
    // Picture liveliness, normalised over the candidates.
    let stats: Vec<(usize, usize)> = starts.iter().map(|&s| picture_stats(project, s, s + seconds)).collect();
    let max_cuts = stats.iter().map(|s| s.0).max().unwrap_or(1).max(1) as f32;
    let max_media = stats.iter().map(|s| s.1).max().unwrap_or(1).max(1) as f32;
    let mut best: Option<Window> = None;
    for (&s, &(cuts, media)) in starts.iter().zip(&stats) {
        let energy: Vec<f32> = beats.iter().filter(|b| b.time >= s && b.time < s + seconds).map(|b| b.energy).collect();
        let music = if energy.is_empty() { 0.5 } else { energy.iter().sum::<f32>() / energy.len() as f32 };
        let chorus = sections.iter().any(|x| x.label.eq_ignore_ascii_case("chorus") && (x.start - s).abs() < 0.3);
        let picture = 0.5 * cuts as f32 / max_cuts + 0.5 * media as f32 / max_media;
        // An intro-only window is a weak teaser; later, louder, busier is better.
        let score = 0.6 * music + 0.3 * picture + if chorus { 0.12 } else { 0.0 };
        if best.as_ref().is_none_or(|b| score > b.score) {
            let why = format!(
                "{}{} cuts across {} different shots, song energy {:.0}%",
                if chorus { "starts on a chorus · " } else { "" },
                cuts,
                media,
                music * 100.0
            );
            best = Some(Window { start: s, end: s + seconds, score, why });
        }
    }
    let mut w = best?;
    // End on a downbeat near the target length.
    let target = w.start + seconds;
    if let Some(e) = beats.iter().filter(|b| b.downbeat && (b.time - target).abs() <= 1.5).map(|b| b.time).min_by(|a, b| (a - target).abs().total_cmp(&(b - target).abs())) {
        w.end = e.min(total);
    }
    Some(w)
}

/// A new project with just the window: song fades, a short fade from black, and an
/// end card (`end_card` empty = none). `vertical` re-fits it for 9:16.
pub fn make(project: &Project, w: &Window, end_card: &str, vertical: bool) -> Project {
    let mut p = project.clone();
    p.keep_ranges(&[(w.start, w.end)]);
    let len = p.duration();
    for c in p.tracks.iter_mut().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| t.clips.iter_mut()) {
        c.fade_in = c.fade_in.max(0.3_f64.min(c.duration / 4.0));
        c.fade_out = c.fade_out.max(1.2_f64.min(c.duration / 4.0));
    }
    if let Some(main) = p.tracks.iter().rposition(|t| t.kind == TrackKind::Video) {
        if let Some(first) = p.tracks[main].clips.first_mut() {
            first.fade_in = first.fade_in.max(0.4_f64.min(first.duration / 3.0));
        }
        if let Some(last) = p.tracks[main].clips.last_mut() {
            last.fade_out = last.fade_out.max(0.8_f64.min(last.duration / 3.0));
        }
    }
    let card = end_card.trim();
    if !card.is_empty() && len > 4.0 {
        let style = TextStyle { content: card.to_string(), ..TextStyle::default() };
        let mut c = p.new_text_clip(style, (len - 2.8).max(0.0));
        c.duration = 2.6;
        c.fade_in = 0.4;
        c.fade_out = 0.4;
        let ti = p.insert_track(TrackKind::Text, None);
        p.tracks[ti].name = "End card".into();
        p.tracks[ti].clips.push(c);
    }
    p.name = format!("{} - teaser {:.0}s", project.name, len);
    p.markers.clear();
    if vertical {
        p = crate::ui::export_queue::reshaped(&p, crate::model::Aspect::Portrait);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};

    #[test]
    fn prefers_lively_picture_and_makes_a_teaser() {
        let mut p = Project::default();
        let mk = |p: &mut Project, i: usize| p.add_media(MediaItem { id: 0, path: format!("{i}.mp4").into(), name: format!("{i}"), kind: MediaKind::Video, duration: 60.0, width: 1280, height: 720, fps: 30.0, has_audio: false });
        let main = p.main_track();
        // 0–60 s: one long shot; 60–120 s: a new shot every 2 s.
        let a = mk(&mut p, 0);
        let mut c = p.new_media_clip(a, 0.0).unwrap();
        c.duration = 60.0;
        p.tracks[main].clips.push(c);
        for k in 0..30 {
            let m = mk(&mut p, k + 1);
            let mut c = p.new_media_clip(m, 60.0 + k as f64 * 2.0).unwrap();
            c.duration = 2.0;
            p.tracks[main].clips.push(c);
        }
        let w = best(&p, 30.0).unwrap();
        assert!(w.start >= 60.0, "the busy half wins: {w:?}");
        let t = make(&p, &w, "Full video out now", true);
        assert!((t.duration() - 30.0).abs() < 0.01);
        assert_eq!(t.aspect, crate::model::Aspect::Portrait);
        assert!(t.tracks.iter().any(|tr| tr.name == "End card" && tr.clips.len() == 1));
        assert!(t.name.contains("teaser 30s"));
    }

    /// Manual: `TEASER_PROJECT=song.rcproj TEASER_OUT=dir cargo test --lib teaser_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn teaser_live() {
        let Ok(path) = std::env::var("TEASER_PROJECT") else { return };
        let out = std::path::PathBuf::from(std::env::var("TEASER_OUT").unwrap());
        let p = Project::load(std::path::Path::new(&path)).unwrap();
        for secs in [15.0, 30.0, 60.0] {
            let w = best(&p, secs).unwrap();
            println!("{secs:.0}s: {:.1}–{:.1} ({})", w.start, w.end, w.why);
        }
        let w = best(&p, 30.0).unwrap();
        let t = make(&p, &w, "Full video out now", true);
        println!("teaser: {} | {:.1}s | {:?}", t.name, t.duration(), t.aspect);
        std::fs::write(out.join("teaser-start.png"), crate::export::render_frame_png(&t, 1.0, 360).unwrap()).unwrap();
        std::fs::write(out.join("teaser-end.png"), crate::export::render_frame_png(&t, t.duration() - 1.2, 360).unwrap()).unwrap();
    }
}
