//! Uninstalling a game is its store's job: Game Manager hands over, and the store or the game's
//! own uninstaller asks before it removes anything. Nothing here deletes a file.
//!
//! `route` picks how (reading the registry only):
//! 1. **Steam**: `steam://uninstall/<appid>`. Steam asks, then removes the game.
//!    **Ubisoft Connect**: `uplay://uninstall/<install id>`, the link its own Uninstall entries
//!    hand to the launcher (`upc.exe uplay://uninstall/<id>`).
//! 2. **An Uninstall entry** (HKLM 64 and 32-bit, HKCU) whose `InstallLocation` is the game's
//!    folder: its `UninstallString`, the game's own uninstaller (GOG's `unins000.exe`, the EA
//!    app's cleanup, an MSI). Started through the shell, so one that needs administrator rights
//!    asks for them.
//! 3. **The store's app**, where the user uninstalls it: Epic Games Launcher's library (Epic keeps
//!    no Uninstall entry), GOG Galaxy on the game, else Windows' Installed apps (Xbox games, and
//!    anything without an entry).
//! 4. **Games added by hand**: none; the app removes them from its list instead.
//!
//! A game's mods outside its folder (ReShade's Vulkan layer entry, NVIDIA driver profiles) are not
//! touched; the app says so before handing over.

use crate::{Game, Launch, Result, Store, UNINSTALL_KEYS, folder_key, launch};
use std::path::{Path, PathBuf};
use windows_registry::{CURRENT_USER, Key, LOCAL_MACHINE};

/// How a game is uninstalled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Route {
    /// Steam's own uninstall, for this app id.
    Steam(String),
    /// The game's own uninstaller, from its Uninstall entry.
    Uninstaller {
        /// The entry's `DisplayName`.
        name: String,
        program: PathBuf,
        args: String,
    },
    /// The store's app, where the user uninstalls the game.
    StoreApp { store: Store, uri: String },
}

impl Route {
    /// What handing over does, for the confirmation.
    pub fn describe(&self) -> String {
        match self {
            Route::Steam(_) => "Steam asks you to confirm, then uninstalls it.".to_owned(),
            Route::Uninstaller { name, .. } => format!(
                "Its uninstaller ({name}) opens and asks before it removes anything; it may ask \
                 for administrator rights."
            ),
            Route::StoreApp {
                store: Store::Epic, ..
            } => "The Epic Games Launcher's library opens: uninstall it there, from the game's \
                  … menu."
                .to_owned(),
            Route::StoreApp {
                store: Store::Gog, ..
            } => "GOG Galaxy opens on the game: uninstall it there.".to_owned(),
            Route::StoreApp {
                store: Store::Ubisoft,
                ..
            } => "Ubisoft Connect opens and asks before it uninstalls it.".to_owned(),
            Route::StoreApp { .. } => {
                "Windows' Installed apps opens: find the game there and uninstall it.".to_owned()
            }
        }
    }
}

/// A Windows Uninstall entry with a folder and a command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub folder: PathBuf,
    pub command: String,
}

/// How `game` is uninstalled; `None` for games added by hand. Reads the registry.
pub fn route(game: &Game) -> Option<Route> {
    route_in(game, &entries())
}

/// Hands over to the route's store or uninstaller. Call it on the UI thread, like `launch`.
pub fn run(route: &Route) -> Result<()> {
    match route {
        Route::Steam(app_id) => launch(&Launch::Uri(format!("steam://uninstall/{app_id}"))),
        Route::Uninstaller { program, args, .. } => launch(&Launch::Exe {
            path: program.clone(),
            args: args.clone(),
            working_dir: None,
        }),
        Route::StoreApp { uri, .. } => launch(&Launch::Uri(uri.clone())),
    }
}

pub(crate) fn route_in(game: &Game, entries: &[Entry]) -> Option<Route> {
    match game.store {
        Store::Custom => return None,
        Store::Steam => {
            if let Some(app_id) = game.id.strip_prefix("steam:") {
                return Some(Route::Steam(app_id.to_owned()));
            }
        }
        Store::Ubisoft => {
            if let Some(id) = game.id.strip_prefix("ubisoft:") {
                return Some(Route::StoreApp {
                    store: Store::Ubisoft,
                    uri: format!("uplay://uninstall/{id}"),
                });
            }
        }
        _ => {}
    }
    let entry = game.install_dir.as_deref().and_then(|dir| {
        let key = folder_key(dir);
        entries
            .iter()
            .find(|entry| folder_key(&entry.folder) == key)
    });
    if let Some(entry) = entry
        && let Some((program, args)) = split_command(&entry.command)
    {
        return Some(Route::Uninstaller {
            name: entry.name.clone(),
            program,
            args,
        });
    }
    let uri = match (game.store, game.id.split_once(':')) {
        (Store::Epic, _) => "com.epicgames.launcher://library".to_owned(),
        (Store::Gog, Some((_, id))) => format!("goggalaxy://openGameView/{id}"),
        _ => "ms-settings:appsfeatures".to_owned(),
    };
    Some(Route::StoreApp {
        store: game.store,
        uri,
    })
}

/// An `UninstallString` as program and arguments: a quoted program, else everything up to the
/// first `.exe`, else the first word. `None` when it is empty.
pub(crate) fn split_command(command: &str) -> Option<(PathBuf, String)> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    let (program, args) = if let Some(rest) = command.strip_prefix('"') {
        let end = rest.find('"').unwrap_or(rest.len());
        (&rest[..end], rest.get(end + 1..).unwrap_or_default())
    } else if let Some(at) = command.to_ascii_lowercase().find(".exe") {
        command.split_at(at + 4)
    } else {
        command.split_once(' ').unwrap_or((command, ""))
    };
    let program = program.trim();
    (!program.is_empty()).then(|| (PathBuf::from(program), args.trim().to_owned()))
}

/// The Uninstall entries of this machine and user that name a folder and a command.
fn entries() -> Vec<Entry> {
    let mut entries = Vec::new();
    for (root, path) in UNINSTALL_KEYS
        .iter()
        .map(|path| (&LOCAL_MACHINE, *path))
        .chain(std::iter::once((&CURRENT_USER, UNINSTALL_KEYS[0])))
    {
        let Ok(parent) = root.open(path) else {
            continue;
        };
        let Ok(names) = parent.keys() else {
            continue;
        };
        for name in names {
            if let Ok(key) = parent.open(&name)
                && let Some(entry) = entry(&key)
            {
                entries.push(entry);
            }
        }
    }
    entries
}

fn entry(key: &Key) -> Option<Entry> {
    let folder = key.get_string("InstallLocation").ok()?;
    let folder = folder.trim().trim_matches('"');
    let command = key.get_string("UninstallString").ok()?;
    if folder.is_empty() || command.trim().is_empty() {
        return None;
    }
    Some(Entry {
        name: key.get_string("DisplayName").unwrap_or_default(),
        folder: Path::new(folder).to_path_buf(),
        command,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: &str, store: Store, dir: Option<&str>) -> Game {
        Game {
            id: id.into(),
            name: "A game".into(),
            store,
            install_dir: dir.map(PathBuf::from),
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: None,
            arch: None,
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    #[test]
    fn splits_uninstall_commands() {
        assert_eq!(
            split_command(r#""C:\GOG Games\Game\unins000.exe""#),
            Some((
                PathBuf::from(r"C:\GOG Games\Game\unins000.exe"),
                String::new()
            ))
        );
        assert_eq!(
            split_command(r#""C:\Program Files (x86)\Steam\steam.exe" steam://uninstall/220"#),
            Some((
                PathBuf::from(r"C:\Program Files (x86)\Steam\steam.exe"),
                "steam://uninstall/220".to_owned()
            ))
        );
        assert_eq!(
            split_command("MsiExec.exe /X{01234567-89AB-CDEF-0123-456789ABCDEF}"),
            Some((
                PathBuf::from("MsiExec.exe"),
                "/X{01234567-89AB-CDEF-0123-456789ABCDEF}".to_owned()
            ))
        );
        assert_eq!(
            split_command(
                r"C:\Program Files (x86)\Common Files\EAInstaller\Syndicate\Cleanup.exe uninstall_game"
            ),
            Some((
                PathBuf::from(
                    r"C:\Program Files (x86)\Common Files\EAInstaller\Syndicate\Cleanup.exe"
                ),
                "uninstall_game".to_owned()
            ))
        );
        assert_eq!(split_command("   "), None);
    }

    #[test]
    fn routes_by_store_then_entry_then_store_app() {
        let entries = vec![Entry {
            name: "The Witcher 3".into(),
            folder: PathBuf::from(r"D:\GOG Games\The Witcher 3\"),
            command: r#""D:\GOG Games\The Witcher 3\unins000.exe""#.into(),
        }];
        // Steam's own uninstall, whatever the registry holds.
        assert_eq!(
            route_in(&game("steam:220", Store::Steam, Some(r"C:\HL2")), &entries),
            Some(Route::Steam("220".into()))
        );
        // Ubisoft Connect's own link, not the entry that hands the same link to its launcher.
        let ubisoft = vec![Entry {
            name: "Child of Light".into(),
            folder: PathBuf::from("C:/Ubisoft/games/Child of Light/"),
            command: r#""C:\Ubisoft\upc.exe" uplay://uninstall/609"#.into(),
        }];
        let child_of_light = game(
            "ubisoft:609",
            Store::Ubisoft,
            Some(r"C:\Ubisoft\games\Child of Light"),
        );
        let route = route_in(&child_of_light, &ubisoft);
        assert_eq!(
            route,
            Some(Route::StoreApp {
                store: Store::Ubisoft,
                uri: "uplay://uninstall/609".into()
            })
        );
        assert!(route.is_some_and(|route| route.describe().starts_with("Ubisoft Connect")));
        // The entry whose folder is the game's (case and a trailing slash don't matter).
        assert_eq!(
            route_in(
                &game(
                    "gog:1207664643",
                    Store::Gog,
                    Some(r"d:\gog games\the witcher 3")
                ),
                &entries
            ),
            Some(Route::Uninstaller {
                name: "The Witcher 3".into(),
                program: PathBuf::from(r"D:\GOG Games\The Witcher 3\unins000.exe"),
                args: String::new(),
            })
        );
        // Without one, the store's app.
        assert_eq!(
            route_in(&game("gog:42", Store::Gog, Some(r"D:\Other")), &entries),
            Some(Route::StoreApp {
                store: Store::Gog,
                uri: "goggalaxy://openGameView/42".into()
            })
        );
        assert_eq!(
            route_in(
                &game("epic:Ginger", Store::Epic, Some(r"C:\Epic\Cyberpunk")),
                &[]
            ),
            Some(Route::StoreApp {
                store: Store::Epic,
                uri: "com.epicgames.launcher://library".into()
            })
        );
        assert_eq!(
            route_in(&game("xbox:Family", Store::Xbox, None), &[]),
            Some(Route::StoreApp {
                store: Store::Xbox,
                uri: "ms-settings:appsfeatures".into()
            })
        );
        // Games added by hand are removed from the list instead.
        assert_eq!(
            route_in(
                &game(
                    "custom:1",
                    Store::Custom,
                    Some(r"D:\GOG Games\The Witcher 3")
                ),
                &entries
            ),
            None
        );
    }
}
