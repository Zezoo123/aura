# aura

A now-playing display for Spotify that lives in your terminal.

<p align="center">
  <img src="docs/screens/split.png" alt="aura split layout: album art, synced lyrics, art-derived colors" width="900">
</p>

- **Real album art** using your terminal's native graphics (iTerm2, Kitty, WezTerm, Ghostty, Sixel), with a half-block fallback everywhere else.
- **Colors that follow the music.** The whole UI is themed from the album art: accents, gradients, text tones, and a blurred ambient backdrop. Colors cross-fade when the track changes.
- **Synced lyrics** (via [LRCLIB](https://lrclib.net), no API key) that scroll with playback, with a nudge control when they run early or late.
- **Zero setup on macOS.** No Spotify developer app, no OAuth, no Premium requirement. aura talks to the Spotify desktop app directly and gets track changes the instant they happen.
- **Full control.** Play/pause, next/previous, seek, volume, mute, shuffle, repeat, like. Keyboard and mouse.
- **Search and browse.** Press `/` to search songs, artists, albums and playlists; `tab` for your playlists, liked songs, recent plays, top tracks and the queue. Play anything, or queue it.
- **Three layouts plus a mini bar** that adapt to the window size, from a 2-line strip to a full-screen cover.
- **Scriptable.** `aura status` prints JSON, and `aura play | pause | next | prev | seek | volume | open` drive playback from shell scripts.

## Install

Requires macOS with the Spotify desktop app, and a Rust toolchain (`brew install rust`).

```bash
git clone https://github.com/Zezoo123/aura && cd aura
cargo install --path .
```

Then run `aura` in a terminal with graphics support (iTerm2, Kitty, WezTerm, Ghostty). It also works in plain terminals, just with chunkier art.

**First run:** macOS asks whether your terminal app may control Spotify (an Automation permission). Click **Allow**. If you ever deny it, re-enable it under System Settings → Privacy & Security → Automation → your terminal → Spotify.

## Screens

| Cover | Lyrics |
| --- | --- |
| ![cover layout](docs/screens/cover.png) | ![lyrics layout](docs/screens/lyrics.png) |

| Search & browse | Help |
| --- | --- |
| ![browser](docs/screens/browser.png) | ![help](docs/screens/help.png) |

## Search, playlists, liked songs

The now-playing display works with no setup at all. Search and browsing use Spotify's Web API, which needs a developer app on your account (Spotify's rule, not ours; since February 2026 it also requires Premium). It takes two minutes:

1. Open the [Spotify developer dashboard](https://developer.spotify.com/dashboard) and create an app. Any name.
2. Set the Redirect URI to exactly `http://127.0.0.1:8888/callback`.
3. Copy the Client ID, then either press `/` inside aura and paste it, or run:

```bash
aura login --client-id <CLIENT_ID>
```

Your browser opens once to approve access. Tokens are stored in `~/.config/aura/auth.json` (owner-only permissions) and refreshed automatically. `aura logout` forgets them.

Inside the browser panel: `enter` plays a song or opens a playlist/album/artist, `a` (or `+`) adds a song to the queue, `p` plays a whole playlist or album, `←` goes back, `tab` switches sections, `esc` closes. Playing returns you to the player; queueing keeps the panel open so you can add more. Songs opened from a playlist keep playing that playlist afterwards. Playback starts through the Web API, so the Spotify window never comes to the front; if nothing is playing anywhere, aura starts the desktop app hidden in the background.

## Keys

| Key | Action |
| --- | --- |
| `/` | search |
| `tab` | browse: playlists · liked · recent · top · queue |
| `h` | ♥ like / unlike |
| `space` / `enter` | play · pause |
| `n` / `p` | next · previous |
| `←` `→` | seek 5s (`shift` for 15s) |
| `↑` `↓` / `+` `-` | volume |
| `m` | mute |
| `s` / `r` | shuffle · repeat |
| `l` | lyrics on/off |
| `[` `]` | nudge lyrics ±250 ms |
| `b` | ambient backdrop on/off |
| `v` | cycle layout (`1` cover, `2` split, `3` lyrics) |
| `o` | bring Spotify to the front |
| `y` | copy track link |
| `R` | reload art and lyrics |
| `?` | help |
| `q` | quit |

Mouse: click the progress bar to seek, scroll anywhere for volume, click the art to play/pause, click the buttons.

## Options

```
aura [--layout cover|split|lyrics] [--protocol halfblocks|sixel|kitty|iterm2]
     [--no-ambient] [--no-lyrics] [--lyrics-offset MS] [--fps N] [--poll-ms N]
```

## Scripting

```bash
aura status            # JSON: state, position, volume, track metadata, artwork URL
aura toggle | play | pause | next | prev
aura seek 92.5         # seconds
aura volume 40
aura open https://open.spotify.com/track/…   # or spotify:track:…
aura search daft punk  # JSON results (needs aura login)
aura playlists         # JSON list of your playlists (needs aura login)
aura devices           # JSON list of Spotify Connect devices (needs aura login)
```

## How it works

Playback state comes from the Spotify app's scripting interface (JavaScript for Automation, one process per poll). Track and play-state changes arrive through a small Swift helper that subscribes to Spotify's `PlaybackStateChanged` notification, so the UI reacts immediately instead of waiting for the next poll. Artwork is fetched once per album and cached under `~/Library/Caches/aura`, along with lyrics lookups. Colors are extracted with a small k-means pass over the art; image resizing and encoding happen on a worker thread so the UI never stalls. Search and library data come from the Web API (PKCE OAuth, no client secret). Anything you pick plays through the Web API on the active device (or this Mac's Spotify app, launched hidden if needed), falling back to AppleScript only when the API refuses.

## Debugging

Set `AURA_DEBUG=/path/to/log` to append input events, actions, and slow frames to a file.
