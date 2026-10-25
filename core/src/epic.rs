//! Finds games installed through the Epic Games Launcher.
//!
//! Two launcher generations are supported:
//! - The current launcher (`%LOCALAPPDATA%\Epic Games\Launcher`) keeps an append-only log of
//!   installations in `%LOCALAPPDATA%\com.epicgames.eglv2\installations.jsonl`. It stores no
//!   display names, so titles come from the launcher's catalog cache.
//! - The legacy launcher writes one JSON manifest (`*.item`) per installed app to
//!   `%ProgramData%\Epic\EpicGamesLauncher\Data\Manifests`.
//!
//! Both identify a game by the same app name (`artifactId` / `AppName`), so a game migrated
//! from the legacy launcher is only listed once.

use crate::{Error, Game, IconRef, Launch, Result, Store};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Scans both launcher generations for installed games.
pub fn scan() -> Result<Vec<Game>> {
    let mut games = Vec::new();

    if let Some(data) = eglv2_dir() {
        let log = data.join("installations.jsonl");
        if let Ok(text) = fs::read_to_string(&log) {
            let titles = fs::read_to_string(data.join(r"cache\prod\catalog-cache.json"))
                .map(|text| parse_catalog_titles(&text))
                .unwrap_or_default();
            games.extend(
                parse_installations(&text, &titles)
                    .into_iter()
                    .filter(install_dir_exists),
            );
        }
    }

    if let Some(dir) = legacy_manifests_dir() {
        let entries = fs::read_dir(&dir).map_err(Error::io(&dir))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("item"))
                && let Ok(text) = fs::read_to_string(&path)
                && let Some(game) = parse_legacy_manifest(&text)
                && install_dir_exists(&game)
                && !games.iter().any(|g| g.id == game.id)
            {
                games.push(game);
            }
        }
    }

    Ok(games)
}

fn install_dir_exists(game: &Game) -> bool {
    game.install_dir.as_deref().is_some_and(Path::is_dir)
}

fn launch_uri(namespace: &str, item_id: &str, app_name: &str) -> Launch {
    Launch::Uri(format!(
        "com.epicgames.launcher://apps/{namespace}%3A{item_id}%3A{app_name}?action=launch&silent=true"
    ))
}

// ---------------------------------------------------------------------------------------------
// Current launcher (eglv2)
// ---------------------------------------------------------------------------------------------

/// `%LOCALAPPDATA%\com.epicgames.eglv2`, the current launcher's data folder.
pub fn eglv2_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("com.epicgames.eglv2");
    dir.is_dir().then_some(dir)
}

/// One line of `installations.jsonl`. The header line has no `installationId`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Installation {
    #[serde(default)]
    installation_id: String,
    #[serde(default)]
    complete: bool,
    #[serde(default)]
    dir: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    item_id: String,
    #[serde(default)]
    artifact_id: String,
    #[serde(default)]
    launch_executable: String,
}

/// Replays the installation log: later lines for the same installation replace earlier ones.
fn parse_installations(text: &str, titles: &HashMap<String, String>) -> Vec<Game> {
    let mut latest: Vec<Installation> = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<Installation>(line) else {
            continue;
        };
        if record.installation_id.is_empty() {
            continue;
        }
        match latest
            .iter_mut()
            .find(|r| r.installation_id == record.installation_id)
        {
            Some(existing) => *existing = record,
            None => latest.push(record),
        }
    }

    latest
        .into_iter()
        .filter(|r| {
            r.complete
                && !r.dir.is_empty()
                && !r.artifact_id.is_empty()
                && (r.platform.is_empty() || r.platform.eq_ignore_ascii_case("windows"))
                // DLC and add-ons are installed without an executable of their own.
                && !r.launch_executable.is_empty()
        })
        .map(|r| {
            let dir = PathBuf::from(&r.dir);
            let name = titles
                .get(&r.item_id)
                .cloned()
                .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
                .unwrap_or_else(|| r.artifact_id.clone());
            Game {
                id: format!("epic:{}", r.artifact_id),
                name,
                store: Store::Epic,
                icon: program_icon(&dir, &r.launch_executable),
                install_dir: Some(dir),
                launch: launch_uri(&r.namespace, &r.item_id, &r.artifact_id),
                alt_launches: Vec::new(),
                program: None,
                arch: None,
                edits: None,
                favorite: false,
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct Catalog {
    #[serde(default)]
    items: HashMap<String, CatalogEntry>,
}

#[derive(Deserialize)]
struct CatalogEntry {
    item: CatalogItem,
}

#[derive(Deserialize)]
struct CatalogItem {
    #[serde(default)]
    title: String,
}

/// Maps catalog item id → title from the launcher's `catalog-cache.json`.
fn parse_catalog_titles(text: &str) -> HashMap<String, String> {
    serde_json::from_str::<Catalog>(text)
        .map(|catalog| {
            catalog
                .items
                .into_iter()
                .filter(|(_, entry)| !entry.item.title.is_empty())
                .map(|(id, entry)| (id, entry.item.title))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------
// Legacy launcher
// ---------------------------------------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Manifest {
    display_name: String,
    app_name: String,
    #[serde(default)]
    catalog_namespace: String,
    #[serde(default)]
    catalog_item_id: String,
    #[serde(default)]
    install_location: String,
    #[serde(default)]
    main_game_app_name: String,
    #[serde(default)]
    app_categories: Vec<String>,
    #[serde(default, rename = "bIsIncompleteInstall")]
    is_incomplete_install: bool,
    #[serde(default)]
    launch_executable: String,
}

/// The game's own program, whose icon Epic also uses. Paths in Epic's files use `/`.
fn program_icon(dir: &Path, launch_executable: &str) -> Option<IconRef> {
    let exe = launch_executable.trim().replace('/', "\\");
    (!exe.is_empty()).then(|| IconRef::file(dir.join(exe.trim_start_matches('\\'))))
}

/// Locates the legacy launcher's manifest folder.
pub fn legacy_manifests_dir() -> Option<PathBuf> {
    let data = windows_registry::LOCAL_MACHINE
        .open(r"SOFTWARE\WOW6432Node\Epic Games\EpicGamesLauncher")
        .and_then(|key| key.get_string("AppDataPath"))
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var_os("ProgramData")
                .map(|p| PathBuf::from(p).join(r"Epic\EpicGamesLauncher\Data"))
        })?;
    let dir = data.join("Manifests");
    dir.is_dir().then_some(dir)
}

/// Converts one legacy manifest into a game, or `None` for DLC, non-game apps and broken installs.
fn parse_legacy_manifest(text: &str) -> Option<Game> {
    let manifest: Manifest = serde_json::from_str(text).ok()?;

    if manifest.is_incomplete_install {
        return None;
    }
    // DLC manifests point at the base game through MainGameAppName.
    if !manifest.main_game_app_name.is_empty() && manifest.main_game_app_name != manifest.app_name {
        return None;
    }
    // Engine versions and plug-ins are installed through the launcher too.
    if !manifest.app_categories.is_empty() && !manifest.app_categories.iter().any(|c| c == "games")
    {
        return None;
    }

    let install_dir =
        (!manifest.install_location.is_empty()).then(|| PathBuf::from(&manifest.install_location));
    Some(Game {
        id: format!("epic:{}", manifest.app_name),
        name: manifest.display_name,
        store: Store::Epic,
        icon: install_dir
            .as_deref()
            .and_then(|dir| program_icon(dir, &manifest.launch_executable)),
        install_dir,
        launch: launch_uri(
            &manifest.catalog_namespace,
            &manifest.catalog_item_id,
            &manifest.app_name,
        ),
        alt_launches: Vec::new(),
        program: None,
        arch: None,
        edits: None,
        favorite: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed copy of a real installations.jsonl: header, then an install that was logged
    // incomplete and later complete.
    const INSTALLATIONS: &str = r#"{"version":1,"writtenAt":"2026-10-16T21:08:27Z","entries":1}
{"installationId":"EB19","complete":true,"dir":"C:\\Program Files\\Epic Games\\RollerCoasterTycoon3","platform":"Windows","namespace":"f01b","itemId":"df38","artifactId":"281a","launchExecutable":"RCT3.exe"}
{"installationId":"F72B","complete":false,"dir":"C:\\Users\\me\\Epic Games\\BrothersATOTS","platform":"Windows","namespace":"6156","itemId":"77a5","artifactId":"Tamarind","launchExecutable":"Binaries/Win32/brothers.exe"}
{"installationId":"F72B","complete":true,"dir":"C:\\Users\\me\\Epic Games\\BrothersATOTS","platform":"Windows","namespace":"6156","itemId":"77a5","artifactId":"Tamarind","launchExecutable":"Binaries/Win32/brothers.exe"}
"#;

    const CATALOG: &str = r#"{"items":{
        "df38":{"item":{"id":"df38","title":"RollerCoaster Tycoon® 3: Complete Edition","keyImages":[]},"expiresAt":1},
        "zzzz":{"item":{"id":"zzzz","title":"Other"}}
    }}"#;

    #[test]
    fn replays_installation_log() {
        let titles = parse_catalog_titles(CATALOG);
        let games = parse_installations(INSTALLATIONS, &titles);
        assert_eq!(games.len(), 2);

        let rct = &games[0];
        assert_eq!(rct.id, "epic:281a");
        assert_eq!(rct.name, "RollerCoaster Tycoon® 3: Complete Edition");
        assert_eq!(
            rct.launch,
            Launch::Uri(
                "com.epicgames.launcher://apps/f01b%3Adf38%3A281a?action=launch&silent=true".into()
            )
        );

        // No catalog title: falls back to the install folder name.
        let brothers = &games[1];
        assert_eq!(brothers.id, "epic:Tamarind");
        assert_eq!(brothers.name, "BrothersATOTS");
        // launchExecutable uses forward slashes.
        assert_eq!(
            brothers.icon,
            Some(IconRef::file(
                r"C:\Users\me\Epic Games\BrothersATOTS\Binaries\Win32\brothers.exe"
            ))
        );
    }

    #[test]
    fn latest_incomplete_record_hides_game() {
        let log = format!(
            "{INSTALLATIONS}{}",
            r#"{"installationId":"EB19","complete":false,"dir":"C:\\x","artifactId":"281a","launchExecutable":"RCT3.exe"}"#
        );
        let games = parse_installations(&log, &HashMap::new());
        assert!(games.iter().all(|g| g.id != "epic:281a"));
    }

    #[test]
    fn skips_installs_without_executable() {
        let log = r#"{"installationId":"D1","complete":true,"dir":"C:\\dlc","artifactId":"dlc","launchExecutable":""}"#;
        assert!(parse_installations(log, &HashMap::new()).is_empty());
    }

    #[test]
    fn tolerates_bad_lines_and_catalog() {
        let log = format!("not json\n\n{INSTALLATIONS}");
        assert_eq!(parse_installations(&log, &HashMap::new()).len(), 2);
        assert!(parse_catalog_titles("{ broken").is_empty());
    }

    const LEGACY: &str = r#"{
        "FormatVersion": 0,
        "bIsIncompleteInstall": false,
        "DisplayName": "Fortnite",
        "InstallLocation": "C:\\Program Files\\Epic Games\\Fortnite",
        "AppName": "Fortnite",
        "CatalogNamespace": "fn",
        "CatalogItemId": "4fe7",
        "MainGameAppName": "Fortnite",
        "AppCategories": ["public", "games", "applications"]
    }"#;

    #[test]
    fn parses_legacy_manifest() {
        let game = parse_legacy_manifest(LEGACY).unwrap();
        assert_eq!(game.id, "epic:Fortnite");
        assert_eq!(game.name, "Fortnite");
        assert_eq!(
            game.launch,
            Launch::Uri(
                "com.epicgames.launcher://apps/fn%3A4fe7%3AFortnite?action=launch&silent=true"
                    .into()
            )
        );
    }

    #[test]
    fn legacy_skips_incomplete_dlc_and_non_games() {
        for (from, to) in [
            (
                r#""bIsIncompleteInstall": false"#,
                r#""bIsIncompleteInstall": true"#,
            ),
            (
                r#""MainGameAppName": "Fortnite""#,
                r#""MainGameAppName": "Other""#,
            ),
            (r#""public", "games", "applications""#, r#""engines""#),
        ] {
            assert!(
                parse_legacy_manifest(&LEGACY.replace(from, to)).is_none(),
                "{to}"
            );
        }
        assert!(parse_legacy_manifest("{ not json").is_none());
    }
}
