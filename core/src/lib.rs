//! Game discovery and launching for Game Manager. Contains no UI code.

mod com;
pub mod config;
pub mod custom;
pub mod ea;
pub mod epic;
mod error;
pub mod gog;
pub mod graphics;
mod http;
pub mod icon;
mod json_file;
mod launcher;
pub mod pcgamingwiki;
pub mod pe;
mod shortcut;
pub mod snapshot;
pub mod steam;
mod text;
pub mod vdf;
pub mod xbox;

pub use error::{Error, Result, ScanError, ScanPart};
pub use launcher::{launch, open_folder};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Store {
    Steam,
    Epic,
    Gog,
    Ea,
    Xbox,
    /// Added by hand in the app (see [`custom`]).
    Custom,
}

impl Store {
    /// Every store, in display order.
    pub const ALL: [Store; 6] = [
        Store::Steam,
        Store::Epic,
        Store::Gog,
        Store::Ea,
        Store::Xbox,
        Store::Custom,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Store::Steam => "Steam",
            Store::Epic => "Epic Games",
            Store::Gog => "GOG",
            Store::Ea => "EA",
            Store::Xbox => "Xbox",
            Store::Custom => "Custom",
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
    /// The game's own executable, when known. Scanners set it when Play does not start the game
    /// directly (Steam); otherwise `scan_all` takes the icon's `.exe` or the program Play starts.
    #[serde(default)]
    pub program: Option<PathBuf>,
    /// The game's processor architecture: from the package (Xbox) or from `program`'s PE header.
    #[serde(default)]
    pub arch: Option<Arch>,
    /// What the user changed in the game's settings (already applied to the fields above), so the
    /// app can show and reset it. `None` when the game is as detected.
    #[serde(default)]
    pub edits: Option<config::GameEdits>,
    /// One of the user's favorites (`game-settings.json`).
    #[serde(default)]
    pub favorite: bool,
}

/// `launch` with these command-line arguments, when it can take them: a program gets them as its
/// arguments, a Steam game as Steam launch options. `None` for other store links.
pub(crate) fn with_args(launch: &Launch, args: &str) -> Option<Launch> {
    match launch {
        Launch::Exe {
            path, working_dir, ..
        } => Some(Launch::Exe {
            path: path.clone(),
            args: args.to_owned(),
            working_dir: working_dir.clone(),
        }),
        Launch::Uri(uri) => steam::run_uri_with_args(uri, args).map(Launch::Uri),
    }
}

/// Whether Play can pass command-line arguments to this launch (see `with_args`).
pub fn accepts_args(launch: &Launch) -> bool {
    with_args(launch, "").is_some()
}

/// The processor architecture a game's program is built for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Arch {
    X86,
    X64,
    Arm64,
    Arm,
    /// A .NET program without a fixed architecture: it runs as 64-bit on 64-bit Windows.
    AnyCpu,
}

impl Arch {
    /// 32 or 64, as the program runs on 64-bit Windows.
    pub fn bits(self) -> u8 {
        match self {
            Arch::X86 | Arch::Arm => 32,
            Arch::X64 | Arch::Arm64 | Arch::AnyCpu => 64,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Arch::X86 => "32-bit (x86)",
            Arch::X64 => "64-bit (x64)",
            Arch::Arm64 => "64-bit (ARM64)",
            Arch::Arm => "32-bit (ARM)",
            Arch::AnyCpu => "Any CPU (.NET)",
        }
    }
}

#[derive(Debug, Default)]
pub struct ScanReport {
    pub games: Vec<Game>,
    /// One entry per store or configuration file that could not be read.
    pub errors: Vec<ScanError>,
}

/// A store scanner. Returns `Ok(vec![])` when the store is not installed.
type Scanner = fn() -> Result<Vec<Game>>;

/// Scanners in priority order: when two stores report the same install folder, the earlier one
/// keeps the game (e.g. an EA game bought on Steam also ships EA's `__Installer` folder).
const SCANNERS: [(Store, Scanner); 5] = [
    (Store::Steam, steam::scan),
    (Store::Epic, epic::scan),
    (Store::Gog, gog::scan),
    (Store::Ea, ea::scan),
    (Store::Xbox, xbox::scan),
];

/// Scans all supported stores, adds the custom games and returns them all sorted by name.
pub fn scan_all() -> ScanReport {
    let mut report = ScanReport::default();
    let mut failed = |part, error| report.errors.push(ScanError { part, error });
    for (store, scan) in SCANNERS {
        match scan() {
            Ok(games) => report.games.extend(games),
            Err(error) => failed(ScanPart::Store(store), error),
        }
    }
    // A configuration file that cannot be read is reported and skipped; it never hides games.
    let defaults = config::load_defaults().unwrap_or_else(|error| {
        failed(ScanPart::GameConfiguration, error);
        config::GameDefaults::default()
    });
    let settings = config::load_settings().unwrap_or_else(|error| {
        failed(ScanPart::GameSettings, error);
        config::GameSettings::default()
    });
    // Configured programs and then the derived ones first, so a duplicate dropped below can still
    // give its program.
    config::apply(&mut report.games, &defaults, &settings);
    report.games.iter_mut().for_each(default_program);
    dedupe_install_dirs(&mut report.games);
    // After deduplication: a game added by hand stays even when a store reports its folder too.
    match custom::scan() {
        Ok(mut games) => {
            config::apply(&mut games, &defaults, &settings);
            report.games.extend(games);
        }
        Err(error) => report.errors.push(ScanError {
            part: ScanPart::Store(Store::Custom),
            error,
        }),
    }
    report.games.iter_mut().for_each(complete_game);
    sort_games(&mut report.games);
    report
}

/// Fills what a scanner left out: the game's program, its icon and its architecture. Reads the
/// program's header, so it is blocking. `scan_all` calls it for every game.
pub fn complete_game(game: &mut Game) {
    default_program(game);
    default_icon_to_program(game);
    if game.arch.is_none() {
        game.arch = game.program.as_deref().and_then(pe::architecture);
    }
}

/// When the scanner named no program: the icon's `.exe` if it is in the install folder (an EA
/// game's icon is the game while Play may start a launcher), else the `.exe` that Play starts.
fn default_program(game: &mut Game) {
    if game.program.is_some() {
        return;
    }
    let icon_exe = game.icon.as_ref().map(|icon| &icon.path).filter(|path| {
        is_exe(path)
            && game
                .install_dir
                .as_deref()
                .is_none_or(|dir| folder_key(path).starts_with(&format!("{}\\", folder_key(dir))))
    });
    game.program = icon_exe.cloned().or_else(|| match &game.launch {
        Launch::Exe { path, .. } if is_exe(path) => Some(path.clone()),
        _ => None,
    });
}

fn is_exe(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

/// Library order: by name, ignoring case.
pub fn sort_games(games: &mut [Game]) {
    games.sort_by_key(|game| game.name.to_lowercase());
}

/// A game started through a program shows that program's icon unless the scanner found a better one.
fn default_icon_to_program(game: &mut Game) {
    if game.icon.is_none()
        && let Launch::Exe { path, .. } = &game.launch
    {
        game.icon = Some(IconRef::file(path));
    }
}

/// Keeps only the first game for each install folder. A dropped duplicate still gives its program
/// when the kept game has none: Steam hands some games to the EA app (`steam2ea://`) while the EA
/// scanner knows their `.exe`.
fn dedupe_install_dirs(games: &mut Vec<Game>) {
    let mut kept_by_folder: HashMap<String, usize> = HashMap::new();
    let mut kept: Vec<Game> = Vec::with_capacity(games.len());
    for game in games.drain(..) {
        let Some(key) = game.install_dir.as_deref().map(folder_key) else {
            kept.push(game);
            continue;
        };
        match kept_by_folder.get(&key) {
            Some(&first) => {
                if kept[first].program.is_none() {
                    kept[first].program = game.program;
                }
            }
            None => {
                kept_by_folder.insert(key, kept.len());
                kept.push(game);
            }
        }
    }
    *games = kept;
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
            program: None,
            arch: None,
            edits: None,
            favorite: false,
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

    #[test]
    fn programs_and_steam_games_accept_arguments() {
        assert!(accepts_args(&exe(r"C:\Games\x.exe")));
        assert!(accepts_args(&Launch::Uri("steam://rungameid/220".into())));
        assert!(!accepts_args(&Launch::Uri(
            "com.epicgames.launcher://apps/x".into()
        )));
        assert!(!accepts_args(&Launch::Uri(
            r"shell:AppsFolder\Family_abc!Game".into()
        )));
    }

    #[test]
    fn dropped_duplicate_gives_its_program_to_the_kept_game() {
        let folder = r"C:\Steam\steamapps\common\BurnoutPR";
        let steam = game("steam:1238080", Store::Steam, Some(folder));
        let mut ea = game("ea:1", Store::Ea, Some(folder));
        ea.program = Some(PathBuf::from(format!(r"{folder}\BurnoutPR.exe")));
        let mut games = vec![steam.clone(), ea.clone()];
        dedupe_install_dirs(&mut games);
        assert_eq!(games.len(), 1);
        assert_eq!(games[0].id, "steam:1238080");
        assert_eq!(games[0].program, ea.program);

        // The kept game's own program wins.
        let mut steam_with_program = steam;
        steam_with_program.program = Some(PathBuf::from(format!(r"{folder}\Steam.exe")));
        let mut games = vec![steam_with_program.clone(), ea];
        dedupe_install_dirs(&mut games);
        assert_eq!(games[0].program, steam_with_program.program);
    }

    fn exe(path: &str) -> Launch {
        Launch::Exe {
            path: PathBuf::from(path),
            args: String::new(),
            working_dir: None,
        }
    }

    #[test]
    fn program_is_the_icon_exe_in_the_install_folder_or_what_play_starts() {
        // EA: Play starts a launcher, the icon is the game itself.
        let mut ea = game("ea:1", Store::Ea, Some(r"C:\EA\Sims"));
        ea.launch = exe(r"C:\EA\Sims\Bin\TS4_Launcher_x64.exe");
        ea.icon = Some(IconRef::file(r"C:\EA\Sims\Bin\TS4_x64.exe"));
        default_program(&mut ea);
        assert_eq!(
            ea.program,
            Some(PathBuf::from(r"C:\EA\Sims\Bin\TS4_x64.exe"))
        );

        // An `.ico` icon, or an `.exe` icon outside the install folder: what Play starts.
        let mut gog = game("gog:1", Store::Gog, Some(r"C:\GOG\PoP"));
        gog.launch = exe(r"C:\GOG\PoP\PrinceOfPersia.exe");
        gog.icon = Some(IconRef::file(r"C:\GOG\PoP\goggame-1.ico"));
        default_program(&mut gog);
        assert_eq!(
            gog.program,
            Some(PathBuf::from(r"C:\GOG\PoP\PrinceOfPersia.exe"))
        );
        let mut elsewhere = gog.clone();
        elsewhere.program = None;
        elsewhere.icon = Some(IconRef::file(r"C:\GOG\PoP Tools\tool.exe"));
        default_program(&mut elsewhere);
        assert_eq!(elsewhere.program, gog.program);

        // Store URIs and scripts have no program; a program set by the scanner stays.
        let mut steam = game("steam:1", Store::Steam, Some(r"C:\Steam\common\X"));
        default_program(&mut steam);
        assert_eq!(steam.program, None);
        let mut script = game("custom:1", Store::Custom, None);
        script.launch = exe(r"C:\Games\start.bat");
        default_program(&mut script);
        assert_eq!(script.program, None);
        steam.program = Some(PathBuf::from(r"C:\Steam\common\X\x.exe"));
        default_program(&mut steam);
        assert_eq!(
            steam.program,
            Some(PathBuf::from(r"C:\Steam\common\X\x.exe"))
        );
    }

    #[test]
    fn completed_games_get_their_architecture() {
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let wow64 = windows.join(r"SysWOW64\notepad.exe");
        if !wow64.is_file() {
            return;
        }
        let mut notepad = game("custom:1", Store::Custom, None);
        notepad.launch = exe(&wow64.to_string_lossy());
        complete_game(&mut notepad);
        assert_eq!(notepad.program, Some(wow64));
        assert_eq!(notepad.arch, Some(Arch::X86));

        // A value from the scanner (e.g. an Xbox package) is kept.
        let mut packaged = notepad.clone();
        packaged.arch = Some(Arch::Arm64);
        complete_game(&mut packaged);
        assert_eq!(packaged.arch, Some(Arch::Arm64));
    }
}
