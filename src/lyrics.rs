//! Synced lyrics from LRCLIB (free, no key), cached on disk.

use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::spotify::Track;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LyricLine {
    pub at_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Lyrics {
    pub synced: Vec<LyricLine>,
    pub plain: Vec<String>,
    pub instrumental: bool,
    pub source: String,
}

impl Lyrics {
    pub fn is_empty(&self) -> bool {
        self.synced.is_empty() && self.plain.is_empty() && !self.instrumental
    }

    /// Index of the line active at `pos_ms`, if any.
    pub fn index_at(&self, pos_ms: i64) -> Option<usize> {
        if self.synced.is_empty() {
            return None;
        }
        let mut idx = None;
        for (i, l) in self.synced.iter().enumerate() {
            if (l.at_ms as i64) <= pos_ms {
                idx = Some(i);
            } else {
                break;
            }
        }
        idx
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LrcResponse {
    #[serde(default)]
    instrumental: bool,
    #[serde(default)]
    plain_lyrics: Option<String>,
    #[serde(default)]
    synced_lyrics: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
}

#[derive(Serialize, Deserialize)]
struct Cached {
    lyrics: Option<Lyrics>,
}

fn cache_path(track: &Track) -> Result<std::path::PathBuf> {
    let dir = crate::cache_dir()?.join("lyrics");
    fs::create_dir_all(&dir)?;
    let key = format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}",
        track.name.to_lowercase(),
        track.artist.to_lowercase(),
        track.album.to_lowercase(),
        track.duration_ms / 1000
    );
    let hash = Sha256::digest(key.as_bytes());
    Ok(dir.join(format!("{:x}.json", hash)))
}

pub fn parse_lrc(src: &str) -> Vec<LyricLine> {
    let mut out = Vec::new();
    for raw in src.lines() {
        let line = raw.trim_end();
        let mut rest = line;
        let mut times = Vec::new();
        while rest.starts_with('[') {
            let Some(end) = rest.find(']') else { break };
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            if let Some(ms) = parse_timestamp(tag) {
                times.push(ms);
            }
        }
        let text = rest.trim().to_string();
        for t in times {
            out.push(LyricLine { at_ms: t, text: text.clone() });
        }
    }
    out.sort_by_key(|l| l.at_ms);
    out
}

fn parse_timestamp(tag: &str) -> Option<u64> {
    // mm:ss.xx or mm:ss.xxx or mm:ss
    let (m, s) = tag.split_once(':')?;
    let m: u64 = m.trim().parse().ok()?;
    let secs: f64 = s.trim().parse().ok()?;
    Some(m * 60_000 + (secs * 1000.0).round() as u64)
}

fn primary_artist(artist: &str) -> String {
    let a = artist.split(',').next().unwrap_or(artist);
    a.trim().to_string()
}

fn clean_title(name: &str) -> String {
    // "Song (feat. X)" -> "Song"; "Song - Remastered 2011" -> "Song"
    let mut s = name.to_string();
    for marker in [" (feat.", " (with ", " [feat.", " - Remaster", " - Live", " (Remaster"] {
        if let Some(i) = s.to_lowercase().find(&marker.to_lowercase()) {
            s.truncate(i);
        }
    }
    s.trim().to_string()
}

fn convert(resp: LrcResponse) -> Lyrics {
    let synced = resp.synced_lyrics.as_deref().map(parse_lrc).unwrap_or_default();
    let plain = resp
        .plain_lyrics
        .as_deref()
        .map(|p| p.lines().map(|l| l.to_string()).collect())
        .unwrap_or_default();
    Lyrics {
        synced,
        plain,
        instrumental: resp.instrumental,
        source: "lrclib.net".into(),
    }
}

fn get(agent: &ureq::Agent, track: &Track, title: &str, artist: &str, with_album: bool) -> Result<Option<Lyrics>> {
    let mut req = agent
        .get("https://lrclib.net/api/get")
        .query("track_name", title)
        .query("artist_name", artist)
        .query("duration", &(track.duration_ms / 1000).to_string());
    if with_album {
        req = req.query("album_name", &track.album);
    }
    let resp = req.call();
    match resp {
        Ok(mut r) => {
            let body: LrcResponse = r.body_mut().read_json().context("parsing lrclib reply")?;
            Ok(Some(convert(body)))
        }
        Err(ureq::Error::StatusCode(404)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn search(agent: &ureq::Agent, track: &Track, title: &str, artist: &str) -> Result<Option<Lyrics>> {
    let mut r = agent
        .get("https://lrclib.net/api/search")
        .query("track_name", title)
        .query("artist_name", artist)
        .call()?;
    let list: Vec<LrcResponse> = r.body_mut().read_json().context("parsing lrclib search")?;
    let want = track.duration_ms as f64 / 1000.0;
    let mut best: Option<(f64, LrcResponse)> = None;
    for item in list {
        let has_sync = item.synced_lyrics.as_deref().map(|s| !s.is_empty()).unwrap_or(false);
        let dur_diff = item.duration.map(|d| (d - want).abs()).unwrap_or(999.0);
        let score = dur_diff + if has_sync { 0.0 } else { 30.0 };
        if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
            best = Some((score, item));
        }
    }
    Ok(best.filter(|(s, _)| *s < 60.0).map(|(_, r)| convert(r)))
}

/// Look lyrics up, trying progressively looser matches, and cache the outcome.
pub fn lookup(agent: &ureq::Agent, track: &Track) -> Result<Option<Lyrics>> {
    let path = cache_path(track)?;
    if let Ok(bytes) = fs::read(&path) {
        if let Ok(c) = serde_json::from_slice::<Cached>(&bytes) {
            return Ok(c.lyrics);
        }
    }
    let title = clean_title(&track.name);
    let artist = primary_artist(&track.artist);
    let attempts: Vec<Box<dyn Fn() -> Result<Option<Lyrics>>>> = vec![
        Box::new(|| get(agent, track, &track.name, &track.artist, true)),
        Box::new(|| get(agent, track, &title, &artist, false)),
        Box::new(|| search(agent, track, &title, &artist)),
    ];
    let mut found: Option<Lyrics> = None;
    for a in attempts {
        match a() {
            Ok(Some(l)) if !l.is_empty() => {
                found = Some(l);
                break;
            }
            Ok(_) => {}
            Err(e) => {
                // Network trouble: don't cache, surface the error.
                return Err(e);
            }
        }
    }
    let cached = Cached { lyrics: found.clone() };
    if let Ok(json) = serde_json::to_vec(&cached) {
        let _ = fs::write(&path, json);
    }
    Ok(found)
}
