//! Game discovery and launching for Game Manager. Contains no UI code.

pub mod ea;
pub mod epic;
pub mod gog;
pub mod icon;
mod launcher;
pub mod steam;
mod text;
pub mod vdf;
pub mod xbox;

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
    Xbox,
}

impl Store {
    /// Every store, in display order.
    pub const ALL: [Store; 5] = [
        Store::Steam,
        Store::Epic,
        Store::Gog,
        Store::Ea,
        Store::Xbox,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
            Store::Gog => "GOG",
            Store::Ea => "EA",
            Store::Xbox => "Xbox",
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

/// Where a game's icon lives: an `.ico` file, or an `.exe`/`.dll` and the index of the icon in it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IconRef {
    pub path: PathBuf,
    pub index: i32,
}

impl IconRef {
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            index: 0,
        }
    }

    /// Parses a registry icon location such as `"C:\Game\game.exe",0` or `C:\Game\game.ico`.
    pub fn parse(location: &str) -> Option<Self> {
        let location = location.trim();
        let (path, index) = match location.rsplit_once(',') {
            Some((path, index)) if index.trim().parse::<i32>().is_ok() => {
                (path, index.trim().parse().unwrap_or(0))
            }
            _ => (location, 0),
        };
        let path = path.trim().trim_matches('"').trim();
        (!path.is_empty()).then(|| Self {
            path: PathBuf::from(path),
            index,
        })
    }
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
    /// The game's icon. When a scanner finds none, the program started by Play is used.
    #[serde(default)]
    pub icon: Option<IconRef>,
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
const SCANNERS: [(Store, Scanner); 5] = [
    (Store::Steam, steam::scan),
    (Store::Epic, epic::scan),
    (Store::Gog, gog::scan),
    (Store::Ea, ea::scan),
    (Store::Xbox, xbox::scan),
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
    for game in &mut report.games {
        default_icon_to_program(game);
    }
    report.games.sort_by_key(|game| game.name.to_lowercase());
    report
}

/// A game started through a program shows that program's icon unless the scanner found a better one.
fn default_icon_to_program(game: &mut Game) {
    if game.icon.is_none()
        && let Launch::Exe { path, .. } = &game.launch
    {
        game.icon = Some(IconRef::file(path));
    }
}

/// Keeps only the first game for each install folder.
fn dedupe_install_dirs(games: &mut Vec<Game>) {
    let mut seen = HashSet::new();
    games.retain(|game| match &game.install_dir {
        Some(dir) => seen.insert(folder_key(dir)),
        None => true,
    });
}

/// Windows uninstall entries, 64-bit and 32-bit views (under `HKEY_LOCAL_MACHINE`).
pub(crate) const UNINSTALL_KEYS: [&str; 2] = [
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
];

/// The path with the letter case it has on disk (Steam stores its own folder in lowercase).
/// Paths that cannot be resolved are returned unchanged.
pub(crate) fn on_disk_case(path: &Path) -> PathBuf {
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return path.to_path_buf();
    };
    // canonicalize returns verbatim paths (`\\?\C:\...`); show them the usual way.
    let text = canonical.to_string_lossy();
    if let Some(share) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{share}"))
    } else if let Some(local) = text.strip_prefix(r"\\?\") {
        PathBuf::from(local)
    } else {
        canonical
    }
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
            icon: None,
        }
    }

    #[test]
    fn restores_on_disk_letter_case() {
        let dir = std::env::temp_dir().join(format!("GM-Case-Test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lowercase = PathBuf::from(dir.to_string_lossy().to_lowercase());
        let restored = on_disk_case(&lowercase);
        assert!(restored.ends_with(dir.file_name().unwrap()), "{restored:?}");
        assert!(!restored.to_string_lossy().starts_with(r"\\?\"));
        std::fs::remove_dir_all(&dir).unwrap();

        let missing = PathBuf::from(r"c:\does\not\exist");
        assert_eq!(on_disk_case(&missing), missing);
    }

    #[test]
    fn parses_registry_icon_locations() {
        // Real DisplayIcon values from EA uninstall entries.
        assert_eq!(
            IconRef::parse(r#""C:\Program Files\EA Games\Bejeweled 3\Bejeweled3.exe""#),
            Some(IconRef::file(
                r"C:\Program Files\EA Games\Bejeweled 3\Bejeweled3.exe"
            ))
        );
        assert_eq!(
            IconRef::parse(r"C:\Program Files\EA Games\Theme Hospital\game.ico"),
            Some(IconRef::file(
                r"C:\Program Files\EA Games\Theme Hospital\game.ico"
            ))
        );
        assert_eq!(
            IconRef::parse(r#""C:\Games\X\x.exe",-101"#),
            Some(IconRef {
                path: PathBuf::from(r"C:\Games\X\x.exe"),
                index: -101,
            })
        );
        // A comma that is part of the path, not an index.
        assert_eq!(
            IconRef::parse(r"C:\Games\Hello, World\a.ico"),
            Some(IconRef::file(r"C:\Games\Hello, World\a.ico"))
        );
        assert_eq!(IconRef::parse("  "), None);
    }

    #[test]
    fn icon_defaults_to_the_program_started_by_play() {
        let mut exe_game = game("gog:1", Store::Gog, None);
        exe_game.launch = Launch::Exe {
            path: PathBuf::from(r"C:\GOG Games\X\x.exe"),
            args: String::new(),
            working_dir: None,
        };
        default_icon_to_program(&mut exe_game);
        assert_eq!(exe_game.icon, Some(IconRef::file(r"C:\GOG Games\X\x.exe")));

        let mut uri_game = game("steam:1", Store::Steam, None);
        default_icon_to_program(&mut uri_game);
        assert_eq!(uri_game.icon, None);

        let mut with_icon = exe_game.clone();
        with_icon.icon = Some(IconRef::file(r"C:\GOG Games\X\goggame-1.ico"));
        default_icon_to_program(&mut with_icon);
        assert_eq!(
            with_icon.icon,
            Some(IconRef::file(r"C:\GOG Games\X\goggame-1.ico"))
        );
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
