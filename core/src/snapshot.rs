//! The last scan's games, saved in `%LOCALAPPDATA%\Game Manager\cache\library.json` so the next
//! start can show the library at once while a fresh scan runs.

use crate::{Game, json_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Bump whenever `Game` changes in a way an older snapshot cannot be read as; such a snapshot is
/// then skipped instead of misread.
const VERSION: u32 = 1;

#[derive(Default, Deserialize)]
struct Snapshot {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    games: Vec<Game>,
}

#[derive(Serialize)]
struct SnapshotRef<'a> {
    version: u32,
    games: &'a [Game],
}

fn snapshot_file() -> Result<PathBuf, String> {
    Ok(json_file::local_file("cache")?.join("library.json"))
}

/// Saves the games of a scan. Blocking.
pub fn save(games: &[Game]) -> Result<(), String> {
    save_to(&snapshot_file()?, games)
}

/// The games of the last saved scan; `None` when there is none, or it cannot be read, or it was
/// written by an incompatible version. Blocking.
pub fn load() -> Option<Vec<Game>> {
    load_from(&snapshot_file().ok()?)
}

fn save_to(file: &Path, games: &[Game]) -> Result<(), String> {
    json_file::save(
        file,
        &SnapshotRef {
            version: VERSION,
            games,
        },
    )
}

fn load_from(file: &Path) -> Option<Vec<Game>> {
    let snapshot: Snapshot = json_file::load(file).ok()?;
    (snapshot.version == VERSION && !snapshot.games.is_empty()).then_some(snapshot.games)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Arch, IconRef, Launch, LaunchOption, Store};
    use std::fs;

    fn temp_file(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Snapshot-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("library.json")
    }

    fn games() -> Vec<Game> {
        vec![Game {
            id: "gog:1495134320".into(),
            name: "The Witcher 3: Wild Hunt - Game of the Year Edition".into(),
            store: Store::Gog,
            install_dir: Some(PathBuf::from(r"C:\GOG Games\The Witcher 3")),
            launch: Launch::Exe {
                path: PathBuf::from(r"C:\GOG Games\The Witcher 3\REDprelauncher.exe"),
                args: String::new(),
                working_dir: Some(PathBuf::from(r"C:\GOG Games\The Witcher 3")),
            },
            alt_launches: vec![LaunchOption {
                label: "Play via Galaxy".into(),
                launch: Launch::Uri("goggalaxy://openGameView/1495134320".into()),
            }],
            icon: Some(IconRef::file(
                r"C:\GOG Games\The Witcher 3\goggame-1495134320.ico",
            )),
            program: Some(PathBuf::from(
                r"C:\GOG Games\The Witcher 3\bin\x64_dx12\witcher3.exe",
            )),
            arch: Some(Arch::X64),
        }]
    }

    #[test]
    fn saves_and_loads_the_games() {
        let file = temp_file("round-trip");
        assert_eq!(load_from(&file), None);
        save_to(&file, &games()).unwrap();
        assert_eq!(load_from(&file), Some(games()));
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    #[test]
    fn skips_snapshots_it_cannot_use() {
        let file = temp_file("unusable");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, r#"{ "version": 999, "games": [] }"#).unwrap();
        assert_eq!(load_from(&file), None);
        fs::write(&file, "{ not json").unwrap();
        assert_eq!(load_from(&file), None);
        save_to(&file, &[]).unwrap();
        assert_eq!(load_from(&file), None);
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }
}
