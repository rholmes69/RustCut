//! Lyrics for music videos: timed lines/words from an `.lrc` file, from pasted lyrics
//! aligned to a speech transcript of the vocals, or from the transcript alone; laid
//! out as caption clips in three styles; exported back to `.lrc`.

use crate::captions::Segment;
use crate::model::{Ease, Keyframe, Project, Prop, TextStyle, TrackKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub text: String,
    pub start: f64,
    pub end: f64,
    pub words: Vec<Word>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LyricStyle {
    /// One caption per line.
    Line,
    /// One big word at a time, popping in.
    PopWords,
    /// The line builds up word by word.
    BuildUp,
    /// Karaoke: the whole line in white, each word lighting up as it's sung.
    Karaoke,
    /// One big line at a time in the middle of the frame.
    BigCentered,
}

impl LyricStyle {
    pub const ALL: [LyricStyle; 5] = [LyricStyle::Karaoke, LyricStyle::Line, LyricStyle::BigCentered, LyricStyle::PopWords, LyricStyle::BuildUp];
    pub fn label(self) -> &'static str {
        match self {
            LyricStyle::Line => "Lines",
            LyricStyle::PopWords => "Pop words (one at a time)",
            LyricStyle::BuildUp => "Build-up (word by word)",
            LyricStyle::Karaoke => "Karaoke (words light up)",
            LyricStyle::BigCentered => "Big centred lines",
        }
    }
    pub fn parse(s: &str) -> Option<LyricStyle> {
        match s.trim().to_ascii_lowercase().replace([' ', '-'], "_").as_str() {
            "line" | "lines" => Some(LyricStyle::Line),
            "pop" | "pop_words" | "words" => Some(LyricStyle::PopWords),
            "build" | "build_up" | "buildup" => Some(LyricStyle::BuildUp),
            "karaoke" | "highlight" => Some(LyricStyle::Karaoke),
            "big" | "big_centered" | "big_centred" | "centered" | "centred" => Some(LyricStyle::BigCentered),
            _ => None,
        }
    }
}

fn parse_stamp(s: &str) -> Option<f64> {
    let (m, rest) = s.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let sec: f64 = rest.trim().replace(',', ".").parse().ok()?;
    Some(m * 60.0 + sec)
}

/// Parses `.lrc` text: `[mm:ss.xx]line`, several stamps per line, optional
/// `<mm:ss.xx>` word stamps (enhanced LRC) and `[offset:+/-ms]`. Times are song seconds.
/// `None` if the text has no timestamps (plain lyrics).
pub fn parse_lrc(text: &str) -> Option<Vec<Line>> {
    let mut offset = 0.0;
    let mut entries: Vec<(f64, String, Vec<(f64, String)>)> = Vec::new();
    for raw in text.lines() {
        let mut rest = raw.trim();
        let mut stamps = Vec::new();
        while let Some(stripped) = rest.strip_prefix('[') {
            let Some(close) = stripped.find(']') else { break };
            let tag = &stripped[..close];
            if let Some(v) = tag.strip_prefix("offset:") {
                offset = v.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
            } else if let Some(t) = parse_stamp(tag) {
                stamps.push(t);
            }
            rest = stripped[close + 1..].trim_start();
        }
        if stamps.is_empty() {
            continue;
        }
        // Enhanced word stamps.
        let mut words = Vec::new();
        let mut plain = String::new();
        let mut cursor = rest;
        while let Some(i) = cursor.find('<') {
            plain += &cursor[..i];
            let Some(j) = cursor[i..].find('>') else { break };
            let stamp = &cursor[i + 1..i + j];
            let after = &cursor[i + j + 1..];
            let word_end = after.find('<').unwrap_or(after.len());
            if let Some(t) = parse_stamp(stamp) {
                let w = after[..word_end].trim();
                if !w.is_empty() {
                    words.push((t, w.to_string()));
                }
            }
            cursor = after;
        }
        plain += cursor;
        let line: String = if words.is_empty() { plain.trim().to_string() } else { words.iter().map(|w| w.1.as_str()).collect::<Vec<_>>().join(" ") };
        for t in stamps {
            entries.push((t, line.clone(), words.clone()));
        }
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut lines = Vec::new();
    for (k, (t, text, words)) in entries.iter().enumerate() {
        if text.is_empty() {
            continue; // blank stamped lines just end the previous line
        }
        let next = entries.get(k + 1).map_or(t + 5.0, |e| e.0);
        let start = t - offset;
        let end = (next - offset).min(start + 8.0).max(start + 0.3);
        let words = if words.is_empty() {
            spread_words(text, start, end)
        } else {
            words
                .iter()
                .enumerate()
                .map(|(i, (wt, w))| Word { text: w.clone(), start: wt - offset, end: words.get(i + 1).map_or(end, |n| n.0 - offset) })
                .collect()
        };
        lines.push(Line { text: text.clone(), start, end, words });
    }
    Some(lines)
}

/// Lines of plain lyrics, without section tags like `[Chorus]` or `(x2)` and blank lines.
pub fn plain_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        // Section tags ([Chorus]) and lines that are only a bracketed note or ad-lib.
        .filter(|l| !l.is_empty() && !(l.starts_with('[') && l.ends_with(']')) && !(l.starts_with('(') && l.ends_with(')')))
        .map(strip_repeat_mark)
        .filter(|l| !l.is_empty())
        .collect()
}

/// Removes a trailing repeat mark such as "(x2)", "x2" or "×3"; keeps backing vocals like "(one by one)".
fn strip_repeat_mark(line: &str) -> String {
    let t = line.trim_end();
    let lower = t.to_lowercase();
    for open in ["(x", "(×", " x", " ×"] {
        if let Some(i) = lower.rfind(open) {
            let tail = &lower[i + open.len()..];
            let digits = tail.trim_end_matches(')');
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                return t[..i].trim_end().to_string();
            }
        }
    }
    t.to_string()
}

/// Words spread over `[start, end]` by length.
pub fn spread_words(text: &str, start: f64, end: f64) -> Vec<Word> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let total: usize = words.iter().map(|w| w.chars().count() + 1).sum();
    let mut t = start;
    words
        .iter()
        .map(|w| {
            let d = (end - start) * (w.chars().count() + 1) as f64 / total.max(1) as f64;
            let word = Word { text: w.to_string(), start: t, end: t + d };
            t += d;
            word
        })
        .collect()
}

fn norm(w: &str) -> String {
    w.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

fn similar(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    if a.len() < 4 || b.len() < 4 {
        return false;
    }
    // One edit apart (whisper often mishears a letter).
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > 1 {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        prev = cur;
    }
    prev[b.len()] <= 1
}

/// Transcript words with times (each segment's span shared out by word length).
pub fn transcript_words(segments: &[Segment]) -> Vec<Word> {
    segments.iter().flat_map(|s| spread_words(&s.text, s.start, s.end)).filter(|w| !norm(&w.text).is_empty()).collect()
}

/// Aligns lyric lines to transcript words (dynamic programming over words), so every
/// lyric word gets a time; words whisper missed are interpolated.
pub fn align(lyrics: &[String], segments: &[Segment]) -> Vec<Line> {
    let heard = transcript_words(segments);
    let lw: Vec<(usize, String)> = lyrics.iter().enumerate().flat_map(|(i, l)| l.split_whitespace().map(move |w| (i, w.to_string()))).collect();
    let (n, m) = (lw.len(), heard.len());
    let mut times: Vec<Option<(f64, f64)>> = vec![None; n];
    if n > 0 && m > 0 {
        let a: Vec<String> = lw.iter().map(|w| norm(&w.1)).collect();
        let b: Vec<String> = heard.iter().map(|w| norm(&w.text)).collect();
        // cost[i][j]: aligning a[..i] with b[..j]; gaps cost 1, mismatches 1.5, matches 0.
        let mut cost = vec![vec![0.0f32; m + 1]; n + 1];
        for i in 0..=n {
            cost[i][0] = i as f32;
        }
        for j in 0..=m {
            cost[0][j] = j as f32 * 0.5; // skipping heard words (noise) is cheap
        }
        for i in 1..=n {
            for j in 1..=m {
                let sub = if similar(&a[i - 1], &b[j - 1]) { 0.0 } else { 1.5 };
                cost[i][j] = (cost[i - 1][j - 1] + sub).min(cost[i - 1][j] + 1.0).min(cost[i][j - 1] + 0.5);
            }
        }
        let (mut i, mut j) = (n, m);
        while i > 0 && j > 0 {
            let sub = if similar(&a[i - 1], &b[j - 1]) { 0.0 } else { 1.5 };
            if (cost[i][j] - (cost[i - 1][j - 1] + sub)).abs() < 1e-4 {
                if sub == 0.0 {
                    times[i - 1] = Some((heard[j - 1].start, heard[j - 1].end));
                }
                i -= 1;
                j -= 1;
            } else if (cost[i][j] - (cost[i - 1][j] + 1.0)).abs() < 1e-4 {
                i -= 1;
            } else {
                j -= 1;
            }
        }
    }
    // Interpolate unmatched words between matched neighbours.
    let known: Vec<usize> = (0..n).filter(|&k| times[k].is_some()).collect();
    let span = (heard.first().map_or(0.0, |w| w.start), heard.last().map_or(n as f64 * 0.5, |w| w.end));
    for k in 0..n {
        if times[k].is_some() {
            continue;
        }
        let prev = known.iter().rev().find(|&&x| x < k).copied();
        let next = known.iter().find(|&&x| x > k).copied();
        let (mut t0, k0) = prev.map_or((span.0, None), |p| (times[p].unwrap().1, Some(p)));
        let (t1, k1) = next.map_or((span.1, None), |q| (times[q].unwrap().0, Some(q)));
        let first = k0.map_or(0, |p| p + 1);
        let last = k1.unwrap_or(n);
        let slots = (last - first).max(1) as f64;
        // No gap to put the missed words in: take the second half of the previous word.
        if t1 - t0 < 0.08 * slots {
            if let Some(p) = k0 {
                let (ps, pe) = times[p].unwrap();
                t0 = (ps + pe) / 2.0;
                times[p] = Some((ps, t0));
            }
        }
        let d = ((t1 - t0) / slots).max(0.05);
        let pos = (k - first) as f64;
        times[k] = Some((t0 + pos * d, t0 + (pos + 1.0) * d));
    }
    let mut lines: Vec<Line> = Vec::new();
    for (li, text) in lyrics.iter().enumerate() {
        let words: Vec<Word> = lw
            .iter()
            .zip(&times)
            .filter(|((i, _), _)| *i == li)
            .map(|((_, w), t)| {
                let (s, e) = t.unwrap_or((0.0, 0.0));
                Word { text: w.clone(), start: s, end: e.max(s + 0.05) }
            })
            .collect();
        if words.is_empty() {
            continue;
        }
        lines.push(Line { text: text.clone(), start: words[0].start, end: words.last().unwrap().end, words });
    }
    // A line far longer than its words could take was spread over an instrumental gap by
    // interpolation: pack it next to the side that was actually heard.
    let heard_flags: Vec<bool> = known.iter().fold(vec![false; n], |mut v, &k| {
        v[k] = true;
        v
    });
    let mut w = 0;
    for line in lines.iter_mut() {
        let count = line.words.len();
        let flags = &heard_flags[w..w + count];
        w += count;
        let expected = 0.45 * count as f64 + 0.5;
        let span = line.end - line.start;
        if span <= expected * 2.2 {
            continue;
        }
        let (first_heard, last_heard) = (flags[0], flags[count - 1]);
        let (a, b) = if last_heard || !first_heard {
            (line.end - expected, line.end) // singing resumes just before the heard end
        } else {
            (line.start, line.start + expected) // the line ends early, then the gap
        };
        line.words = spread_words(&line.text, a, b);
        line.start = a;
        line.end = b;
    }
    // Keep lines in order and non-overlapping.
    for k in 1..lines.len() {
        if lines[k].start < lines[k - 1].end {
            let cut = lines[k].start.max(lines[k - 1].start + 0.1);
            lines[k - 1].end = cut;
        }
    }
    lines
}

/// Lines straight from a transcript: break at pauses over 0.6 s or after 7 words.
pub fn lines_from_transcript(segments: &[Segment]) -> Vec<Line> {
    let words = transcript_words(segments);
    let mut lines = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    for w in words {
        if let Some(last) = cur.last() {
            let ends_phrase = last.text.ends_with(['.', ',', '!', '?', ';']);
            let first = w.text.trim_start_matches(|c: char| !c.is_alphanumeric());
            let new_line = first.chars().next().is_some_and(char::is_uppercase) && !matches!(first, "I" | "I'm" | "I've" | "I'll" | "I'd");
            if w.start - last.end > 0.6 || cur.len() >= 9 || (cur.len() >= 3 && ends_phrase) || (cur.len() >= 2 && new_line) {
                let text = cur.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                lines.push(Line { text, start: cur[0].start, end: cur.last().unwrap().end, words: std::mem::take(&mut cur) });
            }
        }
        cur.push(w);
    }
    if !cur.is_empty() {
        let text = cur.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        lines.push(Line { text, start: cur[0].start, end: cur.last().unwrap().end, words: cur });
    }
    lines
}

/// Moves song-time lines onto the timeline via the song clip (`start`, `source_in`, `speed`).
pub fn to_timeline(lines: &mut [Line], clip_start: f64, source_in: f64, speed: f64) {
    let f = |t: f64| clip_start + (t - source_in) / speed.max(1e-6);
    for l in lines.iter_mut() {
        l.start = f(l.start);
        l.end = f(l.end);
        for w in &mut l.words {
            w.start = f(w.start);
            w.end = f(w.end);
        }
    }
}

/// Karaoke highlight colour.
pub const HIGHLIGHT: [u8; 4] = [255, 214, 0, 255];

/// Splits lines longer than `max_chars` into balanced single-line parts (word times kept).
pub fn split_long_lines(lines: &[Line], max_chars: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for l in lines {
        let len = l.text.chars().count();
        if len <= max_chars || l.words.len() < 2 {
            out.push(l.clone());
            continue;
        }
        let parts = len.div_ceil(max_chars).min(l.words.len());
        let target = len as f64 / parts as f64;
        let mut cur: Vec<Word> = Vec::new();
        let mut cur_len = 0usize;
        let mut made = 0;
        for (i, w) in l.words.iter().enumerate() {
            let wl = w.text.chars().count() + usize::from(!cur.is_empty());
            let remaining_words = l.words.len() - i;
            let remaining_parts = parts - made;
            if !cur.is_empty() && made + 1 < parts && (cur_len + wl > max_chars || (cur_len as f64 >= target && remaining_words >= remaining_parts - 1)) {
                out.push(line_of(std::mem::take(&mut cur), None));
                made += 1;
                cur_len = 0;
            }
            cur_len += w.text.chars().count() + usize::from(!cur.is_empty());
            cur.push(w.clone());
        }
        if !cur.is_empty() {
            out.push(line_of(cur, Some(l.end)));
        }
    }
    out
}

fn line_of(words: Vec<Word>, end: Option<f64>) -> Line {
    let text = words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
    Line { text, start: words[0].start, end: end.unwrap_or(words.last().unwrap().end), words }
}

/// Moves word starts onto the nearest beat or half-beat within 0.2 s (keeping order),
/// so words pop on the beat.
pub fn snap_words(lines: &mut [Line], beats: &[f64]) {
    if beats.len() < 2 {
        return;
    }
    let mut grid: Vec<f64> = Vec::with_capacity(beats.len() * 2);
    for w in beats.windows(2) {
        grid.push(w[0]);
        grid.push((w[0] + w[1]) / 2.0);
    }
    grid.push(*beats.last().unwrap());
    let mut last = f64::NEG_INFINITY;
    for l in lines.iter_mut() {
        for w in l.words.iter_mut() {
            let i = grid.partition_point(|g| *g < w.start);
            let near = [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|k| grid.get(k).copied()).min_by(|a, b| (a - w.start).abs().total_cmp(&(b - w.start).abs()));
            if let Some(g) = near.filter(|g| (g - w.start).abs() <= 0.2 && *g > last + 0.05) {
                w.start = g;
            }
            last = w.start;
        }
        if let Some(first) = l.words.first() {
            l.start = first.start;
        }
    }
}

/// How many characters of caption text fit across the canvas at `font_size`.
fn chars_across(project: &Project, font_size: f32) -> usize {
    let (cw, _) = project.aspect.size_for(1080);
    ((cw as f32 * 0.88) / (font_size * 0.56)).floor().max(8.0) as usize
}

fn fresh_track(project: &mut Project, name: &str, captions: bool) -> usize {
    let ti = project.insert_track(TrackKind::Text, None);
    project.tracks[ti].captions = captions;
    project.tracks[ti].name = name.into();
    ti
}

fn no_overlap(clips: &mut [crate::model::Clip]) {
    clips.sort_by(|a, b| a.start.total_cmp(&b.start));
    for k in 1..clips.len() {
        let s = clips[k].start;
        let prev = &mut clips[k - 1];
        if prev.end() > s {
            prev.duration = (s - prev.start).max(crate::model::MIN_CLIP_LEN);
        }
    }
}

/// Lays lyrics out as caption clips on a "Lyrics" track (replacing earlier lyric
/// tracks). Karaoke adds a "Lyrics highlight" track above it. `y` is the vertical
/// position (fraction of canvas height; big centred lines ignore it). With
/// `beat_snap`, pop words land on the nearest beat. Returns the clip count.
pub fn apply_lyrics(project: &mut Project, lines: &[Line], style: LyricStyle, y: f32, beat_snap: bool) -> usize {
    project.tracks.retain(|t| !(t.name == "Lyrics" || t.name == "Lyrics highlight") || t.kind != TrackKind::Text);
    project.main_track();
    let base = crate::captions::caption_styles().into_iter().next().map(|(_, s)| s).unwrap_or_default();
    let mut lines: Vec<Line> = lines.to_vec();
    if beat_snap && style == LyricStyle::PopWords {
        let beats: Vec<f64> = crate::music::timeline_beats(project).iter().map(|b| b.time).collect();
        snap_words(&mut lines, &beats);
    }
    let big = base.font_size * 1.8;
    match style {
        LyricStyle::Karaoke => lines = split_long_lines(&lines, chars_across(project, base.font_size)),
        LyricStyle::BigCentered => lines = split_long_lines(&lines, chars_across(project, big)),
        _ => {}
    }
    let ti = fresh_track(project, "Lyrics", true);
    let mut clips = Vec::new();
    let mut highlights = Vec::new();
    for (k, line) in lines.iter().enumerate() {
        let next_start = lines.get(k + 1).map_or(f64::INFINITY, |n| n.start);
        let line_end = (line.end + 0.8).min(next_start);
        match style {
            LyricStyle::Line | LyricStyle::BigCentered => {
                let (size, wrap, yy) = if style == LyricStyle::BigCentered { (big, usize::MAX, 0.0) } else { (base.font_size, 30, y) };
                let style = TextStyle { content: crate::captions::wrap_caption(&line.text, wrap), font_size: size, ..base.clone() };
                let mut c = project.new_text_clip(style, line.start);
                c.duration = (line_end - line.start).max(0.2);
                c.transform.y = yy;
                clips.push(c);
            }
            LyricStyle::Karaoke => {
                // The full line in white…
                let full = line.text.clone();
                let white = TextStyle { content: full.clone(), align_to: Some(full.clone()), ..base.clone() };
                let mut c = project.new_text_clip(white, line.start);
                c.duration = (line_end - line.start).max(0.2);
                c.transform.y = y;
                clips.push(c);
                // …and the sung part on top, growing word by word from the same left edge.
                for (i, w) in line.words.iter().enumerate() {
                    let end = line.words.get(i + 1).map_or(line_end, |n| n.start);
                    if end - w.start < 0.03 {
                        continue;
                    }
                    let sung = line.words[..=i].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                    let lit = TextStyle { content: sung, color: HIGHLIGHT, align_to: Some(full.clone()), ..base.clone() };
                    let mut h = project.new_text_clip(lit, w.start);
                    h.duration = end - w.start;
                    h.transform.y = y;
                    highlights.push(h);
                }
            }
            LyricStyle::PopWords => {
                for (i, w) in line.words.iter().enumerate() {
                    let end = line.words.get(i + 1).map_or(line_end, |n| n.start).min(w.start + 1.5);
                    if end - w.start < 0.05 {
                        continue;
                    }
                    let style = TextStyle { content: w.text.clone(), font_size: big, ..base.clone() };
                    let mut c = project.new_text_clip(style, w.start);
                    c.duration = end - w.start;
                    c.transform.y = y;
                    let pop = (c.duration * 0.6).min(0.12);
                    c.keyframes.insert(Prop::Scale, vec![
                        Keyframe { time: 0.0, value: 1.3, ease: Ease::EaseOut },
                        Keyframe { time: pop, value: 1.0, ease: Ease::Linear },
                    ]);
                    clips.push(c);
                }
            }
            LyricStyle::BuildUp => {
                for (i, w) in line.words.iter().enumerate() {
                    let end = line.words.get(i + 1).map_or(line_end, |n| n.start);
                    if end - w.start < 0.05 {
                        continue;
                    }
                    let shown = line.words[..=i].iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
                    let style = TextStyle { content: crate::captions::wrap_caption(&shown, 30), ..base.clone() };
                    let mut c = project.new_text_clip(style, w.start);
                    c.duration = end - w.start;
                    c.transform.y = y;
                    clips.push(c);
                }
            }
        }
    }
    no_overlap(&mut clips);
    let mut n = clips.len();
    project.tracks[ti].clips = clips;
    if !highlights.is_empty() {
        no_overlap(&mut highlights);
        n += highlights.len();
        // Above the white lines (tracks draw top-down, upper on top).
        let hi = fresh_track(project, "Lyrics highlight", false);
        project.tracks[hi].clips = highlights;
    }
    n
}

fn stamp(t: f64) -> String {
    let t = t.max(0.0);
    format!("{:02}:{:05.2}", (t / 60.0).floor() as u64, t % 60.0)
}

/// `.lrc` text (line stamps) for lines in song time.
pub fn to_lrc(lines: &[Line], title: &str) -> String {
    let mut out = format!("[ti:{title}]\n[re:RustCut]\n");
    for l in lines {
        out += &format!("[{}]{}\n", stamp(l.start), l.text.replace('\n', " "));
    }
    out
}

/// Lyric lines back from the timeline's "Lyrics" track (merging word clips into lines), in timeline time.
pub fn timeline_lines(project: &Project) -> Vec<Line> {
    let Some(track) = project.tracks.iter().find(|t| t.captions && t.name == "Lyrics").or_else(|| project.tracks.iter().find(|t| t.captions)) else {
        return Vec::new();
    };
    let mut lines: Vec<Line> = Vec::new();
    for c in &track.clips {
        let Some(text) = c.text().map(|t| t.content.replace('\n', " ")) else { continue };
        // Build-up clips extend the previous one; pop words join until a pause.
        if let Some(last) = lines.last_mut() {
            let continues = text.starts_with(&last.text) && text.len() > last.text.len();
            let joins = !text.contains(' ') && !last.text.is_empty() && c.start - last.end < 0.05 && last.words.len() < 7 && !continues && last.words.len() == last.text.split_whitespace().count();
            if continues {
                last.text = text;
                last.end = c.end();
                continue;
            }
            if joins && text.split_whitespace().count() == 1 && last.text.split_whitespace().count() >= 1 && track.clips.iter().all(|x| x.text().is_some_and(|t| !t.content.contains(' '))) {
                last.text += &format!(" {text}");
                last.end = c.end();
                last.words.push(Word { text: text.clone(), start: c.start, end: c.end() });
                continue;
            }
        }
        lines.push(Line { text: text.clone(), start: c.start, end: c.end(), words: vec![Word { text, start: c.start, end: c.end() }] });
    }
    lines
}

/// Where the lyrics come from.
#[derive(Clone, Debug)]
pub enum Input {
    /// Pasted or loaded text: `.lrc` (timed) or plain lines (aligned to the vocals).
    Text(String),
    /// No lyrics: transcribe the vocals.
    Transcribe,
}

/// Timed lyric lines in timeline time for the song clip. Transcribes the vocals with
/// whisper when the text has no timestamps (or there is no text). `Ok(None)` if cancelled.
pub fn timed_lines(
    project: &Project,
    song_clip: crate::model::Id,
    input: &Input,
    language: &str,
    cancel: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(f32, &str),
) -> anyhow::Result<Option<Vec<Line>>> {
    let clip = project.clip(song_clip).cloned().ok_or_else(|| anyhow::anyhow!("the song clip was removed"))?;
    if let Input::Text(text) = input {
        if let Some(mut lines) = parse_lrc(text) {
            to_timeline(&mut lines, clip.start, clip.source_in, clip.speed);
            lines.retain(|l| l.end > clip.start && l.start < clip.end());
            return Ok(Some(lines));
        }
    }
    let (engine, model) = crate::ai::rough_cut::speech_setup()
        .ok_or_else(|| anyhow::anyhow!("timing lyrics needs a speech model: download one in the Captions tab (or use an .lrc file with timestamps)"))?;
    let opts = crate::captions::Options { model, language: language.to_string(), translate: false, max_len: Some(10) };
    progress(0.02, "Listening to the vocals…");
    let Some(segments) = crate::captions::transcribe(project, crate::captions::Source::Clip(song_clip), &opts, &engine, cancel, |p, _| progress(0.05 + 0.9 * p, "Listening to the vocals…"))? else {
        return Ok(None);
    };
    if segments.is_empty() {
        anyhow::bail!("no vocals were recognised in the song");
    }
    Ok(Some(match input {
        Input::Text(text) => {
            let lines = plain_lines(text);
            if lines.is_empty() {
                lines_from_transcript(&segments)
            } else {
                align(&lines, &segments)
            }
        }
        Input::Transcribe => lines_from_transcript(&segments),
    }))
}

/// Lines back in song time (for `.lrc` export) via the song clip.
pub fn to_song_time(lines: &mut [Line], clip_start: f64, source_in: f64, speed: f64) {
    let f = |t: f64| source_in + (t - clip_start) * speed;
    for l in lines.iter_mut() {
        l.start = f(l.start);
        l.end = f(l.end);
        for w in &mut l.words {
            w.start = f(w.start);
            w.end = f(w.end);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;


    /// Manual: `RUSTCUT_SONG=path cargo test --lib real_song_lyrics -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_song_lyrics() {
        let Ok(song) = std::env::var("RUSTCUT_SONG") else { return };
        let mut p = Project::default();
        let m = p.add_media(crate::media::ffmpeg::probe(std::path::Path::new(&song)).unwrap().into_media_item(song.clone().into()));
        let mut c = p.new_media_clip(m, 0.0).unwrap();
        c.duration = c.duration.min(60.0);
        let id = c.id;
        p.place_clip(c, None);
        let t = std::time::Instant::now();
        let lines = timed_lines(&p, id, &Input::Transcribe, "auto", &std::sync::atomic::AtomicBool::new(false), &mut |_, _| {}).unwrap().unwrap();
        println!("{} lines in {:?}", lines.len(), t.elapsed());
        for l in lines.iter().take(12) {
            println!("  [{:6.2}–{:6.2}] {}", l.start, l.end, l.text);
        }
    }

    #[test]
    fn karaoke_layers_white_line_and_growing_highlight() {
        let lines = vec![Line { text: "One two three".into(), start: 1.0, end: 2.5, words: spread_words("One two three", 1.0, 2.5) }];
        let mut p = Project::default();
        let n = apply_lyrics(&mut p, &lines, LyricStyle::Karaoke, 0.3, false);
        assert_eq!(n, 4, "1 white line + 3 highlight steps");
        let white = p.tracks.iter().position(|t| t.name == "Lyrics").unwrap();
        let hi = p.tracks.iter().position(|t| t.name == "Lyrics highlight").unwrap();
        assert!(hi < white, "highlight draws on top");
        assert!(p.tracks[white].captions && !p.tracks[hi].captions, "only the white lines export as subtitles");
        let h = &p.tracks[hi].clips;
        let texts: Vec<&str> = h.iter().map(|c| c.text().unwrap().content.as_str()).collect();
        assert_eq!(texts, ["One", "One two", "One two three"]);
        assert!(h.iter().all(|c| c.text().unwrap().align_to.as_deref() == Some("One two three") && c.text().unwrap().color == HIGHLIGHT));
        assert_eq!(p.tracks[white].clips[0].text().unwrap().align_to.as_deref(), Some("One two three"));
        // Re-applying replaces both tracks.
        apply_lyrics(&mut p, &lines, LyricStyle::Line, 0.3, false);
        assert!(!p.tracks.iter().any(|t| t.name == "Lyrics highlight"));
        assert_eq!(timeline_lines(&p)[0].text, "One two three");
    }

    #[test]
    fn long_lines_split_and_big_lines_centre() {
        let text = "this is a really quite long lyric line that will not fit on one row";
        let lines = vec![Line { text: text.into(), start: 0.0, end: 6.0, words: spread_words(text, 0.0, 6.0) }];
        let parts = split_long_lines(&lines, 24);
        assert!(parts.len() >= 3 && parts.iter().all(|l| l.text.chars().count() <= 24), "{parts:?}");
        assert_eq!(parts.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join(" "), text);
        assert!(parts.windows(2).all(|w| w[0].end <= w[1].start + 1e-9));
        let mut p = Project::default();
        p.aspect = crate::model::Aspect::Portrait;
        apply_lyrics(&mut p, &lines, LyricStyle::BigCentered, 0.3, false);
        let track = p.tracks.iter().find(|t| t.name == "Lyrics").unwrap();
        assert!(track.clips.len() >= 3);
        assert!(track.clips.iter().all(|c| c.transform.y == 0.0 && c.text().unwrap().font_size > 100.0));
    }

    #[test]
    fn words_snap_to_the_beat() {
        let mut lines = vec![Line { text: "a b c".into(), start: 1.08, end: 3.0, words: vec![
            Word { text: "a".into(), start: 1.08, end: 1.5 },
            Word { text: "b".into(), start: 1.66, end: 2.0 },
            Word { text: "c".into(), start: 2.4, end: 3.0 },
        ] }];
        let beats: Vec<f64> = (0..10).map(|i| i as f64 * 0.5).collect(); // grid of 0.25 s with half-beats
        snap_words(&mut lines, &beats);
        let starts: Vec<f64> = lines[0].words.iter().map(|w| w.start).collect();
        assert_eq!(starts, vec![1.0, 1.75, 2.5]);
        assert_eq!(lines[0].start, 1.0);
    }


    /// Manual visual check: `RUSTCUT_OUT=dir cargo test --lib karaoke_frame -- --ignored`
    #[test]
    #[ignore]
    fn karaoke_frame() {
        let Ok(dir) = std::env::var("RUSTCUT_OUT") else { return };
        let text = "A gallery of moments under a digital sky";
        let lines = vec![Line { text: text.into(), start: 0.0, end: 4.0, words: spread_words(text, 0.0, 4.0) }];
        for (aspect, name) in [(crate::model::Aspect::Portrait, "portrait"), (crate::model::Aspect::Landscape, "landscape")] {
            let mut p = Project::default();
            p.aspect = aspect;
            apply_lyrics(&mut p, &lines, LyricStyle::Karaoke, 0.0, false);
            for t in [1.6, 3.9] {
                let png = crate::export::render_frame_png(&p, t, 720).unwrap();
                std::fs::write(std::path::Path::new(&dir).join(format!("karaoke-{name}-{t}.png")), png).unwrap();
            }
        }
    }

    #[test]
    fn parses_lrc_with_offsets_repeats_and_word_stamps() {
        let lrc = "[ti:Song]\n[offset:+500]\n[00:12.00]First line here\n[00:15.50][01:02.00]Chorus line\n[00:18.00]\n[00:20.00]<00:20.00>Word <00:20.40>by <00:20.90>word\n";
        let lines = parse_lrc(lrc).unwrap();
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert!((lines[0].start - 11.5).abs() < 1e-9, "offset applied");
        assert!((lines[1].end - 17.5).abs() < 1e-9, "a blank stamp ends the line");
        assert_eq!(lines[2].text, "Word by word");
        assert!((lines[2].words[1].start - 19.9).abs() < 1e-9);
        assert_eq!(lines[3].text, "Chorus line");
        assert!((lines[3].start - 61.5).abs() < 1e-9);
        assert!(parse_lrc("just\nplain lyrics").is_none());
    }

    #[test]
    fn plain_lyrics_drop_section_tags() {
        let l = plain_lines("[Verse 1]\nHello darkness\n\n[Chorus]\nSing it loud (x2)\n(Adding more layers)\nOne by one (one by one)\nAgain x3\n");
        assert_eq!(l, vec!["Hello darkness", "Sing it loud", "One by one (one by one)", "Again"]);
    }

    #[test]
    fn a_line_after_an_instrumental_is_not_stretched_over_it() {
        let lyrics = vec!["the end of time".to_string(), "I've got a checklist it's getting long".to_string(), "all the qualities".to_string()];
        // Whisper heard the first line, nothing during a 15 s solo, and only the end of the second line.
        let segs = vec![
            Segment { start: 10.0, end: 12.0, text: "the end of time".into() },
            Segment { start: 28.5, end: 29.5, text: "getting long".into() },
            Segment { start: 30.0, end: 31.5, text: "all the qualities".into() },
        ];
        let lines = align(&lyrics, &segs);
        assert!(lines[1].start > 24.0, "starts just before it's heard, not during the solo: {}", lines[1].start);
        assert!((lines[1].end - 29.5).abs() < 0.6);
        assert!(lines[0].end <= 12.5);
    }

    #[test]
    fn aligns_lyrics_to_a_noisy_transcript() {
        let lyrics = vec!["Hello darkness my old friend".to_string(), "I've come to talk with you again".to_string()];
        // Whisper heard most words, misheard "friend" → "fiend", missed "with", added noise.
        let segs = vec![
            Segment { start: 10.0, end: 11.0, text: "yeah".into() },
            Segment { start: 12.0, end: 14.0, text: "hello darkness my".into() },
            Segment { start: 14.0, end: 15.5, text: "old fiend".into() },
            Segment { start: 17.0, end: 19.0, text: "ive come to talk".into() },
            Segment { start: 19.0, end: 21.0, text: "you again".into() },
        ];
        let lines = align(&lyrics, &segs);
        assert_eq!(lines.len(), 2);
        assert!((lines[0].start - 12.0).abs() < 0.01, "starts on 'hello', not the noise: {}", lines[0].start);
        assert!(lines[0].end <= lines[1].start + 1e-9);
        assert!((lines[1].start - 17.0).abs() < 0.01);
        let with = &lines[1].words[4];
        assert_eq!(with.text, "with");
        assert!(with.start >= lines[1].words[3].end - 1e-9 && with.end <= lines[1].words[5].start + 1e-9, "interpolated between neighbours");
    }

    #[test]
    fn styles_make_the_right_clips_and_lrc_roundtrips() {
        let lines = vec![
            Line { text: "One two three".into(), start: 1.0, end: 2.5, words: spread_words("One two three", 1.0, 2.5) },
            Line { text: "Four five".into(), start: 3.0, end: 4.0, words: spread_words("Four five", 3.0, 4.0) },
        ];
        let mut p = Project::default();
        assert_eq!(apply_lyrics(&mut p, &lines, LyricStyle::Line, 0.3, false), 2);
        assert_eq!(apply_lyrics(&mut p, &lines, LyricStyle::PopWords, 0.0, false), 5);
        assert_eq!(p.tracks.iter().filter(|t| t.name == "Lyrics").count(), 1, "replaced, not stacked");
        let track = p.tracks.iter().find(|t| t.name == "Lyrics").unwrap();
        assert!(track.clips[0].keyframes.contains_key(&Prop::Scale));
        assert!(track.clips.windows(2).all(|w| w[0].end() <= w[1].start + 1e-9));
        assert_eq!(apply_lyrics(&mut p, &lines, LyricStyle::BuildUp, 0.3, false), 5);
        let back = timeline_lines(&p);
        assert_eq!(back.len(), 2, "{back:?}");
        assert_eq!(back[0].text, "One two three");
        let lrc = to_lrc(&back, "t");
        assert!(lrc.contains("[00:01.00]One two three"));
        let parsed = parse_lrc(&lrc).unwrap();
        assert_eq!(parsed[1].text, "Four five");
    }
}
