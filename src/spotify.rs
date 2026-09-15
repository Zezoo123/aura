//! Spotify backend for macOS.
//!
//! State is read through the Spotify desktop app's AppleScript dictionary (no OAuth,
//! no developer app, no Premium requirement). Track changes arrive instantly through a
//! tiny Swift helper that listens for Spotify's distributed notifications; a slow
//! background poll fills in what the notification lacks (artwork URL, volume, shuffle).

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command as Proc, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};

use crate::app::Msg;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub duration_ms: u64,
    pub artwork_url: String,
    pub track_number: u32,
    pub popularity: u32,
}

impl Track {
    pub fn web_url(&self) -> String {
        // spotify:track:ID -> https://open.spotify.com/track/ID
        let mut parts = self.id.split(':');
        let _ = parts.next();
        match (parts.next(), parts.next()) {
            (Some(kind), Some(id)) => format!("https://open.spotify.com/{kind}/{id}"),
            _ => self.id.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub running: bool,
    pub state: PlayerState,
    pub position_ms: u64,
    pub taken_at: Instant,
    pub volume: u8,
    pub shuffle: bool,
    pub repeat: bool,
    pub track: Option<Track>,
}

impl Snapshot {
    pub fn not_running() -> Self {
        Snapshot {
            running: false,
            state: PlayerState::Stopped,
            position_ms: 0,
            taken_at: Instant::now(),
            volume: 0,
            shuffle: false,
            repeat: false,
            track: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Command {
    PlayPause,
    Play,
    Pause,
    Next,
    Prev,
    Seek(u64),
    SetVolume(u8),
    SetShuffle(bool),
    SetRepeat(bool),
    PlayUri(String),
    /// Play a track inside a context (album / playlist / collection) so "next" follows it.
    PlayInContext(String, String),
    Activate,
}

/// Accept `spotify:track:X` or `https://open.spotify.com/track/X?si=…` and return the URI form.
pub fn to_uri(input: &str) -> String {
    let s = input.trim();
    if let Some(rest) = s
        .strip_prefix("https://open.spotify.com/")
        .or_else(|| s.strip_prefix("http://open.spotify.com/"))
    {
        let path = rest.split(['?', '#']).next().unwrap_or("");
        // intl-xx/ prefix appears on localized links.
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && !p.starts_with("intl-")).collect();
        if parts.len() >= 2 {
            return format!("spotify:{}:{}", parts[parts.len() - 2], parts[parts.len() - 1]);
        }
    }
    s.to_string()
}

pub fn script_for(cmd: &Command) -> String {
    cmd.script()
}

impl Command {
    fn script(&self) -> String {
        let body = match self {
            Command::PlayPause => "playpause".to_string(),
            Command::Play => "play".to_string(),
            Command::Pause => "pause".to_string(),
            Command::Next => "next track".to_string(),
            Command::Prev => "previous track".to_string(),
            Command::Seek(ms) => format!("set player position to {}", *ms as f64 / 1000.0),
            Command::SetVolume(v) => format!("set sound volume to {}", (*v).min(100)),
            Command::SetShuffle(on) => format!("set shuffling to {on}"),
            Command::SetRepeat(on) => format!("set repeating to {on}"),
            Command::PlayUri(uri) => format!("play track \"{}\"", uri.replace('"', "")),
            Command::PlayInContext(uri, ctx) => format!(
                "play track \"{}\" in context \"{}\"",
                uri.replace('"', ""),
                ctx.replace('"', "")
            ),
            Command::Activate => "activate".to_string(),
        };
        format!("tell application \"Spotify\" to {body}")
    }
}

/// JavaScript-for-Automation poll: one process launch, JSON out. Referencing the
/// application object does not launch Spotify; `running()` is checked first.
const STATE_SCRIPT: &str = r#"
function run() {
  const s = Application('Spotify');
  if (!s.running()) return JSON.stringify({running: false});
  const out = {running: true};
  try { out.state = s.playerState(); } catch (e) { out.state = 'stopped'; }
  try { out.position = s.playerPosition(); } catch (e) { out.position = 0; }
  try { out.volume = s.soundVolume(); } catch (e) { out.volume = 0; }
  try { out.shuffle = s.shuffling(); } catch (e) { out.shuffle = false; }
  try { out.repeat = s.repeating(); } catch (e) { out.repeat = false; }
  try {
    const t = s.currentTrack();
    out.track = {
      id: t.id(), name: t.name(), artist: t.artist(), album: t.album(),
      album_artist: t.albumArtist(), duration: t.duration(), artwork_url: t.artworkUrl(),
      track_number: t.trackNumber(), popularity: t.popularity()
    };
  } catch (e) {}
  return JSON.stringify(out);
}
"#;

#[derive(serde::Deserialize)]
struct RawState {
    running: bool,
    #[serde(default)]
    state: String,
    #[serde(default)]
    position: f64,
    #[serde(default)]
    volume: f64,
    #[serde(default)]
    shuffle: bool,
    #[serde(default)]
    repeat: bool,
    #[serde(default)]
    track: Option<RawTrack>,
}

#[derive(serde::Deserialize)]
struct RawTrack {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artist: String,
    #[serde(default)]
    album: String,
    #[serde(default)]
    album_artist: String,
    #[serde(default)]
    duration: f64,
    #[serde(default)]
    artwork_url: String,
    #[serde(default)]
    track_number: f64,
    #[serde(default)]
    popularity: f64,
}

pub fn osascript(script: &str) -> Result<String> {
    run_osascript(script, false)
}

fn run_osascript(script: &str, javascript: bool) -> Result<String> {
    let mut proc = Proc::new("osascript");
    if javascript {
        proc.args(["-l", "JavaScript"]);
    }
    let mut child = proc
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning osascript (is this macOS?)")?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(script.as_bytes())
        .context("writing AppleScript")?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "osascript failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// Read the full player state once.
pub fn poll() -> Result<Snapshot> {
    let taken_at = Instant::now();
    let raw = run_osascript(STATE_SCRIPT, true)?;
    parse_snapshot(&raw, taken_at)
}

fn parse_snapshot(raw: &str, taken_at: Instant) -> Result<Snapshot> {
    let r: RawState = serde_json::from_str(raw.trim()).with_context(|| format!("unexpected reply from Spotify: {raw:?}"))?;
    if !r.running {
        return Ok(Snapshot::not_running());
    }
    let state = match r.state.as_str() {
        "playing" => PlayerState::Playing,
        "paused" => PlayerState::Paused,
        _ => PlayerState::Stopped,
    };
    let track = r.track.filter(|t| !t.id.is_empty()).map(|t| Track {
        id: t.id,
        name: t.name,
        artist: t.artist,
        album: t.album,
        album_artist: t.album_artist,
        duration_ms: t.duration.max(0.0) as u64,
        artwork_url: t.artwork_url,
        track_number: t.track_number.max(0.0) as u32,
        popularity: t.popularity.max(0.0) as u32,
    });
    Ok(Snapshot {
        running: true,
        state,
        position_ms: (r.position.max(0.0) * 1000.0) as u64,
        taken_at,
        volume: r.volume.clamp(0.0, 100.0) as u8,
        shuffle: r.shuffle,
        repeat: r.repeat,
        track,
    })
}

enum Job {
    Poll,
    Run(Command),
}

/// Handle to the background Spotify worker.
#[derive(Clone)]
pub struct Spotify {
    tx: mpsc::Sender<Job>,
}

impl Spotify {
    /// Start the worker: a poll/command thread plus (if available) the event helper.
    pub fn start(out: mpsc::Sender<Msg>, poll_every: Duration) -> Spotify {
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = Spotify { tx: tx.clone() };

        // Poll + command thread. Commands and polls are serialized so a poll issued
        // right after a command always observes the command's effect.
        let out_poll = out.clone();
        thread::Builder::new()
            .name("spotify-poll".into())
            .spawn(move || {
                let mut last_poll = Instant::now() - poll_every;
                let mut interval = poll_every;
                loop {
                    let wait = interval.saturating_sub(last_poll.elapsed());
                    let job = match rx.recv_timeout(wait) {
                        Ok(j) => j,
                        Err(mpsc::RecvTimeoutError::Timeout) => Job::Poll,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    };
                    if let Job::Run(cmd) = job {
                        let res = osascript(&cmd.script());
                        if let Err(e) = res {
                            let _ = out_poll.send(Msg::Error(format!("{e:#}")));
                        }
                        let _ = out_poll.send(Msg::CommandDone);
                    }
                    // Coalesce any queued pokes so we don't poll in a burst.
                    while let Ok(Job::Poll) = rx.try_recv() {}
                    match poll() {
                        Ok(s) => {
                            // Idle players change rarely: back off to save CPU.
                            interval = if s.running && s.state == PlayerState::Playing {
                                poll_every
                            } else {
                                poll_every * 3
                            };
                            if out_poll.send(Msg::Player(s)).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = out_poll.send(Msg::Error(format!("{e:#}")));
                        }
                    }
                    last_poll = Instant::now();
                }
            })
            .expect("spawn poll thread");

        // Event helper: instant notifications on track / state changes.
        let poke = tx.clone();
        let out_ev = out;
        thread::Builder::new()
            .name("spotify-events".into())
            .spawn(move || {
                if let Err(e) = run_helper(poke, out_ev.clone()) {
                    let _ = out_ev.send(Msg::HelperUnavailable(format!("{e:#}")));
                }
            })
            .expect("spawn helper thread");

        handle
    }

    pub fn run(&self, cmd: Command) {
        let _ = self.tx.send(Job::Run(cmd));
    }

    pub fn poke(&self) {
        let _ = self.tx.send(Job::Poll);
    }
}

static HELPER_BIN: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/spotify-events"));

fn helper_path() -> Result<PathBuf> {
    if HELPER_BIN.is_empty() {
        return Err(anyhow!("helper was not built (swiftc missing at build time)"));
    }
    let dir = crate::cache_dir()?.join("bin");
    std::fs::create_dir_all(&dir)?;
    // Version the file by content length + a cheap hash so upgrades replace it.
    let tag = HELPER_BIN
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let path = dir.join(format!("aura-events-{tag:016x}"));
    if !path.exists() {
        let tmp = dir.join(format!(".aura-events-{}", std::process::id()));
        std::fs::write(&tmp, HELPER_BIN)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
        std::fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

fn run_helper(poke: mpsc::Sender<Job>, out: mpsc::Sender<Msg>) -> Result<()> {
    let path = helper_path()?;
    let mut child = Proc::new(&path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .with_context(|| format!("starting helper {}", path.display()))?;
    let stdout = child.stdout.take().unwrap();
    let reader = BufReader::new(stdout);
    let mut ready = false;
    for line in reader.lines() {
        let line = line?;
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let event = v.get("event").and_then(|e| e.as_str()).unwrap_or("");
        match event {
            "ready" => {
                ready = true;
                let _ = out.send(Msg::HelperReady);
            }
            "playback" => {
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let state = match s("Player State").as_str() {
                    "Playing" => PlayerState::Playing,
                    "Paused" => PlayerState::Paused,
                    _ => PlayerState::Stopped,
                };
                let position_ms = (s("Playback Position").parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                let _ = out.send(Msg::Notify {
                    at: Instant::now(),
                    state,
                    position_ms,
                    track_id: s("Track ID"),
                });
                let _ = poke.send(Job::Poll);
            }
            "launched" | "quit" => {
                let _ = poke.send(Job::Poll);
            }
            _ => {}
        }
    }
    let status = child.wait()?;
    if !ready {
        return Err(anyhow!("helper exited before becoming ready ({status})"));
    }
    Err(anyhow!("helper exited ({status})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_state_json() {
        let raw = r#"{"running":true,"state":"playing","position":12.5,"volume":43,"shuffle":true,"repeat":false,
            "track":{"id":"spotify:track:x","name":"N","artist":"A","album":"B","album_artist":"A","duration":178928,
                     "artwork_url":"https://i.scdn.co/image/abc","track_number":1,"popularity":70}}"#;
        let s = parse_snapshot(raw, Instant::now()).unwrap();
        assert_eq!(s.state, PlayerState::Playing);
        assert_eq!(s.position_ms, 12500);
        assert_eq!(s.volume, 43);
        let t = s.track.unwrap();
        assert_eq!(t.duration_ms, 178928);
        assert_eq!(t.web_url(), "https://open.spotify.com/track/x");
        let off = parse_snapshot(r#"{"running":false}"#, Instant::now()).unwrap();
        assert!(!off.running);
    }

    #[test]
    fn converts_links_to_uris() {
        assert_eq!(to_uri("https://open.spotify.com/track/abc?si=1"), "spotify:track:abc");
        assert_eq!(to_uri("https://open.spotify.com/intl-de/album/xyz"), "spotify:album:xyz");
        assert_eq!(to_uri("spotify:playlist:p"), "spotify:playlist:p");
    }

    #[test]
    fn command_scripts_escape_quotes() {
        let s = Command::PlayInContext("spotify:track:a\"b".into(), "spotify:playlist:c".into()).script();
        assert_eq!(s, "tell application \"Spotify\" to play track \"spotify:track:ab\" in context \"spotify:playlist:c\"");
    }
}
