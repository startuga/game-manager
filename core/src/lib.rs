//! Game discovery and launching for Game Manager. Contains no UI code.

pub mod ea;
pub mod epic;
pub mod gog;
mod launcher;
pub mod steam;
pub mod vdf;

pub use launcher::launch;

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Store {
    Steam,
    Epic,
    Gog,
    Ea,
}

impl Store {
    pub fn name(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
            Store::Gog => "GOG",
            Store::Ea => "EA",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Launch {
    /// A URI handled by a store client, e.g. `steam://rungameid/570`.
    Uri(String),
    /// A program started directly.
    Exe {
        path: PathBuf,
        /// Command-line arguments, passed as one string.
        args: String,
        /// Folder the program starts in. `None` means the program's own folder.
        working_dir: Option<PathBuf>,
    },
}

/// An alternative way to start a game, offered next to the default Play action.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOption {
    pub label: String,
    pub launch: Launch,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    /// Stable id of the form `<store>:<store id>`.
    pub id: String,
    pub name: String,
    pub store: Store,
    pub install_dir: Option<PathBuf>,
    /// The default Play action.
    pub launch: Launch,
    /// Extra launch actions, e.g. starting a GOG game through GOG Galaxy.
    #[serde(default)]
    pub alt_launches: Vec<LaunchOption>,
}

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub games: Vec<Game>,
    /// One entry per store that failed to scan.
    pub errors: Vec<String>,
}

/// A store scanner. Returns `Ok(vec![])` when the store is not installed.
type Scanner = fn() -> Result<Vec<Game>, String>;

/// Scanners in priority order: when two stores report the same install folder, the earlier one
/// keeps the game (e.g. an EA game bought on Steam also ships EA's `__Installer` folder).
const SCANNERS: [(Store, Scanner); 4] = [
    (Store::Steam, steam::scan),
    (Store::Epic, epic::scan),
    (Store::Gog, gog::scan),
    (Store::Ea, ea::scan),
];

/// Scans all supported stores and returns the games sorted by name.
pub fn scan_all() -> ScanReport {
    let mut report = ScanReport::default();
    for (store, scan) in SCANNERS {
        match scan() {
            Ok(games) => report.games.extend(games),
            Err(e) => report.errors.push(format!("{}: {e}", store.name())),
        }
    }
    dedupe_install_dirs(&mut report.games);
    report.games.sort_by_key(|game| game.name.to_lowercase());
    report
}

/// Keeps only the first game for each install folder.
fn dedupe_install_dirs(games: &mut Vec<Game>) {
    let mut seen = HashSet::new();
    games.retain(|game| match &game.install_dir {
        Some(dir) => seen.insert(folder_key(dir)),
        None => true,
    });
}

/// Case-insensitive comparison key for a folder path.
pub(crate) fn folder_key(dir: &Path) -> String {
    dir.to_string_lossy()
        .trim_end_matches(['\\', '/'])
        .replace('/', "\\")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: &str, store: Store, dir: Option<&str>) -> Game {
        Game {
            id: id.into(),
            name: id.into(),
            store,
            install_dir: dir.map(PathBuf::from),
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
        }
    }

    #[test]
    fn same_install_folder_keeps_first_store() {
        let mut games = vec![
            game(
                "steam:1",
                Store::Steam,
                Some(r"D:\Steam\steamapps\common\Titanfall2"),
            ),
            game("gog:2", Store::Gog, Some(r"C:\GOG Games\X")),
            game(
                "ea:3",
                Store::Ea,
                Some(r"d:\steam\steamapps\common\titanfall2\"),
            ),
            game("ea:4", Store::Ea, None),
            game("ea:5", Store::Ea, None),
        ];
        dedupe_install_dirs(&mut games);
        let ids: Vec<_> = games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["steam:1", "gog:2", "ea:4", "ea:5"]);
    }
}
