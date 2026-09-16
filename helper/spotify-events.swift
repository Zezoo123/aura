// aura helper: streams Spotify playback events as JSON lines on stdout.
// Listens to the distributed notification Spotify posts on every state change,
// plus launch/quit of the Spotify app. Exits when its parent process dies.
import Foundation
import AppKit

setvbuf(stdout, nil, _IOLBF, 0)

func emit(_ obj: [String: Any]) {
    if let d = try? JSONSerialization.data(withJSONObject: obj),
       let s = String(data: d, encoding: .utf8) {
        print(s)
    }
}

let dnc = DistributedNotificationCenter.default()
for (name, app) in [("com.spotify.client.PlaybackStateChanged", "spotify"),
                    ("com.apple.Music.playerInfo", "music")] {
    dnc.addObserver(forName: NSNotification.Name(name), object: nil, queue: nil) { note in
        var obj: [String: Any] = ["event": "playback", "app": app]
        if let info = note.userInfo {
            for (k, v) in info { obj["\(k)"] = "\(v)" }
        }
        emit(obj)
    }
}

let ws = NSWorkspace.shared.notificationCenter
for (name, ev) in [(NSWorkspace.didLaunchApplicationNotification, "launched"),
                   (NSWorkspace.didTerminateApplicationNotification, "quit")] {
    ws.addObserver(forName: name, object: nil, queue: nil) { note in
        if let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
           let bid = app.bundleIdentifier, bid == "com.spotify.client" || bid == "com.apple.Music" {
            emit(["event": ev, "app": bid == "com.apple.Music" ? "music" : "spotify"])
        }
    }
}

let parent = getppid()
let watchdog = Timer(timeInterval: 1.0, repeats: true) { _ in
    if getppid() != parent { exit(0) }
}
RunLoop.main.add(watchdog, forMode: .common)
emit(["event": "ready"])
RunLoop.main.run()
