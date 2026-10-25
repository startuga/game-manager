//! Finds games installed through Steam.
//!
//! Games come from each library's `appmanifest_<appid>.acf`. Play goes through Steam
//! (`steam://rungameid/<appid>`), so the game's own program is read from Steam's binary app info
//! cache, `appcache\appinfo.vdf`: the launch entry Steam would use on 64-bit Windows.

use crate::text::percent_encode;
use crate::vdf::BinaryKeys;
use crate::{Error, Game, IconRef, Launch, Result, Store, UNINSTALL_KEYS, on_disk_case, vdf};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// `appinfo.vdf` versions: 28 writes keys in place, 29 keeps them in a string table.
const APPINFO_V28: u32 = 0x0756_4428;
const APPINFO_V29: u32 = 0x0756_4429;
/// Bytes between an app's size and its KeyValues: info state, last update, PICS token, SHA-1 of
/// the text form, change number and SHA-1 of the binary form.
const APPINFO_ENTRY_HEADER: usize = 60;

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
pub fn scan() -> Result<Vec<Game>> {
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
    add_programs(&steam, &mut games);
    Ok(games)
}

/// Sets each game's program from the app info cache. Without a readable cache the games simply
/// have no program.
fn add_programs(steam: &Path, games: &mut [Game]) {
    let wanted: HashSet<u32> = games.iter().filter_map(app_id).collect();
    let Ok(data) = fs::read(steam.join("appcache").join("appinfo.vdf")) else {
        return;
    };
    let Some(apps) = parse_app_info(&data, &wanted) else {
        return;
    };
    for game in games.iter_mut() {
        let Some(app) = app_id(game).and_then(|id| apps.get(&id)) else {
            continue;
        };
        if let Some(dir) = &game.install_dir {
            game.program = launch_program(app, dir);
        }
    }
}

fn app_id(game: &Game) -> Option<u32> {
    game.id.strip_prefix("steam:")?.parse().ok()
}

/// The app info of the `wanted` apps in `appinfo.vdf`; `None` for an unknown file version. Other
/// apps are skipped without being parsed.
fn parse_app_info(data: &[u8], wanted: &HashSet<u32>) -> Option<HashMap<u32, vdf::Value>> {
    let u32_at = |pos: usize| {
        data.get(pos..pos.checked_add(4)?)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap_or_default()))
    };
    let (table, mut pos) = match u32_at(0)? {
        APPINFO_V28 => (None, 8),
        APPINFO_V29 => {
            let offset = u64::from_le_bytes(data.get(8..16)?.try_into().ok()?);
            (
                Some(string_table(data.get(usize::try_from(offset).ok()?..)?)?),
                16,
            )
        }
        _ => return None,
    };
    let keys = table
        .as_deref()
        .map_or(BinaryKeys::Inline, BinaryKeys::Table);

    let mut apps = HashMap::new();
    // Each entry: app id (0 ends the list), size of the rest, fixed header, KeyValues.
    while let Some(id) = u32_at(pos).filter(|&id| id != 0) {
        let Some(size) = u32_at(pos + 4) else {
            break;
        };
        let start = pos + 8;
        let end = start + size as usize;
        if wanted.contains(&id)
            && let Some(kv) = data.get(start + APPINFO_ENTRY_HEADER..end)
            && let Ok(app) = vdf::parse_binary(kv, keys)
        {
            apps.insert(id, app);
        }
        pos = end;
    }
    Some(apps)
}

/// Version 29's key table: a count, then that many null-terminated strings.
fn string_table(data: &[u8]) -> Option<Vec<String>> {
    let count = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
    let table: Vec<String> = data[4..]
        .split(|&b| b == 0)
        .take(count)
        .map(|key| String::from_utf8_lossy(key).into_owned())
        .collect();
    (table.len() == count).then_some(table)
}

/// The program Steam starts for an app on 64-bit Windows: among the launch entries for Windows,
/// default ones before extra options (editors, tools), 64-bit before unmarked before 32-bit, in
/// Steam's order otherwise. The first whose `.exe` exists wins.
fn launch_program(app: &vdf::Value, install_dir: &Path) -> Option<PathBuf> {
    let launch = app.get("appinfo")?.get("config")?.get("launch")?;
    let mut candidates: Vec<((u8, u8), &str)> = launch
        .entries()
        .iter()
        .filter_map(|(_, entry)| {
            let executable = entry.get_str("executable")?;
            let config = entry.get("config");
            let setting = |key| config.and_then(|c| c.get_str(key)).unwrap_or("");
            let oslist = setting("oslist");
            let for_windows = oslist.is_empty()
                || oslist
                    .split(',')
                    .any(|os| os.trim().eq_ignore_ascii_case("windows"));
            if !for_windows {
                return None;
            }
            let kind = entry.get_str("type").unwrap_or("");
            let kind_rank = u8::from(
                !(kind.is_empty()
                    || kind.eq_ignore_ascii_case("default")
                    || kind.eq_ignore_ascii_case("none")),
            );
            let arch_rank = match setting("osarch") {
                "64" => 0,
                "" => 1,
                _ => 2,
            };
            Some(((kind_rank, arch_rank), executable))
        })
        .collect();
    // A stable sort keeps Steam's order among equals.
    candidates.sort_by_key(|(rank, _)| *rank);
    candidates.into_iter().find_map(|(_, executable)| {
        let relative = executable.trim().replace('/', "\\");
        let relative = relative.trim_start_matches(".\\").trim_start_matches('\\');
        let path = install_dir.join(relative);
        let is_exe = path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
        (is_exe && path.is_file()).then_some(path)
    })
}

fn library_folders(steam: &Path) -> Result<Vec<PathBuf>> {
    let file = steam.join("steamapps").join("libraryfolders.vdf");
    let mut folders = vec![steam.to_path_buf()];

    let Ok(text) = fs::read_to_string(&file) else {
        return Ok(folders);
    };
    let root = vdf::parse(&text).map_err(|source| Error::Vdf {
        path: file.clone(),
        source,
    })?;
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
    // Redistributables Steam installs alongside games are hidden by `config/games.json`.

    let install_dir = state
        .get_str("installdir")
        .map(|dir| on_disk_case(&steamapps.join("common").join(dir)));

    Some(Game {
        id: format!("steam:{app_id}"),
        name: name.to_owned(),
        store: Store::Steam,
        install_dir,
        launch: Launch::Uri(format!("steam://rungameid/{app_id}")),
        alt_launches: Vec::new(),
        icon: icon(app_id),
        program: None,
        arch: None,
        edits: None,
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

/// Steam's Play link with launch options: `steam://rungameid/<appid>` becomes
/// `steam://run/<appid>//<percent-encoded options>/`, which starts the app with those options.
/// Empty options keep the plain link. `None` for links that are not a Steam game's.
pub(crate) fn run_uri_with_args(uri: &str, args: &str) -> Option<String> {
    let app_id = uri.strip_prefix("steam://rungameid/")?;
    if app_id.is_empty() || !app_id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let args = args.trim();
    if args.is_empty() {
        return Some(uri.to_owned());
    }
    Some(format!("steam://run/{app_id}//{}/", percent_encode(args)))
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    a.to_string_lossy()
        .trim_end_matches('\\')
        .eq_ignore_ascii_case(b.to_string_lossy().trim_end_matches('\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Binary KeyValues `appinfo { config { launch { 0 { executable <exe> } } } }` with inline
    /// keys (`table` = None) or string-table keys.
    fn app_kv(exe: &str, table: Option<&[&str]>) -> Vec<u8> {
        let key = |data: &mut Vec<u8>, name: &str| match table {
            Some(table) => {
                let index = table.iter().position(|k| *k == name).unwrap() as u32;
                data.extend(index.to_le_bytes());
            }
            None => {
                data.extend(name.as_bytes());
                data.push(0);
            }
        };
        let mut data = Vec::new();
        for name in ["appinfo", "config", "launch", "0"] {
            data.push(0x00);
            key(&mut data, name);
        }
        data.push(0x01);
        key(&mut data, "executable");
        data.extend(exe.as_bytes());
        data.push(0);
        data.extend([0x08; 5]);
        data
    }

    /// An `appinfo.vdf` with the given apps; version 29 when `table` is given.
    fn app_info_file(apps: &[(u32, Vec<u8>)], table: Option<&[&str]>) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend(
            if table.is_some() {
                APPINFO_V29
            } else {
                APPINFO_V28
            }
            .to_le_bytes(),
        );
        data.extend(1u32.to_le_bytes());
        if table.is_some() {
            data.extend(0u64.to_le_bytes()); // string table offset, set below
        }
        for (id, kv) in apps {
            data.extend(id.to_le_bytes());
            data.extend(((APPINFO_ENTRY_HEADER + kv.len()) as u32).to_le_bytes());
            data.extend([0xAB; APPINFO_ENTRY_HEADER]);
            data.extend(kv);
        }
        data.extend(0u32.to_le_bytes());
        if let Some(table) = table {
            let offset = data.len() as u64;
            data[8..16].copy_from_slice(&offset.to_le_bytes());
            data.extend((table.len() as u32).to_le_bytes());
            for key in table {
                data.extend(key.as_bytes());
                data.push(0);
            }
        }
        data
    }

    fn executable(apps: &HashMap<u32, vdf::Value>, id: u32) -> Option<&str> {
        apps.get(&id)?
            .get("appinfo")?
            .get("config")?
            .get("launch")?
            .get("0")?
            .get_str("executable")
    }

    #[test]
    fn reads_wanted_apps_from_both_versions() {
        let wanted = HashSet::from([620980, 220]);
        let table = ["appinfo", "config", "launch", "0", "executable"];
        for table in [None, Some(table.as_slice())] {
            let file = app_info_file(
                &[
                    (620980, app_kv("Beat Saber.exe", table)),
                    // Not installed: skipped without parsing, even though it is not valid.
                    (12345, vec![0x42; 10]),
                    (220, app_kv("hl2.exe", table)),
                ],
                table,
            );
            let apps = parse_app_info(&file, &wanted).unwrap();
            assert_eq!(apps.len(), 2);
            assert_eq!(executable(&apps, 620980), Some("Beat Saber.exe"));
            assert_eq!(executable(&apps, 220), Some("hl2.exe"));
        }
        assert!(parse_app_info(&[0x27, 0x44, 0x56, 0x07, 1, 0, 0, 0], &wanted).is_none());
        assert!(parse_app_info(&[], &wanted).is_none());
    }

    #[test]
    fn launch_options_go_through_steams_run_link() {
        let run = |args: &str| run_uri_with_args("steam://rungameid/220", args);
        assert_eq!(
            run(" -novid -console ").as_deref(),
            Some("steam://run/220//-novid%20-console/")
        );
        assert_eq!(
            run(r#"+map "c1a0" /x"#).as_deref(),
            Some("steam://run/220//%2Bmap%20%22c1a0%22%20%2Fx/")
        );
        assert_eq!(run("  ").as_deref(), Some("steam://rungameid/220"));
        assert_eq!(
            run_uri_with_args("com.epicgames.launcher://apps/x", "-x"),
            None
        );
        assert_eq!(run_uri_with_args("steam://rungameid/", "-x"), None);
        assert_eq!(run_uri_with_args("steam://rungameid/22a", "-x"), None);
    }

    #[test]
    fn chooses_the_launch_entry_steam_uses() {
        let dir = std::env::temp_dir().join(format!("GM-Steam-Launch-{}", std::process::id()));
        fs::create_dir_all(dir.join("bin64")).unwrap();
        for file in ["Game.exe", r"bin64\Game64.exe", "Editor.exe"] {
            fs::write(dir.join(file), b"MZ").unwrap();
        }
        let app = |launch: &str| {
            vdf::parse(&format!(
                r#""appinfo" {{ "config" {{ "launch" {{ {launch} }} }} }}"#
            ))
            .unwrap()
        };
        let entries = r#"
            "0" { "executable" "Editor.exe" "type" "editor" }
            "1" { "executable" "Game.exe" "type" "default" "config" { "oslist" "windows" "osarch" "32" } }
            "2" { "executable" "bin64/Game64.exe" "type" "default" "config" { "oslist" "windows" "osarch" "64" } }
            "3" { "executable" "game.sh" "config" { "oslist" "linux" } }
        "#;
        assert_eq!(
            launch_program(&app(entries), &dir),
            Some(dir.join(r"bin64\Game64.exe"))
        );

        // A missing 64-bit program falls back to the next entry; non-Windows entries never win.
        let missing = entries.replace("bin64/Game64.exe", "bin64/Gone.exe");
        assert_eq!(
            launch_program(&app(&missing), &dir),
            Some(dir.join("Game.exe"))
        );
        let linux_only = r#""0" { "executable" "Game.exe" "config" { "oslist" "linux,macos" } }"#;
        assert_eq!(launch_program(&app(linux_only), &dir), None);
        // Only an editor: still better than nothing. A leading `./` is ignored.
        let editor = r#""0" { "executable" "./Editor.exe" "type" "editor" }"#;
        assert_eq!(
            launch_program(&app(editor), &dir),
            Some(dir.join("Editor.exe"))
        );

        fs::remove_dir_all(&dir).unwrap();
    }
}
