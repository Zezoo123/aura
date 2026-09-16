//! Apple Music (Music.app) scripting: transport commands, artwork export and the
//! local library for the browser. Ids are Music's persistent IDs, carried as
//! `music:track:ID` and `music:playlist:ID`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::player::{jxa, osascript, Command};
use crate::web::{PlaylistItem, TrackItem};

fn q(s: &str) -> String {
    s.replace('\\', "").replace('"', "")
}

/// Strip the `music:track:` / `music:playlist:` prefix.
pub fn pid(id: &str) -> &str {
    id.rsplit(':').next().unwrap_or(id)
}

pub fn is_playlist(id: &str) -> bool {
    id.starts_with("music:playlist:")
}

pub fn script(cmd: &Command) -> String {
    let body = match cmd {
        Command::PlayPause => "playpause".to_string(),
        Command::Play => "play".to_string(),
        Command::Pause => "pause".to_string(),
        Command::Next => "next track".to_string(),
        Command::Prev => "previous track".to_string(),
        Command::Seek(ms) => format!("set player position to {}", *ms as f64 / 1000.0),
        Command::SetVolume(v) => format!("set sound volume to {}", (*v).min(100)),
        Command::SetShuffle(on) => format!("set shuffle enabled to {on}"),
        Command::SetRepeat(on) => format!("set song repeat to {}", if *on { "all" } else { "off" }),
        Command::SetLiked(on) => format!("set favorited of current track to {on}"),
        Command::Activate => "activate".to_string(),
        Command::PlayUri(id) if is_playlist(id) => {
            format!("play (first playlist whose persistent ID is \"{}\")", q(pid(id)))
        }
        Command::PlayUri(id) => format!(
            "play (first track of library playlist 1 whose persistent ID is \"{}\")",
            q(pid(id))
        ),
        Command::PlayInContext(id, ctx) => {
            return format!(
                "tell application \"Music\"\n    set pl to first playlist whose persistent ID is \"{}\"\n    play (first track of pl whose persistent ID is \"{}\")\nend tell",
                q(pid(ctx)),
                q(pid(id))
            );
        }
    };
    format!("tell application \"Music\" to {body}")
}

/// Write the artwork of a track to `path` (JPEG or PNG bytes, whatever Music holds).
pub fn export_artwork(track_pid: &str, path: &Path) -> Result<()> {
    let script = format!(
        r#"tell application "Music"
    set trk to missing value
    try
        if persistent ID of current track is "{id}" then set trk to current track
    end try
    if trk is missing value then set trk to (first track of library playlist 1 whose persistent ID is "{id}")
    set d to raw data of artwork 1 of trk
end tell
set f to open for access POSIX file "{path}" with write permission
set eof f to 0
write d to f
close access f
"#,
        id = q(track_pid),
        path = path.display().to_string().replace('"', "")
    );
    osascript(&script).context("exporting artwork from Music")?;
    Ok(())
}

/// One row of the local library.
#[derive(Clone, Debug, Deserialize)]
pub struct LibraryTrack {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: String,
    #[serde(default)]
    pub album: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub favorited: bool,
    #[serde(default)]
    pub added: String,
    #[serde(default)]
    pub plays: u64,
}

impl LibraryTrack {
    pub fn item(&self) -> TrackItem {
        TrackItem {
            id: self.id.clone(),
            uri: format!("music:track:{}", self.id),
            name: self.name.clone(),
            artists: self.artist.clone(),
            album: self.album.clone(),
            duration_ms: (self.duration * 1000.0) as u64,
            art_url: None,
        }
    }
}

const LIBRARY_SCRIPT: &str = r#"
function run() {
  const m = Application('Music');
  if (!m.running()) return '[]';
  const t = m.libraryPlaylists()[0].tracks;
  const ids = t.persistentID(), names = t.name(), artists = t.artist(), albums = t.album(),
        durs = t.duration(), favs = t.favorited(), added = t.dateAdded(), plays = t.playedCount();
  const out = [];
  for (let i = 0; i < ids.length; i++) {
    out.push({id: ids[i], name: names[i], artist: artists[i], album: albums[i], duration: durs[i] || 0,
              favorited: !!favs[i], added: added[i] ? new Date(added[i]).toISOString() : '', plays: plays[i] || 0});
  }
  return JSON.stringify(out);
}
"#;

/// Every track in the library, in one round trip per property.
pub fn library() -> Result<Vec<LibraryTrack>> {
    let raw = jxa(LIBRARY_SCRIPT)?;
    serde_json::from_str(&raw).context("parsing Music library")
}

const PLAYLISTS_SCRIPT: &str = r#"
function run() {
  const m = Application('Music');
  if (!m.running()) return '[]';
  const out = [];
  for (const p of m.userPlaylists()) {
    let kind = 'none';
    try { kind = p.specialKind(); } catch (e) {}
    if (kind !== 'none') continue;
    let n = 0; try { n = p.tracks.length; } catch (e) {}
    out.push({id: p.persistentID(), name: p.name(), count: n});
  }
  return JSON.stringify(out);
}
"#;

#[derive(Deserialize)]
struct RawPlaylist {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    count: u64,
}

pub fn playlists() -> Result<Vec<PlaylistItem>> {
    let raw = jxa(PLAYLISTS_SCRIPT)?;
    let list: Vec<RawPlaylist> = serde_json::from_str(&raw).context("parsing Music playlists")?;
    Ok(list
        .into_iter()
        .map(|p| PlaylistItem {
            uri: format!("music:playlist:{}", p.id),
            id: p.id,
            name: p.name,
            owner: String::new(),
            total: p.count,
            art_url: None,
        })
        .collect())
}

pub fn playlist_tracks(playlist_pid: &str) -> Result<Vec<TrackItem>> {
    let script = format!(
        r#"
function run() {{
  const m = Application('Music');
  const pl = m.playlists.whose({{persistentID: "{id}"}})[0];
  const t = pl.tracks;
  const ids = t.persistentID(), names = t.name(), artists = t.artist(), albums = t.album(), durs = t.duration();
  const out = [];
  for (let i = 0; i < ids.length; i++) out.push({{id: ids[i], name: names[i], artist: artists[i], album: albums[i], duration: durs[i] || 0}});
  return JSON.stringify(out);
}}
"#,
        id = q(playlist_pid)
    );
    let raw = jxa(&script)?;
    let list: Vec<LibraryTrack> = serde_json::from_str(&raw).context("parsing playlist tracks")?;
    Ok(list.iter().map(|t| t.item()).collect())
}

/// Plain lyrics stored on the track in Music, if any.
pub fn lyrics(track_pid: &str) -> Result<Option<String>> {
    let script = format!(
        r#"
function run() {{
  const m = Application('Music');
  let t = null;
  try {{ const c = m.currentTrack(); if (c.persistentID() === "{id}") t = c; }} catch (e) {{}}
  if (!t) {{ const r = m.libraryPlaylists()[0].tracks.whose({{persistentID: "{id}"}}); if (r.length) t = r[0]; }}
  if (!t) return '';
  try {{ return t.lyrics() || ''; }} catch (e) {{ return ''; }}
}}
"#,
        id = q(track_pid)
    );
    let raw = jxa(&script)?;
    Ok(if raw.trim().is_empty() { None } else { Some(raw) })
}

/// Case-insensitive substring search over the cached library, best matches first.
pub fn search(lib: &[LibraryTrack], query: &str) -> Vec<TrackItem> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    let words: Vec<&str> = q.split_whitespace().collect();
    let mut scored: Vec<(i32, &LibraryTrack)> = lib
        .iter()
        .filter_map(|t| {
            let name = t.name.to_lowercase();
            let artist = t.artist.to_lowercase();
            let album = t.album.to_lowercase();
            let hay = format!("{name} {artist} {album}");
            if !words.iter().all(|w| hay.contains(w)) {
                return None;
            }
            let mut score = 0;
            if name.starts_with(&q) {
                score += 30;
            } else if name.contains(&q) {
                score += 20;
            }
            if artist.contains(&q) {
                score += 10;
            }
            if album.contains(&q) {
                score += 5;
            }
            score += (t.plays.min(50)) as i32 / 5;
            Some((score, t))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().take(60).map(|(_, t)| t.item()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_target_music() {
        assert_eq!(script(&Command::SetRepeat(true)), "tell application \"Music\" to set song repeat to all");
        assert!(script(&Command::PlayUri("music:playlist:AB12".into())).contains("first playlist whose persistent ID is \"AB12\""));
        assert!(script(&Command::PlayInContext("music:track:T1".into(), "music:playlist:P1".into())).contains("set pl to first playlist"));
    }

    #[test]
    fn search_ranks_title_matches_first() {
        let lib = vec![
            LibraryTrack { id: "1".into(), name: "Blue Monday".into(), artist: "New Order".into(), album: "Power".into(), duration: 1.0, favorited: false, added: "".into(), plays: 3 },
            LibraryTrack { id: "2".into(), name: "Something".into(), artist: "Blue Band".into(), album: "X".into(), duration: 1.0, favorited: false, added: "".into(), plays: 0 },
        ];
        let r = search(&lib, "blue");
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].name, "Blue Monday");
        assert!(search(&lib, "zzz").is_empty());
        assert_eq!(search(&lib, "blue monday")[0].uri, "music:track:1");
    }
}
