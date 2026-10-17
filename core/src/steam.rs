//! Finds games installed through Steam.

use crate::{Game, IconRef, Launch, Store, UNINSTALL_KEYS, vdf};
use std::fs;
use std::path::{Path, PathBuf};

/// Locates the Steam install directory from the registry.
pub fn install_dir() -> Option<PathBuf> {
    let from_user = windows_registry::CURRENT_USER
        .open(r"Software\Valve\Steam")
        .and_then(|key| key.get_string("SteamPath"));
    let from_machine = || {
        windows_registry::LOCAL_MACHINE
            .open(r"SOFTWARE\WOW6432Node\Valve\Steam")
            .and_then(|key| key.get_string("InstallPath"))
    };
    let path = from_user.or_else(|_| from_machine()).ok()?;
    let path = PathBuf::from(path.replace('/', "\\"));
    path.is_dir().then_some(path)
}

/// Scans every Steam library folder for installed games.
pub fn scan() -> Result<Vec<Game>, String> {
    let Some(steam) = install_dir() else {
        return Ok(Vec::new());
    };

    let mut games = Vec::new();
    for library in library_folders(&steam)? {
        let steamapps = library.join("steamapps");
        let Ok(dir) = fs::read_dir(&steamapps) else {
            continue;
        };
        for entry in dir.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("appmanifest_")
                && name.ends_with(".acf")
                && let Some(game) = read_manifest(&entry.path(), &steamapps)
            {
                games.push(game);
            }
        }
    }
    Ok(games)
}

fn library_folders(steam: &Path) -> Result<Vec<PathBuf>, String> {
    let file = steam.join("steamapps").join("libraryfolders.vdf");
    let mut folders = vec![steam.to_path_buf()];

    let Ok(text) = fs::read_to_string(&file) else {
        return Ok(folders);
    };
    let root = vdf::parse(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    if let Some(list) = root.get("libraryfolders") {
        for (_, folder) in list.entries() {
            if let Some(path) = folder.get_str("path") {
                let path = PathBuf::from(path);
                if !folders.iter().any(|f| paths_equal(f, &path)) {
                    folders.push(path);
                }
            }
        }
    }
    Ok(folders)
}

fn read_manifest(manifest: &Path, steamapps: &Path) -> Option<Game> {
    let text = fs::read_to_string(manifest).ok()?;
    let root = vdf::parse(&text).ok()?;
    let state = root.get("AppState")?;
    let app_id = state.get_str("appid")?;
    let name = state.get_str("name")?;

    // Skip redistributables and tools that Steam installs alongside games.
    if app_id == "228980" || name.starts_with("Steamworks") {
        return None;
    }

    let install_dir = state
        .get_str("installdir")
        .map(|dir| steamapps.join("common").join(dir));

    Some(Game {
        id: format!("steam:{app_id}"),
        name: name.to_owned(),
        store: Store::Steam,
        install_dir,
        launch: Launch::Uri(format!("steam://rungameid/{app_id}")),
        alt_launches: Vec::new(),
        icon: icon(app_id),
    })
}

/// The icon Steam uses for the game's shortcuts, from its `Steam App <id>` uninstall entry.
fn icon(app_id: &str) -> Option<IconRef> {
    UNINSTALL_KEYS.iter().find_map(|base| {
        let key = windows_registry::LOCAL_MACHINE
            .open(format!(r"{base}\Steam App {app_id}"))
            .ok()?;
        IconRef::parse(&key.get_string("DisplayIcon").ok()?)
    })
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
}
