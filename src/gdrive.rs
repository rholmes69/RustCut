//! Uploads to the user's Google Drive: exported videos and project packages (a project zip is
//! streamed straight to Drive, so it needs no free disk space).
//!
//! Sign-in uses Google's flow for desktop apps: the browser opens Google's consent page, Google
//! sends the browser back to a one-off listener on 127.0.0.1, and the code it carries is swapped
//! for a refresh token (with PKCE). The user supplies their own OAuth client (a "Desktop app"
//! client from Google Cloud Console). Only the `drive.file` scope is asked for: RustCut can see
//! and change only the files it uploaded, never the rest of the Drive.
//!
//! Settings, including the refresh token, are kept in `gdrive.json` in the config folder.

use crate::model::Project;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const API: &str = "https://www.googleapis.com/drive/v3";
const UPLOAD_API: &str = "https://www.googleapis.com/upload/drive/v3/files";
const SCOPE: &str = "https://www.googleapis.com/auth/drive.file";
/// Uploads go into this folder at the top of My Drive.
pub const FOLDER_NAME: &str = "RustCut";
const FOLDER_MIME: &str = "application/vnd.google-apps.folder";
/// Drive wants every chunk but the last to be a multiple of 256 KiB.
const CHUNK_UNIT: usize = 256 * 1024;
const CHUNK: usize = 32 * CHUNK_UNIT; // 8 MiB
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    /// The signed-in account, shown in the window.
    pub email: String,
    /// The RustCut folder RustCut made on the Drive.
    pub folder_id: String,
}

impl Settings {
    pub fn has_client(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
    pub fn connected(&self) -> bool {
        self.has_client() && !self.refresh_token.is_empty()
    }
}

fn file() -> PathBuf {
    crate::util::config_dir().join("gdrive.json")
}

pub fn load() -> Settings {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

pub fn save(s: &Settings) -> Result<()> {
    std::fs::create_dir_all(crate::util::config_dir())?;
    std::fs::write(file(), serde_json::to_string_pretty(s)?).context("saving gdrive.json")
}

/// Reads the client id and secret from the JSON file Google Cloud Console downloads for an
/// OAuth client (`{"installed": {...}}` for a desktop app).
pub fn parse_client_file(json: &str) -> Result<(String, String)> {
    let v: Value = serde_json::from_str(json).context("not a JSON file")?;
    let c = v.get("installed").or_else(|| v.get("web")).unwrap_or(&v);
    let id = c.get("client_id").and_then(Value::as_str).unwrap_or_default();
    let secret = c.get("client_secret").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() || secret.is_empty() {
        bail!("no client_id / client_secret in this file — download the JSON of a \"Desktop app\" OAuth client");
    }
    if v.get("web").is_some() {
        bail!("this is a \"Web application\" client; create a \"Desktop app\" client instead");
    }
    Ok((id.to_string(), secret.to_string()))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(300))
        .build()
}

/// The message in a Google error body, without the rest of the response.
fn api_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
            let msg = v["error"]["message"]
                .as_str()
                .or_else(|| v["error_description"].as_str())
                .or_else(|| v["error"].as_str())
                .unwrap_or("no details")
                .to_string();
            anyhow!("Google returned HTTP {code}: {msg}")
        }
        other => anyhow!("network error: {other}"),
    }
}

// ── sign-in ──────────────────────────────────────────────────────────────────

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `key=value&…` from a request target such as `/?code=…&state=…`.
fn query_param(target: &str, key: &str) -> Option<String> {
    let q = target.split_once('?')?.1;
    q.split('&').filter_map(|kv| kv.split_once('=')).find(|(k, _)| *k == key).map(|(_, v)| dec(v))
}

/// A random string from the PKCE alphabet (std's `RandomState` is seeded from the OS).
fn random_token(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    (0..len)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_usize(i);
            h.write_u128(nanos);
            ALPHABET[(h.finish() % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

pub fn auth_url(client_id: &str, redirect: &str, challenge: &str, state: &str) -> String {
    format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&code_challenge={}&code_challenge_method=plain&state={}",
        enc(client_id),
        enc(redirect),
        enc(SCOPE),
        enc(challenge),
        enc(state)
    )
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RustCut</title><body style=\"font:16px system-ui;margin:3em\"><h2>RustCut is connected to Google Drive</h2><p>You can close this tab and go back to RustCut.</p>";
const FAIL_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RustCut</title><body style=\"font:16px system-ui;margin:3em\"><h2>Google Drive sign-in didn't finish</h2><p>Go back to RustCut to see why.</p>";

/// Waits for Google to send the browser back with `?code=` (or `?error=`).
fn wait_for_code(listener: &std::net::TcpListener, state: &str, cancel: &AtomicBool) -> Result<String> {
    listener.set_nonblocking(true)?;
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            bail!("cancelled");
        }
        if started.elapsed() > SIGN_IN_TIMEOUT {
            bail!("sign-in timed out after 5 minutes — try again");
        }
        let (mut stream, _) = match listener.accept() {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut req = Vec::new();
        let mut buf = [0u8; 2048];
        while !req.windows(4).any(|w| w == b"\r\n\r\n") && req.len() < 16 * 1024 {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => req.extend_from_slice(&buf[..n]),
            }
        }
        let text = String::from_utf8_lossy(&req);
        let target = text.lines().next().and_then(|l| l.split_whitespace().nth(1)).unwrap_or("").to_string();
        let reply = |stream: &mut std::net::TcpStream, page: &str| {
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}", page.len());
        };
        if let Some(err) = query_param(&target, "error") {
            reply(&mut stream, FAIL_PAGE);
            bail!(if err == "access_denied" { "sign-in was cancelled in the browser".to_string() } else { format!("Google refused the sign-in: {err}") });
        }
        match query_param(&target, "code") {
            Some(code) => {
                if query_param(&target, "state").as_deref() != Some(state) {
                    reply(&mut stream, FAIL_PAGE);
                    bail!("the sign-in reply didn't match this request — try again");
                }
                reply(&mut stream, DONE_PAGE);
                return Ok(code);
            }
            // Browsers also ask for /favicon.ico and the like.
            None => {
                let _ = write!(stream, "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            }
        }
    }
}

/// Signs in with the browser and returns the settings with a refresh token (also saved).
/// `open_url` opens the consent page; the UI also shows it as a link.
pub fn sign_in(client_id: &str, client_secret: &str, open_url: &dyn Fn(&str), cancel: &AtomicBool) -> Result<Settings> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("starting the sign-in listener")?;
    let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let verifier = random_token(64);
    let state = random_token(24);
    open_url(&auth_url(client_id, &redirect, &verifier, &state));
    let code = wait_for_code(&listener, &state, cancel)?;
    let resp: Value = agent()
        .post(TOKEN_URL)
        .send_form(&[
            ("code", code.as_str()),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verifier.as_str()),
        ])
        .map_err(api_error)?
        .into_json()?;
    let refresh = resp["refresh_token"].as_str().ok_or_else(|| anyhow!("Google didn't return a refresh token"))?;
    let access = resp["access_token"].as_str().unwrap_or_default().to_string();
    let mut s = load();
    s.client_id = client_id.to_string();
    s.client_secret = client_secret.to_string();
    s.refresh_token = refresh.to_string();
    s.email = account_email(&access).unwrap_or_default();
    s.folder_id.clear(); // another account may have been used before
    save(&s)?;
    Ok(s)
}

fn account_email(token: &str) -> Result<String> {
    let v: Value = agent()
        .get(&format!("{API}/about?fields=user(emailAddress)"))
        .set("Authorization", &format!("Bearer {token}"))
        .call()
        .map_err(api_error)?
        .into_json()?;
    Ok(v["user"]["emailAddress"].as_str().unwrap_or_default().to_string())
}

/// Forgets the sign-in (and revokes it at Google, best effort). The client id stays.
pub fn sign_out() -> Result<Settings> {
    let mut s = load();
    if !s.refresh_token.is_empty() {
        let _ = agent().post(REVOKE_URL).send_form(&[("token", s.refresh_token.as_str())]);
    }
    s.refresh_token.clear();
    s.email.clear();
    s.folder_id.clear();
    save(&s)?;
    Ok(s)
}

fn access_token(s: &Settings) -> Result<String> {
    if !s.connected() {
        bail!("not signed in to Google Drive");
    }
    let r = agent().post(TOKEN_URL).send_form(&[
        ("client_id", s.client_id.as_str()),
        ("client_secret", s.client_secret.as_str()),
        ("refresh_token", s.refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ]);
    match r {
        Ok(resp) => {
            let v: Value = resp.into_json()?;
            v["access_token"].as_str().map(str::to_string).ok_or_else(|| anyhow!("Google didn't return an access token"))
        }
        Err(ureq::Error::Status(400 | 401, resp)) => {
            let body = resp.into_string().unwrap_or_default();
            if body.contains("invalid_grant") {
                let mut s = load();
                s.refresh_token.clear();
                let _ = save(&s);
                bail!("the Google sign-in has expired or was revoked — sign in again (File → Google Drive…)");
            }
            bail!("Google refused the saved sign-in — check the client id and secret, then sign in again");
        }
        Err(e) => Err(api_error(e)),
    }
}

/// The RustCut folder's id, made on first use. With the `drive.file` scope only folders this
/// app made are visible, so a user's own folder called RustCut is never touched.
fn ensure_folder(s: &mut Settings, token: &str) -> Result<String> {
    let auth = format!("Bearer {token}");
    if !s.folder_id.is_empty() {
        let ok = agent()
            .get(&format!("{API}/files/{}?fields=id,trashed", enc(&s.folder_id)))
            .set("Authorization", &auth)
            .call()
            .ok()
            .and_then(|r| r.into_json::<Value>().ok())
            .map(|v| v["trashed"].as_bool() == Some(false))
            .unwrap_or(false);
        if ok {
            return Ok(s.folder_id.clone());
        }
    }
    let q = format!("name = '{FOLDER_NAME}' and mimeType = '{FOLDER_MIME}' and trashed = false and 'root' in parents");
    let found: Value = agent()
        .get(&format!("{API}/files?q={}&fields=files(id)&spaces=drive", enc(&q)))
        .set("Authorization", &auth)
        .call()
        .map_err(api_error)?
        .into_json()?;
    let id = match found["files"][0]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let made: Value = agent()
                .post(&format!("{API}/files?fields=id"))
                .set("Authorization", &auth)
                .send_json(serde_json::json!({ "name": FOLDER_NAME, "mimeType": FOLDER_MIME }))
                .map_err(api_error)?
                .into_json()?;
            made["id"].as_str().ok_or_else(|| anyhow!("Drive didn't return the new folder's id"))?.to_string()
        }
    };
    s.folder_id = id.clone();
    let _ = save(s);
    Ok(id)
}

// ── uploads ──────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct Uploaded {
    pub name: String,
    pub size: u64,
    /// The file's page on drive.google.com.
    pub link: String,
}

pub fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref() {
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        Some("mkv") => "video/x-matroska",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// Bytes as "153 MB".
pub fn size_label(n: u64) -> String {
    let mb = n as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.2} GB", mb / 1000.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        format!("{mb:.1} MB")
    }
}

/// The `Content-Range` header for bytes `[start, start + len)`; `total` is known on the last chunk.
fn content_range(start: u64, len: usize, total: Option<u64>) -> String {
    let total = total.map(|t| t.to_string()).unwrap_or_else(|| "*".into());
    if len == 0 {
        format!("bytes */{total}")
    } else {
        format!("bytes {}-{}/{total}", start, start + len as u64 - 1)
    }
}

/// How many bytes Drive has from a 308's `Range: bytes=0-N` header.
fn committed(range: Option<&str>) -> u64 {
    range.and_then(|r| r.rsplit('-').next()).and_then(|n| n.trim().parse::<u64>().ok()).map(|n| n + 1).unwrap_or(0)
}

/// A Drive resumable upload as a `Write`: bytes are buffered and sent in 8 MiB chunks, retried
/// on network trouble from where Drive says it got to.
struct Resumable<'a> {
    session: String,
    settings: &'a Settings,
    token: String,
    buf: Vec<u8>,
    /// Bytes Drive has confirmed (the buffer starts here).
    sent: u64,
    result: Option<Value>,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(u64),
}

impl<'a> Resumable<'a> {
    fn start(settings: &'a Settings, name: &str, mime: &str, size: Option<u64>, cancel: &'a AtomicBool, progress: &'a mut dyn FnMut(u64)) -> Result<Self> {
        let mut s = settings.clone();
        let token = access_token(&s)?;
        let folder = ensure_folder(&mut s, &token)?;
        let mut req = agent()
            .post(&format!("{UPLOAD_API}?uploadType=resumable&fields=id,name,size,webViewLink"))
            .set("Authorization", &format!("Bearer {token}"))
            .set("X-Upload-Content-Type", mime);
        if let Some(n) = size {
            req = req.set("X-Upload-Content-Length", &n.to_string());
        }
        let resp = req.send_json(serde_json::json!({ "name": name, "parents": [folder] })).map_err(api_error)?;
        let session = resp.header("Location").ok_or_else(|| anyhow!("Drive didn't start the upload"))?.to_string();
        Ok(Self { session, settings, token, buf: Vec::with_capacity(CHUNK), sent: 0, result: None, cancel, progress })
    }

    /// Sends what's buffered (all of it on the last call, else whole 256 KiB units).
    fn send(&mut self, last: bool) -> Result<()> {
        let mut tries = 0;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            if tries > 20 {
                bail!("Drive kept refusing the upload — try again later");
            }
            let len = if last { self.buf.len() } else { self.buf.len() / CHUNK_UNIT * CHUNK_UNIT };
            if len == 0 && !last {
                return Ok(());
            }
            let total = last.then(|| self.sent + self.buf.len() as u64);
            let r = agent()
                .put(&self.session)
                .set("Authorization", &format!("Bearer {}", self.token))
                .set("Content-Range", &content_range(self.sent, len, total))
                .send_bytes(&self.buf[..len]);
            match r {
                Ok(resp) if resp.status() == 308 => {
                    let got = committed(resp.header("Range")).max(self.sent);
                    let used = ((got - self.sent) as usize).min(self.buf.len());
                    self.buf.drain(..used);
                    self.sent = got;
                    (self.progress)(self.sent);
                    if !last {
                        return Ok(());
                    }
                    // A final chunk Drive only partly took: send the rest.
                }
                Ok(resp) => {
                    self.sent += self.buf.len() as u64;
                    self.buf.clear();
                    (self.progress)(self.sent);
                    self.result = Some(resp.into_json()?);
                    return Ok(());
                }
                Err(ureq::Error::Status(401, _)) if tries < 3 => {
                    // The hour-long access token ran out during a big upload.
                    self.token = access_token(self.settings)?;
                }
                Err(ureq::Error::Status(404 | 410, _)) => bail!("Drive dropped the upload session — start the upload again"),
                Err(e @ ureq::Error::Status(code, _)) if !(code == 429 || code >= 500) => return Err(api_error(e)),
                Err(e) => {
                    if tries >= 6 {
                        return Err(api_error(e));
                    }
                    std::thread::sleep(Duration::from_secs(1 << tries.min(5)));
                    self.resync()?;
                }
            }
            tries += 1;
        }
    }

    /// Asks Drive how far it got and drops what it already has from the buffer.
    fn resync(&mut self) -> Result<()> {
        let resp = agent()
            .put(&self.session)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Content-Range", "bytes */*")
            .send_bytes(&[]);
        if let Ok(resp) = resp {
            if resp.status() == 308 {
                let got = committed(resp.header("Range"));
                if got > self.sent {
                    let used = ((got - self.sent) as usize).min(self.buf.len());
                    self.buf.drain(..used);
                    self.sent = got;
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Value> {
        self.send(true)?;
        self.result.take().ok_or_else(|| anyhow!("Drive didn't confirm the upload"))
    }

    fn abort(&self) {
        let _ = agent().delete(&self.session).set("Authorization", &format!("Bearer {}", self.token)).call();
    }
}

impl Write for Resumable<'_> {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= CHUNK {
            self.send(false).map_err(|e| std::io::Error::other(format!("{e:#}")))?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn uploaded(v: &Value, fallback_name: &str, size: u64) -> Uploaded {
    Uploaded {
        name: v["name"].as_str().unwrap_or(fallback_name).to_string(),
        size,
        link: v["webViewLink"].as_str().map(str::to_string).unwrap_or_else(|| format!("https://drive.google.com/file/d/{}/view", v["id"].as_str().unwrap_or_default())),
    }
}

/// Uploads a file (e.g. an exported video) to the RustCut folder. `Ok(None)` when cancelled.
pub fn upload_file(path: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(f32, &str)) -> Result<Option<Uploaded>> {
    let s = load();
    let size = std::fs::metadata(path).with_context(|| format!("reading {}", path.display()))?.len();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "video.mp4".into());
    report(0.0, "Connecting to Google Drive…");
    let mut progress = |n: u64| report(if size > 0 { n as f32 / size as f32 } else { 1.0 }, &format!("Uploading {} of {}", size_label(n), size_label(size)));
    let mut up = Resumable::start(&s, &name, mime_for(path), Some(size), cancel, &mut progress)?;
    let mut f = std::fs::File::open(path)?;
    if let Err(e) = std::io::copy(&mut f, &mut up) {
        up.abort();
        return if cancel.load(Ordering::Relaxed) { Ok(None) } else { Err(e.into()) };
    }
    match up.finish() {
        Ok(v) => Ok(Some(uploaded(&v, &name, size))),
        Err(_) if cancel.load(Ordering::Relaxed) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Packs the project and all its media as a zip straight into the RustCut folder, with no
/// local temporary file. Returns the upload and how many media files were missing.
pub fn upload_project_zip(project: &Project, name: &str, cancel: &AtomicBool, report: &mut dyn FnMut(f32, &str)) -> Result<Option<(Uploaded, usize)>> {
    let s = load();
    let total = crate::package::media_bytes(project).max(1);
    report(0.0, "Connecting to Google Drive…");
    let sent = std::cell::Cell::new(0u64);
    let mut upload_progress = |n: u64| sent.set(n);
    let mut up = Resumable::start(&s, name, "application/zip", None, cancel, &mut upload_progress)?;
    let mut packed = |n: u64| report((n as f32 / total as f32).min(0.999), &format!("Packing and uploading {} of {}", size_label(n), size_label(total)));
    let keep_going = || !cancel.load(Ordering::Relaxed);
    let res = crate::package::stream_zip(project, &mut up, &mut packed, &keep_going);
    let pack = match res {
        Ok(r) => r,
        Err(e) => {
            up.abort();
            return if cancel.load(Ordering::Relaxed) { Ok(None) } else { Err(e) };
        }
    };
    let v = match up.finish() {
        Ok(v) => v,
        Err(_) if cancel.load(Ordering::Relaxed) => return Ok(None),
        Err(e) => return Err(e),
    };
    let size = v["size"].as_str().and_then(|n| n.parse().ok()).unwrap_or(sent.get());
    Ok(Some((uploaded(&v, name, size), pack.missing.len())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_file_and_settings() {
        let desktop = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"p","client_secret":"GOCSPX-x","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(parse_client_file(desktop).unwrap(), ("123-abc.apps.googleusercontent.com".into(), "GOCSPX-x".into()));
        assert!(parse_client_file(r#"{"web":{"client_id":"a","client_secret":"b"}}"#).is_err(), "web clients can't use the loopback flow");
        assert!(parse_client_file(r#"{"installed":{}}"#).is_err());
        assert!(parse_client_file("nope").is_err());

        let mut s = Settings { client_id: "id".into(), client_secret: "secret".into(), ..Default::default() };
        assert!(s.has_client() && !s.connected());
        s.refresh_token = "r".into();
        assert!(s.connected());
        let back: Settings = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
        assert_eq!(serde_json::from_str::<Settings>("{}").unwrap(), Settings::default());
    }

    #[test]
    fn urls_and_callback_parsing() {
        assert_eq!(enc("a b/c:d"), "a%20b%2Fc%3Ad");
        assert_eq!(dec("a%20b%2Fc+d%"), "a b/c d%");
        let url = auth_url("id.apps", "http://127.0.0.1:5000/", "verif", "st8");
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2F"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file"));
        assert!(url.contains("access_type=offline") && url.contains("code_challenge=verif"));
        let target = "/?state=st8&code=4%2F0Ab_x-y&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.file";
        assert_eq!(query_param(target, "code").as_deref(), Some("4/0Ab_x-y"));
        assert_eq!(query_param(target, "state").as_deref(), Some("st8"));
        assert_eq!(query_param("/favicon.ico", "code"), None);
        let a = random_token(64);
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token(64));
    }

    #[test]
    fn chunk_headers() {
        assert_eq!(content_range(0, CHUNK, None), "bytes 0-8388607/*");
        assert_eq!(content_range(CHUNK as u64, 10, Some(CHUNK as u64 + 10)), "bytes 8388608-8388617/8388618");
        assert_eq!(content_range(100, 0, Some(100)), "bytes */100");
        assert_eq!(committed(Some("bytes=0-8388607")), 8388608);
        assert_eq!(committed(None), 0);
        assert_eq!(CHUNK % CHUNK_UNIT, 0);
        assert_eq!(mime_for(Path::new("a/B.MP4")), "video/mp4");
        assert_eq!(size_label(152_592_636), "153 MB");
        assert_eq!(size_label(2_400_000_000), "2.40 GB");
    }

    /// Real sign-in and a small upload: `GDRIVE_LIVE=<file> cargo test --lib gdrive_live -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn gdrive_live() {
        let path = PathBuf::from(std::env::var("GDRIVE_LIVE").expect("GDRIVE_LIVE=<file to upload>"));
        let cancel = AtomicBool::new(false);
        let r = upload_file(&path, &cancel, &mut |p, s| eprintln!("{:3.0}% {s}", p * 100.0)).unwrap().unwrap();
        eprintln!("uploaded {} ({}) → {}", r.name, size_label(r.size), r.link);
    }
}
