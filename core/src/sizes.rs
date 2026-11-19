//! Each game's size on disk: its install folder walked, every file's length added up.
//!
//! - **The walk** (`measure`) doesn't follow links (junctions, symbolic links): a folder linked
//!   in is counted where it really is, if at all. Subfolders that can't be read (a protected
//!   one) are left out; an install folder that can't be read at all has no size.
//! - **Kept** in `%LOCALAPPDATA%\Game Manager\cache\sizes.json`, each size with when it was
//!   measured: shown at once at the next start, and measured again after `KEEP` (`stale`). The
//!   app measures one game at a time in the background.

use crate::{Game, Result, json_file};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

const CACHE_FILE: &str = "sizes.json";
/// How long a size is kept before the folder is measured again, in seconds.
const KEEP: u64 = 24 * 60 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Measured {
    bytes: u64,
    /// When, in seconds since 1970.
    measured: u64,
}

/// One reader or writer of the cache at a time.
static CACHE: Mutex<()> = Mutex::new(());

fn cache_file() -> Result<PathBuf> {
    Ok(json_file::local_file("cache")?.join(CACHE_FILE))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn load(file: &Path) -> BTreeMap<String, Measured> {
    let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    json_file::load(file).unwrap_or_default()
}

/// The sizes measured before, by game id, however old. Blocking.
pub fn cached() -> HashMap<String, u64> {
    cache_file().map_or_default(|file| {
        load(&file)
            .into_iter()
            .map(|(id, measured)| (id, measured.bytes))
            .collect()
    })
}

/// The games whose size isn't known or was measured over a day ago, the never measured first.
/// Blocking.
pub fn stale(games: &[Game]) -> Vec<Game> {
    let kept = cache_file().map_or_default(|file| load(&file));
    stale_in(games, &kept, now())
}

fn stale_in(games: &[Game], kept: &BTreeMap<String, Measured>, now: u64) -> Vec<Game> {
    let mut stale: Vec<(Option<u64>, &Game)> = games
        .iter()
        .filter(|game| game.install_dir.is_some())
        .filter_map(|game| match kept.get(&game.id) {
            Some(measured) if now.saturating_sub(measured.measured) < KEEP => None,
            Some(measured) => Some((Some(measured.measured), game)),
            None => Some((None, game)),
        })
        .collect();
    stale.sort_by_key(|(measured, _)| *measured);
    stale.into_iter().map(|(_, game)| game.clone()).collect()
}

/// The game's install folder's size in bytes, and keeps it; `None` without a folder that can be
/// read. Blocking: walks the folder.
pub fn measure(game: &Game) -> Option<u64> {
    let bytes = folder_size(game.install_dir.as_deref()?)?;
    if let Ok(file) = cache_file() {
        remember_in(&file, &game.id, bytes, now());
    }
    Some(bytes)
}

fn remember_in(file: &Path, id: &str, bytes: u64, now: u64) {
    let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut kept: BTreeMap<String, Measured> = json_file::load(file).unwrap_or_default();
    kept.insert(
        id.to_owned(),
        Measured {
            bytes,
            measured: now,
        },
    );
    let _ = json_file::save(file, &kept);
}

/// The lengths of the files under `folder`, links not followed; `None` when it can't be read.
pub(crate) fn folder_size(folder: &Path) -> Option<u64> {
    let mut total = 0u64;
    let mut folders = vec![folder.to_path_buf()];
    let mut first = true;
    while let Some(dir) = folders.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            // The install folder itself unreadable: no size. A subfolder: left out.
            Err(_) if first => return None,
            Err(_) => continue,
        };
        first = false;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                folders.push(entry.path());
            } else if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Launch, Store};

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Sizes-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn game(id: &str, dir: Option<&Path>) -> Game {
        Game {
            id: id.into(),
            name: id.into(),
            store: Store::Custom,
            install_dir: dir.map(Path::to_path_buf),
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: None,
            arch: None,
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    #[test]
    fn a_folder_is_its_files_without_links() {
        let dir = temp_dir("walk");
        let game_dir = dir.join("Game");
        fs::create_dir_all(game_dir.join(r"bin\x64")).unwrap();
        fs::write(game_dir.join("data.pak"), vec![0u8; 1000]).unwrap();
        fs::write(game_dir.join(r"bin\x64\game.exe"), vec![0u8; 234]).unwrap();
        // A junction to a big folder elsewhere isn't counted.
        let elsewhere = dir.join("Elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        fs::write(elsewhere.join("huge.bin"), vec![0u8; 5000]).unwrap();
        let link = game_dir.join("linked");
        let made = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&elsewhere)
            .output()
            .unwrap();
        assert!(made.status.success(), "mklink /J");
        assert_eq!(folder_size(&game_dir), Some(1234));
        assert_eq!(folder_size(&dir.join("missing")), None);
        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn sizes_are_kept_a_day_the_unknown_measured_first() {
        let dir = temp_dir("keep");
        let file = dir.join(CACHE_FILE);
        let now = 1_795_000_000;
        remember_in(&file, "a", 10, now - KEEP - 5);
        remember_in(&file, "b", 20, now - 60);
        remember_in(&file, "c", 30, now - KEEP - 100);
        let kept = load(&file);
        let games = [
            game("a", Some(&dir)),
            game("b", Some(&dir)),
            game("c", Some(&dir)),
            game("new", Some(&dir)),
            game("no folder", None),
        ];
        let ids: Vec<String> = stale_in(&games, &kept, now)
            .into_iter()
            .map(|game| game.id)
            .collect();
        assert_eq!(ids, ["new", "c", "a"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
