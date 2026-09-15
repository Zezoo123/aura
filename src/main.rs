//! aura: a now-playing display for Spotify that lives in your terminal.

mod app;
mod art;
mod auth;
mod browser;
mod config;
mod lyrics;
mod spotify;
mod theme;
mod ui;
mod web;

use std::{
    io::{stdout, IsTerminal},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand, ValueEnum};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::SetTitle,
};
use ratatui::backend::Backend;
use ratatui_image::picker::{cap_parser::QueryStdioOptions, Picker, ProtocolType};

use app::{App, Layout, Msg, Options};
use config::Config;
use spotify::{Command, Spotify};

#[derive(Parser, Debug)]
#[command(name = "aura", version, about = "Album art, adaptive colors and synced lyrics for Spotify, in your terminal.")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Sub>,

    /// Start in a specific layout (default adapts to the window size).
    #[arg(long, value_enum)]
    layout: Option<LayoutArg>,

    /// Force a graphics protocol instead of auto-detecting.
    #[arg(long, value_enum)]
    protocol: Option<ProtoArg>,

    /// Disable the blurred album-art backdrop.
    #[arg(long)]
    no_ambient: bool,

    /// Start with lyrics hidden.
    #[arg(long)]
    no_lyrics: bool,

    /// Shift synced lyrics by this many milliseconds (positive = later).
    #[arg(long, default_value_t = 0)]
    lyrics_offset: i64,

    /// Redraws per second.
    #[arg(long, default_value_t = 30)]
    fps: u32,

    /// Background state poll interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    poll_ms: u64,
}

#[derive(Subcommand, Debug)]
enum Sub {
    /// Print the current player state as JSON and exit.
    Status,
    /// Connect a Spotify account (enables search, playlists, liked songs, queue).
    Login {
        /// Client ID of your Spotify developer app (saved to ~/.config/aura/config.toml).
        #[arg(long)]
        client_id: Option<String>,
        /// Loopback port for the redirect URI (default 8888).
        #[arg(long)]
        port: Option<u16>,
    },
    /// Forget the saved Spotify tokens.
    Logout,
    /// Search Spotify and print results as JSON (needs `aura login`).
    Search { query: Vec<String> },
    /// List your playlists as JSON (needs `aura login`).
    Playlists,
    /// Toggle play / pause.
    Toggle,
    Play,
    Pause,
    Next,
    Prev,
    /// Seek to a position in seconds.
    Seek { seconds: f64 },
    /// Set the volume (0-100).
    Volume { level: u8 },
    /// Play a Spotify URI or open.spotify.com link (track, album, playlist…).
    Open { uri: String },
    /// Print the cache directory.
    Cache,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LayoutArg {
    Cover,
    Split,
    Lyrics,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProtoArg {
    Halfblocks,
    Sixel,
    Kitty,
    Iterm2,
}

pub fn cache_dir() -> Result<PathBuf> {
    let base = dirs::cache_dir().ok_or_else(|| anyhow!("no cache directory available"))?;
    Ok(base.join("aura"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(sub) = cli.cmd {
        return run_sub(sub);
    }
    let config = Config::load().unwrap_or_else(|e| {
        eprintln!("warning: {e:#}");
        Config::default()
    });
    let tokens = auth::load_tokens().unwrap_or_else(|e| {
        eprintln!("warning: {e:#}");
        None
    });
    if !stdout().is_terminal() {
        return Err(anyhow!("aura needs a terminal (try `aura status` for machine-readable output)"));
    }

    // Query the terminal for graphics support before anything else touches stdin.
    let mut picker = match cli.protocol {
        Some(ProtoArg::Halfblocks) => Picker::halfblocks(),
        _ => {
            let mut options = QueryStdioOptions::default();
            // iTerm2 does not speak the Kitty protocol and older builds echo the
            // Kitty capability query into the window title, so skip that query.
            let is_iterm = ["TERM_PROGRAM", "LC_TERMINAL"]
                .iter()
                .any(|v| std::env::var(v).is_ok_and(|s| s.contains("iTerm")));
            if is_iterm {
                options.blacklist_protocols.push(ProtocolType::Kitty);
            }
            Picker::from_query_stdio_with_options(options).unwrap_or_else(|_| Picker::halfblocks())
        }
    };
    if let Some(p) = cli.protocol {
        picker.set_protocol_type(match p {
            ProtoArg::Halfblocks => ProtocolType::Halfblocks,
            ProtoArg::Sixel => ProtocolType::Sixel,
            ProtoArg::Kitty => ProtocolType::Kitty,
            ProtoArg::Iterm2 => ProtocolType::Iterm2,
        });
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let (resize_tx, resize_rx) = mpsc::channel();

    // Image resize/encode worker so big art never stalls the UI.
    {
        let tx = tx.clone();
        thread::Builder::new()
            .name("art-resize".into())
            .spawn(move || {
                for req in resize_rx {
                    match ratatui_image::thread::ResizeRequest::resize_encode(req) {
                        Ok(resp) => {
                            if tx.send(Msg::Resized(resp)).is_err() {
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send(Msg::Error(format!("image encode: {e}")));
                        }
                    }
                }
            })?;
    }

    let spotify = Spotify::start(tx.clone(), Duration::from_millis(cli.poll_ms.max(250)));
    let mut app = App::new(
        spotify,
        picker,
        tx.clone(),
        resize_tx,
        Options {
            layout: cli.layout.map(|l| match l {
                LayoutArg::Cover => Layout::Cover,
                LayoutArg::Split => Layout::Split,
                LayoutArg::Lyrics => Layout::Lyrics,
            }),
            ambient: !cli.no_ambient,
            lyrics: !cli.no_lyrics,
            lyrics_offset_ms: cli.lyrics_offset,
            config,
            tokens,
        },
    );

    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableMouseCapture);
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(stdout(), DisableMouseCapture);
        ratatui::restore();
        prev_hook(info);
    }));

    // Input thread.
    {
        let tx = tx.clone();
        thread::Builder::new()
            .name("input".into())
            .spawn(move || loop {
                match event::poll(Duration::from_millis(250)) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            debug(format!("input: {ev:?}"));
                            if tx.send(Msg::Input(ev)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    },
                    Ok(false) => {}
                    Err(_) => break,
                }
            })?;
    }

    let frame = Duration::from_secs_f64(1.0 / cli.fps.clamp(5, 120) as f64);
    let mut next_frame = Instant::now();
    let mut last_title = String::new();
    let result = loop {
        // Process messages until the next frame is due.
        loop {
            let now = Instant::now();
            if now >= next_frame {
                break;
            }
            match rx.recv_timeout(next_frame - now) {
                Ok(msg) => app.handle(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        while let Ok(msg) = rx.try_recv() {
            app.handle(msg);
        }
        if app.quit {
            break Ok(());
        }
        let t0 = Instant::now();
        app.tick();
        if app.needs_clear {
            app.needs_clear = false;
            // Terminal::clear() would query the cursor position and race our input
            // thread for the reply; clear the screen and both buffers by hand instead.
            let _ = terminal.backend_mut().clear_region(ratatui::backend::ClearType::All);
            terminal.swap_buffers();
        }
        if let Err(e) = terminal.draw(|f| ui::draw(f, &mut app)) {
            break Err(anyhow!("draw failed: {e}"));
        }
        let dt = t0.elapsed();
        if dt > Duration::from_millis(40) {
            debug(format!("slow frame: {dt:?}"));
        }
        let title = app.window_title();
        if title != last_title {
            let _ = execute!(stdout(), SetTitle(&title));
            last_title = title;
        }
        next_frame += frame;
        let now = Instant::now();
        if next_frame < now {
            next_frame = now + frame;
        }
    };

    let _ = execute!(stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn web_client() -> Result<web::WebApi> {
    let config = Config::load()?;
    let tokens = auth::load_tokens()?.ok_or_else(|| anyhow!("not connected to Spotify: run `aura login` first"))?;
    Ok(web::WebApi::new(auth::TokenStore::new(tokens), config.market))
}

fn run_sub(sub: Sub) -> Result<()> {
    let cmd = match sub {
        Sub::Login { client_id, port } => {
            let mut config = Config::load()?;
            if let Some(id) = client_id {
                config.client_id = Some(id.trim().to_string());
            }
            if let Some(p) = port {
                config.redirect_port = Some(p);
            }
            if config.client_id.is_none() {
                println!("aura needs a Spotify developer app to search and browse playlists.\n");
                println!("  1. Open https://developer.spotify.com/dashboard and create an app.");
                println!("  2. Set the Redirect URI to exactly:  {}", config.redirect_uri());
                println!("  3. Copy the Client ID and run:  aura login --client-id <ID>\n");
                return Ok(());
            }
            config.save()?;
            let mut status = |s: &str| println!("{s}");
            auth::login(&config, &mut status)?;
            let web = web_client()?;
            match web.me() {
                Ok((name, _)) => println!("Connected as {name}. Tokens saved to ~/.config/aura/auth.json"),
                Err(e) => println!("Tokens saved, but the profile check failed: {e:#}"),
            }
            return Ok(());
        }
        Sub::Logout => {
            if auth::forget_tokens()? {
                println!("Disconnected from Spotify.");
            } else {
                println!("No saved tokens.");
            }
            return Ok(());
        }
        Sub::Search { query } => {
            let q = query.join(" ");
            let web = web_client()?;
            let r = web.search(&q)?;
            let json = serde_json::json!({
                "tracks": r.tracks.iter().map(|t| serde_json::json!({"uri": t.uri, "name": t.name, "artists": t.artists, "album": t.album, "duration_ms": t.duration_ms})).collect::<Vec<_>>(),
                "albums": r.albums.iter().map(|a| serde_json::json!({"uri": a.uri, "name": a.name, "artists": a.artists, "year": a.year})).collect::<Vec<_>>(),
                "artists": r.artists.iter().map(|a| serde_json::json!({"uri": a.uri, "name": a.name})).collect::<Vec<_>>(),
                "playlists": r.playlists.iter().map(|p| serde_json::json!({"uri": p.uri, "name": p.name, "owner": p.owner, "tracks": p.total})).collect::<Vec<_>>(),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }
        Sub::Playlists => {
            let web = web_client()?;
            let list = web.my_playlists()?;
            let json: Vec<_> = list
                .iter()
                .map(|p| serde_json::json!({"uri": p.uri, "name": p.name, "owner": p.owner, "tracks": p.total}))
                .collect();
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }
        Sub::Status => {
            let s = spotify::poll()?;
            let json = serde_json::json!({
                "running": s.running,
                "state": match s.state {
                    spotify::PlayerState::Playing => "playing",
                    spotify::PlayerState::Paused => "paused",
                    spotify::PlayerState::Stopped => "stopped",
                },
                "position_ms": s.position_ms,
                "volume": s.volume,
                "shuffle": s.shuffle,
                "repeat": s.repeat,
                "track": s.track.as_ref().map(|t| serde_json::json!({
                    "id": t.id,
                    "url": t.web_url(),
                    "name": t.name,
                    "artist": t.artist,
                    "album": t.album,
                    "album_artist": t.album_artist,
                    "duration_ms": t.duration_ms,
                    "artwork_url": t.artwork_url,
                    "track_number": t.track_number,
                    "popularity": t.popularity,
                })),
            });
            println!("{}", serde_json::to_string_pretty(&json)?);
            return Ok(());
        }
        Sub::Cache => {
            println!("{}", cache_dir()?.display());
            return Ok(());
        }
        Sub::Toggle => Command::PlayPause,
        Sub::Play => Command::Play,
        Sub::Pause => Command::Pause,
        Sub::Next => Command::Next,
        Sub::Prev => Command::Prev,
        Sub::Seek { seconds } => Command::Seek((seconds.max(0.0) * 1000.0) as u64),
        Sub::Volume { level } => Command::SetVolume(level.min(100)),
        Sub::Open { uri } => Command::PlayUri(spotify::to_uri(&uri)),
    };
    spotify::osascript(&command_script(&cmd))?;
    Ok(())
}

fn command_script(cmd: &Command) -> String {
    // Re-use the backend's script builder.
    spotify::script_for(cmd)
}

/// Append a line to the file named by `AURA_DEBUG`, if set.
pub fn debug(msg: impl AsRef<str>) {
    use std::io::Write;
    if let Ok(path) = std::env::var("AURA_DEBUG") {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() % 1_000_000)
                .unwrap_or(0);
            let _ = writeln!(f, "[{t:06}] {}", msg.as_ref());
        }
    }
}
