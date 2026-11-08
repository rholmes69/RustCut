//! Microphone capture for voice commands: records the default input device, shows a
//! level, stops by itself after the speaker goes quiet, and writes a 16 kHz mono WAV
//! for local Whisper (see `captions::transcribe_file`).

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Level (RMS) that counts as speech.
const SPEECH: f32 = 0.025;
/// Quiet this long after speaking ends the recording.
const TRAILING_SILENCE: Duration = Duration::from_millis(1300);
/// Give up waiting for speech after this long.
const NO_SPEECH: Duration = Duration::from_secs(6);
const MAX_LEN: Duration = Duration::from_secs(30);
/// Hands-free: a shorter pause ends an utterance, and utterances are capped.
const HF_SILENCE: Duration = Duration::from_millis(900);
const HF_MAX_LEN: Duration = Duration::from_secs(10);

pub struct Recorder {
    _stream: cpal::Stream,
    samples: Arc<Mutex<Vec<f32>>>,
    rate: u32,
    level: Arc<AtomicU32>,
    started: Instant,
    last_speech: Option<Instant>,
    /// Hands-free: wait for speech forever (keeping a short pre-roll) and cap each
    /// utterance at a few seconds, so a wake word is caught even over music.
    hands_free: bool,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        Self::open(false)
    }

    /// Always-on listening for a wake word (see `hands_free`).
    pub fn start_hands_free() -> Result<Self> {
        Self::open(true)
    }

    fn open(hands_free: bool) -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or_else(|| anyhow!("no microphone found"))?;
        let config = device.default_input_config().context("the microphone has no usable format")?;
        let rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let samples = Arc::new(Mutex::new(Vec::<f32>::with_capacity(rate as usize * 10)));
        let level = Arc::new(AtomicU32::new(0));
        let err = |e| eprintln!("microphone: {e}");
        macro_rules! build {
            ($t:ty, $conv:expr) => {{
                let (s, l) = (samples.clone(), level.clone());
                device.build_input_stream(
                    &config.clone().into(),
                    move |data: &[$t], _: &_| {
                        let conv = $conv;
                        let mut buf = s.lock().unwrap();
                        let mut sum = 0.0f32;
                        let mut n = 0;
                        for frame in data.chunks(channels.max(1)) {
                            let v = frame.iter().map(|x| conv(*x)).sum::<f32>() / frame.len() as f32;
                            sum += v * v;
                            n += 1;
                            buf.push(v);
                        }
                        if n > 0 {
                            l.store((sum / n as f32).sqrt().to_bits(), Ordering::Relaxed);
                        }
                    },
                    err,
                    None,
                )
            }};
        }
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => build!(f32, |x: f32| x),
            cpal::SampleFormat::I16 => build!(i16, |x: i16| x as f32 / 32768.0),
            cpal::SampleFormat::U16 => build!(u16, |x: u16| (x as f32 - 32768.0) / 32768.0),
            cpal::SampleFormat::I32 => build!(i32, |x: i32| x as f32 / 2147483648.0),
            f => return Err(anyhow!("unsupported microphone format {f:?}")),
        }
        .context("could not open the microphone")?;
        stream.play().context("could not start the microphone")?;
        Ok(Self { _stream: stream, samples, rate, level, started: Instant::now(), last_speech: None, hands_free })
    }

    /// Current input level, 0..1 (RMS).
    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn heard_speech(&self) -> bool {
        self.last_speech.is_some()
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Call every frame; true when the recording should end (speech then silence,
    /// nothing said for a while, or too long).
    pub fn should_stop(&mut self) -> bool {
        let now = Instant::now();
        if self.level() > SPEECH {
            self.last_speech = Some(now);
        }
        if self.hands_free {
            return match self.last_speech {
                Some(t) => now - t > HF_SILENCE || now - self.speech_began(t) > HF_MAX_LEN,
                None => {
                    // Nothing said yet: keep only a short pre-roll so the first word isn't clipped.
                    let keep = self.rate as usize / 2;
                    let mut buf = self.samples.lock().unwrap();
                    if buf.len() > keep * 4 {
                        let cut = buf.len() - keep;
                        buf.drain(..cut);
                    }
                    self.started = now;
                    false
                }
            };
        }
        match self.last_speech {
            Some(t) => now - t > TRAILING_SILENCE || self.elapsed() > MAX_LEN,
            None => self.elapsed() > NO_SPEECH,
        }
    }

    /// When the current utterance started (hands-free: `started` resets while idle).
    fn speech_began(&self, _last: Instant) -> Instant {
        self.started
    }

    /// Throws away what was recorded so far (e.g. the Copilot's own voice).
    pub fn discard(&mut self) {
        self.samples.lock().unwrap().clear();
        self.last_speech = None;
        self.started = Instant::now();
    }

    /// Stops recording and writes a 16 kHz mono WAV. Returns its length in seconds.
    pub fn finish(self, wav: &Path) -> Result<f64> {
        let samples = std::mem::take(&mut *self.samples.lock().unwrap());
        let rate = self.rate;
        drop(self);
        let pcm = resample_from(&samples, rate, 16_000);
        write_wav(wav, &pcm, 16_000)?;
        Ok(pcm.len() as f64 / 16_000.0)
    }
}

fn resample_from(x: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || x.is_empty() {
        return x.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = (x.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let p = i as f64 * ratio;
            let j = p as usize;
            let f = (p - j as f64) as f32;
            let a = x[j.min(x.len() - 1)];
            let b = x[(j + 1).min(x.len() - 1)];
            a + (b - a) * f
        })
        .collect()
}

fn write_wav(path: &Path, pcm: &[f32], rate: u32) -> Result<()> {
    let data_len = (pcm.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resamples_and_writes_a_wav() {
        let x: Vec<f32> = (0..48_000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        let y = resample_from(&x, 48_000, 16_000);
        assert_eq!(y.len(), 16_000);
        let p = std::env::temp_dir().join("rustcut-listen-test.wav");
        write_wav(&p, &y, 16_000).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().len(), 44 + 32_000);
    }

    /// Manual: `VOICE_WAV=cmd.wav cargo test --lib voice_live -- --ignored --nocapture`
    /// transcribes a recorded command with local Whisper and shows what the Copilot does with it.
    #[test]
    #[ignore]
    fn voice_live() {
        let Ok(wav) = std::env::var("VOICE_WAV") else { return };
        let model = crate::captions::MODELS.iter().map(|m| m.name).find(|m| crate::captions::is_downloaded(m)).map(crate::captions::model_path).expect("a downloaded whisper model");
        let engine = crate::captions::detect_engine().expect("a whisper engine");
        let secs = crate::media::ffmpeg::probe(Path::new(&wav)).unwrap().duration;
        let opts = crate::captions::Options { model, language: "en".into(), translate: false, max_len: None };
        let t0 = Instant::now();
        let text = crate::captions::transcribe_file(Path::new(&wav), secs, &opts, &engine, &std::sync::atomic::AtomicBool::new(false)).unwrap().unwrap();
        println!("heard {text:?} in {:.1} s → {:?}", t0.elapsed().as_secs_f64(), crate::ai::copilot::quick_command(&text));
    }
}
