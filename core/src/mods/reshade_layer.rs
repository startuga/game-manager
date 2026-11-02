//! ReShade's Vulkan layer: how ReShade runs in Vulkan games and on top of DXVK (which turns
//! Direct3D into Vulkan). It is one install for the whole computer, laid out exactly as ReShade's
//! own setup lays it out, so both can manage it side by side:
//!
//! - `%ProgramData%\ReShade` holds `ReShade32.dll`, `ReShade64.dll` and their layer manifests,
//!   `ReShade32.json` and `ReShade64.json` (whose `library_path` is the DLL next to them). When
//!   Game Manager creates the folder it grants ALL APPLICATION PACKAGES read & execute, so Store
//!   games can load the layer.
//! - The manifests are registered as implicit layers: a value named after the manifest's path,
//!   0, under `HKLM\SOFTWARE\Khronos\Vulkan\ImplicitLayers` (the 32-bit one under
//!   `WOW6432Node`).
//! - The layer loads itself only into the programs listed in `ReShadeApps.ini`, a UTF-8 file
//!   (with a BOM, as the setup writes it) whose `Apps=` line lists program paths separated by
//!   commas. A path with a comma can therefore never be listed.
//!
//! Installing replaces the DLLs, so every listed program then gets that build. Taking the last
//! program off the list removes the layer: its files, registry values and folder. Only the
//! `Apps=` line is rewritten; other programs' entries stay exactly as they were. Everything here
//! needs administrator rights (`HKLM`, `ProgramData`): it runs in the copy of Game Manager
//! started with `--reshade-ops` (see `reshade`).

use crate::{Error, Result, security, version_info};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// The layer's files, in a ReShade build folder and in the layer's folder.
pub(crate) const LAYER_FILES: [&str; 4] = [
    "ReShade32.dll",
    "ReShade64.dll",
    "ReShade32.json",
    "ReShade64.json",
];
const APPS_FILE: &str = "ReShadeApps.ini";
const APPS_KEY: &str = "Apps=";
const BOM: &str = "\u{feff}";

/// A change to the layer, made as administrator.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LayerChange {
    /// A ReShade build folder holding `LAYER_FILES`, to install or update the layer from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_from: Option<PathBuf>,
    /// A program to add to the layer's list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_app: Option<PathBuf>,
    /// A program to take off the list; the layer goes when none is left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_app: Option<PathBuf>,
}

/// The layer as it is now: read without administrator rights.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerStatus {
    /// Its files are there and registered.
    pub installed: bool,
    /// `ReShade64.dll`'s product version.
    pub version: Option<String>,
    /// The programs it loads into.
    pub apps: Vec<String>,
}

impl LayerStatus {
    /// How many programs are on the list: the entries that start a path (`C:\`, `\\server`), as a
    /// path with a comma shows as two entries.
    pub fn programs(&self) -> usize {
        self.apps
            .iter()
            .filter(|app| {
                let app = app.trim().as_bytes();
                app.starts_with(br"\\") || (app.len() > 2 && app[1] == b':' && app[2] == b'\\')
            })
            .count()
    }

    /// Whether `program` is on the layer's list.
    pub fn lists(&self, program: &Path) -> bool {
        let program = program.to_string_lossy();
        self.apps
            .iter()
            .any(|app| app.eq_ignore_ascii_case(&program))
    }
}

/// Where the layer lives: the system's place, or a test's.
struct Place {
    folder: PathBuf,
    root: &'static windows_registry::Key,
    key_64: &'static str,
    key_32: &'static str,
}

impl Place {
    fn system() -> Result<Self> {
        let program_data =
            std::env::var_os("ProgramData").ok_or(Error::MissingEnvVar("ProgramData"))?;
        Ok(Self {
            folder: PathBuf::from(program_data).join("ReShade"),
            root: windows_registry::LOCAL_MACHINE,
            key_64: r"SOFTWARE\Khronos\Vulkan\ImplicitLayers",
            key_32: r"SOFTWARE\WOW6432Node\Khronos\Vulkan\ImplicitLayers",
        })
    }

    /// The manifests and the key each is registered under.
    fn manifests(&self) -> [(PathBuf, &'static str); 2] {
        [
            (self.folder.join("ReShade64.json"), self.key_64),
            (self.folder.join("ReShade32.json"), self.key_32),
        ]
    }
}

/// The layer on this computer.
pub fn status() -> LayerStatus {
    Place::system()
        .map(|place| status_at(&place))
        .unwrap_or_default()
}

fn status_at(place: &Place) -> LayerStatus {
    let (manifest, key) = &place.manifests()[0];
    let registered = place
        .root
        .open(key)
        .and_then(|key| key.get_u32(manifest.to_string_lossy()))
        .is_ok();
    let apps = read_apps(&place.folder.join(APPS_FILE))
        .map(|text| apps_in(&text))
        .unwrap_or_default();
    LayerStatus {
        installed: registered && manifest.is_file(),
        version: version_info::product_version(&place.folder.join("ReShade64.dll")),
        apps,
    }
}

/// Makes `change` to the layer on this computer. Needs administrator rights.
pub(crate) fn apply(change: &LayerChange) -> Result<()> {
    apply_at(&Place::system()?, change)
}

fn apply_at(place: &Place, change: &LayerChange) -> Result<()> {
    if let Some(build) = &change.install_from {
        install(place, build)?;
    }
    if change.add_app.is_none() && change.remove_app.is_none() {
        return Ok(());
    }
    let file = place.folder.join(APPS_FILE);
    let text = read_apps(&file).unwrap_or_default();
    let had_bom = text.starts_with(BOM) || text.is_empty();
    let (text, left) = apps_after(
        text.trim_start_matches(BOM),
        change.add_app.as_deref(),
        change.remove_app.as_deref(),
    )?;
    if left == 0 {
        return remove(place);
    }
    let text = if had_bom {
        format!("{BOM}{text}")
    } else {
        text
    };
    fs::create_dir_all(&place.folder).map_err(Error::io(&place.folder))?;
    fs::write(&file, text).map_err(Error::io(&file))
}

fn install(place: &Place, build: &Path) -> Result<()> {
    if !place.folder.exists() {
        fs::create_dir_all(&place.folder).map_err(Error::io(&place.folder))?;
        security::grant(
            &place.folder,
            security::ALL_APPLICATION_PACKAGES,
            security::READ_EXECUTE,
        )?;
    }
    for name in LAYER_FILES {
        let (from, to) = (build.join(name), place.folder.join(name));
        // A running Vulkan program holds the DLL: the error names it.
        fs::copy(&from, &to).map_err(Error::io(&to))?;
    }
    for (manifest, key) in place.manifests() {
        place
            .root
            .create(key)
            .and_then(|key| key.set_u32(manifest.to_string_lossy(), 0))
            .map_err(Error::windows(format!(
                "registering {}",
                manifest.display()
            )))?;
    }
    Ok(())
}

/// Removes the layer, as ReShade's setup does when its list ends empty.
fn remove(place: &Place) -> Result<()> {
    for (manifest, key) in place.manifests() {
        if let Ok(key) = place.root.options().read().write().open(key) {
            let _ = key.remove_value(manifest.to_string_lossy());
        }
    }
    for name in LAYER_FILES.iter().chain([&APPS_FILE]) {
        let file = place.folder.join(name);
        if file.exists() {
            fs::remove_file(&file).map_err(Error::io(&file))?;
        }
    }
    // Fails, as it should, while anything else is in the folder.
    let _ = fs::remove_dir(&place.folder);
    Ok(())
}

fn read_apps(file: &Path) -> Option<String> {
    fs::read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// The programs on the `Apps=` line of `ReShadeApps.ini`'s text.
fn apps_in(text: &str) -> Vec<String> {
    text.trim_start_matches(BOM)
        .lines()
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| apps_value(line))
        .map(|value| {
            value
                .split(',')
                .filter(|app| !app.trim().is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn apps_value(line: &str) -> Option<&str> {
    let line = line.trim_start();
    line.get(..APPS_KEY.len())
        .filter(|key| key.eq_ignore_ascii_case(APPS_KEY))
        .map(|_| &line[APPS_KEY.len()..])
}

/// `ReShadeApps.ini`'s text (without BOM) with `add` added to its `Apps=` list and `remove` taken
/// off (paths compared without case), and how many programs the list then holds. Other lines,
/// and the other entries as written, stay.
fn apps_after(text: &str, add: Option<&Path>, remove: Option<&Path>) -> Result<(String, usize)> {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let at = lines
        .iter()
        .take_while(|line| !line.trim_start().starts_with('['))
        .position(|line| apps_value(line).is_some());
    let mut apps = at.map(|at| apps_in(&lines[at])).unwrap_or_default();
    if let Some(remove) = remove {
        let remove = remove.to_string_lossy();
        apps.retain(|app| !app.eq_ignore_ascii_case(&remove));
    }
    if let Some(add) = add {
        let add = add.to_string_lossy();
        if add.contains(',') {
            return Err(Error::NotInstallable(comma_message(&add)));
        }
        if !apps.iter().any(|app| app.eq_ignore_ascii_case(&add)) {
            apps.push(add.into_owned());
        }
    }
    let line = format!("{APPS_KEY}{}", apps.join(","));
    match at {
        Some(at) => lines[at] = line,
        None => lines.insert(0, line),
    }
    Ok((lines.join("\r\n") + "\r\n", apps.len()))
}

/// Why a program whose path has a comma cannot use the layer.
pub(crate) fn comma_message(program: &str) -> String {
    format!(
        "ReShade's Vulkan layer lists programs separated by commas, so it cannot list {program}: \
         move the game to a folder without a comma."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTED: &str = "Apps=C:\\Games\\A\\a.exe,C:\\GOG Games\\Warhammer 40,000 Space \
                          Marine\\SpaceMarine.exe,C:\\Games\\B\\b.exe\r\n\r\n\r\n";

    #[test]
    fn edits_only_its_own_entry_of_the_list() {
        assert_eq!(apps_in(LISTED).len(), 4);
        let status = LayerStatus {
            apps: apps_in(LISTED),
            ..LayerStatus::default()
        };
        assert_eq!(status.programs(), 3);
        let (text, left) = apps_after(LISTED, Some(Path::new(r"C:\Games\C\c.exe")), None).unwrap();
        assert_eq!(left, 5);
        assert!(text.starts_with(&LISTED[..LISTED.len() - 6]), "{text}");
        assert!(text.contains(r"b.exe,C:\Games\C\c.exe"), "{text}");
        assert!(text.ends_with("\r\n\r\n\r\n"), "{text:?}");

        // Already listed (in other case): nothing changes.
        let (same, _) = apps_after(LISTED, Some(Path::new(r"c:\games\a\A.EXE")), None).unwrap();
        assert_eq!(same, LISTED);

        // Removing keeps the others as written, the comma path included.
        let (text, left) = apps_after(LISTED, None, Some(Path::new(r"C:\GAMES\B\B.EXE"))).unwrap();
        assert_eq!(left, 3);
        assert!(text.contains("Warhammer 40,000 Space Marine"), "{text}");
        assert!(!text.contains("b.exe"), "{text}");

        // A new file gets the line; a path with a comma cannot be added.
        let (text, left) = apps_after("", Some(Path::new(r"C:\Games\a.exe")), None).unwrap();
        assert_eq!((text.as_str(), left), ("Apps=C:\\Games\\a.exe\r\n", 1));
        assert!(apps_after("", Some(Path::new(r"C:\Games, Old\a.exe")), None).is_err());
        let (_, left) = apps_after(&text, None, Some(Path::new(r"C:\Games\a.exe"))).unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn installs_lists_and_removes_the_layer() {
        let dir = std::env::temp_dir().join(format!("GM-Layer-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let build = dir.join("build");
        fs::create_dir_all(&build).unwrap();
        for name in LAYER_FILES {
            fs::write(build.join(name), name).unwrap();
        }
        let place = Place {
            folder: dir.join("ReShade"),
            root: windows_registry::CURRENT_USER,
            key_64: r"Software\Game Manager Tests\Layers64",
            key_32: r"Software\Game Manager Tests\Layers32",
        };
        let game = PathBuf::from(r"C:\Games\Test\game.exe");
        let other = PathBuf::from(r"C:\Games\Other\other.exe");

        apply_at(
            &place,
            &LayerChange {
                install_from: Some(build.clone()),
                add_app: Some(game.clone()),
                remove_app: None,
            },
        )
        .unwrap();
        let status = status_at(&place);
        assert!(status.installed && status.lists(&game));
        assert_eq!(
            fs::read_to_string(place.folder.join("ReShade32.dll")).unwrap(),
            "ReShade32.dll"
        );
        let apps = fs::read_to_string(place.folder.join(APPS_FILE)).unwrap();
        assert!(apps.starts_with(BOM), "{apps:?}");

        // Another program keeps the layer when the first leaves.
        let add_other = LayerChange {
            add_app: Some(other.clone()),
            ..LayerChange::default()
        };
        apply_at(&place, &add_other).unwrap();
        let remove_game = LayerChange {
            remove_app: Some(game.clone()),
            ..LayerChange::default()
        };
        apply_at(&place, &remove_game).unwrap();
        let status = status_at(&place);
        assert!(status.installed && !status.lists(&game) && status.lists(&other));

        // The last one takes the layer with it.
        let remove_other = LayerChange {
            remove_app: Some(other),
            ..LayerChange::default()
        };
        apply_at(&place, &remove_other).unwrap();
        assert!(!status_at(&place).installed);
        assert!(!place.folder.exists());
        let _ = windows_registry::CURRENT_USER.remove_tree(r"Software\Game Manager Tests");
        fs::remove_dir_all(dir).unwrap();
    }
}
