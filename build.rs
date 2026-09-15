use std::{env, fs, path::PathBuf, process::Command};

fn main() {
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let dst = out.join("spotify-events");
    println!("cargo:rerun-if-changed=helper/spotify-events.swift");
    println!("cargo:rerun-if-changed=build.rs");

    let built = cfg!(target_os = "macos")
        && Command::new("swiftc")
            .args(["-O", "helper/spotify-events.swift", "-o"])
            .arg(&dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

    if !built {
        fs::write(&dst, b"").unwrap();
        println!("cargo:warning=swiftc unavailable: Spotify event helper disabled, aura will poll instead");
    }
}
