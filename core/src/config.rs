//! Game configuration in JSON files, so no game-specific knowledge lives in the code:
//!
//! - `games.json`, shipped next to the app (copied from `config/games.json` at build time): what is
//!   known about particular games, such as their real program when Play starts a launcher, and
//!   entries that are not games (redistributables) to hide. A game is recognised by its target
//!   binaries in its install folder, not by a store id, so one entry works for every store and
//!   computer.
//! - `%APPDATA%\Game Manager\game-settings.json`: the user's own settings per game, such as the
//!   program chosen with "Choose game .exe".
//!
//! For a game's program, the user's choice wins, then the shipped configuration, then what the
//! store scanner found.

use crate::{Game, Launch, Result, json_file, with_args};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULTS_FILE: &str = "games.json";
const SETTINGS_FILE: &str = "game-settings.json";

/// `games.json`: what is known about particular games.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDefaults {
    /// Entries whose name starts with one of these are not games (e.g. "Steamworks" packages).
    #[serde(default)]
    ignore_name_prefixes: Vec<String>,
    /// Known games, in priority order. Each also has a `name` (and may have a `note`) for people.
    #[serde(default)]
    games: Vec<KnownGame>,
}

#[derive(Debug, Default, Deserialize)]
struct KnownGame {
    /// The game's own binaries, relative to its install folder, best first. A game whose install
    /// folder holds one of them is this game; the first that exists is its program.
    #[serde(default)]
    programs: Vec<PathBuf>,
}

impl GameDefaults {
    fn ignores(&self, game: &Game) -> bool {
        self.ignore_name_prefixes
            .iter()
            .any(|prefix| game.name.starts_with(prefix.as_str()))
    }

    /// The program of the first known game whose binaries are in this game's install folder.
    fn program(&self, game: &Game) -> Option<PathBuf> {
        let dir = game.install_dir.as_deref()?;
        self.games.iter().find_map(|known| {
            known
                .programs
                .iter()
                .map(|program| dir.join(program))
                .find(|program| program.is_file())
        })
    }
}

/// `game-settings.json`: the user's own settings, by game id. Fields this version does not know
/// are kept when the file is saved.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GameSettings {
    #[serde(default)]
    games: BTreeMap<String, GameSetting>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GameSetting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program: Option<PathBuf>,
    /// Kept apart from the edits: Reset to detected does not unfavorite a game.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    favorite: bool,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// What the user changed for one game in its settings. `None` means "as detected"; arguments can
/// also be `Some("")`, i.e. explicitly none.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameEdits {
    /// The name shown in the library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The program Play starts instead of the store's own launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<PathBuf>,
    /// The command-line arguments Play passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<String>,
    /// The game's own program (bitness, Open folder and, later, mods).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<PathBuf>,
}

impl GameEdits {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Blank names and paths mean "as detected"; arguments are only trimmed.
    fn normalized(&self) -> Self {
        let text = |value: &Option<String>| {
            value
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        };
        let path = |value: &Option<PathBuf>| {
            value
                .as_deref()
                .filter(|path| !path.as_os_str().is_empty())
                .map(Path::to_path_buf)
        };
        Self {
            name: text(&self.name),
            launch: path(&self.launch),
            args: self.args.as_deref().map(|args| args.trim().to_owned()),
            program: path(&self.program),
        }
    }
}

impl GameSetting {
    fn edits(&self) -> GameEdits {
        GameEdits {
            name: self.name.clone(),
            launch: self.launch.clone(),
            args: self.args.clone(),
            program: self.program.clone(),
        }
    }

    fn set_edits(&mut self, edits: GameEdits) {
        self.name = edits.name;
        self.launch = edits.launch;
        self.args = edits.args;
        self.program = edits.program;
    }

    /// Nothing left to save for the game.
    fn is_empty(&self) -> bool {
        self.edits().is_empty() && !self.favorite && self.other.is_empty()
    }
}

impl GameSettings {
    /// The user's edits for a game, if any.
    fn edits(&self, id: &str) -> Option<GameEdits> {
        Some(self.games.get(id)?.edits()).filter(|edits| !edits.is_empty())
    }

    fn is_favorite(&self, id: &str) -> bool {
        self.games.get(id).is_some_and(|setting| setting.favorite)
    }
}

/// The shipped configuration: `games.json` in the running program's folder, or in its parent
/// (where examples and tests find it). No file means no defaults.
pub fn load_defaults() -> Result<GameDefaults> {
    match defaults_file() {
        Some(file) => json_file::load(&file),
        None => Ok(GameDefaults::default()),
    }
}

fn defaults_file() -> Option<PathBuf> {
    let program = std::env::current_exe().ok()?;
    program
        .ancestors()
        .skip(1)
        .take(2)
        .map(|dir| dir.join(DEFAULTS_FILE))
        .find(|file| file.is_file())
}

/// The user's settings; empty when there are none yet.
pub fn load_settings() -> Result<GameSettings> {
    json_file::load(&json_file::user_file(SETTINGS_FILE)?)
}

/// Saves the user's edits for a game, replacing its previous ones. Empty edits remove them.
pub fn save_edits(id: &str, edits: &GameEdits) -> Result<()> {
    save_edits_in(&json_file::user_file(SETTINGS_FILE)?, id, edits)
}

/// Removes the user's edits for a game, so it is shown and started as detected.
pub fn clear_edits(id: &str) -> Result<()> {
    save_edits(id, &GameEdits::default())
}

/// Adds a game to the user's favorites, or removes it.
pub fn set_favorite(id: &str, favorite: bool) -> Result<()> {
    set_favorite_in(&json_file::user_file(SETTINGS_FILE)?, id, favorite)
}

fn save_edits_in(file: &Path, id: &str, edits: &GameEdits) -> Result<()> {
    update_setting(file, id, |setting| setting.set_edits(edits.normalized()))
}

fn set_favorite_in(file: &Path, id: &str, favorite: bool) -> Result<()> {
    update_setting(file, id, |setting| setting.favorite = favorite)
}

/// Changes one game's entry in the settings file, dropping the entry once nothing is left in it.
fn update_setting(file: &Path, id: &str, change: impl FnOnce(&mut GameSetting)) -> Result<()> {
    // Loading first also means a file that cannot be read is never overwritten.
    let mut settings: GameSettings = json_file::load(file)?;
    let entry = settings.games.entry(id.to_owned()).or_default();
    change(entry);
    if entry.is_empty() {
        settings.games.remove(id);
    }
    json_file::save(file, &settings)
}

/// Applies the configuration to scanned games: drops entries that are not games, applies the
/// user's edits (name, launch program, arguments) and sets each game's program. A program the
/// user chose also decides the architecture, even over a package's.
pub fn apply(games: &mut Vec<Game>, defaults: &GameDefaults, settings: &GameSettings) {
    games.retain(|game| !defaults.ignores(game));
    for game in games.iter_mut() {
        let edits = settings.edits(&game.id);
        if let Some(edits) = &edits {
            apply_edits(game, edits);
        }
        let chosen_program = edits
            .as_ref()
            .and_then(|edits| edits.program.as_deref())
            .filter(|program| program.is_file());
        if let Some(program) = chosen_program {
            game.program = Some(program.to_path_buf());
            game.arch = None;
        } else if let Some(program) = defaults.program(game) {
            game.program = Some(program);
        }
        game.edits = edits;
        game.favorite = settings.is_favorite(&game.id);
    }
}

/// The user's name, launch program and arguments. A launch program that no longer exists is
/// ignored, so Play falls back to the detected launch.
fn apply_edits(game: &mut Game, edits: &GameEdits) {
    if let Some(name) = &edits.name {
        game.name = name.clone();
    }
    let args = edits.args.as_deref();
    match edits.launch.as_deref().filter(|launch| launch.is_file()) {
        Some(program) => {
            game.launch = Launch::Exe {
                path: program.to_path_buf(),
                args: args.unwrap_or_default().to_owned(),
                working_dir: None,
            };
        }
        None => {
            if let Some(launch) = args.and_then(|args| with_args(&game.launch, args)) {
                game.launch = launch;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Arch, Launch, Store};
    use std::fs;

    fn game(id: &str, name: &str, dir: Option<&Path>) -> Game {
        Game {
            id: id.into(),
            name: name.into(),
            store: Store::Steam,
            install_dir: dir.map(Path::to_path_buf),
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: None,
            arch: None,
            edits: None,
            favorite: false,
        }
    }

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Config-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn defaults(json: &str) -> GameDefaults {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn shipped_file_is_valid() {
        let shipped = defaults(include_str!("../../config/games.json"));
        assert!(shipped.ignores(&game(
            "steam:228980",
            "Steamworks Common Redistributables",
            None
        )));
        assert!(
            shipped
                .games
                .iter()
                .any(|known| known.programs == [PathBuf::from(r"bin\x64\Cyberpunk2077.exe")])
        );
    }

    #[test]
    fn hides_entries_by_name_prefix() {
        let config = defaults(r#"{ "ignoreNamePrefixes": ["Steamworks"] }"#);
        let mut games = vec![
            game("steam:228980", "Steamworks Common Redistributables", None),
            game("steam:220", "Half-Life 2", None),
            game("gog:1", "Not Steamworks", None),
        ];
        apply(&mut games, &config, &GameSettings::default());
        let ids: Vec<_> = games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["steam:220", "gog:1"]);
    }

    #[test]
    fn known_games_are_recognised_by_their_binaries_in_any_store() {
        let cyberpunk = temp_dir("binaries-cyberpunk");
        fs::create_dir_all(cyberpunk.join(r"bin\x64")).unwrap();
        fs::write(cyberpunk.join(r"bin\x64\Cyberpunk2077.exe"), b"MZ").unwrap();
        let other = temp_dir("binaries-other");
        fs::write(other.join("game.exe"), b"MZ").unwrap();
        let config = defaults(
            r#"{ "games": [
                { "name": "The Witcher 3", "programs": ["bin\\x64\\witcher3.exe"] },
                { "name": "Cyberpunk 2077", "note": "for people", "programs": ["bin\\x64\\Cyberpunk2077.exe"] }
            ] }"#,
        );
        // The same entry fits the game from every store; a game without the binaries gets none.
        let mut games = vec![
            game("epic:Ginger", "Cyberpunk 2077", Some(&cyberpunk)),
            game("steam:1091500", "Cyberpunk 2077", Some(&cyberpunk)),
            game("gog:1423049311", "Cyberpunk 2077 (GOG)", Some(&cyberpunk)),
            game("steam:220", "Half-Life 2", Some(&other)),
            game("custom:1", "No folder", None),
        ];
        apply(&mut games, &config, &GameSettings::default());
        let expected = cyberpunk.join(r"bin\x64\Cyberpunk2077.exe");
        assert_eq!(games[0].program.as_ref(), Some(&expected));
        assert_eq!(games[1].program.as_ref(), Some(&expected));
        assert_eq!(games[2].program.as_ref(), Some(&expected));
        assert_eq!(games[3].program, None);
        assert_eq!(games[4].program, None);
        fs::remove_dir_all(&cyberpunk).unwrap();
        fs::remove_dir_all(&other).unwrap();
    }

    #[test]
    fn program_precedence_is_user_then_shipped_then_scanner() {
        let dir = temp_dir("precedence");
        fs::create_dir_all(dir.join(r"bin\x64")).unwrap();
        for file in [
            "REDprelauncher.exe",
            r"bin\x64\Cyberpunk2077.exe",
            "chosen.exe",
        ] {
            fs::write(dir.join(file), b"MZ").unwrap();
        }
        let config = defaults(
            r#"{ "games": [ { "name": "Cyberpunk 2077", "programs": ["bin\\x64_gone\\x.exe", "bin\\x64\\Cyberpunk2077.exe"] } ] }"#,
        );
        let scanned = || {
            let mut cyberpunk = game("epic:Ginger", "Cyberpunk 2077", Some(&dir));
            cyberpunk.program = Some(dir.join("REDprelauncher.exe"));
            cyberpunk.arch = Some(Arch::X64);
            vec![cyberpunk]
        };

        // Shipped: the first program that exists, under the install folder.
        let mut games = scanned();
        apply(&mut games, &config, &GameSettings::default());
        assert_eq!(
            games[0].program,
            Some(dir.join(r"bin\x64\Cyberpunk2077.exe"))
        );
        assert_eq!(games[0].arch, Some(Arch::X64));

        // The user's choice wins and lets the chosen program decide the architecture.
        let mut settings = GameSettings::default();
        settings
            .games
            .entry("epic:Ginger".into())
            .or_default()
            .program = Some(dir.join("chosen.exe"));
        let mut games = scanned();
        apply(&mut games, &config, &settings);
        assert_eq!(games[0].program, Some(dir.join("chosen.exe")));
        assert_eq!(games[0].arch, None);

        // A chosen program that no longer exists falls back; nothing configured keeps the scan.
        settings.games.get_mut("epic:Ginger").unwrap().program = Some(dir.join("moved.exe"));
        let mut games = scanned();
        apply(&mut games, &config, &settings);
        assert_eq!(
            games[0].program,
            Some(dir.join(r"bin\x64\Cyberpunk2077.exe"))
        );
        let mut games = scanned();
        apply(
            &mut games,
            &GameDefaults::default(),
            &GameSettings::default(),
        );
        assert_eq!(games[0].program, Some(dir.join("REDprelauncher.exe")));

        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn saves_edits_and_keeps_other_settings() {
        let dir = temp_dir("settings");
        let file = dir.join(SETTINGS_FILE);
        fs::write(
            &file,
            r#"{ "games": { "gog:1": { "program": "C:\\a.exe", "later": 1 } }, "version": 2 }"#,
        )
        .unwrap();
        let edits = GameEdits {
            name: Some("  Cyberpunk  ".into()),
            launch: Some(PathBuf::from(r"C:\Games\bin\x64\Cyberpunk2077.exe")),
            args: Some(" -skipStartScreen ".into()),
            program: Some(PathBuf::new()),
        };
        save_edits_in(&file, "epic:Ginger", &edits).unwrap();

        let saved = || -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap()
        };
        let cyberpunk = &saved()["games"]["epic:Ginger"];
        // Trimmed, and a blank path means "as detected" (not saved).
        assert_eq!(cyberpunk["name"], "Cyberpunk");
        assert_eq!(cyberpunk["launch"], r"C:\Games\bin\x64\Cyberpunk2077.exe");
        assert_eq!(cyberpunk["args"], "-skipStartScreen");
        assert!(cyberpunk.get("program").is_none());
        assert_eq!(saved()["games"]["gog:1"]["later"], 1);
        assert_eq!(saved()["version"], 2);

        // Clearing removes an entry with nothing else in it, and keeps another's unknown fields.
        save_edits_in(&file, "epic:Ginger", &GameEdits::default()).unwrap();
        save_edits_in(&file, "gog:1", &GameEdits::default()).unwrap();
        assert!(saved()["games"].get("epic:Ginger").is_none());
        assert!(saved()["games"]["gog:1"].get("program").is_none());
        assert_eq!(saved()["games"]["gog:1"]["later"], 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_settings_are_not_overwritten() {
        let dir = temp_dir("broken");
        let file = dir.join(SETTINGS_FILE);
        fs::write(&file, "{ broken").unwrap();
        let edits = GameEdits {
            name: Some("x".into()),
            ..GameEdits::default()
        };
        assert!(save_edits_in(&file, "epic:Ginger", &edits).is_err());
        assert!(set_favorite_in(&file, "epic:Ginger", true).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ broken");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn favorites_are_saved_apart_from_the_edits() {
        let dir = temp_dir("favorites");
        let file = dir.join(SETTINGS_FILE);
        fs::write(&file, r#"{ "games": { "gog:1": { "later": 1 } } }"#).unwrap();
        let saved = || -> serde_json::Value {
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap()
        };

        set_favorite_in(&file, "epic:Ginger", true).unwrap();
        set_favorite_in(&file, "gog:1", true).unwrap();
        assert_eq!(saved()["games"]["epic:Ginger"]["favorite"], true);
        assert_eq!(saved()["games"]["gog:1"]["later"], 1);

        // Reset to detected (clearing the edits) keeps the game a favorite.
        let renamed = GameEdits {
            name: Some("Cyberpunk".into()),
            ..GameEdits::default()
        };
        save_edits_in(&file, "epic:Ginger", &renamed).unwrap();
        save_edits_in(&file, "epic:Ginger", &GameEdits::default()).unwrap();
        assert_eq!(saved()["games"]["epic:Ginger"]["favorite"], true);
        assert!(saved()["games"]["epic:Ginger"].get("name").is_none());

        // Unfavoriting drops an entry with nothing else in it, not one with other settings.
        set_favorite_in(&file, "epic:Ginger", false).unwrap();
        set_favorite_in(&file, "gog:1", false).unwrap();
        assert!(saved()["games"].get("epic:Ginger").is_none());
        assert!(saved()["games"]["gog:1"].get("favorite").is_none());
        assert_eq!(saved()["games"]["gog:1"]["later"], 1);

        // `apply` marks the scanned games.
        set_favorite_in(&file, "epic:Ginger", true).unwrap();
        let settings: GameSettings = json_file::load(&file).unwrap();
        let mut games = vec![
            game("epic:Ginger", "Cyberpunk 2077", None),
            game("gog:1", "The Witcher 3", None),
        ];
        apply(&mut games, &GameDefaults::default(), &settings);
        assert!(games[0].favorite);
        assert!(!games[1].favorite);
        fs::remove_dir_all(&dir).unwrap();
    }

    fn settings_with(id: &str, edits: GameEdits) -> GameSettings {
        let mut settings = GameSettings::default();
        settings
            .games
            .entry(id.into())
            .or_default()
            .set_edits(edits);
        settings
    }

    #[test]
    fn edits_rename_and_change_how_play_starts() {
        let dir = temp_dir("edits");
        fs::write(dir.join("launcher.exe"), b"MZ").unwrap();
        let launcher = dir.join("launcher.exe");
        let exe = |path: &str, args: &str| Launch::Exe {
            path: PathBuf::from(path),
            args: args.into(),
            working_dir: Some(PathBuf::from(r"C:\GOG\Game")),
        };
        let apply_one = |mut game: Game, edits: GameEdits| {
            let id = game.id.clone();
            let mut games = vec![game.clone()];
            apply(
                &mut games,
                &GameDefaults::default(),
                &settings_with(&id, edits),
            );
            game = games.remove(0);
            game
        };

        // Name and a launch program with arguments; the program starts in its own folder.
        let mut steam = game("steam:220", "Half-Life 2", None);
        steam.launch = Launch::Uri("steam://rungameid/220".into());
        let edits = GameEdits {
            name: Some("HL2".into()),
            launch: Some(launcher.clone()),
            args: Some("-novid".into()),
            program: None,
        };
        let edited = apply_one(steam.clone(), edits.clone());
        assert_eq!(edited.name, "HL2");
        assert_eq!(
            edited.launch,
            Launch::Exe {
                path: launcher.clone(),
                args: "-novid".into(),
                working_dir: None
            }
        );
        assert_eq!(edited.edits, Some(edits));

        // Arguments alone: Steam launch options, a program's own arguments, none for Epic.
        let args = |text: &str| GameEdits {
            args: Some(text.into()),
            ..GameEdits::default()
        };
        assert_eq!(
            apply_one(steam.clone(), args("-novid -console")).launch,
            Launch::Uri("steam://run/220//-novid%20-console/".into())
        );
        let mut gog = game("gog:1", "Some GOG game", None);
        gog.launch = exe(r"C:\GOG\Game\game.exe", "-old");
        assert_eq!(
            apply_one(gog.clone(), args("-new")).launch,
            exe(r"C:\GOG\Game\game.exe", "-new")
        );
        assert_eq!(
            apply_one(gog, args("")).launch,
            exe(r"C:\GOG\Game\game.exe", "")
        );
        let mut epic = game("epic:Ginger", "Cyberpunk 2077", None);
        epic.launch = Launch::Uri("com.epicgames.launcher://apps/x?action=launch".into());
        assert_eq!(apply_one(epic.clone(), args("-x")).launch, epic.launch);

        // A launch program that no longer exists falls back to the detected launch.
        let gone = GameEdits {
            launch: Some(dir.join("gone.exe")),
            ..GameEdits::default()
        };
        assert_eq!(apply_one(steam.clone(), gone).launch, steam.launch);
        // No edits: as detected, and nothing to reset.
        let mut games = vec![steam.clone()];
        apply(
            &mut games,
            &GameDefaults::default(),
            &GameSettings::default(),
        );
        assert_eq!(games[0], steam);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_the_shipped_file_next_to_the_program() {
        // Tests run in target\<profile>\deps; the build copies games.json to target\<profile>.
        let shipped = load_defaults().unwrap();
        assert!(!shipped.games.is_empty());
    }
}
