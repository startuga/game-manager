//! Finds games installed through GOG, by GOG Galaxy or by GOG's offline installers.
//!
//! Both write one registry key per installed game under
//! `HKLM\SOFTWARE\WOW6432Node\GOG.com\Games\<gameId>`. Each game folder also holds a
//! `goggame-<gameId>.info` JSON file whose primary play task says exactly how to start it.
//!
//! The default Play action starts the game directly (GOG games are DRM-free). When GOG Galaxy is
//! installed, "Play via Galaxy" is offered as an extra action for playtime tracking and cloud saves.

use crate::{Error, Game, IconRef, Launch, LaunchOption, Result, Store};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

const GAMES_KEY: &str = r"SOFTWARE\WOW6432Node\GOG.com\Games";
const GALAXY_KEY: &str = r"SOFTWARE\WOW6432Node\GOG.com\GalaxyClient";

/// The registry values the scanner uses from `GOG.com\Games\<gameId>`.
#[derive(Default)]
struct RegistryEntry {
    game_id: String,
    name: String,
    path: String,
    exe: String,
    launch_param: String,
    working_dir: String,
    depends_on: String,
}

/// `goggame-<gameId>.info`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Info {
    #[serde(default)]
    root_game_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    play_tasks: Vec<PlayTask>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayTask {
    #[serde(default)]
    is_primary: bool,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    arguments: String,
    #[serde(default)]
    working_dir: String,
    /// `game`, `launcher`, `tool`, `document`, `other`…
    #[serde(default)]
    category: String,
}

/// Scans the GOG registry key for installed games.
pub fn scan() -> Result<Vec<Game>> {
    let Ok(games_key) = windows_registry::LOCAL_MACHINE.open(GAMES_KEY) else {
        return Ok(Vec::new());
    };
    let ids = games_key.keys().map_err(Error::windows(GAMES_KEY))?;
    let galaxy = galaxy_client();

    let mut games = Vec::new();
    for id in ids {
        let Ok(key) = games_key.open(&id) else {
            continue;
        };
        let get = |name: &str| key.get_string(name).unwrap_or_default();
        let entry = RegistryEntry {
            name: get("gameName"),
            path: get("path"),
            exe: get("exe"),
            launch_param: get("launchParam"),
            working_dir: get("workingDir"),
            depends_on: get("dependsOn"),
            game_id: id,
        };

        let dir = Path::new(&entry.path);
        if entry.path.is_empty() || !dir.is_dir() {
            continue;
        }
        let info = fs::read_to_string(dir.join(format!("goggame-{}.info", entry.game_id)))
            .ok()
            .and_then(|text| parse_info(&text));
        if let Some(mut game) = build_game(&entry, info.as_ref(), galaxy.as_deref()) {
            let ico = dir.join(format!("goggame-{}.ico", entry.game_id));
            if ico.is_file() {
                game.icon = Some(IconRef::file(ico));
            }
            games.push(game);
        }
    }
    Ok(games)
}

/// Locates `GalaxyClient.exe`, if GOG Galaxy is installed.
fn galaxy_client() -> Option<PathBuf> {
    let key = windows_registry::LOCAL_MACHINE.open(GALAXY_KEY).ok()?;
    let dir = key.open("paths").ok()?.get_string("client").ok()?;
    let exe = key
        .get_string("clientExecutable")
        .unwrap_or_else(|_| "GalaxyClient.exe".to_owned());
    let path = Path::new(&dir).join(exe);
    path.is_file().then_some(path)
}

fn parse_info(text: &str) -> Option<Info> {
    serde_json::from_str(text.strip_prefix('\u{feff}').unwrap_or(text)).ok()
}

/// Combines a registry entry and its `.info` file into a game, or `None` for DLC and entries
/// with nothing to launch.
fn build_game(entry: &RegistryEntry, info: Option<&Info>, galaxy: Option<&Path>) -> Option<Game> {
    // DLC is registered as depending on its base game, and its .info names a different root game.
    if !entry.depends_on.trim().is_empty() {
        return None;
    }
    if let Some(info) = info
        && !info.root_game_id.is_empty()
        && info.root_game_id != entry.game_id
    {
        return None;
    }

    let dir = PathBuf::from(&entry.path);
    let launch = info
        .and_then(|info| primary_task_launch(info, &dir))
        .or_else(|| registry_launch(entry))?;

    let name = [
        entry.name.as_str(),
        info.map_or("", |info| info.name.as_str()),
    ]
    .into_iter()
    .find(|name| !name.is_empty())
    .map(str::to_owned)
    .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
    .unwrap_or_else(|| entry.game_id.clone());

    let alt_launches = galaxy
        .map(|client| {
            vec![LaunchOption {
                label: "Play via Galaxy".to_owned(),
                launch: galaxy_launch(client, &entry.game_id, &entry.path),
            }]
        })
        .unwrap_or_default();
    let program = info.and_then(|info| game_program(info, &dir));

    Some(Game {
        id: format!("gog:{}", entry.game_id),
        name,
        store: Store::Gog,
        install_dir: Some(dir),
        launch,
        alt_launches,
        icon: None,
        program,
        arch: None,
        edits: None,
        favorite: false,
    })
}

/// The game's own program when Play starts a launcher: the first file task in the `game`
/// category whose `.exe` exists (Witcher 3's primary task is `REDprelauncher.exe`, a launcher,
/// while its game tasks start `bin\x64_dx12\witcher3.exe`). `None` lets Play's program count.
fn game_program(info: &Info, dir: &Path) -> Option<PathBuf> {
    info.play_tasks
        .iter()
        .filter(|task| task.kind == "FileTask" && task.category.eq_ignore_ascii_case("game"))
        .map(|task| dir.join(task.path.replace('/', "\\")))
        .find(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                && path.is_file()
        })
}

/// The `.info` file's primary file task, or its first file task if none is marked primary.
fn primary_task_launch(info: &Info, dir: &Path) -> Option<Launch> {
    let file_tasks = || {
        info.play_tasks
            .iter()
            .filter(|task| task.kind == "FileTask" && !task.path.is_empty())
    };
    let task = file_tasks()
        .find(|task| task.is_primary)
        .or_else(|| file_tasks().next())?;

    let working_dir = if task.working_dir.is_empty() {
        dir.to_path_buf()
    } else {
        dir.join(&task.working_dir)
    };
    Some(Launch::Exe {
        path: dir.join(&task.path),
        args: task.arguments.clone(),
        working_dir: Some(working_dir),
    })
}

/// Fallback when there is no usable `.info` file.
fn registry_launch(entry: &RegistryEntry) -> Option<Launch> {
    if entry.exe.is_empty() {
        return None;
    }
    let working_dir = [entry.working_dir.as_str(), entry.path.as_str()]
        .into_iter()
        .find(|dir| !dir.is_empty())
        .map(PathBuf::from);
    Some(Launch::Exe {
        path: PathBuf::from(&entry.exe),
        args: entry.launch_param.trim().to_owned(),
        working_dir,
    })
}

fn galaxy_launch(client: &Path, game_id: &str, game_dir: &str) -> Launch {
    // A trailing backslash would escape the closing quote of /path="...".
    let game_dir = game_dir.trim_end_matches(['\\', '/']);
    Launch::Exe {
        path: client.to_path_buf(),
        args: format!(r#"/command=runGame /gameId={game_id} /path="{game_dir}""#),
        working_dir: client.parent().map(Path::to_path_buf),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed copy of a real goggame-1207659042.info (tab-indented, several play tasks).
    const FAR_CRY_INFO: &str = "{
\t\"version\" : 1,
\t\"gameId\" : \"1207659042\",
\t\"rootGameId\" : \"1207659042\",
\t\"name\"             : \"Far Cry 2 Fortune's Edition\",
\t\"playTasks\"        : [
\t\t{
\t\t\t\"name\" : \"Far Cry 2 (Safe Mode)\",
\t\t\t\"type\" : \"FileTask\",
\t\t\t\"path\" : \"bin\\\\FarCry2.exe\",
\t\t\t\"workingDir\" : \"bin\",
\t\t\t\"arguments\" : \"-RenderProfile_ResolutionX 640\"
\t\t},
\t\t{
\t\t\t\"isPrimary\" : true,
\t\t\t\"type\"      : \"FileTask\",
\t\t\t\"path\"      : \"bin\\\\farcry2.exe\",
\t\t\t\"workingDir\" : \"bin\"
\t\t}
\t],
\t\"supportTasks\" : [ { \"name\" : \"Manual\", \"type\" : \"FileTask\", \"path\" : \"Manual.pdf\" } ]
}";

    // Trimmed copy of a real goggame-1407750955.info (URL task, no working dir).
    const TOMB_RAIDER_INFO: &str = r#"{
    "gameId": "1407750955",
    "name": "Tomb Raider I-III Remastered",
    "playTasks": [
        { "category": "game", "isPrimary": true, "name": "Tomb Raider I-III Remastered", "path": "tomb123.exe", "type": "FileTask" },
        { "category": "document", "link": "http://www.gog.com/support/", "name": "Support", "type": "URLTask" }
    ],
    "rootGameId": "1407750955",
    "version": 1
}"#;

    fn far_cry_entry() -> RegistryEntry {
        RegistryEntry {
            game_id: "1207659042".into(),
            name: "Far Cry 2 Fortune's Edition".into(),
            path: r"C:\GOG Games\Far Cry 2".into(),
            exe: r"C:\GOG Games\Far Cry 2\bin\farcry2.exe".into(),
            working_dir: r"C:\GOG Games\Far Cry 2\bin\".into(),
            ..Default::default()
        }
    }

    fn tomb_raider_entry() -> RegistryEntry {
        RegistryEntry {
            game_id: "1407750955".into(),
            name: "Tomb Raider I-III Remastered".into(),
            path: r"C:\GOG Games\Tomb Raider I-III Remastered".into(),
            exe: r"C:\GOG Games\Tomb Raider I-III Remastered\tomb123.exe".into(),
            ..Default::default()
        }
    }

    #[test]
    fn uses_primary_play_task_with_working_dir() {
        let info = parse_info(FAR_CRY_INFO).unwrap();
        let game = build_game(&far_cry_entry(), Some(&info), None).unwrap();
        assert_eq!(game.id, "gog:1207659042");
        assert_eq!(game.name, "Far Cry 2 Fortune's Edition");
        assert_eq!(game.store, Store::Gog);
        assert_eq!(
            game.launch,
            Launch::Exe {
                path: PathBuf::from(r"C:\GOG Games\Far Cry 2\bin\farcry2.exe"),
                args: String::new(),
                working_dir: Some(PathBuf::from(r"C:\GOG Games\Far Cry 2\bin")),
            }
        );
        assert!(game.alt_launches.is_empty());
    }

    #[test]
    fn task_without_working_dir_runs_in_game_folder() {
        let info = parse_info(TOMB_RAIDER_INFO).unwrap();
        let game = build_game(&tomb_raider_entry(), Some(&info), None).unwrap();
        assert_eq!(
            game.launch,
            Launch::Exe {
                path: PathBuf::from(r"C:\GOG Games\Tomb Raider I-III Remastered\tomb123.exe"),
                args: String::new(),
                working_dir: Some(PathBuf::from(r"C:\GOG Games\Tomb Raider I-III Remastered")),
            }
        );
    }

    #[test]
    fn falls_back_to_registry_without_info() {
        let mut entry = far_cry_entry();
        entry.launch_param = " -nointro ".into();
        let game = build_game(&entry, None, None).unwrap();
        assert_eq!(
            game.launch,
            Launch::Exe {
                path: PathBuf::from(r"C:\GOG Games\Far Cry 2\bin\farcry2.exe"),
                args: "-nointro".into(),
                working_dir: Some(PathBuf::from(r"C:\GOG Games\Far Cry 2\bin\")),
            }
        );
    }

    #[test]
    fn broken_info_falls_back_to_registry() {
        assert!(parse_info("{ not json").is_none());
        let game = build_game(&tomb_raider_entry(), None, None).unwrap();
        assert!(matches!(game.launch, Launch::Exe { .. }));
    }

    #[test]
    fn name_falls_back_to_info() {
        let mut entry = tomb_raider_entry();
        entry.name.clear();
        let info = parse_info(TOMB_RAIDER_INFO).unwrap();
        let game = build_game(&entry, Some(&info), None).unwrap();
        assert_eq!(game.name, "Tomb Raider I-III Remastered");
    }

    #[test]
    fn skips_dlc() {
        let mut entry = tomb_raider_entry();
        entry.depends_on = "1407750955".into();
        assert!(build_game(&entry, None, None).is_none());

        let info = parse_info(
            &TOMB_RAIDER_INFO.replace(r#""rootGameId": "1407750955""#, r#""rootGameId": "1""#),
        )
        .unwrap();
        assert!(build_game(&tomb_raider_entry(), Some(&info), None).is_none());
    }

    #[test]
    fn skips_entries_with_nothing_to_launch() {
        let mut entry = tomb_raider_entry();
        entry.exe.clear();
        assert!(build_game(&entry, None, None).is_none());
    }

    #[test]
    fn offers_galaxy_launch_when_installed() {
        let client = Path::new(r"C:\Program Files (x86)\GOG Galaxy\GalaxyClient.exe");
        let mut entry = far_cry_entry();
        entry.path.push('\\');
        let game = build_game(&entry, None, Some(client)).unwrap();

        // Direct launch stays the default.
        assert!(matches!(&game.launch, Launch::Exe { path, .. } if path.ends_with("farcry2.exe")));
        assert_eq!(
            game.alt_launches,
            vec![LaunchOption {
                label: "Play via Galaxy".into(),
                launch: Launch::Exe {
                    path: client.to_path_buf(),
                    args: r#"/command=runGame /gameId=1207659042 /path="C:\GOG Games\Far Cry 2""#
                        .into(),
                    working_dir: Some(PathBuf::from(r"C:\Program Files (x86)\GOG Galaxy")),
                },
            }]
        );
    }

    #[test]
    fn program_is_the_first_existing_game_task() {
        let dir = std::env::temp_dir().join(format!("GM-GOG-Program-{}", std::process::id()));
        fs::create_dir_all(dir.join(r"bin\x64")).unwrap();
        for file in ["REDprelauncher.exe", r"bin\x64\witcher3.exe"] {
            fs::write(dir.join(file), b"MZ").unwrap();
        }
        // Shaped like Witcher 3's info: a primary launcher task, then game tasks.
        let info = parse_info(
            r#"{ "playTasks": [
                { "category": "launcher", "isPrimary": true, "type": "FileTask", "path": "REDprelauncher.exe" },
                { "category": "game", "type": "FileTask", "path": "bin/x64_dx12/witcher3.exe" },
                { "category": "game", "type": "FileTask", "path": "bin/x64/witcher3.exe" },
                { "category": "other", "type": "URLTask", "link": "https://support.gog.com" }
            ] }"#,
        )
        .unwrap();
        assert_eq!(
            primary_task_launch(&info, &dir).map(|launch| match launch {
                Launch::Exe { path, .. } => path,
                Launch::Uri(_) => PathBuf::new(),
            }),
            Some(dir.join("REDprelauncher.exe"))
        );
        assert_eq!(
            game_program(&info, &dir),
            Some(dir.join(r"bin\x64\witcher3.exe"))
        );

        let launcher_only = parse_info(
            r#"{ "playTasks": [ { "category": "launcher", "isPrimary": true, "type": "FileTask", "path": "REDprelauncher.exe" } ] }"#,
        )
        .unwrap();
        assert_eq!(game_program(&launcher_only, &dir), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
