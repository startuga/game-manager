//! Game discovery and launching for Game Manager. Contains no UI code.

pub mod epic;
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
}

impl Store {
    pub fn name(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Launch {
    /// A URI handled by a store client, e.g. `steam://rungameid/570`.
    Uri(String),
    /// A game executable started directly.
    Exe(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Game {
    /// Stable id of the form `<store>:<store id>`.
    pub id: String,
    pub name: String,
    pub store: Store,
    pub install_dir: Option<PathBuf>,
    pub launch: Launch,
}

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub games: Vec<Game>,
    /// One entry per store that failed to scan.
    pub errors: Vec<String>,
}

/// A store scanner. Returns `Ok(vec![])` when the store is not installed.
type Scanner = fn() -> Result<Vec<Game>, String>;

const SCANNERS: [(Store, Scanner); 2] = [(Store::Steam, steam::scan), (Store::Epic, epic::scan)];

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
