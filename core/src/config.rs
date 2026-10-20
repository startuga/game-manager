//! Game configuration in JSON files, so no game-specific knowledge lives in the code:
//!
//! - `games.json`, shipped next to the app (copied from `config/games.json` at build time): what is
//!   known about particular games, such as their real program when Play starts a launcher, and
//!   entries that are not games (redistributables) to hide.
//! - `%APPDATA%\Game Manager\game-settings.json`: the user's own settings per game, such as the
//!   program chosen with "Choose game .exe".
//!
//! For a game's program, the user's choice wins, then the shipped configuration, then what the
//! store scanner found.

use crate::{Game, json_file};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DEFAULTS_FILE: &str = "games.json";
const SETTINGS_FILE: &str = "game-settings.json";

/// `games.json`: what is known about particular games, by game id.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameDefaults {
    /// Per store (the id prefix: `steam`, `epic`, `gog`…): names of entries that are not games.
    #[serde(default)]
    ignore_name_prefixes: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    games: BTreeMap<String, GameDefault>,
}

#[derive(Debug, Default, Deserialize)]
struct GameDefault {
    /// Not a game (e.g. a redistributable package): never listed.
    #[serde(default)]
    ignore: bool,
    /// The game's own program, relative to its install folder; the first that exists wins.
    #[serde(default)]
    programs: Vec<PathBuf>,
}

impl GameDefaults {
    fn ignores(&self, game: &Game) -> bool {
        let by_id = self.games.get(&game.id).is_some_and(|entry| entry.ignore);
        let by_name = game
            .id
            .split_once(':')
            .and_then(|(store, _)| self.ignore_name_prefixes.get(store))
            .is_some_and(|prefixes| {
                prefixes
                    .iter()
                    .any(|prefix| game.name.starts_with(prefix.as_str()))
            });
        by_id || by_name
    }

    fn program(&self, game: &Game) -> Option<PathBuf> {
        let dir = game.install_dir.as_deref()?;
        self.games
            .get(&game.id)?
            .programs
            .iter()
            .map(|program| dir.join(program))
            .find(|program| program.is_file())
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
    /// The game's own program, chosen by the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    program: Option<PathBuf>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl GameSettings {
    /// The program the user chose for a game, while that file exists.
    fn program(&self, id: &str) -> Option<&Path> {
        self.games
            .get(id)?
            .program
            .as_deref()
            .filter(|program| program.is_file())
    }
}

/// The shipped configuration: `games.json` in the running program's folder, or in its parent
/// (where examples and tests find it). No file means no defaults.
pub fn load_defaults() -> Result<GameDefaults, String> {
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
pub fn load_settings() -> Result<GameSettings, String> {
    json_file::load(&json_file::user_file(SETTINGS_FILE)?)
}

/// Records the program the user chose for a game.
pub fn set_program(id: &str, program: &Path) -> Result<(), String> {
    set_program_in(&json_file::user_file(SETTINGS_FILE)?, id, program)
}

fn set_program_in(file: &Path, id: &str, program: &Path) -> Result<(), String> {
    // Loading first also means a file that cannot be read is never overwritten.
    let mut settings: GameSettings = json_file::load(file)?;
    settings.games.entry(id.to_owned()).or_default().program = Some(program.to_path_buf());
    json_file::save(file, &settings)
}

/// Applies the configuration to scanned games: drops entries that are not games and sets each
/// game's program. A program the user chose also decides the architecture, even over a package's.
pub fn apply(games: &mut Vec<Game>, defaults: &GameDefaults, settings: &GameSettings) {
    games.retain(|game| !defaults.ignores(game));
    for game in games.iter_mut() {
        if let Some(program) = settings.program(&game.id) {
            game.program = Some(program.to_path_buf());
            game.arch = None;
        } else if let Some(program) = defaults.program(game) {
            game.program = Some(program);
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
        assert_eq!(
            shipped.games["epic:Ginger"].programs,
            [PathBuf::from(r"bin\x64\Cyberpunk2077.exe")]
        );
    }

    #[test]
    fn hides_entries_by_id_and_by_name_prefix_per_store() {
        let config = defaults(
            r#"{ "ignoreNamePrefixes": { "steam": ["Steamworks"] },
                 "games": { "steam:1": { "ignore": true, "note": "for people" } } }"#,
        );
        let mut games = vec![
            game("steam:1", "Redistributable", None),
            game("steam:2", "Steamworks Shared", None),
            game("gog:3", "Steamworks: a GOG game", None),
            game("steam:4", "Half-Life 2", None),
        ];
        apply(&mut games, &config, &GameSettings::default());
        let ids: Vec<_> = games.iter().map(|g| g.id.as_str()).collect();
        assert_eq!(ids, ["gog:3", "steam:4"]);
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
            r#"{ "games": { "epic:Ginger": { "programs": ["bin\\x64_gone\\x.exe", "bin\\x64\\Cyberpunk2077.exe"] } } }"#,
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
    fn saves_the_chosen_program_and_keeps_other_settings() {
        let dir = temp_dir("settings");
        let file = dir.join(SETTINGS_FILE);
        fs::write(
            &file,
            r#"{ "games": { "gog:1": { "program": "C:\\a.exe", "later": 1 } }, "version": 2 }"#,
        )
        .unwrap();
        set_program_in(
            &file,
            "epic:Ginger",
            Path::new(r"C:\Games\Cyberpunk2077.exe"),
        )
        .unwrap();

        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(
            saved["games"]["epic:Ginger"]["program"],
            r"C:\Games\Cyberpunk2077.exe"
        );
        assert_eq!(saved["games"]["gog:1"]["program"], r"C:\a.exe");
        assert_eq!(saved["games"]["gog:1"]["later"], 1);
        assert_eq!(saved["version"], 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn unreadable_settings_are_not_overwritten() {
        let dir = temp_dir("broken");
        let file = dir.join(SETTINGS_FILE);
        fs::write(&file, "{ broken").unwrap();
        assert!(set_program_in(&file, "epic:Ginger", Path::new(r"C:\x.exe")).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ broken");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_the_shipped_file_next_to_the_program() {
        // Tests run in target\<profile>\deps; the build copies games.json to target\<profile>.
        let shipped = load_defaults().unwrap();
        assert!(shipped.games.contains_key("epic:Ginger"));
    }
}
