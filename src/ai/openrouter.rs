//! Minimal OpenRouter client (https://openrouter.ai), shared by all AI features.
//!
//! - The API key comes from `OPENROUTER_API_KEY`, else from the per-user settings
//!   file; it is never written to projects or logs.
//! - Chat requests can ask for JSON-schema structured output; replies are parsed
//!   defensively (code fences, providers that ignore `response_format`).
//! - Every call reports token usage and cost (`usage.cost`, in credits/USD).

use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

pub const BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_VISION_MODEL: &str = "google/gemini-3.1-flash-lite";
pub const DEFAULT_VIDEO_MODEL: &str = "google/veo-3.1-lite";
pub const DEFAULT_EDIT_MODEL: &str = "google/gemini-3.8-flash";
pub const KEY_ENV: &str = "OPENROUTER_API_KEY";

// ── settings ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    /// Stored key (the environment variable takes precedence).
    pub api_key: Option<String>,
    /// Model used for image understanding (auto-reframe, …).
    pub vision_model: String,
    /// Model used for video generation (B-roll, shot extension).
    pub video_model: String,
    /// Model that watches/reads the timeline to make editing decisions (rough cut).
    pub edit_model: String,
    /// Model for image generation (thumbnails, title cards, background fill).
    pub image_model: String,
    /// Local decision model in Ollama (offline rough cut, smart transitions).
    pub local_model: String,
    pub ollama_url: String,
    /// ElevenLabs key (ELEVENLABS_API_KEY takes precedence) and the character → voice map.
    pub elevenlabs_key: Option<String>,
    pub voices: Vec<super::elevenlabs::VoiceProfile>,
    pub tts_model: String,
    /// Estimated USD per ElevenLabs character, for the cost ledger.
    pub tts_usd_per_char: f64,
    /// fal.ai key for lip-sync (FAL_KEY takes precedence) and the model to use.
    pub fal_key: Option<String>,
    pub lipsync_model: String,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            api_key: None,
            vision_model: DEFAULT_VISION_MODEL.into(),
            video_model: DEFAULT_VIDEO_MODEL.into(),
            edit_model: DEFAULT_EDIT_MODEL.into(),
            image_model: super::image_gen::DEFAULT_IMAGE_MODEL.into(),
            local_model: super::local::DEFAULT_LOCAL_MODEL.into(),
            ollama_url: super::local::DEFAULT_OLLAMA_URL.into(),
            elevenlabs_key: None,
            voices: Vec::new(),
            tts_model: super::elevenlabs::DEFAULT_MODEL.into(),
            tts_usd_per_char: super::elevenlabs::DEFAULT_USD_PER_CHAR,
            fal_key: None,
            lipsync_model: "lipsync-2".into(),
        }
    }
}

impl AiSettings {
    fn path() -> PathBuf {
        util::config_dir().join("ai.json")
    }

    pub fn load() -> Self {
        std::fs::read_to_string(Self::path()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, serde_json::to_string_pretty(self)?).with_context(|| format!("writing {}", path.display()))
    }

    /// Where the key comes from, for display.
    pub fn key_source(&self) -> Option<&'static str> {
        if env_key().is_some() {
            Some("OPENROUTER_API_KEY environment variable")
        } else if self.api_key.as_deref().is_some_and(|k| !k.trim().is_empty()) {
            Some("RustCut settings")
        } else {
            None
        }
    }

    /// The key to use: environment first, then settings.
    pub fn api_key(&self) -> Option<String> {
        env_key().or_else(|| self.api_key.clone().filter(|k| !k.trim().is_empty()).map(|k| k.trim().to_string()))
    }
}

impl AiSettings {
    /// The ElevenLabs key: environment first, then settings.
    pub fn elevenlabs_key(&self) -> Option<String> {
        super::elevenlabs::env_key().or_else(|| self.elevenlabs_key.clone().filter(|k| !k.trim().is_empty()).map(|k| k.trim().to_string()))
    }

    /// The fal.ai key: environment first, then settings.
    pub fn fal_key(&self) -> Option<String> {
        super::lipsync::env_key().or_else(|| self.fal_key.clone().filter(|k| !k.trim().is_empty()).map(|k| k.trim().to_string()))
    }

    /// The voice profile for a character name (case-insensitive) or a voice id.
    pub fn voice(&self, name_or_id: &str) -> Option<super::elevenlabs::VoiceProfile> {
        let q = name_or_id.trim().to_lowercase();
        self.voices.iter().find(|v| v.name.to_lowercase() == q || v.voice_id == name_or_id.trim()).cloned()
    }
}

fn env_key() -> Option<String> {
    std::env::var(KEY_ENV).ok().map(|k| k.trim().to_string()).filter(|k| !k.is_empty())
}

pub const NO_KEY: &str = "No OpenRouter API key. Add one in AI → OpenRouter settings, or set the OPENROUTER_API_KEY environment variable.";

// ── client ───────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    /// Credits charged (≈ USD).
    pub cost: f64,
}

impl std::ops::AddAssign for Usage {
    fn add_assign(&mut self, o: Usage) {
        self.prompt_tokens += o.prompt_tokens;
        self.completion_tokens += o.completion_tokens;
        self.cost += o.cost;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    /// USD per million input tokens.
    pub prompt_per_m: f64,
    /// USD per million output tokens.
    pub completion_per_m: f64,
    pub structured_output: bool,
    /// Accepts video input (`video_url` parts).
    pub video_input: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct KeyInfo {
    pub label: String,
    pub usage: f64,
    pub limit_remaining: Option<f64>,
    pub is_free_tier: bool,
}

#[derive(Clone)]
pub struct Client {
    key: String,
    agent: ureq::Agent,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(20)).timeout(Duration::from_secs(180)).build()
}

/// Turns an HTTP error into a message a person can act on.
fn describe_error(status: u16, body: &str) -> String {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(300).collect());
    let hint = match status {
        401 => "the API key was rejected — check it in AI → OpenRouter settings",
        402 => "your OpenRouter account is out of credits",
        403 => "the request was refused (moderation or key restrictions)",
        404 => "model or endpoint not found — pick another model in AI → OpenRouter settings",
        408 | 504 => "the model timed out",
        429 => "rate limited — wait a moment and try again",
        s if s >= 500 => "OpenRouter or the model provider had a problem",
        _ => "request failed",
    };
    format!("OpenRouter: {hint} (HTTP {status}): {detail}")
}

impl Client {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into(), agent: agent() }
    }

    pub fn from_settings(settings: &AiSettings) -> Result<Self> {
        settings.api_key().map(Self::new).ok_or_else(|| anyhow!(NO_KEY))
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let url = format!("{BASE_URL}{path}");
        let mut last_err = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(1500 * attempt));
            }
            let res = self
                .agent
                .post(&url)
                .set("Authorization", &format!("Bearer {}", self.key))
                .set("HTTP-Referer", "https://github.com/rustcut/rustcut")
                .set("X-Title", "RustCut")
                .send_json(body.clone());
            match res {
                Ok(r) => return r.into_json::<Value>().context("OpenRouter returned unreadable JSON"),
                Err(ureq::Error::Status(code, r)) => {
                    last_err = describe_error(code, &r.into_string().unwrap_or_default());
                    // Retry only transient failures.
                    if !(code == 429 || code == 408 || code >= 500) {
                        break;
                    }
                }
                Err(e) => last_err = format!("OpenRouter: network error: {e}"),
            }
        }
        bail!(last_err)
    }

    /// POST without retries — for requests that start billable work (a retry could
    /// create a duplicate job).
    pub fn post_once(&self, path: &str, body: &Value) -> Result<Value> {
        let res = self
            .agent
            .post(&format!("{BASE_URL}{path}"))
            .set("Authorization", &format!("Bearer {}", self.key))
            .set("HTTP-Referer", "https://github.com/rustcut/rustcut")
            .set("X-Title", "RustCut")
            .send_json(body.clone());
        match res {
            Ok(r) => r.into_json::<Value>().context("OpenRouter returned unreadable JSON"),
            Err(ureq::Error::Status(code, r)) => bail!(describe_error(code, &r.into_string().unwrap_or_default())),
            Err(e) => bail!("OpenRouter: network error: {e}"),
        }
    }

    /// Authenticated GET returning JSON.
    pub fn get_json(&self, path: &str) -> Result<Value> {
        self.get(path, true)
    }

    /// Downloads an authenticated URL (absolute, or a path under the API) to a file.
    pub fn download(&self, url: &str, dest: &std::path::Path) -> Result<u64> {
        let url = if url.starts_with("http") { url.to_string() } else { format!("{BASE_URL}{url}") };
        let res = self.agent.get(&url).set("Authorization", &format!("Bearer {}", self.key)).call();
        let resp = match res {
            Ok(r) => r,
            Err(ureq::Error::Status(code, r)) => bail!(describe_error(code, &r.into_string().unwrap_or_default())),
            Err(e) => bail!("OpenRouter: network error: {e}"),
        };
        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part).with_context(|| format!("creating {}", part.display()))?;
        let n = std::io::copy(&mut resp.into_reader(), &mut file).context("download interrupted")?;
        drop(file);
        if n < 1024 {
            let _ = std::fs::remove_file(&part);
            bail!("OpenRouter: the downloaded video is empty ({n} bytes)");
        }
        std::fs::rename(&part, dest)?;
        Ok(n)
    }

    fn get(&self, path: &str, auth: bool) -> Result<Value> {
        let mut req = self.agent.get(&format!("{BASE_URL}{path}"));
        if auth {
            req = req.set("Authorization", &format!("Bearer {}", self.key));
        }
        match req.call() {
            Ok(r) => r.into_json::<Value>().context("OpenRouter returned unreadable JSON"),
            Err(ureq::Error::Status(code, r)) => bail!(describe_error(code, &r.into_string().unwrap_or_default())),
            Err(e) => bail!("OpenRouter: network error: {e}"),
        }
    }

    /// Checks the key and returns its label and credit usage.
    pub fn key_info(&self) -> Result<KeyInfo> {
        let v = self.get("/key", true)?;
        let d = &v["data"];
        Ok(KeyInfo {
            label: d["label"].as_str().unwrap_or("").to_string(),
            usage: d["usage"].as_f64().unwrap_or(0.0),
            limit_remaining: d["limit_remaining"].as_f64(),
            is_free_tier: d["is_free_tier"].as_bool().unwrap_or(false),
        })
    }

    /// Sends a chat completion and returns the assistant text plus usage.
    pub fn chat(&self, body: &Value) -> Result<(String, Usage)> {
        let v = self.post("/chat/completions", body)?;
        if let Some(err) = v.get("error") {
            bail!("OpenRouter: {}", err["message"].as_str().unwrap_or("model error"));
        }
        let content = &v["choices"][0]["message"]["content"];
        let text = match content {
            Value::String(s) => s.clone(),
            // Some providers return content parts.
            Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(""),
            _ => String::new(),
        };
        if text.trim().is_empty() {
            let reason = v["choices"][0]["finish_reason"].as_str().unwrap_or("unknown");
            bail!("OpenRouter: the model returned an empty reply (finish reason: {reason})");
        }
        let usage = parse_usage(&v["usage"]);
        crate::economics::record(crate::economics::Spend {
            kind: "llm",
            model: body["model"].as_str().unwrap_or("chat"),
            usd: usage.cost,
            units: (usage.prompt_tokens + usage.completion_tokens) as f64,
            unit: "tokens",
            ..Default::default()
        });
        Ok((text, usage))
    }

    /// Chat with a JSON-schema response format; returns the parsed JSON.
    pub fn chat_json(&self, model: &str, messages: Value, schema_name: &str, schema: Value) -> Result<(Value, Usage)> {
        let body = json!({
            "model": model,
            "messages": messages,
            "temperature": 0,
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": schema_name, "strict": true, "schema": schema },
            },
        });
        let (text, usage) = self.chat(&body)?;
        let value = parse_json_reply(&text).with_context(|| {
            format!("the model's reply was not valid JSON: {}", text.chars().take(200).collect::<String>())
        })?;
        Ok((value, usage))
    }

    /// Vision-capable models, cheapest first (public endpoint, no key needed).
    pub fn vision_models() -> Result<Vec<ModelInfo>> {
        let v = Client::new("").get("/models", false)?;
        let mut models: Vec<ModelInfo> = v["data"]
            .as_array()
            .map(|a| a.as_slice())
            .unwrap_or_default()
            .iter()
            .filter(|m| {
                m["architecture"]["input_modalities"].as_array().is_some_and(|i| i.iter().any(|x| x == "image"))
                    && m["architecture"]["output_modalities"].as_array().is_some_and(|o| o.iter().any(|x| x == "text"))
            })
            .filter_map(|m| {
                let price = |k: &str| m["pricing"][k].as_str().and_then(|p| p.parse::<f64>().ok());
                let id = m["id"].as_str()?.to_string();
                if id.ends_with(":batch") {
                    return None;
                }
                Some(ModelInfo {
                    name: m["name"].as_str().unwrap_or(&id).to_string(),
                    prompt_per_m: price("prompt")? * 1e6,
                    completion_per_m: price("completion").unwrap_or(0.0) * 1e6,
                    video_input: m["architecture"]["input_modalities"].as_array().is_some_and(|i| i.iter().any(|x| x == "video")),
                    structured_output: m["supported_parameters"]
                        .as_array()
                        .is_some_and(|p| p.iter().any(|x| x == "response_format" || x == "structured_outputs")),
                    id,
                })
            })
            .filter(|m| m.prompt_per_m >= 0.0)
            .collect();
        models.sort_by(|a, b| a.prompt_per_m.total_cmp(&b.prompt_per_m).then_with(|| a.id.cmp(&b.id)));
        Ok(models)
    }
}

fn parse_usage(u: &Value) -> Usage {
    Usage {
        prompt_tokens: u["prompt_tokens"].as_u64().unwrap_or(0),
        completion_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
        cost: u["cost"].as_f64().unwrap_or(0.0),
    }
}

/// Parses JSON from a model reply, tolerating ```json fences and surrounding prose.
pub fn parse_json_reply(text: &str) -> Result<Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Ok(v);
    }
    let unfenced = t
        .trim_start_matches("```json")
        .trim_start_matches("```JSON")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if let Ok(v) = serde_json::from_str(unfenced) {
        return Ok(v);
    }
    let (start, end) = (t.find('{'), t.rfind('}'));
    match (start, end) {
        (Some(s), Some(e)) if e > s => Ok(serde_json::from_str(&t[s..=e])?),
        _ => bail!("no JSON object found"),
    }
}

/// An `image_url` content part for a JPEG.
pub fn jpeg_part(jpeg: &[u8]) -> Value {
    json!({ "type": "image_url", "image_url": { "url": format!("data:image/jpeg;base64,{}", util::base64(jpeg)) } })
}

/// A `video_url` content part for an MP4 (base64 data URL).
pub fn mp4_part(mp4: &[u8]) -> Value {
    json!({ "type": "video_url", "video_url": { "url": format!("data:video/mp4;base64,{}", util::base64(mp4)) } })
}

pub fn text_part(text: impl Into<String>) -> Value {
    json!({ "type": "text", "text": text.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_fenced_and_wrapped_json() {
        assert_eq!(parse_json_reply(r#"{"a":1}"#).unwrap()["a"], 1);
        assert_eq!(parse_json_reply("```json\n{\"a\":2}\n```").unwrap()["a"], 2);
        assert_eq!(parse_json_reply("Sure! Here you go: {\"a\":3} Hope it helps").unwrap()["a"], 3);
        assert!(parse_json_reply("no json here").is_err());
    }

    #[test]
    fn errors_are_actionable() {
        let e = describe_error(401, r#"{"error":{"message":"No auth credentials found"}}"#);
        assert!(e.contains("rejected") && e.contains("No auth credentials found"));
        assert!(describe_error(402, "").contains("out of credits"));
        assert!(describe_error(503, "busy").contains("provider"));
    }

    #[test]
    fn usage_and_parts() {
        let u = parse_usage(&json!({ "prompt_tokens": 10, "completion_tokens": 2, "cost": 0.0001 }));
        assert_eq!(u, Usage { prompt_tokens: 10, completion_tokens: 2, cost: 0.0001 });
        let p = jpeg_part(&[0xFF, 0xD8]);
        assert_eq!(p["image_url"]["url"], "data:image/jpeg;base64,/9g=");
    }

    #[test]
    fn env_key_wins() {
        let s = AiSettings { api_key: Some("  sk-file  ".into()), ..Default::default() };
        if env_key().is_none() {
            assert_eq!(s.api_key().as_deref(), Some("sk-file"));
            assert_eq!(s.key_source(), Some("RustCut settings"));
        }
        assert_eq!(AiSettings::default().vision_model, DEFAULT_VISION_MODEL);
    }
}
