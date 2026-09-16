//! Application state, message handling and actions.

use std::{
    sync::{mpsc, Arc},
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui_image::{
    picker::Picker,
    thread::{ResizeRequest, ResizeResponse, ThreadProtocol},
};

use crate::{
    art::{self, Art},
    auth::{self, TokenStore, Tokens},
    browser::{Browser, Focus, Row, Tab, View},
    config::Config,
    lyrics::{self, Lyrics},
    music,
    player::{Command, Player, PlayerState, PollResult, Service, Snapshot},
    theme::Theme,
    web::{AlbumItem, PlaylistItem, SearchResults, TrackItem, WebApi},
};

pub enum Msg {
    Player(PollResult),
    Notify {
        service: Service,
        at: Instant,
        state: PlayerState,
        position_ms: Option<u64>,
        track_id: String,
    },
    CommandDone,
    Error(String),
    HelperReady,
    HelperUnavailable(String),
    Art {
        key: String,
        result: Result<Art, String>,
    },
    Lyrics {
        track_id: String,
        result: Result<Option<Lyrics>, String>,
    },
    Resized(ResizeResponse),
    Input(Event),
    Web(u64, WebReply),
    Me(Result<(String, String), String>),
    Liked(String, Result<bool, String>),
    LoginStatus(String),
    LoginDone(Result<Tokens, String>),
}

/// Replies from Web API worker threads (tagged with the browser request id).
pub enum WebReply {
    MusicLibrary(Result<Vec<music::LibraryTrack>, String>, Option<Tab>),
    Search(Result<SearchResults, String>),
    Playlists(Result<Vec<PlaylistItem>, String>),
    Tracks(String, Option<String>, Result<Vec<TrackItem>, String>),
    Albums(String, Result<Vec<AlbumItem>, String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Cover,
    Split,
    Lyrics,
    Mini,
}

impl Layout {
    pub fn next(self) -> Layout {
        match self {
            Layout::Cover => Layout::Split,
            Layout::Split => Layout::Lyrics,
            Layout::Lyrics => Layout::Cover,
            Layout::Mini => Layout::Cover,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Layout::Cover => "cover",
            Layout::Split => "split",
            Layout::Lyrics => "lyrics",
            Layout::Mini => "mini",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    PlayPause,
    Next,
    Prev,
    SeekRel(i64),
    VolumeRel(i16),
    Mute,
    Shuffle,
    Repeat,
    ToggleLyrics,
    ToggleAmbient,
    CycleLayout,
    SetLayout(Layout),
    OpenInSpotify,
    CopyLink,
    LyricsOffset(i64),
    ScrollLyrics(i32),
    ToggleHelp,
    Refresh,
    Launch,
    Quit,
    OpenBrowser(Option<Tab>),
    Like,
    SwitchService,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LyricsStatus {
    Idle,
    Loading,
    Found,
    NotFound,
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelperStatus {
    Starting,
    Live,
    Polling,
}

/// Screen regions recorded during the last draw, for mouse hit-testing.
#[derive(Default)]
pub struct HitAreas {
    pub progress: Option<Rect>,
    pub art: Option<Rect>,
    pub buttons: Vec<(Rect, Action)>,
    pub browser_panel: Option<Rect>,
    pub browser_list: Option<Rect>,
    pub browser_input: Option<Rect>,
}

pub struct Options {
    pub service: Option<Service>,
    pub layout: Option<Layout>,
    pub ambient: bool,
    pub lyrics: bool,
    pub lyrics_offset_ms: i64,
    pub config: Config,
    pub tokens: Option<Tokens>,
}

pub struct App {
    pub player: Player,
    pub active: Service,
    pub forced: Option<Service>,
    pub last_poll: Option<PollResult>,
    music_lib: Option<Arc<Vec<music::LibraryTrack>>>,
    agent: ureq::Agent,
    pub config: Config,
    pub web: Option<WebApi>,
    pub me_name: Option<String>,
    pub me_id: Option<String>,
    pub browser: Browser,
    pub liked: Option<bool>,
    liked_for: Option<String>,
    pub picker: Picker,
    resize_tx: mpsc::Sender<ResizeRequest>,
    msg_tx: mpsc::Sender<Msg>,

    pub snap: Snapshot,
    pub theme: Theme,
    theme_anim: Option<(Theme, Instant)>,
    pub art: Option<Art>,
    pub art_proto: Option<ThreadProtocol>,
    art_key: Option<String>,
    pub art_loading: bool,

    pub lyrics: Option<Lyrics>,
    lyrics_for: Option<String>,
    pub lyrics_status: LyricsStatus,
    pub lyrics_offset_ms: i64,
    pub lyric_scroll: f32,
    pub plain_scroll: i32,

    pub layout_override: Option<Layout>,
    pub ambient: bool,
    pub show_lyrics: bool,
    pub help: bool,
    pub toast: Option<(String, Instant)>,
    pub last_error: Option<(String, Instant)>,
    pub helper: HelperStatus,
    pub hit: HitAreas,
    pub started: Instant,
    pub quit: bool,
    pub needs_clear: bool,

    last_cmd_at: Instant,
    muted_from: Option<u8>,
    pending_volume: Option<(u8, Instant)>,
    pending_seek: Option<(u64, Instant)>,
    end_poked_for: Option<String>,
}

const THEME_FADE: Duration = Duration::from_millis(600);
const TOAST_TTL: Duration = Duration::from_millis(1800);

impl App {
    pub fn new(
        player: Player,
        picker: Picker,
        msg_tx: mpsc::Sender<Msg>,
        resize_tx: mpsc::Sender<ResizeRequest>,
        opts: Options,
    ) -> App {
        let agent: ureq::Agent = ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(12)))
            .user_agent("aura/0.1 (terminal now-playing; https://github.com/Zezoo123/aura)")
            .build()
            .new_agent();
        let now = Instant::now();
        let web = opts
            .tokens
            .map(|t| WebApi::new(TokenStore::new(t), opts.config.market.clone()));
        let mut app = App {
            player,
            active: opts.service.unwrap_or(Service::Spotify),
            forced: opts.service,
            last_poll: None,
            music_lib: None,
            agent,
            config: opts.config,
            web,
            me_name: None,
            me_id: None,
            browser: Browser::default(),
            liked: None,
            liked_for: None,
            picker,
            resize_tx,
            msg_tx,
            snap: Snapshot::not_running(opts.service.unwrap_or(Service::Spotify)),
            theme: Theme::fallback(),
            theme_anim: None,
            art: None,
            art_proto: None,
            art_key: None,
            art_loading: false,
            lyrics: None,
            lyrics_for: None,
            lyrics_status: LyricsStatus::Idle,
            lyrics_offset_ms: opts.lyrics_offset_ms,
            lyric_scroll: 0.0,
            plain_scroll: 0,
            layout_override: opts.layout,
            ambient: opts.ambient,
            show_lyrics: opts.lyrics,
            help: false,
            toast: None,
            last_error: None,
            helper: HelperStatus::Starting,
            hit: HitAreas::default(),
            started: now,
            quit: false,
            needs_clear: false,
            last_cmd_at: now - Duration::from_secs(10),
            muted_from: None,
            pending_volume: None,
            pending_seek: None,
            end_poked_for: None,
        };
        app.fetch_me();
        app
    }

    // ----- derived state -------------------------------------------------

    /// Interpolated playback position in ms.
    pub fn position_ms(&self) -> u64 {
        if let Some((ms, _)) = self.pending_seek {
            return ms;
        }
        let s = &self.snap;
        let mut p = s.position_ms;
        if s.state == PlayerState::Playing {
            p += s.taken_at.elapsed().as_millis() as u64;
        }
        if let Some(t) = &s.track {
            p = p.min(t.duration_ms);
        }
        p
    }

    pub fn volume(&self) -> u8 {
        self.pending_volume.map(|(v, _)| v).unwrap_or(self.snap.volume)
    }

    pub fn theme_now(&self) -> Theme {
        match &self.theme_anim {
            Some((from, at)) => {
                let t = (at.elapsed().as_secs_f32() / THEME_FADE.as_secs_f32()).clamp(0.0, 1.0);
                from.lerp(&self.theme, ease(t))
            }
            None => self.theme.clone(),
        }
    }

    pub fn layout_for(&self, w: u16, h: u16) -> Layout {
        if h < 7 || w < 40 {
            return Layout::Mini;
        }
        if let Some(l) = self.layout_override {
            if l != Layout::Mini {
                return l;
            }
        }
        if w >= 96 && h >= 22 {
            Layout::Split
        } else {
            Layout::Cover
        }
    }

    /// Terminal window title: "aura · Song – Artist".
    pub fn window_title(&self) -> String {
        match &self.snap.track {
            Some(t) if self.snap.running => {
                let glyph = if self.snap.state == PlayerState::Playing { "▶" } else { "❚❚" };
                format!("{glyph} {} – {} · aura", t.name, t.artist)
            }
            _ => "aura".to_string(),
        }
    }

    pub fn toast_text(&self) -> Option<&str> {
        self.toast
            .as_ref()
            .filter(|(_, at)| at.elapsed() < TOAST_TTL)
            .map(|(s, _)| s.as_str())
    }

    fn toast(&mut self, s: impl Into<String>) {
        self.toast = Some((s.into(), Instant::now()));
    }

    // ----- message handling ---------------------------------------------

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Player(p) => {
                if p.taken_at < self.last_cmd_at {
                    return; // stale: observed before our last command landed
                }
                let chosen = p.choose(self.active, self.forced);
                let s = p.get(chosen).clone();
                crate::debug(format!("snapshot: {:?} state={:?} pos={}", chosen, s.state, s.position_ms));
                self.last_poll = Some(p);
                if chosen != self.active {
                    self.active = chosen;
                    self.needs_clear = true;
                }
                self.apply_snapshot(s);
            }
            Msg::Notify {
                service,
                at,
                state,
                position_ms,
                track_id,
            } => {
                if at < self.last_cmd_at || service != self.active {
                    return;
                }
                let same = track_id.is_empty() || self.snap.track.as_ref().map(|t| t.id == track_id).unwrap_or(false);
                if same && self.snap.running {
                    if let Some(p) = position_ms {
                        self.snap.position_ms = p;
                        self.snap.taken_at = at;
                        self.pending_seek = None;
                    } else if self.snap.state != state {
                        self.snap.position_ms = self.position_ms();
                        self.snap.taken_at = at;
                    }
                    self.snap.state = state;
                }
            }
            Msg::CommandDone => {}
            Msg::Error(e) => {
                self.last_error = Some((e, Instant::now()));
            }
            Msg::HelperReady => self.helper = HelperStatus::Live,
            Msg::HelperUnavailable(why) => {
                crate::debug(format!("event helper unavailable: {why}"));
                self.helper = HelperStatus::Polling;
            }
            Msg::Art { key, result } => {
                if self.art_key.as_deref() != Some(key.as_str()) {
                    return;
                }
                self.art_loading = false;
                match result {
                    Ok(art) => {
                        self.theme_anim = Some((self.theme_now(), Instant::now()));
                        self.theme = art.theme.clone();
                        let proto = self.picker.new_resize_protocol(art.image.clone());
                        self.art_proto = Some(ThreadProtocol::new(self.resize_tx.clone(), Some(proto)));
                        self.art = Some(art);
                        self.needs_clear = true;
                    }
                    Err(e) => {
                        self.last_error = Some((format!("artwork: {e}"), Instant::now()));
                    }
                }
            }
            Msg::Lyrics { track_id, result } => {
                if self.lyrics_for.as_deref() != Some(track_id.as_str()) {
                    return;
                }
                match result {
                    Ok(Some(l)) => {
                        self.lyrics = Some(l);
                        self.lyrics_status = LyricsStatus::Found;
                    }
                    Ok(None) => {
                        self.lyrics = None;
                        self.lyrics_status = LyricsStatus::NotFound;
                    }
                    Err(e) => {
                        self.lyrics = None;
                        self.lyrics_status = LyricsStatus::Error(e);
                    }
                }
            }
            Msg::Resized(resp) => {
                if let Some(p) = &mut self.art_proto {
                    p.update_resized_protocol(resp);
                }
            }
            Msg::Input(ev) => self.input(ev),
            Msg::Web(req, reply) => self.on_web_reply(req, reply),
            Msg::Me(Ok((name, id))) => {
                self.me_name = Some(name);
                self.me_id = Some(id);
            }
            Msg::Me(Err(e)) => {
                self.last_error = Some((format!("Spotify account: {e}"), Instant::now()));
            }
            Msg::Liked(uri, result) => {
                if self.snap.track.as_ref().map(|t| t.id == uri).unwrap_or(false) {
                    match result {
                        Ok(b) => self.liked = Some(b),
                        Err(e) => crate::debug(format!("liked check failed: {e}")),
                    }
                }
            }
            Msg::LoginStatus(line) => {
                self.browser.setup_status.push(line);
            }
            Msg::LoginDone(Ok(tokens)) => {
                self.web = Some(WebApi::new(TokenStore::new(tokens), self.config.market.clone()));
                self.browser.setup = false;
                self.browser.setup_status.clear();
                self.browser.tab = Tab::Search;
                self.browser.focus = Focus::Input;
                self.fetch_me();
                self.refresh_liked();
                self.toast("connected to Spotify");
            }
            Msg::LoginDone(Err(e)) => {
                self.browser.setup_status.push(format!("login failed: {e}"));
                self.browser.setup_status.push("press enter to try again".into());
            }
        }
    }

    fn apply_snapshot(&mut self, s: Snapshot) {
        let prev_track = self.snap.track.as_ref().map(|t| t.id.clone());
        let prev_running = self.snap.running;
        let prev_service = self.snap.service;
        self.snap = s;
        if let Some(t) = &self.snap.track {
            if let Some(l) = t.liked {
                self.liked = Some(l);
            }
        }
        if self.pending_seek.is_some() && self.snap.state != PlayerState::Playing {
            // keep the optimistic seek until the player confirms
        } else {
            self.pending_seek = None;
        }
        let track_id = self.snap.track.as_ref().map(|t| t.id.clone());
        if track_id != prev_track || prev_running != self.snap.running || prev_service != self.snap.service {
            self.on_track_changed();
        }
    }

    fn on_track_changed(&mut self) {
        self.refresh_liked();
        self.lyric_scroll = 0.0;
        self.plain_scroll = 0;
        self.end_poked_for = None;
        let Some(track) = self.snap.track.clone() else {
            self.art = None;
            self.art_proto = None;
            self.art_key = None;
            self.lyrics = None;
            self.lyrics_for = None;
            self.lyrics_status = LyricsStatus::Idle;
            self.theme_anim = Some((self.theme_now(), Instant::now()));
            self.theme = Theme::fallback();
            self.needs_clear = true;
            return;
        };
        // Artwork (same album => keep what we have).
        match track.art.key() {
            Some(key) if self.art_key.as_deref() != Some(key.as_str()) => {
                self.art_key = Some(key.clone());
                self.art_loading = true;
                let agent = self.agent.clone();
                let tx = self.msg_tx.clone();
                let art_ref = track.art.clone();
                thread::spawn(move || {
                    let result = art::load(&agent, &art_ref).map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Msg::Art { key, result });
                });
            }
            Some(_) => {}
            None => {
                self.art = None;
                self.art_proto = None;
                self.art_key = None;
                self.theme_anim = Some((self.theme_now(), Instant::now()));
                self.theme = Theme::fallback();
                self.needs_clear = true;
            }
        }
        // Lyrics.
        if self.lyrics_for.as_deref() != Some(track.id.as_str()) {
            self.lyrics_for = Some(track.id.clone());
            self.lyrics = None;
            self.lyrics_status = LyricsStatus::Loading;
            let agent = self.agent.clone();
            let tx = self.msg_tx.clone();
            let id = track.id.clone();
            let service = self.active;
            thread::spawn(move || {
                let mut result = lyrics::lookup(&agent, &track).map_err(|e| format!("{e:#}"));
                if service == Service::AppleMusic && matches!(result, Ok(None)) {
                    // Music.app may hold plain lyrics for the track itself.
                    if let Ok(Some(text)) = music::lyrics(&track.id) {
                        result = Ok(Some(Lyrics {
                            plain: text.lines().map(|l| l.to_string()).collect(),
                            source: "Music".into(),
                            ..Default::default()
                        }));
                    }
                }
                let _ = tx.send(Msg::Lyrics { track_id: id, result });
            });
        }
    }

    /// Called once per frame.
    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some((_, at)) = &self.theme_anim {
            if at.elapsed() >= THEME_FADE {
                self.theme_anim = None;
            }
        }
        if let Some((v, at)) = self.pending_volume {
            if at.elapsed() > Duration::from_millis(120) {
                self.command(Command::SetVolume(v));
                self.snap.volume = v;
                self.pending_volume = None;
            }
        }
        if let Some((ms, at)) = self.pending_seek {
            if at.elapsed() > Duration::from_millis(150) {
                self.command(Command::Seek(ms));
                self.snap.position_ms = ms;
                self.snap.taken_at = Instant::now();
                self.pending_seek = None;
            }
        }
        // Track ran to its end: make sure we notice the next one promptly.
        if let Some(t) = &self.snap.track {
            if self.snap.state == PlayerState::Playing
                && self.position_ms() >= t.duration_ms
                && self.end_poked_for.as_deref() != Some(t.id.as_str())
            {
                self.end_poked_for = Some(t.id.clone());
                self.player.poke();
            }
        }
        // Smooth lyric scroll towards the active line.
        if let Some(l) = &self.lyrics {
            if let Some(i) = l.index_at(self.position_ms() as i64 + self.lyrics_offset_ms) {
                let target = i as f32;
                let d = target - self.lyric_scroll;
                if d.abs() < 0.01 {
                    self.lyric_scroll = target;
                } else {
                    self.lyric_scroll += d * 0.22;
                }
            } else {
                self.lyric_scroll += (-1.0 - self.lyric_scroll) * 0.22;
            }
        }
        if let Some((_, at)) = &self.toast {
            if at.elapsed() > TOAST_TTL + Duration::from_millis(50) {
                self.toast = None;
            }
        }
        let _ = now;
    }

    fn command(&mut self, cmd: Command) {
        self.last_cmd_at = Instant::now();
        self.player.run(self.active, cmd);
    }

    // ----- input --------------------------------------------------------

    fn input(&mut self, ev: Event) {
        match ev {
            Event::Key(k) => {
                if k.kind == KeyEventKind::Release {
                    return;
                }
                if self.browser.open && !self.help {
                    self.browser_key(k);
                    return;
                }
                if let Some(a) = keymap(k, self.help) {
                    self.act(a);
                }
            }
            Event::Mouse(m) => self.mouse(m),
            Event::Resize(_, _) => self.needs_clear = true,
            _ => {}
        }
    }

    fn mouse(&mut self, m: MouseEvent) {
        match m.kind {
            MouseEventKind::ScrollUp => self.act(Action::VolumeRel(3)),
            MouseEventKind::ScrollDown => self.act(Action::VolumeRel(-3)),
            MouseEventKind::Down(MouseButton::Left) => {
                let (x, y) = (m.column, m.row);
                if self.help {
                    self.act(Action::ToggleHelp);
                    return;
                }
                if self.browser.open {
                    self.browser_click(x, y);
                    return;
                }
                if let Some(r) = self.hit.progress {
                    if contains(r, x, y) && r.width > 0 {
                        if let Some(t) = &self.snap.track {
                            let frac = (x.saturating_sub(r.x)) as f64 / r.width as f64;
                            let ms = (t.duration_ms as f64 * frac) as u64;
                            self.pending_seek = Some((ms, Instant::now()));
                        }
                        return;
                    }
                }
                for (r, a) in self.hit.buttons.clone() {
                    if contains(r, x, y) {
                        self.act(a);
                        return;
                    }
                }
                if let Some(r) = self.hit.art {
                    if contains(r, x, y) {
                        self.act(Action::PlayPause);
                    }
                }
            }
            _ => {}
        }
    }

    pub fn act(&mut self, a: Action) {
        crate::debug(format!("act: {a:?}"));
        match a {
            Action::Quit => self.quit = true,
            Action::ToggleHelp => {
                self.help = !self.help;
                self.needs_clear = true;
            }
            Action::Launch => {
                self.command(Command::Activate);
                self.toast(format!("launching {}…", self.active.app_name()));
            }
            Action::SwitchService => {
                let next = self.active.other();
                self.forced = Some(next);
                self.active = next;
                self.needs_clear = true;
                if self.browser.open {
                    self.close_browser();
                }
                if let Some(p) = self.last_poll.clone() {
                    let s = p.get(next).clone();
                    self.apply_snapshot(s);
                } else {
                    self.apply_snapshot(Snapshot::not_running(next));
                }
                self.player.poke();
                self.toast(format!("{} {}", next.glyph(), next.name()));
            }
            Action::Refresh => {
                self.player.poke();
                self.art_key = None;
                self.lyrics_for = None;
                self.on_track_changed();
                self.toast("refreshed");
            }
            Action::PlayPause => {
                if !self.snap.running {
                    return self.act(Action::Launch);
                }
                let pos = self.position_ms();
                self.snap.position_ms = pos;
                self.snap.taken_at = Instant::now();
                self.snap.state = match self.snap.state {
                    PlayerState::Playing => PlayerState::Paused,
                    _ => PlayerState::Playing,
                };
                self.command(Command::PlayPause);
            }
            Action::Next => {
                self.command(Command::Next);
            }
            Action::Prev => {
                self.command(Command::Prev);
            }
            Action::SeekRel(d) => {
                if let Some(t) = &self.snap.track {
                    let cur = self.position_ms() as i64;
                    let ms = (cur + d).clamp(0, t.duration_ms as i64) as u64;
                    self.pending_seek = Some((ms, Instant::now()));
                }
            }
            Action::VolumeRel(d) => {
                let v = (self.volume() as i16 + d).clamp(0, 100) as u8;
                self.pending_volume = Some((v, Instant::now()));
                self.muted_from = None;
                self.toast(format!("volume {v}%"));
            }
            Action::Mute => {
                if let Some(v) = self.muted_from.take() {
                    self.pending_volume = Some((v, Instant::now()));
                    self.toast(format!("unmuted · {v}%"));
                } else {
                    self.muted_from = Some(self.volume());
                    self.pending_volume = Some((0, Instant::now()));
                    self.toast("muted");
                }
            }
            Action::Shuffle => {
                let on = !self.snap.shuffle;
                self.snap.shuffle = on;
                self.command(Command::SetShuffle(on));
                self.toast(if on { "shuffle on" } else { "shuffle off" });
            }
            Action::Repeat => {
                let on = !self.snap.repeat;
                self.snap.repeat = on;
                self.command(Command::SetRepeat(on));
                self.toast(if on { "repeat on" } else { "repeat off" });
            }
            Action::ToggleLyrics => {
                self.show_lyrics = !self.show_lyrics;
                self.needs_clear = true;
                self.toast(if self.show_lyrics { "lyrics on" } else { "lyrics off" });
            }
            Action::ToggleAmbient => {
                self.ambient = !self.ambient;
                self.needs_clear = true;
                self.toast(if self.ambient { "ambient on" } else { "ambient off" });
            }
            Action::CycleLayout => {
                let cur = self.layout_override.unwrap_or(Layout::Cover);
                let next = cur.next();
                self.act(Action::SetLayout(next));
            }
            Action::SetLayout(l) => {
                self.layout_override = Some(l);
                self.needs_clear = true;
                self.toast(format!("layout · {}", l.name()));
            }
            Action::OpenInSpotify => {
                self.command(Command::Activate);
            }
            Action::CopyLink => {
                if let Some(t) = &self.snap.track {
                    let Some(url) = t.url.clone() else {
                        self.toast(format!("{} tracks have no shareable link", self.active.name()));
                        return;
                    };
                    let ok = std::process::Command::new("pbcopy")
                        .stdin(std::process::Stdio::piped())
                        .spawn()
                        .and_then(|mut c| {
                            use std::io::Write;
                            c.stdin.take().unwrap().write_all(url.as_bytes())?;
                            c.wait()
                        })
                        .is_ok();
                    self.toast(if ok { "link copied" } else { "copy failed" });
                }
            }
            Action::LyricsOffset(d) => {
                self.lyrics_offset_ms += d;
                let o = self.lyrics_offset_ms;
                self.toast(format!("lyrics offset {}{} ms", if o >= 0 { "+" } else { "" }, o));
            }
            Action::ScrollLyrics(d) => {
                self.plain_scroll = (self.plain_scroll + d).max(0);
            }
            Action::OpenBrowser(tab) => self.open_browser(tab),
            Action::Like => self.toggle_like(),
        }
    }
}

fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

fn ease(t: f32) -> f32 {
    // smoothstep
    t * t * (3.0 - 2.0 * t)
}

pub fn keymap(k: KeyEvent, help_open: bool) -> Option<Action> {
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    if help_open {
        return match k.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter => Some(Action::ToggleHelp),
            KeyCode::Char('c') if ctrl => Some(Action::Quit),
            _ => None,
        };
    }
    Some(match k.code {
        KeyCode::Char('c') if ctrl => Action::Quit,
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Char('?') => Action::ToggleHelp,
        KeyCode::Char(' ') | KeyCode::Enter => Action::PlayPause,
        KeyCode::Char('n') | KeyCode::Char('>') => Action::Next,
        KeyCode::Char('p') | KeyCode::Char('<') => Action::Prev,
        KeyCode::Right => Action::SeekRel(if shift { 15_000 } else { 5_000 }),
        KeyCode::Left => Action::SeekRel(if shift { -15_000 } else { -5_000 }),
        KeyCode::Char('.') => Action::SeekRel(15_000),
        KeyCode::Char(',') => Action::SeekRel(-15_000),
        KeyCode::Up | KeyCode::Char('+') | KeyCode::Char('=') => Action::VolumeRel(5),
        KeyCode::Down | KeyCode::Char('-') | KeyCode::Char('_') => Action::VolumeRel(-5),
        KeyCode::Char('m') => Action::Mute,
        KeyCode::Char('s') => Action::Shuffle,
        KeyCode::Char('r') => Action::Repeat,
        KeyCode::Char('l') => Action::ToggleLyrics,
        KeyCode::Char('b') => Action::ToggleAmbient,
        KeyCode::Char('v') => Action::CycleLayout,
        KeyCode::Char('/') => Action::OpenBrowser(Some(Tab::Search)),
        KeyCode::Tab => Action::OpenBrowser(None),
        KeyCode::Char('h') => Action::Like,
        KeyCode::Char('1') => Action::SetLayout(Layout::Cover),
        KeyCode::Char('2') => Action::SetLayout(Layout::Split),
        KeyCode::Char('3') => Action::SetLayout(Layout::Lyrics),
        KeyCode::Char('o') => Action::OpenInSpotify,
        KeyCode::Char('y') => Action::CopyLink,
        KeyCode::Char('[') => Action::LyricsOffset(-250),
        KeyCode::Char(']') => Action::LyricsOffset(250),
        KeyCode::Char('j') => Action::ScrollLyrics(1),
        KeyCode::Char('k') => Action::ScrollLyrics(-1),
        KeyCode::Char('R') => Action::Refresh,
        KeyCode::Char('x') => Action::SwitchService,
        _ => return None,
    })
}


// ------------------------------------------------------------- Web API side

impl App {
    fn fetch_me(&mut self) {
        let Some(web) = self.web.clone() else { return };
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            let _ = tx.send(Msg::Me(web.me().map_err(|e| format!("{e:#}"))));
        });
    }

    fn refresh_liked(&mut self) {
        self.liked = None;
        if self.active == Service::AppleMusic {
            self.liked = self.snap.track.as_ref().and_then(|t| t.liked);
            return;
        }
        let Some(web) = self.web.clone() else { return };
        let Some(t) = &self.snap.track else { return };
        if self.liked_for.as_deref() == Some(t.id.as_str()) && self.liked.is_some() {
            return;
        }
        self.liked_for = Some(t.id.clone());
        let uri = t.id.clone();
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            let r = web.library_contains(&uri).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Liked(uri, r));
        });
    }

    fn toggle_like(&mut self) {
        let Some(t) = self.snap.track.clone() else { return };
        let want = !self.liked.unwrap_or(false);
        if self.active == Service::AppleMusic {
            self.liked = Some(want);
            self.command(Command::SetLiked(want));
            self.toast(if want { "♥ favorited" } else { "removed from favorites" });
            return;
        }
        let Some(web) = self.web.clone() else {
            self.toast("connect Spotify first: press / then follow the steps");
            return;
        };
        self.liked = Some(want);
        self.toast(if want { "♥ added to liked songs" } else { "removed from liked songs" });
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            if let Err(e) = web.library_save(&t.id, want) {
                let _ = tx.send(Msg::Error(format!("{e:#}")));
                let _ = tx.send(Msg::Liked(t.id.clone(), Ok(!want)));
            }
        });
    }

    fn open_browser(&mut self, tab: Option<Tab>) {
        if self.browser.open {
            if let Some(t) = tab {
                self.switch_tab(t);
            } else {
                let next = self.browser.tab.next(self.active);
                self.switch_tab(next);
            }
            return;
        }
        self.browser.open = true;
        self.browser.setup = false;
        self.needs_clear = true;
        if !Tab::for_service(self.active).contains(&self.browser.tab) {
            self.browser.tab = Tab::Search;
        }
        if self.web.is_none() && std::env::var_os("AURA_DEMO_BROWSER").is_some() {
            self.browser.set_view(crate::browser::demo_view());
            self.browser.focus = Focus::List;
            return;
        }
        if self.active.caps().needs_web && self.web.is_none() {
            self.browser.setup = true;
            if self.browser.setup_input.is_empty() {
                if let Some(id) = &self.config.client_id {
                    self.browser.setup_input = id.clone();
                }
            }
            return;
        }
        let t = tab.unwrap_or(self.browser.tab);
        self.switch_tab(t);
    }

    fn close_browser(&mut self) {
        self.browser.open = false;
        self.needs_clear = true;
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.browser.tab = tab;
        self.browser.stack.clear();
        self.browser.error = None;
        if self.active == Service::AppleMusic {
            self.switch_tab_music(tab);
            return;
        }
        match tab {
            Tab::Search => {
                self.browser.focus = Focus::Input;
                if self.browser.query.is_empty() {
                    self.browser.set_view(View::Empty);
                }
            }
            Tab::Playlists => {
                self.browser.focus = Focus::List;
                match self.browser.cache_playlists.clone() {
                    Some(p) => self.browser.set_view(View::Playlists(p)),
                    None => {
                        self.browser.set_view(View::Empty);
                        let web = self.web.clone();
                        self.request(move || WebReply::Playlists(web.map(|w| w.my_playlists()).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "loading playlists");
                    }
                }
            }
            Tab::Liked => {
                self.browser.focus = Focus::List;
                let ctx = self.me_id.as_ref().map(|id| format!("spotify:user:{id}:collection"));
                match self.browser.cache_liked.clone() {
                    Some(items) => self.browser.set_view(View::Tracks { title: "liked songs".into(), context: ctx, items }),
                    None => {
                        self.browser.set_view(View::Empty);
                        let web = self.web.clone();
                        self.request(
                            move || WebReply::Tracks("liked songs".into(), ctx, web.map(|w| w.saved_tracks(500)).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))),
                            "loading liked songs",
                        );
                    }
                }
            }
            Tab::Recent => {
                self.browser.focus = Focus::List;
                self.browser.set_view(View::Empty);
                let web = self.web.clone();
                self.request(move || WebReply::Tracks("recently played".into(), None, web.map(|w| w.recently_played()).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "loading history");
            }
            Tab::Top => {
                self.browser.focus = Focus::List;
                self.browser.set_view(View::Empty);
                let web = self.web.clone();
                self.request(move || WebReply::Tracks("your top songs · last 4 weeks".into(), None, web.map(|w| w.top_tracks()).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "loading top songs");
            }
            Tab::Queue => {
                self.browser.focus = Focus::List;
                self.browser.set_view(View::Empty);
                let web = self.web.clone();
                self.request(move || WebReply::Tracks("up next".into(), None, web.map(|w| w.queue()).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "loading queue");
            }
        }
    }

    /// Apple Music tabs are served from the cached local library.
    fn switch_tab_music(&mut self, tab: Tab) {
        self.browser.focus = if tab == Tab::Search { Focus::Input } else { Focus::List };
        if tab == Tab::Search {
            if self.browser.query.is_empty() {
                self.browser.set_view(View::Empty);
            }
            self.ensure_music_library(None);
            return;
        }
        if tab == Tab::Playlists {
            self.browser.set_view(View::Empty);
            self.request(|| WebReply::Playlists(music::playlists().map_err(|e| format!("{e:#}"))), "loading playlists");
            return;
        }
        self.browser.set_view(View::Empty);
        self.ensure_music_library(Some(tab));
    }

    /// Load the Music library once, then (optionally) show a derived tab from it.
    fn ensure_music_library(&mut self, then: Option<Tab>) {
        if let Some(lib) = self.music_lib.clone() {
            if let Some(tab) = then {
                let view = music_tab_view(&lib, tab);
                self.browser.set_view(view);
            }
            return;
        }
        self.request(
            move || WebReply::MusicLibrary(music::library().map_err(|e| format!("{e:#}")), then),
            "reading your Music library",
        );
    }

    /// Run a browser data request on a thread; the reply is tagged with a fresh request id.
    fn request<F>(&mut self, f: F, label: &str)
    where
        F: FnOnce() -> WebReply + Send + 'static,
    {
        let id = self.browser.next_req();
        self.browser.loading = Some(label.to_string());
        self.browser.error = None;
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            let reply = f();
            let _ = tx.send(Msg::Web(id, reply));
        });
    }

    fn on_web_reply(&mut self, req: u64, reply: WebReply) {
        if req != self.browser.req {
            return; // stale
        }
        self.browser.loading = None;
        match reply {
            WebReply::MusicLibrary(Ok(lib), then) => {
                let lib = Arc::new(lib);
                self.music_lib = Some(lib.clone());
                match then {
                    Some(tab) => {
                        let view = music_tab_view(&lib, tab);
                        self.browser.set_view(view);
                    }
                    None => {
                        if !self.browser.query.trim().is_empty() {
                            self.run_search();
                        }
                    }
                }
            }
            WebReply::MusicLibrary(Err(e), _) => self.browser.error = Some(e),
            WebReply::Search(Ok(r)) => {
                self.browser.set_view(View::Search(r));
                self.browser.focus = Focus::List;
            }
            WebReply::Playlists(Ok(p)) => {
                self.browser.cache_playlists = Some(p.clone());
                self.browser.set_view(View::Playlists(p));
            }
            WebReply::Tracks(title, context, Ok(items)) => {
                if title == "liked songs" {
                    self.browser.cache_liked = Some(items.clone());
                }
                if self.browser.stack.is_empty() || self.browser.view.title() != Some(title.as_str()) {
                    self.browser.set_view(View::Tracks { title, context, items });
                } else {
                    self.browser.set_view(View::Tracks { title, context, items });
                }
            }
            WebReply::Albums(title, Ok(items)) => {
                self.browser.set_view(View::Albums { title, items });
            }
            WebReply::Search(Err(e)) | WebReply::Playlists(Err(e)) | WebReply::Tracks(_, _, Err(e)) | WebReply::Albums(_, Err(e)) => {
                self.browser.error = Some(e);
            }
        }
    }

    fn run_search(&mut self) {
        let q = self.browser.query.trim().to_string();
        if q.is_empty() {
            return;
        }
        self.browser.searched = q.clone();
        self.browser.stack.clear();
        if self.active == Service::AppleMusic {
            match self.music_lib.clone() {
                Some(lib) => {
                    let items = music::search(&lib, &q);
                    self.browser.set_view(View::Tracks { title: format!("library · “{q}”"), context: None, items });
                    self.browser.focus = Focus::List;
                }
                None => self.ensure_music_library(None),
            }
            return;
        }
        let web = self.web.clone();
        self.request(move || WebReply::Search(web.map(|w| w.search(&q)).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "searching");
    }

    fn open_row(&mut self, row: Row) {
        match row {
            Row::Track(t, ctx) => {
                // Without a context, play this track followed by the rest of the list.
                let following: Vec<String> = if ctx.is_some() {
                    Vec::new()
                } else {
                    self.browser
                        .rows()
                        .into_iter()
                        .skip(self.browser.selected)
                        .filter_map(|r| if let Row::Track(t, _) = r { Some(t.uri) } else { None })
                        .take(50)
                        .collect()
                };
                self.play_track(&t, ctx.as_deref(), following);
                self.close_browser();
            }
            Row::Playlist(p) => {
                let title = p.name.clone();
                let id = p.id.clone();
                let ctx = Some(p.uri.clone());
                self.browser.push_view(View::Empty);
                if self.active == Service::AppleMusic {
                    self.request(move || WebReply::Tracks(title, ctx, music::playlist_tracks(&id).map_err(|e| format!("{e:#}"))), "loading playlist");
                } else {
                    let web = self.web.clone();
                    self.request(
                        move || WebReply::Tracks(title, ctx, web.map(|w| w.playlist_items(&id, 500)).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))),
                        "loading playlist",
                    );
                }
            }
            Row::Album(a) => {
                let title = format!("{} · {}", a.name, a.artists);
                let ctx = Some(a.uri.clone());
                self.browser.push_view(View::Empty);
                let web = self.web.clone();
                self.request(move || WebReply::Tracks(title, ctx, web.map(|w| w.album_tracks(&a)).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "loading album");
            }
            Row::Artist(a) => {
                let title = a.name.clone();
                let id = a.id.clone();
                self.browser.push_view(View::Empty);
                let web = self.web.clone();
                self.request(move || WebReply::Albums(title, web.map(|w| w.artist_albums(&id)).unwrap_or_else(|| Err(anyhow::anyhow!("not connected"))).map_err(|e| format!("{e:#}"))), "loading albums");
            }
            Row::Header(_) | Row::Note(_) => {}
        }
    }

    fn play_track(&mut self, t: &TrackItem, context: Option<&str>, following: Vec<String>) {
        self.toast(format!("▶ {} · {}", t.name, t.artists));
        if self.active == Service::AppleMusic {
            match context {
                Some(c) => self.command(Command::PlayInContext(t.uri.clone(), c.to_string())),
                None => self.command(Command::PlayUri(t.uri.clone())),
            }
            self.player.poke();
            return;
        }
        self.last_cmd_at = Instant::now();
        let uri = t.uri.clone();
        let ctx = context.map(|c| c.to_string());
        let uris = if following.is_empty() { vec![uri.clone()] } else { following };
        self.player_action(
            move |web| web.with_device(|dev| web.play(dev, ctx.as_deref(), &uris, Some(&uri))),
            match context {
                Some(c) => Command::PlayInContext(t.uri.clone(), c.to_string()),
                None => Command::PlayUri(t.uri.clone()),
            },
        );
    }

    /// Run a playback action through the Web API on a thread (it never raises the
    /// Spotify window); fall back to AppleScript only when the API cannot do it.
    fn player_action<F>(&mut self, via_web: F, fallback: Command)
    where
        F: FnOnce(&WebApi) -> anyhow::Result<()> + Send + 'static,
    {
        let web = self.web.clone();
        let player = self.player.clone();
        let service = self.active;
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            let result = match &web {
                Some(w) => via_web(w),
                None => Err(anyhow::anyhow!("not connected")),
            };
            if let Err(e) = result {
                let premium = e
                    .downcast_ref::<crate::web::ApiError>()
                    .map(|a| a.premium_required())
                    .unwrap_or(false);
                crate::debug(format!("web playback failed ({e:#}); falling back to AppleScript"));
                if premium {
                    let _ = tx.send(Msg::Error("Spotify Premium is needed to start playback from here; using the desktop app instead".into()));
                }
                player.run(service, fallback);
            } else {
                // Give Spotify a moment to switch, then refresh the display.
                thread::sleep(Duration::from_millis(350));
                player.poke();
            }
        });
    }

    /// Play a whole context (playlist / album / artist) from the top.
    fn play_context(&mut self, row: &Row) {
        let (uri, name) = match row {
            Row::Playlist(p) => (p.uri.clone(), p.name.clone()),
            Row::Album(a) => (a.uri.clone(), a.name.clone()),
            Row::Artist(a) => (a.uri.clone(), a.name.clone()),
            Row::Track(t, ctx) => {
                let t = t.clone();
                self.play_track(&t, ctx.as_deref(), Vec::new());
                self.close_browser();
                return;
            }
            _ => return,
        };
        self.close_browser();
        self.toast(format!("▶ {name}"));
        if self.active == Service::AppleMusic {
            self.command(Command::PlayUri(uri));
            self.player.poke();
            return;
        }
        self.last_cmd_at = Instant::now();
        let ctx = uri.clone();
        self.player_action(
            move |web| web.with_device(|dev| web.play(dev, Some(&ctx), &[], None)),
            Command::PlayUri(uri),
        );
    }

    fn queue_row(&mut self, row: &Row) {
        let Row::Track(t, _) = row else {
            self.toast("only songs can be queued");
            return;
        };
        if !self.active.caps().queue {
            self.toast(format!("{} has no queue control", self.active.name()));
            return;
        }
        let Some(web) = self.web.clone() else {
            self.toast("connect Spotify to use the queue");
            return;
        };
        let t = t.clone();
        let tx = self.msg_tx.clone();
        self.toast(format!("+ queued · {}", t.name));
        thread::spawn(move || {
            if let Err(e) = web.with_device(|dev| web.add_to_queue(dev, &t.uri)) {
                let _ = tx.send(Msg::Error(format!("{e:#}")));
            }
        });
    }

    fn start_login(&mut self) {
        let id = self.browser.setup_input.trim().to_string();
        if id.len() < 16 {
            self.browser.setup_status = vec!["that doesn't look like a Client ID (32 hex characters)".into()];
            return;
        }
        self.config.client_id = Some(id);
        if let Err(e) = self.config.save() {
            self.browser.setup_status = vec![format!("could not save config: {e:#}")];
            return;
        }
        self.browser.setup_status = vec!["starting login…".into()];
        let config = self.config.clone();
        let tx = self.msg_tx.clone();
        thread::spawn(move || {
            let tx2 = tx.clone();
            let mut status = move |s: &str| {
                let _ = tx2.send(Msg::LoginStatus(s.to_string()));
            };
            let r = auth::login(&config, &mut status).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::LoginDone(r));
        });
    }

    fn browser_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if k.code == KeyCode::Esc {
            self.close_browser();
            return;
        }
        if self.browser.setup {
            match k.code {
                KeyCode::Enter => self.start_login(),
                KeyCode::Backspace => {
                    self.browser.setup_input.pop();
                }
                KeyCode::Char('u') if ctrl => self.browser.setup_input.clear(),
                KeyCode::Char('o') if ctrl => {
                    let _ = std::process::Command::new("open").arg("https://developer.spotify.com/dashboard").status();
                }
                KeyCode::Char(c) if !ctrl && !c.is_whitespace() => self.browser.setup_input.push(c),
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Tab => {
                let t = self.browser.tab.next(self.active);
                self.switch_tab(t);
            }
            KeyCode::BackTab => {
                let t = self.browser.tab.prev(self.active);
                self.switch_tab(t);
            }
            KeyCode::Char('/') if self.browser.focus == Focus::List => {
                self.browser.tab = Tab::Search;
                self.browser.focus = Focus::Input;
                self.browser.stack.clear();
            }
            KeyCode::Down => {
                self.browser.focus = Focus::List;
                self.browser.move_selection(1);
            }
            KeyCode::Up => {
                if self.browser.focus == Focus::List && self.browser.tab == Tab::Search {
                    let rows = self.browser.rows();
                    let first = rows.iter().position(|r| r.selectable()).unwrap_or(0);
                    if self.browser.selected <= first {
                        self.browser.focus = Focus::Input;
                        return;
                    }
                }
                self.browser.move_selection(-1);
            }
            KeyCode::PageDown => self.browser.move_selection(10),
            KeyCode::PageUp => self.browser.move_selection(-10),
            KeyCode::Home => {
                self.browser.selected = 0;
                self.browser.snap_selection(1);
            }
            KeyCode::End => {
                self.browser.selected = self.browser.rows().len().saturating_sub(1);
                self.browser.snap_selection(-1);
            }
            KeyCode::Enter => {
                if self.browser.focus == Focus::Input {
                    self.run_search();
                } else if let Some(row) = self.browser.selected_row() {
                    self.open_row(row);
                }
            }
            KeyCode::Left => {
                if self.browser.focus == Focus::List && !self.browser.pop_view() && self.browser.tab == Tab::Search {
                    self.browser.focus = Focus::Input;
                }
            }
            KeyCode::Backspace => {
                if self.browser.focus == Focus::Input {
                    self.browser.query.pop();
                } else if !self.browser.pop_view() && self.browser.tab == Tab::Search {
                    self.browser.focus = Focus::Input;
                    self.browser.query.pop();
                }
            }
            KeyCode::Char('u') if ctrl => self.browser.query.clear(),
            KeyCode::Char(c) => {
                if self.browser.focus == Focus::Input {
                    self.browser.query.push(c);
                    return;
                }
                match c {
                    ' ' => self.act(Action::PlayPause),
                    'p' => {
                        if let Some(row) = self.browser.selected_row() {
                            self.play_context(&row);
                        }
                    }
                    'a' | '+' => {
                        if let Some(row) = self.browser.selected_row() {
                            self.queue_row(&row);
                        }
                    }
                    'j' => self.browser.move_selection(1),
                    'k' => self.browser.move_selection(-1),
                    'q' => self.close_browser(),
                    'n' => self.act(Action::Next),
                    'r' if self.browser.tab != Tab::Search => {
                        // reload the current tab
                        self.browser.cache_playlists = None;
                        self.browser.cache_liked = None;
                        self.music_lib = None;
                        let t = self.browser.tab;
                        self.switch_tab(t);
                    }
                    _ => {
                        if self.browser.tab == Tab::Search {
                            self.browser.focus = Focus::Input;
                            self.browser.query.push(c);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn browser_click(&mut self, x: u16, y: u16) {
        for (r, a) in self.hit.buttons.clone() {
            if contains(r, x, y) {
                self.act(a);
                return;
            }
        }
        if let Some(list) = self.hit.browser_list {
            if contains(list, x, y) {
                let idx = self.browser.scroll + (y - list.y) as usize;
                let rows = self.browser.rows();
                if idx < rows.len() && rows[idx].selectable() {
                    if self.browser.selected == idx && self.browser.focus == Focus::List {
                        self.open_row(rows[idx].clone());
                    } else {
                        self.browser.selected = idx;
                        self.browser.focus = Focus::List;
                    }
                }
                return;
            }
        }
        if let Some(input) = self.hit.browser_input {
            if contains(input, x, y) {
                self.browser.focus = Focus::Input;
                return;
            }
        }
        if let Some(panel) = self.hit.browser_panel {
            if !contains(panel, x, y) {
                self.close_browser();
            }
        }
    }
}


/// Derive a Music browser tab from the cached library.
fn music_tab_view(lib: &[music::LibraryTrack], tab: Tab) -> View {
    let mut rows: Vec<&music::LibraryTrack> = lib.iter().collect();
    let title = match tab {
        Tab::Liked => {
            rows.retain(|t| t.favorited);
            rows.sort_by(|a, b| b.added.cmp(&a.added));
            "favorite songs"
        }
        Tab::Recent => {
            rows.sort_by(|a, b| b.added.cmp(&a.added));
            "recently added"
        }
        Tab::Top => {
            rows.retain(|t| t.plays > 0);
            rows.sort_by(|a, b| b.plays.cmp(&a.plays));
            "most played"
        }
        _ => "library",
    };
    View::Tracks {
        title: title.into(),
        context: None,
        items: rows.iter().take(300).map(|t| t.item()).collect(),
    }
}
