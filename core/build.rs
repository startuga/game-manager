//! Copies what the app needs next to the built programs: the shipped game configuration,
//! `config/games.json`, which `gm_core::config` reads at run time, and the 7-Zip command line,
//! `tools/7-Zip`, which unpacks downloaded mods.

#![allow(
    clippy::expect_used,
    reason = "a build script reports failure by panicking; each message says what failed"
)]

use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("..");
    let config = root.join("config").join("games.json");
    let seven_zip = root.join("tools").join("7-Zip");
    println!("cargo::rerun-if-changed={}", config.display());
    println!("cargo::rerun-if-changed={}", seven_zip.display());

    // OUT_DIR is `target\<profile>\build\gm-core-<hash>\out`; the programs are in `target\<profile>`.
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    if config.is_file() {
        std::fs::copy(&config, profile_dir.join("games.json")).expect("copy config/games.json");
    }
    if seven_zip.is_dir() {
        let target = profile_dir.join("7-Zip");
        std::fs::create_dir_all(&target).expect("create the 7-Zip folder");
        for entry in std::fs::read_dir(&seven_zip).expect("list tools/7-Zip") {
            let entry = entry.expect("read tools/7-Zip");
            std::fs::copy(entry.path(), target.join(entry.file_name())).expect("copy 7-Zip");
        }
    }
}
