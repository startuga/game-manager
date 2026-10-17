//! Finds games installed through the EA app (and Origin before it).
//!
//! The EA app's own records are encrypted, so the scanner works from what the EA installer leaves
//! behind:
//! - Every EA-installed game folder has `__Installer\installerdata.xml` (content IDs, titles and,
//!   for newer games, the program to start). Folders are found through Windows uninstall entries
//!   and `Install Dir` values under the `Electronic Arts` / `EA Games` registry keys.
//! - `HKLM\SOFTWARE\WOW6432Node\Origin Games\<contentID>` holds the store title and language.
//! - Older games list their program only in the Game Definition File (GDF) embedded in
//!   `GDFBinary_<locale>.dll`, the same source EA uses for its desktop shortcuts.
//!
//! An `__Installer` folder alone does not mean the EA app installed the game: copies set up by
//! other installers carry it too. A game is listed only when the EA app knows it, i.e. it has an
//! `Origin Games\<contentID>` key or an uninstall entry that runs EA's installer cleanup.

use crate::text::{child_text, decode_text, read_text, xml_body};
use crate::{Game, IconRef, Launch, Store, UNINSTALL_KEYS, folder_key};
use std::fs;
use std::path::{Path, PathBuf};
use windows::Win32::Foundation::FreeLibrary;
use windows::Win32::System::LibraryLoader::{
    FindResourceW, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE, LoadLibraryExW,
    LoadResource, LockResource, SizeofResource,
};
use windows::core::{HSTRING, w};

const PUBLISHER_KEYS: [&str; 4] = [
    r"SOFTWARE\Electronic Arts",
    r"SOFTWARE\WOW6432Node\Electronic Arts",
    r"SOFTWARE\EA Games",
    r"SOFTWARE\WOW6432Node\EA Games",
];
const ORIGIN_GAMES_KEY: &str = r"SOFTWARE\WOW6432Node\Origin Games";
const DEFAULT_LOCALE: &str = "en_US";

/// A folder that may hold an EA game, with what the registry says about it.
#[derive(Debug, Default, PartialEq)]
struct Candidate {
    dir: PathBuf,
    display_name: String,
    product_guid: String,
    /// The folder's uninstall entry runs EA's installer cleanup (`EAInstaller\...\Cleanup.exe`).
    ea_uninstaller: bool,
    /// The uninstall entry's `DisplayIcon`: `game.ico` or the game's program.
    icon: Option<IconRef>,
}

/// Scans the registry for EA-installed game folders.
pub fn scan() -> Result<Vec<Game>, String> {
    let mut candidates = Vec::new();
    for candidate in uninstall_candidates().chain(publisher_candidates()) {
        add_candidate(&mut candidates, candidate);
    }

    let mut games = Vec::new();
    for candidate in candidates {
        let Some(text) = read_text(&installer_data_path(&candidate.dir)) else {
            continue;
        };
        let data = parse_installer_data(&text).unwrap_or_default();
        let store = data
            .content_ids
            .iter()
            .find_map(|id| origin_games_entry(id));
        if store.is_none() && !candidate.ea_uninstaller {
            continue;
        }
        let locale = store
            .as_ref()
            .map(|s| s.locale.as_str())
            .filter(|l| !l.is_empty())
            .unwrap_or(DEFAULT_LOCALE);
        let gdf = read_gdf(&candidate.dir, locale).and_then(|text| parse_gdf(&text));

        let launch = launch_options(
            Some(&data),
            gdf.as_ref(),
            &candidate.dir,
            &read_registry_ref,
        )
        .into_iter()
        .find(|launch| matches!(launch, Launch::Exe { path, .. } if path.is_file()));
        let Some(launch) = launch else {
            continue;
        };

        let names = Names {
            store: store.as_ref().map(|s| s.display_name.as_str()),
            registry: &candidate.display_name,
            installer: data.title(locale),
            gdf: gdf.as_ref().map(|g| g.name.as_str()),
        };
        games.push(Game {
            id: game_id(&data, &candidate),
            name: names.choose(&candidate.dir),
            store: Store::Ea,
            icon: candidate.icon.or_else(|| {
                let ico = candidate.dir.join("game.ico");
                ico.is_file().then(|| IconRef::file(ico))
            }),
            install_dir: Some(candidate.dir),
            launch,
            alt_launches: Vec::new(),
        });
    }
    Ok(games)
}

fn installer_data_path(dir: &Path) -> PathBuf {
    dir.join(r"__Installer\installerdata.xml")
}

/// Adds `candidate` unless its folder is already listed; fills in details the first one lacked.
fn add_candidate(list: &mut Vec<Candidate>, candidate: Candidate) {
    let key = folder_key(&candidate.dir);
    if key.is_empty() {
        return;
    }
    match list.iter_mut().find(|c| folder_key(&c.dir) == key) {
        Some(existing) => {
            if existing.display_name.is_empty() {
                existing.display_name = candidate.display_name;
            }
            if existing.product_guid.is_empty() {
                existing.product_guid = candidate.product_guid;
            }
            existing.ea_uninstaller |= candidate.ea_uninstaller;
            if existing.icon.is_none() {
                existing.icon = candidate.icon;
            }
        }
        None => list.push(candidate),
    }
}

/// Whether an uninstall command runs EA's installer cleanup, e.g.
/// `"C:\Program Files (x86)\Common Files\EAInstaller\Syndicate\Cleanup.exe" uninstall_game`.
fn is_ea_uninstaller(command: &str) -> bool {
    let command = command.to_ascii_lowercase().replace('/', "\\");
    command.contains(r"\eainstaller\") && command.contains("cleanup.exe")
}

// ---------------------------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------------------------

/// Uninstall entries whose install folder has an EA installer folder.
fn uninstall_candidates() -> impl Iterator<Item = Candidate> {
    UNINSTALL_KEYS.into_iter().flat_map(|path| {
        subkeys(path)
            .filter_map(|(guid, key)| {
                let dir = key.get_string("InstallLocation").ok()?;
                let dir = PathBuf::from(dir.trim());
                installer_data_path(&dir).is_file().then(|| Candidate {
                    dir,
                    display_name: key.get_string("DisplayName").unwrap_or_default(),
                    product_guid: guid,
                    ea_uninstaller: is_ea_uninstaller(
                        &key.get_string("UninstallString").unwrap_or_default(),
                    ),
                    icon: key
                        .get_string("DisplayIcon")
                        .ok()
                        .and_then(|location| IconRef::parse(&location)),
                })
            })
            .collect::<Vec<_>>()
    })
}

/// `Electronic Arts\<game>` and `EA Games\<game>` keys with an `Install Dir`.
fn publisher_candidates() -> impl Iterator<Item = Candidate> {
    PUBLISHER_KEYS.into_iter().flat_map(|path| {
        subkeys(path)
            .filter_map(|(_, key)| {
                let dir = PathBuf::from(key.get_string("Install Dir").ok()?.trim());
                installer_data_path(&dir).is_file().then(|| Candidate {
                    dir,
                    display_name: key.get_string("DisplayName").unwrap_or_default(),
                    product_guid: key.get_string("Product GUID").unwrap_or_default(),
                    ea_uninstaller: false,
                    icon: None,
                })
            })
            .collect::<Vec<_>>()
    })
}

fn subkeys(path: &str) -> impl Iterator<Item = (String, windows_registry::Key)> {
    let parent = windows_registry::LOCAL_MACHINE.open(path).ok();
    let names: Vec<String> = parent
        .as_ref()
        .and_then(|key| key.keys().ok())
        .map(|keys| keys.collect())
        .unwrap_or_default();
    names.into_iter().filter_map(move |name| {
        let key = parent.as_ref()?.open(&name).ok()?;
        Some((name, key))
    })
}

struct OriginGamesEntry {
    display_name: String,
    locale: String,
}

/// `Origin Games\<contentID>`: the store title and the language the game was installed in.
fn origin_games_entry(content_id: &str) -> Option<OriginGamesEntry> {
    let key = windows_registry::LOCAL_MACHINE
        .open(format!(r"{ORIGIN_GAMES_KEY}\{content_id}"))
        .ok()?;
    Some(OriginGamesEntry {
        display_name: key.get_string("DisplayName").unwrap_or_default(),
        locale: key.get_string("Locale").unwrap_or_default(),
    })
}

/// Reads a registry reference like `HKEY_LOCAL_MACHINE\SOFTWARE\Respawn\Titanfall2\Install Dir`.
/// EA's 32-bit installer writes to the WOW6432Node view, so that is tried as well.
fn read_registry_ref(reference: &str) -> Option<String> {
    let (hive, rest) = reference.split_once('\\')?;
    let root = match hive.to_ascii_uppercase().as_str() {
        "HKEY_LOCAL_MACHINE" | "HKLM" => windows_registry::LOCAL_MACHINE,
        "HKEY_CURRENT_USER" | "HKCU" => windows_registry::CURRENT_USER,
        _ => return None,
    };
    let (key_path, value) = rest.rsplit_once('\\')?;
    let mut paths = vec![key_path.to_owned()];
    if let Some(tail) = key_path.strip_prefix(r"SOFTWARE\")
        && !tail.starts_with("WOW6432Node")
    {
        paths.push(format!(r"SOFTWARE\WOW6432Node\{tail}"));
    }
    paths
        .iter()
        .find_map(|path| root.open(path).ok()?.get_string(value).ok())
}

// ---------------------------------------------------------------------------------------------
// installerdata.xml
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct InstallerData {
    content_ids: Vec<String>,
    /// `(locale, title)` pairs.
    titles: Vec<(String, String)>,
    launchers: Vec<RuntimeLauncher>,
}

#[derive(Debug, PartialEq)]
struct RuntimeLauncher {
    file_path: String,
    parameters: String,
    trial: bool,
}

impl InstallerData {
    fn title(&self, locale: &str) -> Option<&str> {
        let find = |locale: &str| {
            self.titles
                .iter()
                .find(|(l, _)| l.eq_ignore_ascii_case(locale))
                .map(|(_, t)| t.as_str())
        };
        find(locale)
            .or_else(|| find(DEFAULT_LOCALE))
            .or_else(|| self.titles.first().map(|(_, t)| t.as_str()))
    }
}

/// Parses both manifest generations: `<game manifestVersion="3.0">` (titles in
/// `<localeInfo><title>`) and `<DiPManifest>` (titles in `<gameTitles><gameTitle>`).
fn parse_installer_data(text: &str) -> Option<InstallerData> {
    let doc = roxmltree::Document::parse(xml_body(text)).ok()?;
    let mut data = InstallerData::default();

    for node in doc.descendants().filter(|n| n.is_element()) {
        match node.tag_name().name() {
            // Launchers carry their own <contentID>; only the top-level list identifies the game.
            "contentID" if node.parent().is_some_and(|p| p.has_tag_name("contentIDs")) => {
                push_text(&mut data.content_ids, node)
            }
            "localeInfo" => {
                if let Some(title) = child_text(node, "title") {
                    let locale = node.attribute("locale").unwrap_or_default();
                    data.titles.push((locale.to_owned(), title.to_owned()));
                }
            }
            "gameTitle" => {
                if let Some(title) = node.text().map(str::trim).filter(|t| !t.is_empty()) {
                    let locale = node.attribute("locale").unwrap_or_default();
                    data.titles.push((locale.to_owned(), title.to_owned()));
                }
            }
            "launcher" if node.parent().is_some_and(|p| p.has_tag_name("runtime")) => {
                if let Some(file_path) = child_text(node, "filePath") {
                    data.launchers.push(RuntimeLauncher {
                        file_path: file_path.to_owned(),
                        parameters: child_text(node, "parameters")
                            .unwrap_or_default()
                            .to_owned(),
                        trial: child_text(node, "trial").is_some_and(|t| t == "1" || t == "true"),
                    });
                }
            }
            _ => {}
        }
    }
    Some(data)
}

fn push_text(list: &mut Vec<String>, node: roxmltree::Node) {
    if let Some(text) = node.text().map(str::trim).filter(|t| !t.is_empty()) {
        list.push(text.to_owned());
    }
}

/// Turns a manifest path like `[HKEY_LOCAL_MACHINE\...\Install Dir]bin\game.exe` into a real path.
/// When the registry reference cannot be read, the game's install folder is used instead.
fn resolve_manifest_path(
    file_path: &str,
    install_dir: &Path,
    read_ref: &dyn Fn(&str) -> Option<String>,
) -> PathBuf {
    let file_path = file_path.replace('/', "\\");
    let (base, rest) = match file_path.strip_prefix('[').and_then(|s| s.split_once(']')) {
        Some((reference, rest)) => (
            read_ref(reference)
                .map(PathBuf::from)
                .unwrap_or_else(|| install_dir.to_path_buf()),
            rest.to_owned(),
        ),
        None => (install_dir.to_path_buf(), file_path),
    };
    let rest = rest.trim_start_matches('\\');
    if rest.is_empty() {
        base
    } else {
        base.join(rest)
    }
}

// ---------------------------------------------------------------------------------------------
// Game Definition File
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct Gdf {
    name: String,
    /// Primary play task: `(path relative to the game folder, arguments)`.
    primary: Option<(String, String)>,
    executables: Vec<String>,
}

/// Reads the GDF XML from `GDFBinary_<locale>.dll`, falling back to en_US and then any language.
fn read_gdf(dir: &Path, locale: &str) -> Option<String> {
    let preferred = [locale, DEFAULT_LOCALE].map(|l| dir.join(format!("GDFBinary_{l}.dll")));
    let others = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                let n = n.to_ascii_lowercase();
                n.starts_with("gdfbinary_") && n.ends_with(".dll")
            })
        });
    preferred
        .into_iter()
        .filter(|p| p.is_file())
        .chain(others)
        .find_map(|dll| read_gdf_resource(&dll))
}

/// Loads the DLL as a data file (no code runs) and copies out its `DATA/__GDF_XML` resource.
fn read_gdf_resource(dll: &Path) -> Option<String> {
    let path = HSTRING::from(dll.as_os_str());
    // SAFETY: `path` is a valid wide string. The module is mapped as an image resource only, so
    // none of its code executes. The resource pointer is valid for `size` bytes until the module
    // is freed, and the bytes are copied before `FreeLibrary`.
    unsafe {
        let module = LoadLibraryExW(
            &path,
            None,
            LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE,
        )
        .ok()?;
        let text = (|| {
            let resource = FindResourceW(Some(module), w!("__GDF_XML"), w!("DATA"));
            if resource.is_invalid() {
                return None;
            }
            let size = SizeofResource(Some(module), resource) as usize;
            let data = LockResource(LoadResource(Some(module), resource).ok()?);
            if data.is_null() || size == 0 {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data.cast::<u8>(), size);
            Some(decode_text(bytes))
        })();
        let _ = FreeLibrary(module);
        text
    }
}

fn parse_gdf(text: &str) -> Option<Gdf> {
    let doc = roxmltree::Document::parse(xml_body(text)).ok()?;
    let element = |name: &str| {
        doc.descendants()
            .find(|n| n.is_element() && n.tag_name().name() == name)
    };

    let mut gdf = Gdf {
        name: element("GameDefinition")
            .and_then(|def| child_text(def, "Name"))
            .unwrap_or_default()
            .to_owned(),
        ..Gdf::default()
    };
    gdf.primary = element("Primary").and_then(|primary| {
        let task = primary
            .children()
            .find(|c| c.is_element() && c.tag_name().name() == "FileTask")?;
        let path = task.attribute("path").filter(|p| !p.is_empty())?;
        Some((
            path.to_owned(),
            task.attribute("arguments").unwrap_or_default().to_owned(),
        ))
    });
    gdf.executables = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "GameExecutable")
        .filter_map(|n| n.attribute("path").filter(|p| !p.is_empty()))
        .map(str::to_owned)
        .collect();
    Some(gdf)
}

// ---------------------------------------------------------------------------------------------
// Building the game
// ---------------------------------------------------------------------------------------------

/// Possible launch targets, best first. The scanner uses the first one whose program exists.
fn launch_options(
    data: Option<&InstallerData>,
    gdf: Option<&Gdf>,
    dir: &Path,
    read_ref: &dyn Fn(&str) -> Option<String>,
) -> Vec<Launch> {
    let exe = |path: PathBuf, args: &str| Launch::Exe {
        path,
        args: args.trim().to_owned(),
        working_dir: None,
    };
    let mut options = Vec::new();
    if let Some(data) = data {
        for launcher in data.launchers.iter().filter(|l| !l.trial) {
            let path = resolve_manifest_path(&launcher.file_path, dir, read_ref);
            options.push(exe(path, &launcher.parameters));
        }
    }
    if let Some(gdf) = gdf {
        if let Some((path, args)) = &gdf.primary {
            options.push(exe(dir.join(path), args));
        }
        for path in &gdf.executables {
            options.push(exe(dir.join(path), ""));
        }
    }
    options
}

struct Names<'a> {
    store: Option<&'a str>,
    registry: &'a str,
    installer: Option<&'a str>,
    gdf: Option<&'a str>,
}

impl Names<'_> {
    /// Store title first, so EA names match how Steam and Epic names appear (with ™ and ®).
    fn choose(&self, dir: &Path) -> String {
        [self.store, Some(self.registry), self.installer, self.gdf]
            .into_iter()
            .flatten()
            .map(str::trim)
            .find(|name| !name.is_empty())
            .map(str::to_owned)
            .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default()
    }
}

fn game_id(data: &InstallerData, candidate: &Candidate) -> String {
    let id = data
        .content_ids
        .first()
        .cloned()
        .or_else(|| (!candidate.product_guid.is_empty()).then(|| candidate.product_guid.clone()))
        .unwrap_or_else(|| folder_key(&candidate.dir));
    format!("ea:{id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real installerdata.xml of Syndicate (manifestVersion 3.0, trimmed).
    const SYNDICATE_DATA: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<game gameVersion="2.0.0.3" manifestVersion="3.0">
  <contentIDs>
    <contentID>1025174</contentID>
    <contentID>1009340</contentID>
  </contentIDs>
  <metadata>
    <localeInfo locale="en_US">
      <title>Syndicate</title>
      <eula name="Syndicate End User License Agreement">/Support/eula/en_US_eula.rtf</eula>
    </localeInfo>
  </metadata>
  <executable>
    <filePath>/__Installer/touchup.exe</filePath>
    <parameters>install -locale {locale} -installPath "{installLocation}" -autologging</parameters>
  </executable>
</game>"#;

    // Newer manifest generation (DiPManifest), from EA's documented layout.
    const DIP_DATA: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<DiPManifest version="4.0">
  <contentIDs><contentID>1035052</contentID></contentIDs>
  <gameTitles>
    <gameTitle locale="de_DE">Titanfall 2 DE</gameTitle>
    <gameTitle locale="en_US">Titanfall 2</gameTitle>
  </gameTitles>
  <runtime>
    <launcher uid="trial">
      <filePath>[HKEY_LOCAL_MACHINE\SOFTWARE\Respawn\Titanfall2\Install Dir]Titanfall2_trial.exe</filePath>
      <trial>1</trial>
    </launcher>
    <launcher uid="full">
      <name locale="en_US">Titanfall 2</name>
      <filePath>[HKEY_LOCAL_MACHINE\SOFTWARE\Respawn\Titanfall2\Install Dir]Titanfall2.exe</filePath>
      <parameters> -multiple </parameters>
      <trial>0</trial>
      <contentID>999</contentID>
    </launcher>
  </runtime>
</DiPManifest>"#;

    // GDF XML from Theme Hospital's GDFBinary_en_US.dll (trimmed); stored there as UTF-16 LE.
    const THEME_HOSPITAL_GDF: &str = r#"<?xml version="1.0" encoding="utf-16"?>
<GameDefinitionFile xmlns:baseTypes="urn:schemas-microsoft-com:GamesExplorerBaseTypes.v1" xmlns="urn:schemas-microsoft-com:GameDescription.v1">
	<GameDefinition gameID="{5118A4C2-C8A4-4CE5-AC37-F3E51C25402F}">
		<Name>Theme Hospital</Name>
		<GameExecutables>
			<GameExecutable path="data\Game\DOSBox\EALaunchHelper.exe" />
		</GameExecutables>
		<ExtendedProperties>
			<GameTasks>
				<Play>
					<Primary>
						<FileTask path="data\Game\DOSBox\EALaunchHelper.exe" arguments="" />
					</Primary>
					<Task index="1" name="Theme Hospital End User License Agreement">
						<FileTask path="support\eula\en_US_eula.rtf" arguments="" />
					</Task>
				</Play>
			</GameTasks>
		</ExtendedProperties>
	</GameDefinition>
</GameDefinitionFile>"#;

    fn no_registry(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn parses_legacy_installer_data() {
        let data = parse_installer_data(SYNDICATE_DATA).unwrap();
        assert_eq!(data.content_ids, ["1025174", "1009340"]);
        assert_eq!(data.title("en_US"), Some("Syndicate"));
        assert_eq!(data.title("fr_FR"), Some("Syndicate"));
        // <executable> is EA's installer, not the game.
        assert!(data.launchers.is_empty());
    }

    #[test]
    fn parses_dip_manifest() {
        let data = parse_installer_data(DIP_DATA).unwrap();
        assert_eq!(data.content_ids, ["1035052"]);
        assert_eq!(data.title("de_DE"), Some("Titanfall 2 DE"));
        assert_eq!(data.title("pt_PT"), Some("Titanfall 2"));
        assert_eq!(data.launchers.len(), 2);
        assert!(data.launchers[0].trial);
        assert!(!data.launchers[1].trial);
    }

    #[test]
    fn runtime_launcher_resolves_registry_path_and_skips_trial() {
        let data = parse_installer_data(DIP_DATA).unwrap();
        let read = |reference: &str| {
            (reference == r"HKEY_LOCAL_MACHINE\SOFTWARE\Respawn\Titanfall2\Install Dir")
                .then(|| r"D:\Games\Titanfall2\".to_owned())
        };
        let options = launch_options(Some(&data), None, Path::new(r"C:\x"), &read);
        assert_eq!(
            options,
            [Launch::Exe {
                path: PathBuf::from(r"D:\Games\Titanfall2\Titanfall2.exe"),
                args: "-multiple".into(),
                working_dir: None,
            }]
        );
    }

    #[test]
    fn unreadable_registry_reference_falls_back_to_install_dir() {
        let path = resolve_manifest_path(
            r"[HKEY_LOCAL_MACHINE\SOFTWARE\Missing\Install Dir]bin/game.exe",
            Path::new(r"C:\Games\X"),
            &no_registry,
        );
        assert_eq!(path, PathBuf::from(r"C:\Games\X\bin\game.exe"));
        let relative = resolve_manifest_path("game.exe", Path::new(r"C:\Games\X"), &no_registry);
        assert_eq!(relative, PathBuf::from(r"C:\Games\X\game.exe"));
    }

    #[test]
    fn gdf_primary_task_is_the_launch_target() {
        let gdf = parse_gdf(THEME_HOSPITAL_GDF).unwrap();
        assert_eq!(gdf.name, "Theme Hospital");
        assert_eq!(
            gdf.primary,
            Some((r"data\Game\DOSBox\EALaunchHelper.exe".into(), String::new()))
        );

        let dir = Path::new(r"C:\Program Files\EA Games\Theme Hospital");
        let data = parse_installer_data(SYNDICATE_DATA).unwrap();
        let options = launch_options(Some(&data), Some(&gdf), dir, &no_registry);
        assert_eq!(
            options[0],
            Launch::Exe {
                path: dir.join(r"data\Game\DOSBox\EALaunchHelper.exe"),
                args: String::new(),
                working_dir: None,
            }
        );
    }

    #[test]
    fn decodes_utf16_resource_text() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(THEME_HOSPITAL_GDF.encode_utf16().flat_map(u16::to_le_bytes));
        bytes.extend([0, 0]);
        let text = decode_text(&bytes);
        assert_eq!(text, THEME_HOSPITAL_GDF);
        assert_eq!(parse_gdf(&text).unwrap().name, "Theme Hospital");

        assert_eq!(decode_text(b"<a/>"), "<a/>");
    }

    // Real installerdata.xml of Bejeweled 3 (manifestVersion 2.1, trimmed). On disk it is UTF-16 LE
    // with a BOM, unlike the UTF-8 files of newer manifests.
    const BEJEWELED_DATA: &str = r#"<?xml version="1.0" encoding="utf-16"?>
<game gameVersion="1.1.13.4753" manifestVersion="2.1">
  <contentIDs>
    <contentID>71715</contentID>
  </contentIDs>
  <metadata>
    <os minVersion="5.1"></os>
    <localeInfo locale="en_US">
      <title>Bejeweled® 3</title>
      <exclude>/Bejeweled 3 BP/Bejeweled3.exe</exclude>
    </localeInfo>
    <localeInfo locale="pt_BR">
      <title>Bejeweled® 3</title>
    </localeInfo>
  </metadata>
</game>"#;

    fn utf16_file(text: &str) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        bytes
    }

    #[test]
    fn reads_utf16_installer_data() {
        let text = decode_text(&utf16_file(BEJEWELED_DATA));
        let data = parse_installer_data(&text).unwrap();
        assert_eq!(data.content_ids, ["71715"]);
        assert_eq!(data.title("pt_BR"), Some("Bejeweled® 3"));
    }

    #[test]
    fn reads_installer_data_files_in_both_encodings() {
        let dir = std::env::temp_dir().join(format!("gm-core-ea-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let utf16 = dir.join("utf16.xml");
        let utf8 = dir.join("utf8.xml");
        fs::write(&utf16, utf16_file(BEJEWELED_DATA)).unwrap();
        fs::write(&utf8, SYNDICATE_DATA).unwrap();

        let ids = |path: &Path| {
            parse_installer_data(&read_text(path).unwrap())
                .unwrap()
                .content_ids
        };
        assert_eq!(ids(&utf16), ["71715"]);
        assert_eq!(ids(&utf8), ["1025174", "1009340"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_broken_xml() {
        assert!(parse_installer_data("<game><contentIDs>").is_none());
        assert!(parse_gdf("not xml").is_none());
    }

    #[test]
    fn name_prefers_store_title() {
        let dir = Path::new(r"C:\Program Files\EA Games\Syndicate (1993)");
        let mut names = Names {
            store: Some("Syndicate™ (1993)"),
            registry: "Syndicate",
            installer: Some("Syndicate"),
            gdf: Some("Syndicate"),
        };
        assert_eq!(names.choose(dir), "Syndicate™ (1993)");
        names.store = None;
        names.registry = "";
        assert_eq!(names.choose(dir), "Syndicate");
        names.installer = None;
        names.gdf = None;
        assert_eq!(names.choose(dir), "Syndicate (1993)");
    }

    #[test]
    fn candidates_are_merged_by_folder() {
        let mut list = Vec::new();
        add_candidate(
            &mut list,
            Candidate {
                dir: PathBuf::from(r"C:\Program Files\EA Games\Theme Hospital\"),
                product_guid: "{5118A4C2}".into(),
                ea_uninstaller: true,
                ..Default::default()
            },
        );
        add_candidate(
            &mut list,
            Candidate {
                dir: PathBuf::from(r"c:\program files\ea games\theme hospital"),
                display_name: "Theme Hospital".into(),
                ..Default::default()
            },
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].display_name, "Theme Hospital");
        assert_eq!(list[0].product_guid, "{5118A4C2}");
        assert!(list[0].ea_uninstaller);
    }

    #[test]
    fn recognizes_ea_installer_cleanup() {
        // Real UninstallString values: Syndicate (EA app) and The Sims 4 (other installer).
        assert!(is_ea_uninstaller(
            r#""C:\Program Files (x86)\Common Files\EAInstaller\Syndicate\Cleanup.exe" uninstall_game -autologging"#
        ));
        assert!(!is_ea_uninstaller(r#""C:\Games\The Sims 4\unins000.exe""#));
        assert!(!is_ea_uninstaller(""));
    }

    #[test]
    fn id_uses_first_content_id() {
        let data = parse_installer_data(SYNDICATE_DATA).unwrap();
        assert_eq!(game_id(&data, &Candidate::default()), "ea:1025174");
        let candidate = Candidate {
            product_guid: "{64CF}".into(),
            ..Default::default()
        };
        assert_eq!(game_id(&InstallerData::default(), &candidate), "ea:{64CF}");
    }
}
