//! Copies the shipped game configuration, `config/games.json`, next to the built programs, where
//! `gm_core::config` reads it at run time.

#![allow(
    clippy::expect_used,
    reason = "a build script reports failure by panicking; each message says what failed"
)]

use std::path::PathBuf;

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source = manifest_dir.join("..").join("config").join("games.json");
    println!("cargo::rerun-if-changed={}", source.display());

    // OUT_DIR is `target\<profile>\build\gm-core-<hash>\out`; the programs are in `target\<profile>`.
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    if source.is_file() {
        std::fs::copy(&source, profile_dir.join("games.json")).expect("copy config/games.json");
    }
}
