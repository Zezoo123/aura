//! Service-agnostic player core: the shared state types, the poll/command worker
//! and the macOS event helper. Service-specific scripting lives in `spotify.rs`
//! and `music.rs`.

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

use crate::{app::Msg, music, spotify};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Service {
    Spotify,
    AppleMusic,
}

/// What a service can do beyond now-playing and transport control.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// The browser needs a Web API login (Spotify) rather than local scripting.
    pub needs_web: bool,
    pub queue: bool,
    /// Label for the "recent" tab.
    pub recent_label: &'static str,
    /// Label for the "top" tab.
    pub top_label: &'static str,
}

impl Service {
    pub const ALL: [Service; 2] = [Service::Spotify, Service::AppleMusic];

    pub fn name(self) -> &'static str {
        match self {
            Service::Spotify => "Spotify",
            Service::AppleMusic => "Apple Music",
        }
    }
    pub fn app_name(self) -> &'static str {
        match self {
            Service::Spotify => "Spotify",
            Service::AppleMusic => "Music",
        }
    }
    pub fn glyph(self) -> &'static str {
        match self {
            Service::Spotify => "●",
            Service::AppleMusic => "",
        }
    }
    pub fn other(self) -> Service {
        match self {
            Service::Spotify => Service::AppleMusic,
            Service::AppleMusic => Service::Spotify,
        }
    }
    pub fn caps(self) -> Caps {
        match self {
            Service::Spotify => Caps { needs_web: true, queue: true, recent_label: "recent", top_label: "top" },
            Service::AppleMusic => Caps { needs_web: false, queue: false, recent_label: "added", top_label: "most played" },
        }
    }
    pub fn parse(s: &str) -> Option<Service> {
        match s.to_lowercase().as_str() {
            "spotify" => Some(Service::Spotify),
            "music" | "apple" | "applemusic" | "apple-music" | "apple_music" => Some(Service::AppleMusic),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayerState {
    Playing,
    Paused,
    Stopped,
}

/// Where a track's artwork comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArtRef {
    None,
    Url(String),
    /// Exported from Music.app by persistent ID.
    MusicTrack(String),
}

impl ArtRef {
    /// Stable identity for caching and change detection.
    pub fn key(&self) -> Option<String> {
        match self {
            ArtRef::None => None,
            ArtRef::Url(u) => Some(u.clone()),
            ArtRef::MusicTrack(id) => Some(format!("music:{id}")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub id: String,
    pub name: String,
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub duration_ms: u64,
    pub art: ArtRef,
    pub track_number: u32,
    pub popularity: u32,
    /// Known like/favorite state when the service reports it directly.
    pub liked: Option<bool>,
    /// Shareable web link, when the service has one.
    pub url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub service: Service,
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
    pub fn not_running(service: Service) -> Self {
        Snapshot {
            service,
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

/// One poll covers every supported player.
#[derive(Clone, Debug)]
pub struct PollResult {
    pub spotify: Snapshot,
    pub music: Snapshot,
    pub taken_at: Instant,
}

impl PollResult {
    pub fn get(&self, s: Service) -> &Snapshot {
        match s {
            Service::Spotify => &self.spotify,
            Service::AppleMusic => &self.music,
        }
    }

    /// Which player should be shown, given an optional user override and the
    /// previously shown service: a playing app wins, then the current one if it is
    /// still running, then any running app, then Spotify.
    pub fn choose(&self, current: Service, forced: Option<Service>) -> Service {
        if let Some(f) = forced {
            return f;
        }
        let playing: Vec<Service> = Service::ALL.iter().copied().filter(|s| self.get(*s).state == PlayerState::Playing).collect();
        if playing.contains(&current) {
            return current;
        }
        if let Some(p) = playing.first() {
            return *p;
        }
        if self.get(current).running && self.get(current).track.is_some() {
            return current;
        }
        if let Some(r) = Service::ALL.iter().copied().find(|s| self.get(*s).running && self.get(*s).track.is_some()) {
            return r;
        }
        if self.get(current).running {
            return current;
        }
        Service::ALL.iter().copied().find(|s| self.get(*s).running).unwrap_or(current)
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
    /// Play an item by service-specific id (spotify:… URI or music:track:… / music:playlist:…).
    PlayUri(String),
    /// Play a track inside a context (album / playlist / collection) so "next" follows it.
    PlayInContext(String, String),
    SetLiked(bool),
    Activate,
}

pub fn script_for(service: Service, cmd: &Command) -> String {
    match service {
        Service::Spotify => spotify::script(cmd),
        Service::AppleMusic => music::script(cmd),
    }
}

pub fn osascript(script: &str) -> Result<String> {
    run_osascript(script, false)
}

pub fn jxa(script: &str) -> Result<String> {
    run_osascript(script, true)
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
    child.stdin.take().unwrap().write_all(script.as_bytes()).context("writing script")?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(anyhow!("osascript failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

/// One JXA process reads both players. Referencing an application object does not
/// launch it; `running()` is checked first.
const STATE_SCRIPT: &str = r#"
function run() {
  const out = {};
  const s = Application('Spotify');
  if (s.running()) {
    const o = {};
    try { o.state = s.playerState(); } catch (e) { o.state = 'stopped'; }
    try { o.position = s.playerPosition() || 0; } catch (e) { o.position = 0; }
    try { o.volume = s.soundVolume(); } catch (e) { o.volume = 0; }
    try { o.shuffle = s.shuffling(); } catch (e) { o.shuffle = false; }
    try { o.repeat = s.repeating(); } catch (e) { o.repeat = false; }
    try {
      const t = s.currentTrack();
      o.track = {
        id: t.id(), name: t.name(), artist: t.artist(), album: t.album(),
        album_artist: t.albumArtist(), duration: t.duration(), artwork_url: t.artworkUrl(),
        track_number: t.trackNumber(), popularity: t.popularity()
      };
    } catch (e) {}
    out.spotify = o;
  }
  const m = Application('Music');
  if (m.running()) {
    const o = {};
    try { o.state = m.playerState(); } catch (e) { o.state = 'stopped'; }
    try { o.position = m.playerPosition() || 0; } catch (e) { o.position = 0; }
    try { o.volume = m.soundVolume() || 0; } catch (e) { o.volume = 0; }
    try { o.shuffle = m.shuffleEnabled(); } catch (e) { o.shuffle = false; }
    try { o.repeat = m.songRepeat() !== 'off'; } catch (e) { o.repeat = false; }
    try {
      const t = m.currentTrack();
      o.track = {
        id: t.persistentID(), name: t.name(), artist: t.artist(), album: t.album(),
        album_artist: t.albumArtist(), duration: t.duration() * 1000,
        track_number: t.trackNumber(), favorited: t.favorited(), artworks: t.artworks.length
      };
    } catch (e) {}
    out.music = o;
  }
  return JSON.stringify(out);
}
"#;

#[derive(serde::Deserialize, Default)]
struct RawState {
    #[serde(default)]
    state: String,
    #[serde(default)]
    position: Option<f64>,
    #[serde(default)]
    volume: Option<f64>,
    #[serde(default)]
    shuffle: bool,
    #[serde(default)]
    repeat: bool,
    #[serde(default)]
    track: Option<RawTrack>,
}

#[derive(serde::Deserialize, Default)]
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
    #[serde(default)]
    favorited: Option<bool>,
    #[serde(default)]
    artworks: Option<u32>,
}

#[derive(serde::Deserialize, Default)]
struct RawPoll {
    #[serde(default)]
    spotify: Option<RawState>,
    #[serde(default)]
    music: Option<RawState>,
}

/// Read every player's state once.
pub fn poll() -> Result<PollResult> {
    let taken_at = Instant::now();
    let raw = jxa(STATE_SCRIPT)?;
    parse_poll(&raw, taken_at)
}

fn parse_poll(raw: &str, taken_at: Instant) -> Result<PollResult> {
    let r: RawPoll = serde_json::from_str(raw.trim()).with_context(|| format!("unexpected reply from osascript: {raw:?}"))?;
    Ok(PollResult {
        spotify: r.spotify.map(|s| to_snapshot(Service::Spotify, s, taken_at)).unwrap_or_else(|| Snapshot::not_running(Service::Spotify)),
        music: r.music.map(|s| to_snapshot(Service::AppleMusic, s, taken_at)).unwrap_or_else(|| Snapshot::not_running(Service::AppleMusic)),
        taken_at,
    })
}

fn to_snapshot(service: Service, r: RawState, taken_at: Instant) -> Snapshot {
    let state = match r.state.as_str() {
        "playing" => PlayerState::Playing,
        "paused" => PlayerState::Paused,
        _ => PlayerState::Stopped,
    };
    let track = r.track.filter(|t| !t.id.is_empty()).map(|t| {
        let (art, url, liked) = match service {
            Service::Spotify => (
                if t.artwork_url.is_empty() { ArtRef::None } else { ArtRef::Url(t.artwork_url.clone()) },
                Some(spotify::web_url(&t.id)),
                None,
            ),
            Service::AppleMusic => (
                if t.artworks.unwrap_or(0) > 0 { ArtRef::MusicTrack(t.id.clone()) } else { ArtRef::None },
                None,
                t.favorited,
            ),
        };
        Track {
            id: t.id,
            name: t.name,
            artist: t.artist,
            album: t.album,
            album_artist: t.album_artist,
            duration_ms: t.duration.max(0.0) as u64,
            art,
            track_number: t.track_number.max(0.0) as u32,
            popularity: t.popularity.max(0.0) as u32,
            liked,
            url,
        }
    });
    Snapshot {
        service,
        running: true,
        state,
        position_ms: (r.position.unwrap_or(0.0).max(0.0) * 1000.0) as u64,
        taken_at,
        volume: r.volume.unwrap_or(0.0).clamp(0.0, 100.0) as u8,
        shuffle: r.shuffle,
        repeat: r.repeat,
        track,
    }
}

enum Job {
    Poll,
    Run(Service, Command),
}

/// Handle to the background worker.
#[derive(Clone)]
pub struct Player {
    tx: mpsc::Sender<Job>,
}

impl Player {
    /// Start the worker: a poll/command thread plus (if available) the event helper.
    pub fn start(out: mpsc::Sender<Msg>, poll_every: Duration) -> Player {
        let (tx, rx) = mpsc::channel::<Job>();
        let handle = Player { tx: tx.clone() };

        // Commands and polls are serialized so a poll issued right after a command
        // always observes the command's effect.
        let out_poll = out.clone();
        thread::Builder::new()
            .name("player-poll".into())
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
                    if let Job::Run(service, cmd) = job {
                        if let Err(e) = osascript(&script_for(service, &cmd)) {
                            let _ = out_poll.send(Msg::Error(format!("{e:#}")));
                        }
                        let _ = out_poll.send(Msg::CommandDone);
                    }
                    while let Ok(Job::Poll) = rx.try_recv() {}
                    match poll() {
                        Ok(p) => {
                            let any_playing = Service::ALL.iter().any(|s| p.get(*s).state == PlayerState::Playing);
                            interval = if any_playing { poll_every } else { poll_every * 3 };
                            if out_poll.send(Msg::Player(p)).is_err() {
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

        let poke = tx.clone();
        let out_ev = out;
        thread::Builder::new()
            .name("player-events".into())
            .spawn(move || {
                if let Err(e) = run_helper(poke, out_ev.clone()) {
                    let _ = out_ev.send(Msg::HelperUnavailable(format!("{e:#}")));
                }
            })
            .expect("spawn helper thread");

        handle
    }

    pub fn run(&self, service: Service, cmd: Command) {
        let _ = self.tx.send(Job::Run(service, cmd));
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
        let app = v.get("app").and_then(|e| e.as_str()).unwrap_or("spotify");
        let service = if app == "music" { Service::AppleMusic } else { Service::Spotify };
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
                if service == Service::Spotify {
                    let position_ms = (s("Playback Position").parse::<f64>().unwrap_or(0.0) * 1000.0) as u64;
                    let _ = out.send(Msg::Notify {
                        service,
                        at: Instant::now(),
                        state,
                        position_ms: Some(position_ms),
                        track_id: s("Track ID"),
                    });
                } else {
                    // Music's notification carries a numeric id; the poll resolves the rest.
                    let _ = out.send(Msg::Notify {
                        service,
                        at: Instant::now(),
                        state,
                        position_ms: None,
                        track_id: String::new(),
                    });
                }
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
    fn parses_both_players() {
        let raw = r#"{"spotify":{"state":"playing","position":12.5,"volume":43,"shuffle":true,"repeat":false,
            "track":{"id":"spotify:track:x","name":"N","artist":"A","album":"B","album_artist":"A","duration":178928,
                     "artwork_url":"https://i.scdn.co/image/abc","track_number":1,"popularity":70}},
            "music":{"state":"paused","position":3,"volume":100,"shuffle":false,"repeat":true,
            "track":{"id":"ABCDEF0123456789","name":"M","artist":"C","album":"D","album_artist":"C","duration":200000,
                     "track_number":2,"favorited":true,"artworks":1}}}"#;
        let p = parse_poll(raw, Instant::now()).unwrap();
        assert_eq!(p.spotify.state, PlayerState::Playing);
        assert_eq!(p.spotify.position_ms, 12500);
        let t = p.spotify.track.as_ref().unwrap();
        assert_eq!(t.url.as_deref(), Some("https://open.spotify.com/track/x"));
        assert_eq!(t.art, ArtRef::Url("https://i.scdn.co/image/abc".into()));
        let m = p.music.track.as_ref().unwrap();
        assert_eq!(m.liked, Some(true));
        assert_eq!(m.art, ArtRef::MusicTrack("ABCDEF0123456789".into()));
        assert!(p.music.repeat);
        let nulls = parse_poll(r#"{"music":{"state":"stopped","position":null,"volume":null}}"#, Instant::now()).unwrap();
        assert!(nulls.music.running && nulls.music.track.is_none());
        let none = parse_poll("{}", Instant::now()).unwrap();
        assert!(!none.spotify.running && !none.music.running);
    }

    #[test]
    fn chooses_the_playing_service() {
        let mut p = parse_poll("{}", Instant::now()).unwrap();
        assert_eq!(p.choose(Service::Spotify, None), Service::Spotify);
        p.music.running = true;
        p.music.state = PlayerState::Playing;
        assert_eq!(p.choose(Service::Spotify, None), Service::AppleMusic);
        assert_eq!(p.choose(Service::Spotify, Some(Service::Spotify)), Service::Spotify);
        p.spotify.running = true;
        p.spotify.state = PlayerState::Playing;
        assert_eq!(p.choose(Service::Spotify, None), Service::Spotify);
    }
}
