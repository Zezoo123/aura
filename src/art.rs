//! Album art fetching, caching and preparation.

use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use image::DynamicImage;
use sha2::{Digest, Sha256};

use crate::{
    player::ArtRef,
    theme::{Ambient, Theme},
};

pub struct Art {
    pub image: DynamicImage,
    pub theme: Theme,
    pub ambient: Ambient,
}

fn cache_path(url: &str) -> Result<PathBuf> {
    let dir = crate::cache_dir()?.join("art");
    fs::create_dir_all(&dir)?;
    let hash = Sha256::digest(url.as_bytes());
    Ok(dir.join(format!("{:x}.img", hash)))
}

pub fn fetch_bytes(agent: &ureq::Agent, url: &str) -> Result<Vec<u8>> {
    let path = cache_path(url)?;
    if let Ok(bytes) = fs::read(&path) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    let mut resp = agent
        .get(url)
        .call()
        .with_context(|| format!("downloading artwork {url}"))?;
    let bytes = resp.body_mut().read_to_vec()?;
    let tmp = path.with_extension("part");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, &path)?;
    Ok(bytes)
}

pub fn load(agent: &ureq::Agent, art: &ArtRef) -> Result<Art> {
    let bytes = match art {
        ArtRef::None => anyhow::bail!("no artwork"),
        ArtRef::Url(url) => fetch_bytes(agent, url)?,
        ArtRef::Lookup { artist, title, album } => {
            let key = art.key().unwrap_or_default();
            let path = cache_path(&key)?;
            match fs::read(&path) {
                Ok(b) if !b.is_empty() => b,
                _ => {
                    let url = itunes_artwork(agent, artist, title, album)?
                        .ok_or_else(|| anyhow::anyhow!("no artwork found for {title}"))?;
                    let bytes = fetch_bytes(agent, &url)?;
                    let tmp = path.with_extension("part");
                    fs::write(&tmp, &bytes)?;
                    fs::rename(&tmp, &path)?;
                    bytes
                }
            }
        }
        ArtRef::MusicTrack(pid) => {
            let path = cache_path(&format!("music:{pid}"))?;
            match fs::read(&path) {
                Ok(b) if !b.is_empty() => b,
                _ => {
                    let tmp = path.with_extension("part");
                    crate::music::export_artwork(pid, &tmp)?;
                    fs::rename(&tmp, &path)?;
                    fs::read(&path)?
                }
            }
        }
    };
    let image = image::load_from_memory(&bytes).context("decoding artwork")?;
    let theme = Theme::from_image(&image);
    let ambient = Ambient::from_image(&image, &theme);
    Ok(Art {
        image,
        theme,
        ambient,
    })
}

/// Best-effort cover lookup through Apple's public iTunes Search API (no key).
pub fn itunes_artwork(agent: &ureq::Agent, artist: &str, title: &str, album: &str) -> Result<Option<String>> {
    #[derive(serde::Deserialize)]
    struct Hit {
        #[serde(default, rename = "artistName")]
        artist: String,
        #[serde(default, rename = "trackName")]
        track: String,
        #[serde(default, rename = "collectionName")]
        album: String,
        #[serde(default, rename = "artworkUrl100")]
        art: String,
    }
    #[derive(serde::Deserialize)]
    struct Reply {
        #[serde(default)]
        results: Vec<Hit>,
    }
    let term = format!("{artist} {title}");
    let mut resp = agent
        .get("https://itunes.apple.com/search")
        .query("term", term.trim())
        .query("media", "music")
        .query("entity", "song")
        .query("limit", "8")
        .call()
        .context("iTunes search")?;
    let reply: Reply = resp.body_mut().read_json().context("parsing iTunes search")?;
    let norm = |s: &str| s.to_lowercase();
    let (a, t, al) = (norm(artist), norm(title), norm(album));
    let score = |h: &Hit| {
        let mut n = 0;
        let ha = norm(&h.artist);
        let ht = norm(&h.track);
        let hal = norm(&h.album);
        if !a.is_empty() && (ha == a || ha.contains(&a) || a.contains(&ha)) {
            n += 3;
        }
        if ht == t {
            n += 3;
        } else if ht.contains(&t) || t.contains(&ht) {
            n += 2;
        }
        if !al.is_empty() && (hal == al || hal.starts_with(&al) || al.starts_with(&hal)) {
            n += 2;
        }
        n
    };
    let best = reply.results.iter().filter(|h| !h.art.is_empty()).max_by_key(|h| score(h));
    Ok(best.filter(|h| score(h) >= 3).map(|h| h.art.replace("100x100bb", "600x600bb")))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "hits the network"]
    fn itunes_lookup_finds_a_cover() {
        let agent = ureq::Agent::new_with_defaults();
        let url = super::itunes_artwork(&agent, "Drake", "Headlines", "Take Care (Deluxe)").unwrap();
        let url = url.expect("artwork url");
        assert!(url.contains("600x600bb"), "{url}");
        assert!(super::itunes_artwork(&agent, "zzqx", "no such song 12345", "").unwrap().is_none());
    }
}
