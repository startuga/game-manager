//! Game discovery and launching for Game Manager. Contains no UI code.

pub mod epic;
pub mod gog;
mod launcher;
pub mod steam;
pub mod vdf;

pub use launcher::launch;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Store {
    Steam,
    Epic,
    Gog,
}

impl Store {
    pub fn name(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
            Store::Gog => "GOG",
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

const SCANNERS: [(Store, Scanner); 3] = [
    (Store::Steam, steam::scan),
    (Store::Epic, epic::scan),
    (Store::Gog, gog::scan),
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
    report.games.sort_by_key(|game| game.name.to_lowercase());
    report
}
