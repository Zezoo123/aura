//! Spotify OAuth (Authorization Code with PKCE). Tokens live in
//! `~/.config/aura/auth.json` with owner-only permissions.

use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::OpenOptionsExt,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::{config_dir, Config};

pub const SCOPES: &[&str] = &[
    "user-read-playback-state",
    "user-modify-playback-state",
    "user-read-currently-playing",
    "user-read-recently-played",
    "playlist-read-private",
    "playlist-read-collaborative",
    "user-library-read",
    "user-library-modify",
    "user-top-read",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when `access_token` stops working.
    pub expires_at: u64,
    pub scope: String,
    pub client_id: String,
}

fn tokens_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("auth.json"))
}

pub fn load_tokens() -> Result<Option<Tokens>> {
    let path = tokens_path()?;
    match fs::read(&path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes).context("parsing auth.json")?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).context("reading auth.json"),
    }
}

pub fn save_tokens(t: &Tokens) -> Result<()> {
    let path = tokens_path()?;
    fs::create_dir_all(path.parent().unwrap())?;
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    f.write_all(&serde_json::to_vec_pretty(t)?)?;
    Ok(())
}

pub fn forget_tokens() -> Result<bool> {
    let path = tokens_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn base64url(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() + 2) / 3 * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf)
}

pub fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() + 0 && i + 2 <= bytes.len() - 1 => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    scope: String,
}

/// Interactive login: opens the browser, waits for the redirect on the loopback port,
/// exchanges the code, and saves tokens. `status` receives progress messages.
pub fn login(config: &Config, status: &mut dyn FnMut(&str)) -> Result<Tokens> {
    let client_id = config
        .client_id
        .clone()
        .ok_or_else(|| anyhow!("no client_id configured (run `aura login --client-id <ID>`)"))?;
    let redirect_uri = config.redirect_uri();
    let listener = TcpListener::bind(("127.0.0.1", config.redirect_port()))
        .with_context(|| format!("listening on 127.0.0.1:{} for the OAuth redirect", config.redirect_port()))?;

    let verifier = base64url(&random_bytes(64)?);
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let state = base64url(&random_bytes(16)?);

    let url = format!(
        "https://accounts.spotify.com/authorize?response_type=code&client_id={}&redirect_uri={}&scope={}&code_challenge_method=S256&code_challenge={}&state={}",
        urlencode(&client_id),
        urlencode(&redirect_uri),
        urlencode(&SCOPES.join(" ")),
        challenge,
        state
    );
    status("Opening your browser to authorize aura with Spotify…");
    if std::process::Command::new("open").arg(&url).status().map(|s| !s.success()).unwrap_or(true) {
        status(&format!("Could not open a browser. Visit this URL:\n{url}"));
    }
    status(&format!("Waiting for Spotify to redirect to {redirect_uri} …"));

    listener.set_nonblocking(false)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(300);
    let code = loop {
        if std::time::Instant::now() > deadline {
            bail!("timed out waiting for the browser redirect");
        }
        let (mut stream, _) = listener.accept()?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut buf = [0u8; 4096];
        let n = stream.read(&mut buf).unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]);
        let first = req.lines().next().unwrap_or("");
        let path = first.split_whitespace().nth(1).unwrap_or("");
        let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut code = None;
        let mut got_state = None;
        let mut error = None;
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "code" => code = Some(urldecode(v)),
                "state" => got_state = Some(urldecode(v)),
                "error" => error = Some(urldecode(v)),
                _ => {}
            }
        }
        let (status_line, body) = if let Some(e) = error {
            ("HTTP/1.1 400 Bad Request", format!("<h2>Spotify said: {e}</h2><p>You can close this tab.</p>"))
        } else if got_state.as_deref() != Some(state.as_str()) || code.is_none() {
            ("HTTP/1.1 400 Bad Request", "<h2>Missing or mismatched code.</h2>".to_string())
        } else {
            ("HTTP/1.1 200 OK", "<h2>aura is connected to Spotify.</h2><p>You can close this tab and go back to the terminal.</p>".to_string())
        };
        let html = format!("<!doctype html><meta charset=utf-8><title>aura</title><body style=\"font-family:-apple-system,sans-serif;background:#0d0f12;color:#e8ecef;display:grid;place-items:center;height:100vh;margin:0\"><div style=\"text-align:center\">{body}</div></body>");
        let _ = write!(
            stream,
            "{status_line}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
            html.len()
        );
        let _ = stream.flush();
        if status_line.starts_with("HTTP/1.1 200") {
            break code.unwrap();
        }
        // Browsers also request /favicon.ico; keep listening on non-matching requests.
        if !path.starts_with("/callback") {
            continue;
        }
        bail!("authorization was rejected or the state did not match");
    };

    status("Exchanging the code for tokens…");
    let tokens = exchange(&client_id, &[
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
    ], None)?;
    save_tokens(&tokens)?;
    Ok(tokens)
}

fn exchange(client_id: &str, form: &[(&str, &str)], previous_refresh: Option<&str>) -> Result<Tokens> {
    let agent = ureq::Agent::new_with_defaults();
    let resp = agent
        .post("https://accounts.spotify.com/api/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send_form(form.iter().copied());
    let mut resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(code)) => bail!("Spotify token endpoint returned HTTP {code}"),
        Err(e) => return Err(e).context("calling Spotify token endpoint"),
    };
    let body: TokenResponse = resp.body_mut().read_json().context("parsing token response")?;
    let refresh_token = body
        .refresh_token
        .or_else(|| previous_refresh.map(|s| s.to_string()))
        .ok_or_else(|| anyhow!("token response had no refresh_token"))?;
    Ok(Tokens {
        access_token: body.access_token,
        refresh_token,
        expires_at: now() + body.expires_in.saturating_sub(30),
        scope: body.scope,
        client_id: client_id.to_string(),
    })
}

/// Thread-safe token holder that refreshes on demand.
#[derive(Clone)]
pub struct TokenStore {
    inner: Arc<Mutex<Tokens>>,
}

impl TokenStore {
    pub fn new(t: Tokens) -> TokenStore {
        TokenStore { inner: Arc::new(Mutex::new(t)) }
    }

    /// A valid access token, refreshing (and persisting) if it is about to expire.
    pub fn access_token(&self) -> Result<String> {
        let mut t = self.inner.lock().unwrap();
        if now() + 10 >= t.expires_at {
            let fresh = exchange(&t.client_id.clone(), &[
                ("grant_type", "refresh_token"),
                ("refresh_token", t.refresh_token.as_str()),
                ("client_id", t.client_id.as_str()),
            ], Some(&t.refresh_token))
            .context("refreshing the Spotify token (run `aura login` again if this persists)")?;
            let _ = save_tokens(&fresh);
            *t = fresh;
        }
        Ok(t.access_token.clone())
    }
}
