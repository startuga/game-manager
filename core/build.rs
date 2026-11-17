//! Copies what the app needs next to the built programs: the shipped data in `config/*.json`
//! (`games.json`, which `gm_core::config` reads at run time, and ReShade's pack and add-on lists),
//! and the 7-Zip command line, `tools/7-Zip`, which unpacks downloaded mods.
//!
//! Cargo tells a build script no folder for the finished programs, only its own `OUT_DIR`, so the
//! programs' folder (`target\<profile>`, where `app.exe` and `examples\` are) is the parent of the
//! nearest `build` folder above `OUT_DIR`. That holds for Cargo's layout before Rust 1.100
//! (`target\<profile>\build\gm-core-<hash>\out`) and since (`…\build\gm-core\<hash>\out`), as long
//! as `build.build-dir` isn't moved out of the target folder. When it doesn't hold, the build
//! warns and copies nothing. Unit tests don't depend on it: they read the repository's own files
//! (`config::shipped_file`).

#![allow(
    clippy::expect_used,
    reason = "a build script reports failure by panicking; each message says what failed"
)]

use std::path::{Path, PathBuf};

fn main() {
    let manifest_dir =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("..");
    let config = root.join("config");
    let seven_zip = root.join("tools").join("7-Zip");
    println!("cargo::rerun-if-changed={}", config.display());
    println!("cargo::rerun-if-changed={}", seven_zip.display());

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let Some(profile_dir) = programs_dir(&out_dir) else {
        println!(
            "cargo::warning=games.json and 7-Zip were not copied: no `build` folder above {}",
            out_dir.display()
        );
        return;
    };
    for entry in std::fs::read_dir(&config).expect("list config") {
        let path = entry.expect("read config").path();
        if path.extension().is_some_and(|ext| ext == "json") {
            println!("cargo::rerun-if-changed={}", path.display());
            let name = path.file_name().expect("a config file's name");
            std::fs::copy(&path, profile_dir.join(name)).expect("copy a config file");
        }
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

/// The folder of the finished programs: the parent of the nearest `build` folder above `out_dir`.
fn programs_dir(out_dir: &Path) -> Option<&Path> {
    out_dir
        .ancestors()
        .find(|dir| dir.file_name().is_some_and(|name| name == "build"))?
        .parent()
}
