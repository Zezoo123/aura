//! Spotify Web API client (only the endpoints still available to Development Mode
//! apps after the February 2026 changes).

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

use crate::auth::{urlencode, TokenStore};

const API: &str = "https://api.spotify.com/v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackItem {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub album: String,
    pub duration_ms: u64,
    pub art_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaylistItem {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub owner: String,
    pub total: u64,
    pub art_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlbumItem {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub artists: String,
    pub year: String,
    pub total_tracks: u64,
    pub art_url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtistItem {
    pub id: String,
    pub uri: String,
    pub name: String,
    pub art_url: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct SearchResults {
    pub tracks: Vec<TrackItem>,
    pub albums: Vec<AlbumItem>,
    pub artists: Vec<ArtistItem>,
    pub playlists: Vec<PlaylistItem>,
}

// ---- raw JSON shapes -------------------------------------------------------

#[derive(Deserialize, Default)]
struct Image {
    url: String,
    #[serde(default)]
    width: Option<u32>,
}

fn pick_image(images: &[Image]) -> Option<String> {
    // Prefer a mid-size image (~300px); fall back to whatever exists.
    images
        .iter()
        .min_by_key(|i| (i.width.unwrap_or(300) as i64 - 300).abs())
        .map(|i| i.url.clone())
}

#[derive(Deserialize, Default)]
struct Named {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize, Default)]
struct RawAlbumRef {
    #[serde(default)]
    name: String,
    #[serde(default)]
    images: Vec<Image>,
}

#[derive(Deserialize, Default)]
struct RawTrack {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
    #[serde(default)]
    album: Option<RawAlbumRef>,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default, rename = "type")]
    kind: String,
}

impl RawTrack {
    fn into_item(self) -> Option<TrackItem> {
        let uri = self.uri?;
        if !uri.starts_with("spotify:track:") && self.kind != "track" {
            return None;
        }
        Some(TrackItem {
            id: self.id.unwrap_or_default(),
            uri,
            name: self.name,
            artists: self.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
            album: self.album.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
            duration_ms: self.duration_ms,
            art_url: self.album.as_ref().and_then(|a| pick_image(&a.images)),
        })
    }
}

#[derive(Deserialize, Default)]
struct RawPlaylist {
    #[serde(default)]
    id: String,
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    owner: Option<RawOwner>,
    #[serde(default)]
    images: Option<Vec<Image>>,
    #[serde(default)]
    items: Option<Total>,
    #[serde(default)]
    tracks: Option<Total>,
}

#[derive(Deserialize, Default)]
struct RawOwner {
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize, Default)]
struct Total {
    #[serde(default)]
    total: u64,
}

impl RawPlaylist {
    fn into_item(self) -> PlaylistItem {
        PlaylistItem {
            total: self.items.or(self.tracks).map(|t| t.total).unwrap_or(0),
            id: self.id,
            uri: self.uri,
            name: self.name,
            owner: self.owner.and_then(|o| o.display_name).unwrap_or_default(),
            art_url: self.images.as_deref().and_then(pick_image),
        }
    }
}

#[derive(Deserialize, Default)]
struct RawAlbum {
    #[serde(default)]
    id: String,
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
    #[serde(default)]
    images: Vec<Image>,
    #[serde(default)]
    release_date: String,
    #[serde(default)]
    total_tracks: u64,
}

#[derive(Deserialize, Default)]
struct RawArtist {
    #[serde(default)]
    id: String,
    #[serde(default)]
    uri: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    images: Vec<Image>,
}

#[derive(Deserialize, Default)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    items: Vec<Option<T>>,
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    total: u64,
}

#[derive(Deserialize, Default)]
struct RawSearch {
    #[serde(default)]
    tracks: Option<Page<RawTrack>>,
    #[serde(default)]
    albums: Option<Page<RawAlbum>>,
    #[serde(default)]
    artists: Option<Page<RawArtist>>,
    #[serde(default)]
    playlists: Option<Page<RawPlaylist>>,
}

#[derive(Deserialize, Default)]
struct RawPlaylistEntry {
    #[serde(default)]
    item: Option<RawTrack>,
    #[serde(default)]
    track: Option<RawTrack>,
}

#[derive(Deserialize, Default)]
struct RawSavedEntry {
    #[serde(default)]
    track: Option<RawTrack>,
    #[serde(default)]
    item: Option<RawTrack>,
}

#[derive(Deserialize, Default)]
struct RawQueue {
    #[serde(default)]
    currently_playing: Option<RawTrack>,
    #[serde(default)]
    queue: Vec<RawTrack>,
}

#[derive(Deserialize, Default)]
struct RawAlbumTrack {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    artists: Vec<Named>,
    #[serde(default)]
    duration_ms: u64,
}

#[derive(Deserialize)]
struct RawError {
    error: RawErrorBody,
}
#[derive(Deserialize)]
struct RawErrorBody {
    #[serde(default)]
    status: u16,
    #[serde(default)]
    message: String,
}

// ---- client ----------------------------------------------------------------

#[derive(Clone)]
pub struct WebApi {
    tokens: TokenStore,
    agent: ureq::Agent,
    pub market: Option<String>,
}

impl WebApi {
    pub fn new(tokens: TokenStore, market: Option<String>) -> WebApi {
        let agent: ureq::Agent = ureq::config::Config::builder()
            .timeout_global(Some(std::time::Duration::from_secs(15)))
            .http_status_as_error(false)
            .user_agent("aura/0.1 (terminal now-playing)")
            .build()
            .new_agent();
        WebApi { tokens, agent, market }
    }

    fn check(&self, mut resp: ureq::http::Response<ureq::Body>, what: &str) -> Result<Option<String>> {
        let status = resp.status().as_u16();
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        match status {
            200..=299 => Ok(if text.trim().is_empty() { None } else { Some(text) }),
            401 => bail!("Spotify rejected the token (run `aura login` again)"),
            403 => {
                let msg = serde_json::from_str::<RawError>(&text).map(|e| e.error.message).unwrap_or_default();
                if msg.to_lowercase().contains("premium") {
                    bail!("{what}: Spotify Premium is required for this action")
                }
                bail!("{what}: forbidden ({})", if msg.is_empty() { "403".into() } else { msg })
            }
            404 => {
                let msg = serde_json::from_str::<RawError>(&text).map(|e| e.error.message).unwrap_or_default();
                bail!("{what}: {}", if msg.is_empty() { "not found".into() } else { msg })
            }
            429 => bail!("{what}: rate limited by Spotify, try again in a moment"),
            _ => {
                let msg = serde_json::from_str::<RawError>(&text)
                    .map(|e| format!("{} ({})", e.error.message, e.error.status))
                    .unwrap_or_else(|_| format!("HTTP {status}"));
                bail!("{what}: {msg}")
            }
        }
    }

    fn get(&self, path: &str, query: &[(&str, &str)], what: &str) -> Result<String> {
        let token = self.tokens.access_token()?;
        let mut req = self.agent.get(format!("{API}{path}")).header("Authorization", format!("Bearer {token}"));
        for (k, v) in query {
            req = req.query(*k, *v);
        }
        let resp = req.call().with_context(|| format!("{what}: network error"))?;
        self.check(resp, what)?.ok_or_else(|| anyhow!("{what}: empty reply"))
    }

    fn send(&self, method: &str, path: &str, query: &[(&str, &str)], body: Option<serde_json::Value>, what: &str) -> Result<()> {
        let token = self.tokens.access_token()?;
        let mut url = format!("{API}{path}");
        if !query.is_empty() {
            url.push('?');
            url.push_str(
                &query
                    .iter()
                    .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
                    .collect::<Vec<_>>()
                    .join("&"),
            );
        }
        let auth = format!("Bearer {token}");
        let resp = match (method, body) {
            ("PUT", Some(b)) => self.agent.put(&url).header("Authorization", &auth).send_json(b),
            ("PUT", None) => self.agent.put(&url).header("Authorization", &auth).header("Content-Length", "0").send_empty(),
            ("POST", Some(b)) => self.agent.post(&url).header("Authorization", &auth).send_json(b),
            ("POST", None) => self.agent.post(&url).header("Authorization", &auth).header("Content-Length", "0").send_empty(),
            ("DELETE", Some(b)) => self.agent.delete(&url).header("Authorization", &auth).force_send_body().send_json(b),
            ("DELETE", None) => self.agent.delete(&url).header("Authorization", &auth).call(),
            _ => bail!("unsupported method {method}"),
        };
        let resp = resp.with_context(|| format!("{what}: network error"))?;
        self.check(resp, what).map(|_| ())
    }

    /// (display name, user id)
    pub fn me(&self) -> Result<(String, String)> {
        let text = self.get("/me", &[], "profile")?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        let name = v.get("display_name").and_then(|d| d.as_str()).unwrap_or("Spotify user").to_string();
        let id = v.get("id").and_then(|d| d.as_str()).unwrap_or_default().to_string();
        Ok((name, id))
    }

    pub fn search(&self, q: &str) -> Result<SearchResults> {
        let mut query = vec![("q", q), ("type", "track,album,artist,playlist"), ("limit", "10")];
        if let Some(m) = &self.market {
            query.push(("market", m.as_str()));
        }
        let text = self.get("/search", &query, "search")?;
        let raw: RawSearch = serde_json::from_str(&text).context("parsing search results")?;
        Ok(SearchResults {
            tracks: raw.tracks.map(|p| p.items.into_iter().flatten().filter_map(|t| t.into_item()).collect()).unwrap_or_default(),
            albums: raw
                .albums
                .map(|p| {
                    p.items
                        .into_iter()
                        .flatten()
                        .map(|a| AlbumItem {
                            art_url: pick_image(&a.images),
                            year: a.release_date.chars().take(4).collect(),
                            id: a.id,
                            uri: a.uri,
                            name: a.name,
                            artists: a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "),
                            total_tracks: a.total_tracks,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            artists: raw
                .artists
                .map(|p| {
                    p.items
                        .into_iter()
                        .flatten()
                        .map(|a| ArtistItem { art_url: pick_image(&a.images), id: a.id, uri: a.uri, name: a.name })
                        .collect()
                })
                .unwrap_or_default(),
            playlists: raw
                .playlists
                .map(|p| p.items.into_iter().flatten().map(|pl| pl.into_item()).collect())
                .unwrap_or_default(),
        })
    }

    /// All of the user's playlists (follows pagination).
    pub fn my_playlists(&self) -> Result<Vec<PlaylistItem>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let off = offset.to_string();
            let text = self.get("/me/playlists", &[("limit", "50"), ("offset", &off)], "playlists")?;
            let page: Page<RawPlaylist> = serde_json::from_str(&text).context("parsing playlists")?;
            let n = page.items.len();
            out.extend(page.items.into_iter().flatten().map(|p| p.into_item()));
            offset += n;
            if page.next.is_none() || n == 0 || offset >= page.total as usize || offset >= 1000 {
                break;
            }
        }
        Ok(out)
    }

    /// Tracks in a playlist (up to `max`).
    pub fn playlist_items(&self, id: &str, max: usize) -> Result<Vec<TrackItem>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let off = offset.to_string();
            let mut q = vec![("limit", "50"), ("offset", off.as_str()), ("additional_types", "track")];
            if let Some(m) = &self.market {
                q.push(("market", m.as_str()));
            }
            let text = self.get(&format!("/playlists/{id}/items"), &q, "playlist tracks")?;
            let page: Page<RawPlaylistEntry> = serde_json::from_str(&text).context("parsing playlist tracks")?;
            let n = page.items.len();
            out.extend(
                page.items
                    .into_iter()
                    .flatten()
                    .filter_map(|e| e.item.or(e.track))
                    .filter_map(|t| t.into_item()),
            );
            offset += n;
            if page.next.is_none() || n == 0 || out.len() >= max || offset >= page.total as usize {
                break;
            }
        }
        Ok(out)
    }

    pub fn album_tracks(&self, album: &AlbumItem) -> Result<Vec<TrackItem>> {
        let text = self.get(&format!("/albums/{}/tracks", album.id), &[("limit", "50")], "album tracks")?;
        let page: Page<RawAlbumTrack> = serde_json::from_str(&text).context("parsing album tracks")?;
        Ok(page
            .items
            .into_iter()
            .flatten()
            .filter_map(|t| {
                Some(TrackItem {
                    id: t.id?,
                    uri: t.uri?,
                    name: t.name,
                    artists: t.artists.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", "),
                    album: album.name.clone(),
                    duration_ms: t.duration_ms,
                    art_url: album.art_url.clone(),
                })
            })
            .collect())
    }

    pub fn artist_albums(&self, artist_id: &str) -> Result<Vec<AlbumItem>> {
        let text = self.get(
            &format!("/artists/{artist_id}/albums"),
            &[("limit", "50"), ("include_groups", "album,single")],
            "artist albums",
        )?;
        let page: Page<RawAlbum> = serde_json::from_str(&text).context("parsing artist albums")?;
        Ok(page
            .items
            .into_iter()
            .flatten()
            .map(|a| AlbumItem {
                art_url: pick_image(&a.images),
                year: a.release_date.chars().take(4).collect(),
                id: a.id,
                uri: a.uri,
                name: a.name,
                artists: a.artists.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "),
                total_tracks: a.total_tracks,
            })
            .collect())
    }

    pub fn saved_tracks(&self, max: usize) -> Result<Vec<TrackItem>> {
        let mut out = Vec::new();
        let mut offset = 0;
        loop {
            let off = offset.to_string();
            let text = self.get("/me/tracks", &[("limit", "50"), ("offset", &off)], "liked songs")?;
            let page: Page<RawSavedEntry> = serde_json::from_str(&text).context("parsing liked songs")?;
            let n = page.items.len();
            out.extend(page.items.into_iter().flatten().filter_map(|e| e.track.or(e.item)).filter_map(|t| t.into_item()));
            offset += n;
            if page.next.is_none() || n == 0 || out.len() >= max {
                break;
            }
        }
        Ok(out)
    }

    pub fn queue(&self) -> Result<Vec<TrackItem>> {
        let text = self.get("/me/player/queue", &[], "queue")?;
        let raw: RawQueue = serde_json::from_str(&text).context("parsing queue")?;
        let _ = raw.currently_playing;
        Ok(raw.queue.into_iter().filter_map(|t| t.into_item()).collect())
    }

    pub fn recently_played(&self) -> Result<Vec<TrackItem>> {
        let text = self.get("/me/player/recently-played", &[("limit", "50")], "recently played")?;
        let page: Page<RawSavedEntry> = serde_json::from_str(&text).context("parsing recently played")?;
        let mut out: Vec<TrackItem> = Vec::new();
        for t in page.items.into_iter().flatten().filter_map(|e| e.track.or(e.item)).filter_map(|t| t.into_item()) {
            if !out.iter().any(|x| x.uri == t.uri) {
                out.push(t);
            }
        }
        Ok(out)
    }

    pub fn top_tracks(&self) -> Result<Vec<TrackItem>> {
        let text = self.get("/me/top/tracks", &[("limit", "50"), ("time_range", "short_term")], "top tracks")?;
        let page: Page<RawTrack> = serde_json::from_str(&text).context("parsing top tracks")?;
        Ok(page.items.into_iter().flatten().filter_map(|t| t.into_item()).collect())
    }

    pub fn add_to_queue(&self, uri: &str) -> Result<()> {
        self.send("POST", "/me/player/queue", &[("uri", uri)], None, "queue")
    }

    pub fn library_contains(&self, track_uri: &str) -> Result<bool> {
        let text = self.get("/me/library/contains", &[("uris", track_uri)], "library check")?;
        let v: serde_json::Value = serde_json::from_str(&text)?;
        Ok(v.get(0).and_then(|b| b.as_bool()).unwrap_or(false))
    }

    pub fn library_save(&self, track_uri: &str, save: bool) -> Result<()> {
        let method = if save { "PUT" } else { "DELETE" };
        self.send(method, "/me/library", &[("uris", track_uri)], None, "library")
    }
}
