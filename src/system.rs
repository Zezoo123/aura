//! "System" player: whatever app is reporting to macOS's Now Playing (Tidal, a
//! browser tab, VLC, ...). Read through the private MediaRemote framework, which
//! osascript is allowed to load on macOS 15.4+ where third-party binaries are not.

use crate::player::{Command, Script};

/// JavaScript that fills `out.system` inside the combined poll script.
pub const POLL_SNIPPET: &str = r#"
  try {
    const mr = $.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/');
    mr.load;
    const Req = $.NSClassFromString('MRNowPlayingRequest');
    if (!Req.isNil()) {
      const path = Req.localNowPlayingPlayerPath;
      const client = path.client;
      if (!client.isNil()) {
        const o = {};
        o.bundle = client.bundleIdentifier.js;
        o.app = client.displayName.js;
        o.playing = !!Req.localIsPlaying;
        const info = Req.localNowPlayingItem.nowPlayingInfo;
        const g = k => { const v = info.valueForKey(k); return v.isNil() ? null : v.js; };
        o.title = g('kMRMediaRemoteNowPlayingInfoTitle');
        o.artist = g('kMRMediaRemoteNowPlayingInfoArtist');
        o.album = g('kMRMediaRemoteNowPlayingInfoAlbum');
        o.duration = g('kMRMediaRemoteNowPlayingInfoDuration');
        const elapsed = g('kMRMediaRemoteNowPlayingInfoElapsedTime') || 0;
        const ts = g('kMRMediaRemoteNowPlayingInfoTimestamp');
        o.position = elapsed + (o.playing && ts ? (Date.now() - ts.getTime()) / 1000 : 0);
        try {
          const app = Application.currentApplication(); app.includeStandardAdditions = true;
          o.volume = app.getVolumeSettings().outputVolume;
        } catch (e) {}
        out.system = o;
      }
    }
  } catch (e) {}
"#;

const PRELUDE: &str = r#"ObjC.import('Foundation');
const mr = $.NSBundle.bundleWithPath('/System/Library/PrivateFrameworks/MediaRemote.framework/');
mr.load;
"#;

fn send(code: u32) -> String {
    format!(
        "{PRELUDE}ObjC.bindFunction('MRMediaRemoteSendCommand', ['bool', ['int', 'id']]);\n$.MRMediaRemoteSendCommand({code}, $());\n"
    )
}

pub fn script(cmd: &Command, bundle: &str) -> Script {
    let js = match cmd {
        Command::Play => send(0),
        Command::Pause => send(1),
        Command::PlayPause => send(2),
        Command::Next => send(4),
        Command::Prev => send(5),
        Command::Seek(ms) => format!(
            "{PRELUDE}ObjC.bindFunction('MRMediaRemoteSetElapsedTime', ['void', ['double']]);\n$.MRMediaRemoteSetElapsedTime({});\n",
            *ms as f64 / 1000.0
        ),
        Command::SetVolume(v) => {
            return Script::applescript(format!("set volume output volume {}", (*v).min(100)));
        }
        Command::Activate => {
            let b = bundle.replace('"', "");
            return Script::applescript(format!("tell application id \"{b}\" to activate"));
        }
        // No universal equivalent; the app layer refuses these before they get here.
        Command::SetShuffle(_) | Command::SetRepeat(_) | Command::SetLiked(_) | Command::PlayUri(_) | Command::PlayInContext(_, _) => {
            "0".to_string()
        }
    };
    Script::jxa(js)
}

/// Apps that have their own richer backend; the system player skips them.
pub fn is_native(bundle: &str) -> bool {
    matches!(bundle, "com.spotify.client" | "com.apple.Music")
}
