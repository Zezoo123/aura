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
