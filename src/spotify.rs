//! Spotify-specific scripting for the desktop app.

use crate::player::Command;

/// spotify:track:ID -> https://open.spotify.com/track/ID
pub fn web_url(uri: &str) -> String {
    let mut parts = uri.split(':');
    let _ = parts.next();
    match (parts.next(), parts.next()) {
        (Some(kind), Some(id)) => format!("https://open.spotify.com/{kind}/{id}"),
        _ => uri.to_string(),
    }
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

pub fn script(cmd: &Command) -> String {
    let body = match cmd {
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
        // Liking is done through the Web API; the desktop app has no scripting for it.
        Command::SetLiked(_) => "get player state".to_string(),
        Command::Activate => "activate".to_string(),
    };
    format!("tell application \"Spotify\" to {body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_links_to_uris() {
        assert_eq!(to_uri("https://open.spotify.com/track/abc?si=1"), "spotify:track:abc");
        assert_eq!(to_uri("https://open.spotify.com/intl-de/album/xyz"), "spotify:album:xyz");
        assert_eq!(to_uri("spotify:playlist:p"), "spotify:playlist:p");
        assert_eq!(web_url("spotify:track:x"), "https://open.spotify.com/track/x");
    }

    #[test]
    fn command_scripts_escape_quotes() {
        let s = script(&Command::PlayInContext("spotify:track:a\"b".into(), "spotify:playlist:c".into()));
        assert_eq!(s, "tell application \"Spotify\" to play track \"spotify:track:ab\" in context \"spotify:playlist:c\"");
    }
}
