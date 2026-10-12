//! Voice samples for voice cloning: isolate the vocals of a song (Demucs, installed
//! locally in `tools/venv`), cut each timed lyric line into its own clean clip, tell
//! spoken lines from sung ones by pitch analysis, and write a dataset that voice-cloning
//! tools accept (clips + `metadata.csv` + a 10–20 s reference clip with its text).

use crate::lyrics::Line;
use crate::media::ffmpeg;
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// The Python with Demucs: `RUSTCUT_DEMUCS_PYTHON`, else `tools/venv` next to the
/// executable's project folder or the working directory.
pub fn demucs_python() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("RUSTCUT_DEMUCS_PYTHON") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let rel = if cfg!(windows) { "tools/venv/Scripts/python.exe" } else { "tools/venv/bin/python" };
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        // …/MyVideoEditor/target/release/rustcut.exe → …/MyVideoEditor
        roots.extend(exe.ancestors().skip(1).take(4).map(Path::to_path_buf));
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    roots.into_iter().map(|r| r.join(rel)).find(|p| p.is_file())
}

/// Runs Demucs (two stems) and returns the isolated vocals WAV. Slow on a CPU
/// (several minutes per song). `Ok(None)` if cancelled.
pub fn separate_vocals(song: &Path, out_dir: &Path, cancel: &AtomicBool, progress: &mut dyn FnMut(f32, &str)) -> Result<Option<PathBuf>> {
    let py = demucs_python().ok_or_else(|| anyhow!("vocal separation isn't installed (tools/venv with Demucs)"))?;
    let stem = song.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "song".into());
    let vocals = out_dir.join("htdemucs").join(&stem).join("vocals.wav");
    if vocals.is_file() {
        return Ok(Some(vocals));
    }
    std::fs::create_dir_all(out_dir)?;
    progress(0.02, "Separating the vocals (several minutes)…");
    let mut child = ffmpeg::command(&py.to_string_lossy())
        .args(["-m", "demucs", "--two-stems=vocals", "-n", "htdemucs", "-o"])
        .arg(out_dir)
        .arg(song)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not start Demucs")?;
    // Demucs prints a tqdm bar on stderr: read percentages as they come.
    let mut log = String::new();
    if let Some(mut err) = child.stderr.take() {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        loop {
            if cancel.load(Ordering::Relaxed) {
                let _ = child.kill();
                return Ok(None);
            }
            match err.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&buf[..n]).into_owned();
                    if let Some(p) = s.rsplit('%').nth(1).and_then(|x| x.rsplit(|c: char| !c.is_ascii_digit()).next()).and_then(|x| x.parse::<f32>().ok()) {
                        progress(0.02 + 0.9 * (p / 100.0).min(1.0), "Separating the vocals…");
                    }
                    log.push_str(&s);
                    if log.len() > 20_000 {
                        log.drain(..10_000);
                    }
                }
            }
        }
    }
    let status = child.wait()?;
    if !status.success() || !vocals.is_file() {
        bail!("Demucs failed: {}", log.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output"));
    }
    Ok(Some(vocals))
}

/// Mono samples of a file at `rate`.
fn decode(path: &Path, rate: u32, from: f64, dur: f64) -> Result<Vec<f32>> {
    let out = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{from:.3}"), "-t", &format!("{dur:.3}"), "-i"])
        .arg(path)
        .args(["-vn", "-ac", "1", "-ar", &rate.to_string(), "-f", "s16le", "-"])
        .output()
        .context("failed to run ffmpeg")?;
    Ok(out.stdout.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect())
}

/// Pitch (Hz) per 20 ms frame, or None when unvoiced (simple normalised autocorrelation).
pub fn pitch_track(samples: &[f32], rate: u32) -> Vec<Option<f32>> {
    let frame = (rate as f32 * 0.04) as usize;
    let hop = (rate as f32 * 0.02) as usize;
    let (min_lag, max_lag) = ((rate as f32 / 1000.0) as usize, (rate as f32 / 70.0) as usize);
    let mut out = Vec::new();
    let mut i = 0;
    while i + frame + max_lag < samples.len() {
        let x = &samples[i..i + frame + max_lag];
        let energy: f32 = x[..frame].iter().map(|v| v * v).sum::<f32>() / frame as f32;
        if energy < 1e-4 {
            out.push(None);
            i += hop;
            continue;
        }
        let corr: Vec<f32> = (min_lag..max_lag)
            .map(|lag| {
                let (mut num, mut d1, mut d2) = (0.0f32, 0.0f32, 0.0f32);
                for k in 0..frame {
                    num += x[k] * x[k + lag];
                    d1 += x[k] * x[k];
                    d2 += x[k + lag] * x[k + lag];
                }
                num / (d1 * d2).sqrt().max(1e-9)
            })
            .collect();
        let peak = corr.iter().cloned().fold(0.0f32, f32::max);
        // The first local peak close to the best one is the true period (multiples score as high).
        let lag = (1..corr.len().saturating_sub(1)).find(|&k| corr[k] >= 0.9 * peak && corr[k] >= corr[k - 1] && corr[k] >= corr[k + 1]);
        out.push(lag.filter(|_| peak > 0.75).map(|k| rate as f32 / (k + min_lag) as f32));
        i += hop;
    }
    out
}

/// How much of the voiced time is held on steady notes (0 = speech-like glides, 1 = sustained singing).
pub fn sustain_ratio(pitch: &[Option<f32>]) -> f32 {
    let voiced = pitch.iter().filter(|p| p.is_some()).count();
    if voiced < 5 {
        return 0.0;
    }
    // Frames inside runs of ≥ 10 frames (200 ms) staying within ±40 cents of the run start.
    let mut held = 0usize;
    let mut k = 0;
    while k < pitch.len() {
        let Some(f0) = pitch[k] else {
            k += 1;
            continue;
        };
        let mut j = k + 1;
        while j < pitch.len() && pitch[j].is_some_and(|f| (1200.0 * (f / f0).log2()).abs() < 40.0) {
            j += 1;
        }
        if j - k >= 10 {
            held += j - k;
        }
        k = j.max(k + 1);
    }
    held as f32 / voiced as f32
}

#[derive(Clone, Debug, Serialize)]
pub struct Sample {
    pub file: String,
    pub text: String,
    pub start: f64,
    pub end: f64,
    /// "spoken" or "sung".
    pub kind: String,
    /// Sustained-pitch ratio (low = speech-like).
    pub sustain: f32,
    /// Share of the clip with voice in it.
    pub voiced: f32,
}

/// Cuts each lyric line (timeline = song time when the song starts at 0) from the vocals
/// into `out_dir/clips`, trimmed and levelled, and writes `metadata.csv`, `samples.json`
/// and `reference.wav`/`reference.txt` (the most speech-like clips, 10–20 s).
pub fn build_dataset(vocals: &Path, lines: &[Line], speaker: &str, out_dir: &Path, progress: &mut dyn FnMut(f32, &str)) -> Result<Vec<Sample>> {
    let clips = out_dir.join("clips");
    std::fs::create_dir_all(&clips)?;
    let slug = crate::ai::video_gen::slug(speaker);
    let mut samples = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        progress(i as f32 / lines.len().max(1) as f32, &format!("Cutting line {} of {}…", i + 1, lines.len()));
        let (a, b) = ((l.start - 0.12).max(0.0), l.end + 0.2);
        if b - a < 1.0 || b - a > 15.0 {
            continue; // too short to be useful, or a mis-timed line
        }
        let pcm = decode(vocals, 16000, a, b - a)?;
        let pitch = pitch_track(&pcm, 16000);
        let voiced = pitch.iter().filter(|p| p.is_some()).count() as f32 / pitch.len().max(1) as f32;
        if voiced < 0.2 {
            continue; // little voice in it (timing off, or backing music only)
        }
        let sustain = sustain_ratio(&pitch);
        let kind = if sustain < 0.35 { "spoken" } else { "sung" };
        let name = format!("{slug}_{:03}_{kind}.wav", i + 1);
        let path = clips.join(&name);
        let status = ffmpeg::command(&ffmpeg::ffmpeg_bin())
            .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &format!("{a:.3}"), "-t", &format!("{:.3}", b - a), "-i"])
            .arg(vocals)
            .args([
                "-af",
                "silenceremove=start_periods=1:start_threshold=-45dB:start_silence=0.05,areverse,silenceremove=start_periods=1:start_threshold=-45dB:start_silence=0.1,areverse,highpass=f=70,loudnorm=I=-20:TP=-2:LRA=11",
                "-ac", "1", "-ar", "44100", "-c:a", "pcm_s16le",
            ])
            .arg(&path)
            .status()?;
        if !status.success() {
            continue;
        }
        samples.push(Sample { file: format!("clips/{name}"), text: l.text.clone(), start: a, end: b, kind: kind.into(), sustain, voiced });
    }
    if samples.is_empty() {
        bail!("no usable vocal lines were found");
    }
    // metadata.csv in the common "file|text" layout (LJSpeech-style).
    let mut csv = String::new();
    for s in &samples {
        csv += &format!("{}|{}\n", s.file, s.text.replace('|', " ").replace('\n', " "));
    }
    std::fs::write(out_dir.join("metadata.csv"), csv)?;
    std::fs::write(out_dir.join("samples.json"), serde_json::to_string_pretty(&samples)?)?;
    // Reference: the most speech-like clips, joined up to ~18 s.
    let mut by_speech: Vec<&Sample> = samples.iter().collect();
    by_speech.sort_by(|x, y| x.sustain.total_cmp(&y.sustain));
    let mut chosen: Vec<&Sample> = Vec::new();
    let mut total = 0.0;
    for s in by_speech {
        if total >= 10.0 && total + (s.end - s.start) > 18.0 {
            break;
        }
        total += s.end - s.start;
        chosen.push(s);
        if total >= 18.0 {
            break;
        }
    }
    chosen.sort_by(|x, y| x.start.total_cmp(&y.start));
    let list = out_dir.join("reference-list.txt");
    std::fs::write(&list, chosen.iter().map(|s| format!("file '{}'\n", out_dir.join(&s.file).to_string_lossy().replace('\\', "/").replace('\'', "'\\''"))).collect::<String>())?;
    let _ = ffmpeg::command(&ffmpeg::ffmpeg_bin())
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "concat", "-safe", "0", "-i"])
        .arg(&list)
        .args(["-c:a", "pcm_s16le"])
        .arg(out_dir.join("reference.wav"))
        .status();
    let _ = std::fs::remove_file(&list);
    std::fs::write(out_dir.join("reference.txt"), chosen.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" "))?;
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freqs: &[(f32, f32)], rate: u32) -> Vec<f32> {
        // (frequency, seconds) segments
        let mut out = Vec::new();
        let mut phase = 0.0f32;
        for &(f, secs) in freqs {
            for _ in 0..(secs * rate as f32) as usize {
                phase += 2.0 * std::f32::consts::PI * f / rate as f32;
                out.push(0.5 * phase.sin() + 0.2 * (2.0 * phase).sin());
            }
        }
        out
    }

    /// Manual: build a voice dataset from isolated vocals + lyrics.
    /// `VOICE_VOCALS=vocals.wav VOICE_LYRICS=lyrics.txt VOICE_OUT=dir VOICE_SPEAKER=Name cargo test --lib voice_dataset -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn voice_dataset() {
        let (Ok(vocals), Ok(lyrics), Ok(out), Ok(speaker)) = (std::env::var("VOICE_VOCALS"), std::env::var("VOICE_LYRICS"), std::env::var("VOICE_OUT"), std::env::var("VOICE_SPEAKER")) else { return };
        let mut p = crate::model::Project::default();
        let m = p.add_media(ffmpeg::probe(Path::new(&vocals)).unwrap().into_media_item(vocals.clone().into()));
        let c = p.new_media_clip(m, 0.0).unwrap();
        let id = c.id;
        p.place_clip(c, None);
        let text = std::fs::read_to_string(&lyrics).unwrap();
        let cancel = AtomicBool::new(false);
        let lines = crate::lyrics::timed_lines(&p, id, &crate::lyrics::Input::Text(text), "en", &cancel, &mut |_, _| {}).unwrap().unwrap();
        println!("{} lines timed against the isolated vocals", lines.len());
        let samples = build_dataset(Path::new(&vocals), &lines, &speaker, Path::new(&out), &mut |_, _| {}).unwrap();
        let spoken = samples.iter().filter(|s| s.kind == "spoken").count();
        let secs: f64 = samples.iter().map(|s| s.end - s.start).sum();
        println!("{} clips ({spoken} spoken-like, {} sung), {:.0} s of voice", samples.len(), samples.len() - spoken, secs);
        for s in samples.iter().take(8) {
            println!("  {:6.1}-{:6.1} {:<6} sustain {:.2} voiced {:.2}  {}", s.start, s.end, s.kind, s.sustain, s.voiced, s.text);
        }
    }

    #[test]
    fn pitch_tracking_finds_the_note() {
        let p = pitch_track(&tone(&[(220.0, 0.5)], 16000), 16000);
        let voiced: Vec<f32> = p.iter().flatten().copied().collect();
        assert!(voiced.len() > 10);
        let mean = voiced.iter().sum::<f32>() / voiced.len() as f32;
        assert!((mean - 220.0).abs() < 8.0, "{mean}");
    }

    #[test]
    fn held_notes_read_as_singing_and_glides_as_speech() {
        let sung = tone(&[(220.0, 0.6), (247.0, 0.6), (262.0, 0.6)], 16000);
        assert!(sustain_ratio(&pitch_track(&sung, 16000)) > 0.6);
        // Speech-like: pitch keeps moving every 60 ms.
        let glide: Vec<(f32, f32)> = (0..30).map(|k| (150.0 + 40.0 * ((k as f32) * 1.3).sin(), 0.06)).collect();
        let spoken = tone(&glide, 16000);
        assert!(sustain_ratio(&pitch_track(&spoken, 16000)) < 0.35);
    }
}
