//! The user's Google account, shared by Google Drive, YouTube and Google Calendar.
//!
//! Sign-in uses Google's flow for desktop apps: the browser opens Google's consent page, Google
//! sends the browser back to a one-off listener on 127.0.0.1, and the code it carries is swapped
//! for a refresh token (with PKCE). The user supplies their own OAuth client (a "Desktop app"
//! client from Google Cloud Console). Permissions are asked for one feature at a time
//! (incremental authorization): Drive asks for Drive, YouTube for YouTube uploads, and so on,
//! and the one refresh token covers everything granted so far.
//!
//! Settings, including the refresh token, are kept in `google.json` in the config folder
//! (moved over from the older `gdrive.json` the first time).

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const USERINFO_URL: &str = "https://openidconnect.googleapis.com/v1/userinfo";
/// Asked for with every feature so the window can show which account is connected.
const EMAIL_SCOPES: [&str; 2] = ["openid", "email"];
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(300);

pub const SCOPE_DRIVE: &str = "https://www.googleapis.com/auth/drive.file";
pub const SCOPE_YOUTUBE: &str = "https://www.googleapis.com/auth/youtube.upload";
pub const SCOPE_CALENDAR: &str = "https://www.googleapis.com/auth/calendar.events";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Account {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    /// The signed-in account, shown in the windows.
    pub email: String,
    /// Every scope granted so far.
    pub scopes: Vec<String>,
    /// The RustCut folder RustCut made on the Drive.
    pub folder_id: String,
}

impl Account {
    pub fn has_client(&self) -> bool {
        !self.client_id.trim().is_empty() && !self.client_secret.trim().is_empty()
    }
    pub fn connected(&self) -> bool {
        self.has_client() && !self.refresh_token.is_empty()
    }
    /// Signed in and allowed to use `scope`.
    pub fn can(&self, scope: &str) -> bool {
        self.connected() && self.scopes.iter().any(|s| s == scope)
    }
}

/// The account from `google.json`, or from the older Drive-only `gdrive.json` (whose token
/// was granted Drive access only).
pub fn load() -> Account {
    load_in(&crate::util::config_dir())
}

pub fn save(a: &Account) -> Result<()> {
    save_in(&crate::util::config_dir(), a)
}

fn load_in(dir: &Path) -> Account {
    if let Some(a) = std::fs::read_to_string(dir.join("google.json")).ok().and_then(|s| serde_json::from_str(&s).ok()) {
        return a;
    }
    let old = dir.join("gdrive.json");
    match std::fs::read_to_string(&old).ok().and_then(|s| serde_json::from_str::<Account>(&s).ok()) {
        Some(mut a) => {
            if a.connected() && a.scopes.is_empty() {
                a.scopes = vec![SCOPE_DRIVE.to_string()];
            }
            if save_in(dir, &a).is_ok() {
                let _ = std::fs::remove_file(old);
            }
            a
        }
        None => Account::default(),
    }
}

fn save_in(dir: &Path, a: &Account) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("google.json"), serde_json::to_string_pretty(a)?).context("saving google.json")
}

/// Reads the client id and secret from the JSON file Google Cloud Console downloads for an
/// OAuth client (`{"installed": {...}}` for a desktop app).
pub fn parse_client_file(json: &str) -> Result<(String, String)> {
    let v: Value = serde_json::from_str(json).context("not a JSON file")?;
    if v.get("web").is_some() {
        bail!("this is a \"Web application\" client; create a \"Desktop app\" client instead");
    }
    let c = v.get("installed").unwrap_or(&v);
    let id = c.get("client_id").and_then(Value::as_str).unwrap_or_default();
    let secret = c.get("client_secret").and_then(Value::as_str).unwrap_or_default();
    if id.is_empty() || secret.is_empty() {
        bail!("no client_id / client_secret in this file — download the JSON of a \"Desktop app\" OAuth client");
    }
    Ok((id.to_string(), secret.to_string()))
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(Duration::from_secs(30))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(300))
        .build()
}

/// The message in a Google error body, without the rest of the response.
pub fn api_error(e: ureq::Error) -> anyhow::Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            anyhow!("Google returned HTTP {code}: {}", error_message(&body))
        }
        other => anyhow!("network error: {other}"),
    }
}

pub fn error_message(body: &str) -> String {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let msg = v["error"]["message"].as_str().or_else(|| v["error_description"].as_str()).or_else(|| v["error"].as_str()).unwrap_or("no details");
    let reason = v["error"]["errors"][0]["reason"].as_str().unwrap_or_default();
    if reason.is_empty() || msg.contains(reason) {
        msg.to_string()
    } else {
        format!("{msg} ({reason})")
    }
}

// ── sign-in ──────────────────────────────────────────────────────────────────

pub fn enc(s: &str) -> String {
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
            b'%' if i + 2 < b.len() => match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                Ok(v) => {
                    out.push(v);
                    i += 2;
                }
                Err(_) => out.push(b'%'),
            },
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

pub fn auth_url(client_id: &str, redirect: &str, scopes: &[&str], challenge: &str, state: &str) -> String {
    let mut all: Vec<&str> = EMAIL_SCOPES.to_vec();
    all.extend(scopes.iter().copied().filter(|s| !EMAIL_SCOPES.contains(s)));
    format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&include_granted_scopes=true&code_challenge={}&code_challenge_method=plain&state={}",
        enc(client_id),
        enc(redirect),
        enc(&all.join(" ")),
        enc(challenge),
        enc(state)
    )
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RustCut</title><body style=\"font:16px system-ui;margin:3em\"><h2>RustCut is connected to your Google account</h2><p>You can close this tab and go back to RustCut.</p>";
const FAIL_PAGE: &str = "<!doctype html><meta charset=utf-8><title>RustCut</title><body style=\"font:16px system-ui;margin:3em\"><h2>Google sign-in didn't finish</h2><p>Go back to RustCut to see why.</p>";

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

/// Signs in with the browser, asking for `scopes` on top of what was granted before, and
/// returns the account with a refresh token (also saved). `open_url` opens the consent page.
pub fn sign_in(client_id: &str, client_secret: &str, scopes: &[&str], open_url: &dyn Fn(&str), cancel: &AtomicBool) -> Result<Account> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").context("starting the sign-in listener")?;
    let redirect = format!("http://127.0.0.1:{}/", listener.local_addr()?.port());
    let verifier = random_token(64);
    let state = random_token(24);
    open_url(&auth_url(client_id, &redirect, scopes, &verifier, &state));
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
    let granted: Vec<String> = resp["scope"].as_str().unwrap_or_default().split_whitespace().map(str::to_string).collect();
    let missing: Vec<&str> = scopes.iter().copied().filter(|s| !granted.iter().any(|g| g == s)).collect();
    let mut a = load();
    let email = account_email(&access).unwrap_or_default();
    if !a.email.is_empty() && !email.is_empty() && a.email != email {
        a.folder_id.clear(); // a different account: its Drive has no RustCut folder yet
    }
    a.client_id = client_id.to_string();
    a.client_secret = client_secret.to_string();
    a.refresh_token = refresh.to_string();
    a.email = email;
    a.scopes = granted;
    save(&a)?;
    if !missing.is_empty() {
        bail!("Google didn't grant {} — tick every box on the consent page and sign in again", missing.iter().map(|s| scope_label(s)).collect::<Vec<_>>().join(", "));
    }
    Ok(a)
}

/// What a scope lets RustCut do, for messages.
pub fn scope_label(scope: &str) -> &'static str {
    match scope {
        SCOPE_DRIVE => "Google Drive (files RustCut uploads)",
        SCOPE_YOUTUBE => "YouTube uploads",
        SCOPE_CALENDAR => "Google Calendar events",
        _ => "a permission",
    }
}

fn account_email(token: &str) -> Result<String> {
    let v: Value = agent().get(USERINFO_URL).set("Authorization", &format!("Bearer {token}")).call().map_err(api_error)?.into_json()?;
    Ok(v["email"].as_str().unwrap_or_default().to_string())
}

/// Forgets the sign-in (and revokes it at Google, best effort). The client id stays.
pub fn sign_out() -> Result<Account> {
    let mut a = load();
    if !a.refresh_token.is_empty() {
        let _ = agent().post(REVOKE_URL).send_form(&[("token", a.refresh_token.as_str())]);
    }
    a.refresh_token.clear();
    a.email.clear();
    a.scopes.clear();
    a.folder_id.clear();
    save(&a)?;
    Ok(a)
}

/// A fresh access token for the saved sign-in.
pub fn access_token(a: &Account) -> Result<String> {
    if !a.connected() {
        bail!("not signed in to Google");
    }
    let r = agent().post(TOKEN_URL).send_form(&[
        ("client_id", a.client_id.as_str()),
        ("client_secret", a.client_secret.as_str()),
        ("refresh_token", a.refresh_token.as_str()),
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
                let mut a = load();
                a.refresh_token.clear();
                let _ = save(&a);
                bail!("the Google sign-in has expired or was revoked — sign in again");
            }
            bail!("Google refused the saved sign-in — check the client id and secret, then sign in again");
        }
        Err(e) => Err(api_error(e)),
    }
}

// ── resumable uploads (Drive and YouTube use the same protocol) ─────────────

/// Every chunk but the last must be a multiple of 256 KiB.
const CHUNK_UNIT: usize = 256 * 1024;
const CHUNK: usize = 32 * CHUNK_UNIT; // 8 MiB

/// The `Content-Range` header for bytes `[start, start + len)`; `total` is known on the last chunk.
pub fn content_range(start: u64, len: usize, total: Option<u64>) -> String {
    let total = total.map(|t| t.to_string()).unwrap_or_else(|| "*".into());
    if len == 0 {
        format!("bytes */{total}")
    } else {
        format!("bytes {}-{}/{total}", start, start + len as u64 - 1)
    }
}

/// How many bytes Google has from a 308's `Range: bytes=0-N` header.
pub fn committed(range: Option<&str>) -> u64 {
    range.and_then(|r| r.rsplit('-').next()).and_then(|n| n.trim().parse::<u64>().ok()).map(|n| n + 1).unwrap_or(0)
}

/// A resumable upload as a `Write`: bytes are buffered and sent in 8 MiB chunks, retried on
/// network trouble from where Google says it got to.
pub struct Resumable<'a> {
    session: String,
    account: &'a Account,
    token: String,
    buf: Vec<u8>,
    /// Bytes Google has confirmed (the buffer starts here).
    sent: u64,
    result: Option<Value>,
    cancel: &'a AtomicBool,
    progress: &'a mut dyn FnMut(u64),
}

impl<'a> Resumable<'a> {
    /// Starts a session at `init_url` (which has `uploadType=resumable`) with `metadata`.
    pub fn start(account: &'a Account, token: String, init_url: &str, metadata: &Value, mime: &str, size: Option<u64>, cancel: &'a AtomicBool, progress: &'a mut dyn FnMut(u64)) -> Result<Self> {
        let mut req = agent().post(init_url).set("Authorization", &format!("Bearer {token}")).set("X-Upload-Content-Type", mime);
        if let Some(n) = size {
            req = req.set("X-Upload-Content-Length", &n.to_string());
        }
        let resp = req.send_json(metadata.clone()).map_err(api_error)?;
        let session = resp.header("Location").ok_or_else(|| anyhow!("Google didn't start the upload"))?.to_string();
        Ok(Self { session, account, token, buf: Vec::with_capacity(CHUNK), sent: 0, result: None, cancel, progress })
    }

    /// Sends what's buffered (all of it on the last call, else whole 256 KiB units).
    fn send(&mut self, last: bool) -> Result<()> {
        let mut tries = 0;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            if tries > 20 {
                bail!("Google kept refusing the upload — try again later");
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
                    // A final chunk Google only partly took: send the rest.
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
                    self.token = access_token(self.account)?;
                }
                Err(ureq::Error::Status(404 | 410, _)) => bail!("Google dropped the upload session — start the upload again"),
                Err(e @ ureq::Error::Status(code, _)) if !(code == 429 || code >= 500) => return Err(api_error(e)),
                Err(e) => {
                    if tries >= 6 {
                        return Err(api_error(e));
                    }
                    std::thread::sleep(Duration::from_secs(1 << tries.min(5)));
                    self.resync();
                }
            }
            tries += 1;
        }
    }

    /// Asks Google how far it got and drops what it already has from the buffer.
    fn resync(&mut self) {
        let resp = agent().put(&self.session).set("Authorization", &format!("Bearer {}", self.token)).set("Content-Range", "bytes */*").send_bytes(&[]);
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
    }

    /// Sends the rest and returns the created resource.
    pub fn finish(mut self) -> Result<Value> {
        self.send(true)?;
        self.result.take().ok_or_else(|| anyhow!("Google didn't confirm the upload"))
    }

    pub fn abort(&self) {
        let _ = agent().delete(&self.session).set("Authorization", &format!("Bearer {}", self.token)).call();
    }

    /// The access token in use (refreshed during long uploads).
    pub fn token(&self) -> &str {
        &self.token
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_file_and_account() {
        let desktop = r#"{"installed":{"client_id":"123-abc.apps.googleusercontent.com","project_id":"p","client_secret":"GOCSPX-x","redirect_uris":["http://localhost"]}}"#;
        assert_eq!(parse_client_file(desktop).unwrap(), ("123-abc.apps.googleusercontent.com".into(), "GOCSPX-x".into()));
        assert!(parse_client_file(r#"{"web":{"client_id":"a","client_secret":"b"}}"#).is_err(), "web clients can't use the loopback flow");
        assert!(parse_client_file(r#"{"installed":{}}"#).is_err());
        assert!(parse_client_file("nope").is_err());

        let mut a = Account { client_id: "id".into(), client_secret: "secret".into(), ..Default::default() };
        assert!(a.has_client() && !a.connected() && !a.can(SCOPE_DRIVE));
        a.refresh_token = "r".into();
        a.scopes = vec![SCOPE_DRIVE.into()];
        assert!(a.connected() && a.can(SCOPE_DRIVE) && !a.can(SCOPE_YOUTUBE));
        let back: Account = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
        assert_eq!(back, a);
        assert_eq!(serde_json::from_str::<Account>("{}").unwrap(), Account::default());
    }

    #[test]
    fn old_drive_settings_move_over() {
        let dir = std::env::temp_dir().join(format!("rc-google-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("gdrive.json"), r#"{"client_id":"id","client_secret":"s","refresh_token":"r","email":"me@example.com","folder_id":"f"}"#).unwrap();
        let a = load_in(&dir);
        assert_eq!(a.scopes, vec![SCOPE_DRIVE.to_string()]);
        assert_eq!(a.folder_id, "f");
        assert!(dir.join("google.json").is_file() && !dir.join("gdrive.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn urls_and_callback_parsing() {
        assert_eq!(enc("a b/c:d"), "a%20b%2Fc%3Ad");
        assert_eq!(dec("a%20b%2Fc+d%"), "a b/c d%");
        let url = auth_url("id.apps", "http://127.0.0.1:5000/", &[SCOPE_YOUTUBE], "verif", "st8");
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5000%2F"));
        assert!(url.contains("scope=openid%20email%20https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fyoutube.upload"));
        assert!(url.contains("include_granted_scopes=true") && url.contains("access_type=offline") && url.contains("code_challenge=verif"));
        let target = "/?state=st8&code=4%2F0Ab_x-y&scope=email";
        assert_eq!(query_param(target, "code").as_deref(), Some("4/0Ab_x-y"));
        assert_eq!(query_param(target, "state").as_deref(), Some("st8"));
        assert_eq!(query_param("/favicon.ico", "code"), None);
        let a = random_token(64);
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token(64));
        assert_eq!(error_message(r#"{"error":{"message":"Quota exceeded","errors":[{"reason":"quotaExceeded"}]}}"#), "Quota exceeded (quotaExceeded)");
        assert_eq!(error_message(r#"{"error":"invalid_grant","error_description":"Bad Request"}"#), "Bad Request");
    }

    #[test]
    fn chunk_headers() {
        assert_eq!(content_range(0, CHUNK, None), "bytes 0-8388607/*");
        assert_eq!(content_range(CHUNK as u64, 10, Some(CHUNK as u64 + 10)), "bytes 8388608-8388617/8388618");
        assert_eq!(content_range(100, 0, Some(100)), "bytes */100");
        assert_eq!(committed(Some("bytes=0-8388607")), 8388608);
        assert_eq!(committed(None), 0);
        assert_eq!(CHUNK % CHUNK_UNIT, 0);
        assert_eq!(size_label(152_592_636), "153 MB");
        assert_eq!(size_label(2_400_000_000), "2.40 GB");
    }
}
