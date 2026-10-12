//! ElevenLabs: list the account's voices (including cloned ones), turn dialogue into
//! speech in a chosen voice, and make an instant voice clone from reference clips.
//! The key comes from `ELEVENLABS_API_KEY` or RustCut's AI settings.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const API: &str = "https://api.elevenlabs.io";
pub const KEY_ENV: &str = "ELEVENLABS_API_KEY";
pub const DEFAULT_MODEL: &str = "eleven_multilingual_v2";
/// Speech models offered in the editor (id, label).
pub const MODELS: &[(&str, &str)] = &[
    ("eleven_multilingual_v2", "Multilingual v2 (most natural)"),
    ("eleven_v3", "v3 (most expressive, supports [whispers] tags)"),
    ("eleven_turbo_v2_5", "Turbo v2.5 (fast)"),
    ("eleven_flash_v2_5", "Flash v2.5 (fastest, cheapest)"),
];
/// Estimated USD per character for the cost ledger (Creator plan ≈ $22 for 100k credits).
pub const DEFAULT_USD_PER_CHAR: f64 = 0.00022;

pub const NO_KEY: &str = "No ElevenLabs API key. Add one in AI → AI settings (ElevenLabs), or set ELEVENLABS_API_KEY.";

/// A character in a video and the ElevenLabs voice that speaks for them.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub name: String,
    pub voice_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Voice {
    pub voice_id: String,
    pub name: String,
    /// "cloned", "premade", "generated", "professional"…
    pub category: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Subscription {
    pub tier: String,
    pub used: u64,
    pub limit: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoiceSettings {
    pub stability: f32,
    pub similarity: f32,
    pub style: f32,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self { stability: 0.5, similarity: 0.8, style: 0.0 }
    }
}

pub fn env_key() -> Option<String> {
    std::env::var(KEY_ENV).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

pub struct Client {
    key: String,
    agent: ureq::Agent,
}

fn api_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, r) => {
            let body = r.into_string().unwrap_or_default();
            let msg = serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| v["detail"]["message"].as_str().or_else(|| v["detail"].as_str()).map(str::to_string))
                .unwrap_or_else(|| body.chars().take(300).collect());
            // Restricted keys: name the missing permission and where to enable it.
            if let Some(perm) = msg.split("permission ").nth(1).and_then(|r| r.split_whitespace().next()) {
                let what = match perm {
                    "voices_write" => "Voices → Write (needed to create voice clones)",
                    "voices_read" => "Voices → Read (needed to list your voices)",
                    "text_to_speech" => "Text to Speech → Access (needed to speak lines)",
                    "user_read" => "User → Read (needed to show your credits)",
                    other => other,
                };
                return anyhow!("your ElevenLabs key works but lacks a permission: {what}. In ElevenLabs → profile → API Keys, edit the key to allow it (or make a new key and save it in RustCut).");
            }
            match code {
                401 => anyhow!("ElevenLabs rejected the API key (401): {msg}"),
                402 | 429 => anyhow!("ElevenLabs quota or rate limit ({code}): {msg}"),
                _ => anyhow!("ElevenLabs error {code}: {msg}"),
            }
        }
        other => anyhow!("could not reach ElevenLabs: {other}"),
    }
}

impl Client {
    pub fn new(key: String) -> Self {
        let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(180)).build();
        Self { key, agent }
    }

    pub fn voices(&self) -> Result<Vec<Voice>> {
        let v: Value = self.agent.get(&format!("{API}/v1/voices")).set("xi-api-key", &self.key).call().map_err(api_error)?.into_json()?;
        Ok(parse_voices(&v))
    }

    pub fn subscription(&self) -> Result<Subscription> {
        let v: Value = self.agent.get(&format!("{API}/v1/user/subscription")).set("xi-api-key", &self.key).call().map_err(api_error)?.into_json()?;
        Ok(Subscription {
            tier: v["tier"].as_str().unwrap_or("").to_string(),
            used: v["character_count"].as_u64().unwrap_or(0),
            limit: v["character_limit"].as_u64().unwrap_or(0),
        })
    }

    /// Speech for `text` in `voice_id` as MP3 bytes.
    pub fn speak(&self, voice_id: &str, text: &str, model: &str, s: VoiceSettings) -> Result<Vec<u8>> {
        if text.trim().is_empty() {
            bail!("write the line to speak first");
        }
        let body = json!({
            "text": text,
            "model_id": model,
            "voice_settings": { "stability": s.stability, "similarity_boost": s.similarity, "style": s.style, "use_speaker_boost": true },
        });
        let resp = self
            .agent
            .post(&format!("{API}/v1/text-to-speech/{voice_id}?output_format=mp3_44100_128"))
            .set("xi-api-key", &self.key)
            .set("Accept", "audio/mpeg")
            .send_json(body)
            .map_err(api_error)?;
        let mut bytes = Vec::new();
        resp.into_reader().take(50_000_000).read_to_end(&mut bytes)?;
        if bytes.len() < 500 {
            bail!("ElevenLabs returned no audio");
        }
        Ok(bytes)
    }

    /// Instant voice clone from reference clips. Returns the new voice id.
    pub fn clone_voice(&self, name: &str, description: &str, files: &[PathBuf]) -> Result<String> {
        if files.is_empty() {
            bail!("pick at least one reference clip");
        }
        let boundary = format!("----rustcut{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let mut body: Vec<u8> = Vec::new();
        let mut field = |name: &str, value: &str| {
            body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n").as_bytes());
        };
        field("name", name);
        field("description", description);
        field("remove_background_noise", "false");
        for f in files {
            let data = std::fs::read(f).with_context(|| format!("reading {}", f.display()))?;
            let fname = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "sample.wav".into());
            let mime = if fname.to_lowercase().ends_with(".mp3") { "audio/mpeg" } else { "audio/wav" };
            body.extend(format!("--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{fname}\"\r\nContent-Type: {mime}\r\n\r\n").as_bytes());
            body.extend(data);
            body.extend(b"\r\n");
        }
        body.extend(format!("--{boundary}--\r\n").as_bytes());
        let v: Value = self
            .agent
            .post(&format!("{API}/v1/voices/add"))
            .set("xi-api-key", &self.key)
            .set("Content-Type", &format!("multipart/form-data; boundary={boundary}"))
            .send_bytes(&body)
            .map_err(api_error)?
            .into_json()?;
        v["voice_id"].as_str().map(str::to_string).ok_or_else(|| anyhow!("ElevenLabs did not return a voice id"))
    }
}

pub fn parse_voices(v: &Value) -> Vec<Voice> {
    let mut out: Vec<Voice> = v["voices"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|x| {
            Some(Voice {
                voice_id: x["voice_id"].as_str()?.to_string(),
                name: x["name"].as_str().unwrap_or("").to_string(),
                category: x["category"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();
    // Your own (cloned) voices first, then the rest by name.
    out.sort_by(|a, b| (a.category != "cloned", a.name.to_lowercase()).cmp(&(b.category != "cloned", b.name.to_lowercase())));
    out
}

/// Generates a line, saves it as MP3 in `dir`, and records the estimated cost.
pub fn speak_to_file(client: &Client, voice: &VoiceProfile, text: &str, model: &str, s: VoiceSettings, dir: &Path, usd_per_char: f64) -> Result<PathBuf> {
    let bytes = client.speak(&voice.voice_id, text, model, s)?;
    std::fs::create_dir_all(dir)?;
    let stem = format!("{}-{}", crate::ai::video_gen::slug(&voice.name), crate::ai::video_gen::slug(text));
    let mut path = dir.join(format!("{stem}.mp3"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{stem}-{n}.mp3"));
        n += 1;
    }
    std::fs::write(&path, bytes)?;
    let chars = text.chars().count() as f64;
    crate::economics::record(crate::economics::Spend {
        kind: "tts",
        model,
        usd: chars * usd_per_char,
        units: chars,
        unit: "characters",
        output: Some(&path),
        source: "estimate",
        note: "ElevenLabs credits converted at the plan's price per character",
    });
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PAID, manual: `EL_STAGE=speak|clone EL_OUT=dir [EL_TEXT=… EL_VOICE=Aishyla EL_CLONE_NAME=… EL_CLONE_FILE=…] cargo test --lib elevenlabs_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn elevenlabs_live() {
        let Ok(stage) = std::env::var("EL_STAGE") else { return };
        let mut settings = crate::ai::openrouter::AiSettings::load();
        let client = Client::new(settings.elevenlabs_key().expect("ElevenLabs key in RustCut settings"));
        match stage.as_str() {
            "speak" => {
                let who = std::env::var("EL_VOICE").unwrap();
                let profile = settings.voice(&who).expect("character");
                let text = std::env::var("EL_TEXT").unwrap();
                let t = std::time::Instant::now();
                let path = speak_to_file(&client, &profile, &text, &settings.tts_model, VoiceSettings::default(), Path::new(&std::env::var("EL_OUT").unwrap()), settings.tts_usd_per_char).unwrap();
                let info = crate::media::ffmpeg::probe(&path).unwrap();
                println!("spoke {} chars as {} in {:?}: {} ({:.1} s)", text.chars().count(), profile.name, t.elapsed(), path.display(), info.duration);
            }
            "clone" => {
                let name = std::env::var("EL_CLONE_NAME").unwrap();
                let file = std::path::PathBuf::from(std::env::var("EL_CLONE_FILE").unwrap());
                let id = client.clone_voice(&name, "Created with RustCut from voice reference clips", &[file]).unwrap();
                settings.voices.retain(|p| !p.name.eq_ignore_ascii_case(&name));
                settings.voices.push(VoiceProfile { name: name.clone(), voice_id: id.clone() });
                settings.save().unwrap();
                println!("created voice clone {name} ({} chars id)", id.len());
            }
            _ => {}
        }
        if let Ok(s) = client.subscription() {
            println!("credits: {} of {} used", s.used, s.limit);
        }
    }

    #[test]
    fn voices_parse_with_clones_first() {
        let v = json!({ "voices": [
            { "voice_id": "p1", "name": "Rachel", "category": "premade" },
            { "voice_id": "c1", "name": "Aishyla", "category": "cloned" },
            { "voice_id": "p2", "name": "Adam", "category": "premade" },
            { "name": "broken" }
        ]});
        let list = parse_voices(&v);
        assert_eq!(list.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["Aishyla", "Adam", "Rachel"]);
    }
}
