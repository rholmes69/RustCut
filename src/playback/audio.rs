//! Audio preview: one FFmpeg PCM pipe per audible clip, mixed by rodio.

use crate::media::ffmpeg::{atempo_chain, command, ffmpeg_bin};
use crate::model::{interpolate, Clip, Id, Keyframe, Project, Prop, TrackKind};
use rodio::{OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::HashSet;
use std::io::{BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Stdio};
use std::time::Duration;

const RATE: u32 = 44_100;
/// Clips are started this long before they become audible.
const LOOKAHEAD: f64 = 1.5;

/// Streams interleaved stereo f32 samples from FFmpeg, applying volume and fades.
struct PcmSource {
    child: Child,
    reader: BufReader<ChildStdout>,
    /// Clip-local time of the first sample.
    local0: f64,
    duration: f64,
    fade_in: f64,
    fade_out: f64,
    volume: f32,
    volume_keys: Vec<Keyframe>,
    index: u64,
    gain: f32,
}

impl PcmSource {
    fn gain_at(&self, local: f64) -> f32 {
        let mut g: f64 = 1.0;
        if self.fade_in > 0.0 {
            g = g.min(local / self.fade_in);
        }
        if self.fade_out > 0.0 {
            g = g.min((self.duration - local) / self.fade_out);
        }
        let volume = if self.volume_keys.is_empty() { self.volume } else { interpolate(&self.volume_keys, local).max(0.0) };
        g.clamp(0.0, 1.0) as f32 * volume
    }
}

impl Iterator for PcmSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        let mut buf = [0u8; 4];
        self.reader.read_exact(&mut buf).ok()?;
        // Re-evaluate fades/keyframes every 64 frames (~1.5 ms): smooth and cheap.
        if self.index % 128 == 0 {
            let local = self.local0 + (self.index / 2) as f64 / RATE as f64;
            self.gain = self.gain_at(local);
        }
        self.index += 1;
        Some(f32::from_le_bytes(buf) * self.gain)
    }
}

impl Source for PcmSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        2
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

impl Drop for PcmSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
pub struct AudioEngine {
    output: Option<(OutputStream, OutputStreamHandle)>,
    unavailable: bool,
    sinks: Vec<Sink>,
    started: HashSet<Id>,
    active: bool,
}

impl AudioEngine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn start(&mut self, project: &Project, t: f64) {
        self.stop();
        self.active = true;
        self.tick(project, t);
    }

    pub fn stop(&mut self) {
        for s in self.sinks.drain(..) {
            s.stop();
        }
        self.started.clear();
        self.active = false;
    }

    /// Starts any clip that becomes audible within the look-ahead window.
    pub fn tick(&mut self, project: &Project, t: f64) {
        if !self.active {
            return;
        }
        let mut to_start = Vec::new();
        for track in &project.tracks {
            if track.kind == TrackKind::Text || track.muted {
                continue;
            }
            for clip in &track.clips {
                if self.started.contains(&clip.id)
                    || clip.end() <= t
                    || clip.start - t > LOOKAHEAD
                    || (clip.volume <= 0.001 && !clip.is_animated(Prop::Volume))
                {
                    continue;
                }
                let Some(media) = clip.media_id().and_then(|m| project.media(m)) else { continue };
                if media.has_sound() {
                    to_start.push((clip.clone(), media.path.clone()));
                }
            }
        }
        for (clip, path) in to_start {
            self.started.insert(clip.id);
            self.spawn(&clip, &path, t);
        }
        self.sinks.retain(|s| !s.empty());
    }

    fn sink(&mut self) -> Option<Sink> {
        if self.output.is_none() && !self.unavailable {
            match OutputStream::try_default() {
                Ok(o) => self.output = Some(o),
                Err(_) => self.unavailable = true,
            }
        }
        let (_, handle) = self.output.as_ref()?;
        Sink::try_new(handle).ok()
    }

    fn spawn(&mut self, clip: &Clip, path: &Path, t: f64) {
        let Some(sink) = self.sink() else { return };
        let local0 = (t - clip.start).max(0.0);
        let delay = (clip.start - t).max(0.0);
        let src = clip.source_in + local0 * clip.speed;
        let src_len = (clip.duration - local0) * clip.speed;
        let mut cmd = command(&ffmpeg_bin());
        cmd.args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{src:.3}"), "-t", &format!("{src_len:.3}"), "-i"])
            .arg(path)
            .arg("-vn");
        let filters: Vec<String> =
            atempo_chain(clip.speed).into_iter().chain(clip.denoise.as_ref().map(crate::smart_audio::denoise_filter)).collect();
        if !filters.is_empty() {
            cmd.args(["-af", &filters.join(",")]);
        }
        cmd.args(["-f", "f32le", "-ac", "2", "-ar", &RATE.to_string(), "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else { return };
        let Some(stdout) = child.stdout.take() else { return };
        let (fade_in, fade_out) = clip.fades();
        let source = PcmSource {
            child,
            reader: BufReader::with_capacity(1 << 16, stdout),
            local0,
            duration: clip.duration,
            fade_in,
            fade_out,
            volume: clip.volume,
            volume_keys: clip.keyframes.get(&Prop::Volume).cloned().unwrap_or_default(),
            index: 0,
            gain: 1.0,
        };
        sink.append(source.delay(Duration::from_secs_f64(delay)));
        self.sinks.push(sink);
    }
}
