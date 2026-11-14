//! Where a game keeps its saves and its settings on this PC, as its PCGamingWiki page says (the
//! "Game data" section), for the game page's Saves and Config buttons.
//!
//! - **The wiki's paths** (`pcgamingwiki::game_data`) are cached by game id in
//!   `%LOCALAPPDATA%\Game Manager\cache\game-data.json`, as the wiki writes them: for 30 days, or 7
//!   when the wiki has no page for the game. Each lookup then fills them in for this PC and looks
//!   at the disk, so a folder a game made since shows at once.
//! - **Placeholders** (the wiki's `{{p|…}}`, `Template:Path`): `game` is the game's folder,
//!   `steam` Steam's, `steamlibrary` the Steam library holding the game, `ubisoftconnect` Ubisoft
//!   Connect's; `userprofile\Documents` and `Saved Games` are Windows' known folders (so a Documents
//!   folder moved to OneDrive is found), `appdata`, `localappdata`, `userprofile\appdata\locallow`,
//!   `programdata`, `public`, `programfiles`, `windir` and `temp` theirs too. `uid`, a user or
//!   profile id, stands for any folder there. A path with another placeholder is left out.
//! - **Registry keys** (`{{p|hkcu}}\Software\…`): some games keep their settings there; the key is
//!   only named (`Folder::Registry`), never opened, so nothing writes to the registry.
//! - **Which path**: the ones for the game's store first ("Steam", "GOG.com", "Epic Games Store",
//!   "EA app", "Ubisoft Connect", "Microsoft Store"), then "Windows", then the other PC stores',
//!   and the first that is a folder on this PC wins. A path to files (`*.sav`, `settings.cfg`)
//!   names the folder holding them. When `uid` matches several folders (two Steam accounts), the
//!   folder holding them is opened.
//! - On a library of 38 (2026-11-14): 31 pages; the saves folder on disk for 24 games, the config
//!   folder for 17; 6 kept their settings only in the registry; the rest weren't started yet on
//!   that PC or the wiki's path was wrong (Hogwarts Legacy's config, "Hogwarts Legacy" for
//!   "HogwartsLegacy").

use crate::pcgamingwiki::{self, WikiPath};
use crate::{Game, Result, Store, json_file, steam, ubisoft};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf, Prefix};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{env, fs};
use windows::Win32::{
    CoTaskMemFree, FOLDERID_Documents, FOLDERID_LocalAppDataLow, FOLDERID_SavedGames,
    KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows::core::{GUID, PWSTR};

/// How long the wiki's paths for a game are kept: a page found, and none.
const FOUND_AGE: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const MISSING_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Stands for the wiki's `{{p|uid}}` in a path until it is matched on disk.
const UID: &str = "\u{0}uid\u{0}";
/// How a user id shows in a path that isn't on this PC.
const UID_SHOWN: &str = "<user-id>";

/// A game's folder of one kind on this PC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Folder {
    /// It is there.
    Found(PathBuf),
    /// The wiki's path, which isn't on this PC (the game hasn't made it yet, or the wiki is
    /// wrong), `<user-id>` standing for a user id.
    Missing(String),
    /// The game keeps it in the registry: the key.
    Registry(String),
    /// The wiki lists none (or has no page for the game).
    Unlisted,
}

/// A game's saves and settings folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GameFolders {
    pub saves: Folder,
    pub config: Folder,
}

impl GameFolders {
    pub fn get(&self, kind: FolderKind) -> &Folder {
        match kind {
            FolderKind::Saves => &self.saves,
            FolderKind::Config => &self.config,
        }
    }
}

/// The two folders the game page opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FolderKind {
    Saves,
    Config,
}

impl FolderKind {
    /// The button's label: "Saves", "Config".
    pub fn label(self) -> &'static str {
        match self {
            FolderKind::Saves => "Saves",
            FolderKind::Config => "Config",
        }
    }

    /// In a sentence: "saves folder", "config folder".
    pub fn noun(self) -> &'static str {
        match self {
            FolderKind::Saves => "saves folder",
            FolderKind::Config => "config folder",
        }
    }
}

/// The game's saves and config folders. Blocking: up to five requests to the wiki when its paths
/// aren't cached, then a look at the disk.
pub fn folders(game: &Game) -> Result<GameFolders> {
    let entry = cached_or_read(game)?;
    let places = Places::of_this_pc();
    Ok(GameFolders {
        saves: locate(&entry.saves, game, &places),
        config: locate(&entry.config, game, &places),
    })
}

/// What the wiki says for a game, as cached.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    /// When it was read, in seconds since 1970.
    fetched: u64,
    /// The page's title; `None` when the wiki has none for the game.
    page: Option<String>,
    #[serde(default)]
    saves: Vec<WikiPath>,
    #[serde(default)]
    config: Vec<WikiPath>,
}

impl Entry {
    fn is_fresh(&self, now: u64) -> bool {
        let age = if self.page.is_some() {
            FOUND_AGE
        } else {
            MISSING_AGE
        };
        now.saturating_sub(self.fetched) < age.as_secs()
    }
}

/// The cache file's lock, so two lookups never write it at once.
static CACHE: Mutex<()> = Mutex::new(());

fn cache_file() -> Option<PathBuf> {
    Some(json_file::local_file("cache").ok()?.join("game-data.json"))
}

/// The game's entry: cached, or read from the wiki and cached. A failed request is not cached.
fn cached_or_read(game: &Game) -> Result<Entry> {
    let file = cache_file();
    let now = unix_seconds();
    if let Some(entry) = file.as_deref().and_then(|file| read_entry(file, &game.id))
        && entry.is_fresh(now)
    {
        return Ok(entry);
    }
    let entry = match pcgamingwiki::page(game)? {
        Some(page) => {
            let (saves, config) = pcgamingwiki::game_data(&page.wikitext);
            Entry {
                fetched: now,
                page: Some(page.title),
                saves,
                config,
            }
        }
        None => Entry {
            fetched: now,
            ..Entry::default()
        },
    };
    if let Some(file) = &file {
        write_entry(file, &game.id, entry.clone());
    }
    Ok(entry)
}

fn read_entry(file: &Path, id: &str) -> Option<Entry> {
    let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let entries: BTreeMap<String, Entry> = json_file::load(file).ok()?;
    entries.get(id).cloned()
}

fn write_entry(file: &Path, id: &str, entry: Entry) {
    let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let mut entries: BTreeMap<String, Entry> = json_file::load(file).unwrap_or_default();
    entries.insert(id.to_owned(), entry);
    let _ = json_file::save(file, &entries);
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// This PC's folders the wiki's placeholders stand for (see the module's doc).
#[derive(Clone, Debug, Default)]
pub(crate) struct Places {
    pub user_profile: Option<PathBuf>,
    pub documents: Option<PathBuf>,
    pub saved_games: Option<PathBuf>,
    pub app_data: Option<PathBuf>,
    pub local_app_data: Option<PathBuf>,
    pub local_low: Option<PathBuf>,
    pub program_data: Option<PathBuf>,
    pub public: Option<PathBuf>,
    pub program_files: Option<PathBuf>,
    pub windows: Option<PathBuf>,
    pub temp: Option<PathBuf>,
    pub user_name: Option<String>,
    pub steam: Option<PathBuf>,
    pub ubisoft: Option<PathBuf>,
}

impl Places {
    fn of_this_pc() -> Self {
        let var = |name: &str| env::var_os(name).map(PathBuf::from);
        Self {
            user_profile: var("USERPROFILE"),
            documents: known_folder(&FOLDERID_Documents),
            saved_games: known_folder(&FOLDERID_SavedGames),
            app_data: var("APPDATA"),
            local_app_data: var("LOCALAPPDATA"),
            local_low: known_folder(&FOLDERID_LocalAppDataLow),
            program_data: var("ProgramData"),
            public: var("PUBLIC"),
            program_files: var("ProgramFiles"),
            windows: var("SystemRoot"),
            temp: var("TEMP"),
            user_name: env::var("USERNAME").ok(),
            steam: steam::install_dir(),
            ubisoft: ubisoft::Launcher::folder(),
        }
    }
}

/// A Windows known folder's path (Documents, Saved Games), wherever the user moved it.
fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut path = PWSTR::null();
    // SAFETY: `id` is a known folder id; `path` is a valid out-pointer whose string is freed
    // below.
    let found = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as u32, None, &mut path) };
    // SAFETY: the string SHGetKnownFolderPath wrote, read while it lives.
    let text = (found.is_ok() && !path.is_null()).then(|| unsafe { path.to_string() });
    // SAFETY: allocated by SHGetKnownFolderPath (even on failure), freed once.
    unsafe { CoTaskMemFree(path.0.cast()) };
    text.and_then(std::result::Result::ok).map(PathBuf::from)
}

/// The platforms whose paths count for a game of `store`, in order: its store's, then Windows'.
fn own_platforms(store: Store) -> &'static [&'static str] {
    match store {
        Store::Steam => &["Steam"],
        Store::Gog => &["GOG.com"],
        Store::Epic => &["Epic Games Store", "Epic Games Launcher"],
        Store::Ea => &["EA app", "Origin"],
        Store::Ubisoft => &["Ubisoft Connect", "Uplay"],
        Store::Xbox => &["Microsoft Store"],
        Store::Custom => &[],
    }
}

/// Every PC platform the wiki names, the stores' after Windows.
const PC_PLATFORMS: [&str; 10] = [
    "Windows",
    "Steam",
    "GOG.com",
    "Epic Games Store",
    "Epic Games Launcher",
    "EA app",
    "Origin",
    "Ubisoft Connect",
    "Uplay",
    "Microsoft Store",
];

/// The folder `paths` name on this PC (see the module's doc for the order).
pub(crate) fn locate(paths: &[WikiPath], game: &Game, places: &Places) -> Folder {
    let mut order: Vec<&str> = own_platforms(game.store).to_vec();
    for platform in PC_PLATFORMS {
        if !order.contains(&platform) {
            order.push(platform);
        }
    }
    let mut missing = None;
    let mut registry = None;
    for platform in order {
        let platform_paths = paths
            .iter()
            .filter(|path| path.platform.eq_ignore_ascii_case(platform));
        for wiki in platform_paths {
            match expand(&wiki.path, game, places) {
                Expanded::Path(path) => {
                    if let Some(folder) = on_disk(&path) {
                        // As it is on disk ("C:\Program Files (x86)\Steam", not as Steam
                        // writes it in the registry).
                        return Folder::Found(crate::on_disk_case(&folder));
                    }
                    missing.get_or_insert_with(|| path.replace(UID, UID_SHOWN));
                }
                Expanded::Registry(key) => {
                    registry.get_or_insert(key);
                }
                Expanded::Unknown => {}
            }
        }
    }
    match (missing, registry) {
        (Some(path), _) => Folder::Missing(path),
        (None, Some(key)) => Folder::Registry(key),
        (None, None) => Folder::Unlisted,
    }
}

/// A wiki path filled in for this PC.
#[derive(Debug, PartialEq, Eq)]
enum Expanded {
    /// A file system path, `UID` standing for a user id.
    Path(String),
    /// A registry key.
    Registry(String),
    /// A placeholder this PC hasn't (a store not installed, macOS, a VR runtime's folder), or not
    /// a whole path.
    Unknown,
}

/// The wiki's path with its placeholders filled in and its notes, references and markup left out.
fn expand(raw: &str, game: &Game, places: &Places) -> Expanded {
    let mut text = String::new();
    let mut registry = false;
    let mut rest = raw;
    while let Some(open) = rest.find("{{") {
        text.push_str(&rest[..open]);
        let (inner, after) = balanced(&rest[open + 2..]);
        rest = after;
        // `{{p|…}}` or `{{path|…}}`; any other template (a note, a citation) is left out whole,
        // with whatever it holds.
        let Some((name, argument)) = inner.split_once('|') else {
            continue;
        };
        if !matches!(name.trim().to_ascii_lowercase().as_str(), "p" | "path") {
            continue;
        }
        match place(argument.trim(), game, places) {
            Some(Place::Path(path)) => text.push_str(&path),
            Some(Place::Registry(key)) => {
                registry = true;
                text.push_str(key);
            }
            None => return Expanded::Unknown,
        }
    }
    text.push_str(rest);
    let text = strip_markup(&text);
    if registry {
        return Expanded::Registry(text.trim().trim_end_matches('\\').to_owned());
    }
    let text = text.trim().replace('/', "\\");
    let text = known_folders(text.trim_end_matches(['\\', ' ']), places);
    // A whole local path only: `C:\…` (the wiki writes `<Oculus-Folder>` for some).
    let path = Path::new(&text);
    let local = matches!(
        path.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
    ) && path.has_root();
    if local {
        Expanded::Path(text)
    } else {
        Expanded::Unknown
    }
}

/// The text of a template whose `{{` is already read, up to its matching `}}`, and the text after
/// it (none when it is never closed).
fn balanced(text: &str) -> (&str, &str) {
    let mut depth = 0usize;
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if rest.starts_with("{{") {
            depth += 1;
            at += 2;
        } else if rest.starts_with("}}") {
            if depth == 0 {
                return (&text[..at], &text[at + 2..]);
            }
            depth -= 1;
            at += 2;
        } else {
            at += rest.chars().next().map_or(1, char::len_utf8);
        }
    }
    (text, "")
}

/// `Documents`, `My Documents` and `Saved Games` written out after the user profile (older pages
/// do) as Windows' known folders, which the user may have moved (to OneDrive).
fn known_folders(text: &str, places: &Places) -> String {
    let Some(profile) = &places.user_profile else {
        return text.to_owned();
    };
    let profile = profile.to_string_lossy();
    let moved = [
        ("Documents", &places.documents),
        ("My Documents", &places.documents),
        ("Saved Games", &places.saved_games),
    ];
    for (name, folder) in moved {
        let written = format!(r"{profile}\{name}");
        let starts = text
            .get(..written.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(&written));
        let whole_name = text[written.len().min(text.len())..].is_empty()
            || text[written.len().min(text.len())..].starts_with('\\');
        if let Some(folder) = folder
            && starts
            && whole_name
        {
            return format!("{}{}", folder.display(), &text[written.len()..]);
        }
    }
    text.to_owned()
}

/// What a placeholder stands for.
enum Place {
    Path(String),
    Registry(&'static str),
}

fn place(name: &str, game: &Game, places: &Places) -> Option<Place> {
    let path = |path: &Option<PathBuf>| {
        path.as_ref()
            .map(|path| path.to_string_lossy().into_owned())
    };
    let text = match name.to_ascii_lowercase().as_str() {
        "hkcu" | "hkey_current_user" => return Some(Place::Registry("HKEY_CURRENT_USER")),
        "hklm" | "hkey_local_machine" => return Some(Place::Registry("HKEY_LOCAL_MACHINE")),
        "wow64" => return Some(Place::Registry("WOW6432Node")),
        "game" => path(&game.install_dir)?.trim_end_matches('\\').to_owned(),
        "steamlibrary" => {
            let dir = path(&game.install_dir)?;
            let at = dir.to_ascii_lowercase().find(r"\steamapps\")?;
            dir[..at].to_owned()
        }
        "uid" => UID.to_owned(),
        "steam" => path(&places.steam)?,
        "uplay" | "ubisoftconnect" => path(&places.ubisoft)?,
        "userprofile" => path(&places.user_profile)?,
        "userprofile\\documents" => path(&places.documents)?,
        "userprofile\\appdata\\locallow" => path(&places.local_low)?,
        "appdata" => path(&places.app_data)?,
        "localappdata" => path(&places.local_app_data)?,
        "programdata" | "allusersprofile" => path(&places.program_data)?,
        "public" => path(&places.public)?,
        "programfiles" => path(&places.program_files)?,
        "windir" => path(&places.windows)?,
        "temp" => path(&places.temp)?,
        "username" => places.user_name.clone()?,
        _ => return None,
    };
    Some(Place::Path(text))
}

/// The HTML tags the wiki puts in paths, left out with what they hold (a reference) or alone.
const TAGS: [&str; 7] = ["ref", "br", "code", "nowiki", "sup", "small", "span"];

/// `text` without HTML comments, references and tags (`<!-- -->`, `<ref>…</ref>`, `<br/>`). Other
/// text in angle brackets (`<Oculus-Folder>`) is kept.
fn strip_markup(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tag = &rest[open..];
        let end = tag.find('>');
        let name: String = tag[1..]
            .trim_start_matches('/')
            .chars()
            .take_while(char::is_ascii_alphabetic)
            .collect::<String>()
            .to_ascii_lowercase();
        let skip = if tag.starts_with("<!--") {
            tag.find("-->").map(|close| close + 3)
        } else if name == "ref" && end.is_some_and(|end| !tag[..end].ends_with('/')) {
            tag.find("</ref>").map(|close| close + 6)
        } else if TAGS.contains(&name.as_str()) {
            end.map(|end| end + 1)
        } else {
            None
        };
        match skip {
            Some(skip) => rest = &tag[skip..],
            None => {
                out.push('<');
                rest = &tag[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The folder `path` names on this PC: itself when it is a folder, else the folder holding the
/// files it names (`*.sav`, `settings.cfg`); `UID` matches any folder, and several matches give
/// the folder holding them. `None` when nothing is there.
pub(crate) fn on_disk(path: &str) -> Option<PathBuf> {
    let mut components = Path::new(path).components();
    let (Some(Component::Prefix(prefix)), Some(Component::RootDir)) =
        (components.next(), components.next())
    else {
        return None;
    };
    let mut root = PathBuf::from(prefix.as_os_str());
    root.push("\\");
    let parts: Vec<String> = components
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    let mut candidates = vec![root];
    for (index, part) in parts.iter().enumerate() {
        let last = index + 1 == parts.len();
        let pattern = part.contains(['*', '?']);
        if last && pattern && !part.contains(UID) {
            // Files: their folder.
            candidates.retain(|folder| folder.is_dir());
            break;
        }
        candidates = if part.contains(UID) || pattern {
            candidates
                .iter()
                .flat_map(|folder| matching(folder, part))
                .collect()
        } else {
            candidates.iter().map(|folder| folder.join(part)).collect()
        };
        if candidates.is_empty() {
            return None;
        }
    }
    let mut found: Vec<PathBuf> = candidates
        .into_iter()
        .filter_map(|candidate| {
            if candidate.is_dir() {
                Some(candidate)
            } else if candidate.is_file() || looks_like_a_file(&candidate) {
                candidate
                    .parent()
                    .filter(|parent| parent.is_dir())
                    .map(Path::to_path_buf)
            } else {
                None
            }
        })
        .collect();
    found.sort();
    found.dedup();
    match found.as_slice() {
        [] => None,
        [one] => Some(one.clone()),
        many => common_folder(many),
    }
}

/// The folders in `folder` whose names match `pattern` (`UID` for any text, `*` and `?`), ignoring
/// case.
fn matching(folder: &Path, pattern: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    let pattern = pattern.replace(UID, "*").to_lowercase();
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| {
            glob(
                &pattern,
                &entry.file_name().to_string_lossy().to_lowercase(),
            )
        })
        .map(|entry| entry.path())
        .collect()
}

/// Whether `name` matches `pattern`: `*` any text (at least one character), `?` one character.
fn glob(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let name: Vec<char> = name.chars().collect();
    fn matches(pattern: &[char], name: &[char]) -> bool {
        match pattern.split_first() {
            None => name.is_empty(),
            Some(('*', rest)) => (1..=name.len()).any(|taken| matches(rest, &name[taken..])),
            Some(('?', rest)) => !name.is_empty() && matches(rest, &name[1..]),
            Some((c, rest)) => name.first() == Some(c) && matches(rest, &name[1..]),
        }
    }
    matches(&pattern, &name)
}

/// A name with an extension of one to five letters or digits (`settings.cfg`, `savegame.dat`).
fn looks_like_a_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            (1..=5).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

/// The deepest folder holding every one of `folders`.
fn common_folder(folders: &[PathBuf]) -> Option<PathBuf> {
    let first = folders.first()?;
    first
        .ancestors()
        .find(|ancestor| folders.iter().all(|folder| folder.starts_with(ancestor)))
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Launch;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-GameData-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn game(store: Store, install_dir: &Path) -> Game {
        Game {
            id: "steam:292030".into(),
            name: "The Witcher 3".into(),
            store,
            install_dir: Some(install_dir.to_path_buf()),
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

    fn places(root: &Path) -> Places {
        Places {
            user_profile: Some(root.join("User")),
            documents: Some(root.join(r"OneDrive\Documents")),
            saved_games: Some(root.join(r"User\Saved Games")),
            app_data: Some(root.join(r"User\AppData\Roaming")),
            local_app_data: Some(root.join(r"User\AppData\Local")),
            local_low: Some(root.join(r"User\AppData\LocalLow")),
            steam: Some(root.join("Steam")),
            user_name: Some("player".into()),
            ..Places::default()
        }
    }

    fn wiki(platform: &str, path: &str) -> WikiPath {
        WikiPath {
            platform: platform.into(),
            path: path.into(),
        }
    }

    #[test]
    fn placeholders_are_filled_in_for_this_pc() {
        let root = Path::new(r"C:\Test");
        let places = places(root);
        let witcher = game(Store::Gog, Path::new(r"D:\Games\The Witcher 3\"));
        let expanded = |raw: &str| expand(raw, &witcher, &places);
        // Documents is the known folder, here moved to OneDrive.
        assert_eq!(
            expanded(r"{{p|userprofile\Documents}}\The Witcher 3\*.settings"),
            Expanded::Path(r"C:\Test\OneDrive\Documents\The Witcher 3\*.settings".into())
        );
        assert_eq!(
            expanded(r"{{P|game}}\Profiles\"),
            Expanded::Path(r"D:\Games\The Witcher 3\Profiles".into())
        );
        assert_eq!(
            expanded(r"{{path|localappdata}}/Remedy/AlanWake2/{{p|uid}}"),
            Expanded::Path(format!(
                r"C:\Test\User\AppData\Local\Remedy\AlanWake2\{UID}"
            ))
        );
        // Notes, references and markup go.
        assert_eq!(
            expanded(
                r"{{p|appdata}}\TRX<ref>{{Refcheck|user=X|date=2024}}</ref>{{Note|Shared.}}<br>"
            ),
            Expanded::Path(r"C:\Test\User\AppData\Roaming\TRX".into())
        );
        // Registry keys, unknown placeholders, and paths that aren't whole.
        assert_eq!(
            expanded(r"{{p|hkcu}}\Software\Crystal Dynamics\Tomb Raider: Legend\"),
            Expanded::Registry(
                r"HKEY_CURRENT_USER\Software\Crystal Dynamics\Tomb Raider: Legend".into()
            )
        );
        assert_eq!(
            expanded(r"{{p|osxhome}}/Library/Witcher"),
            Expanded::Unknown
        );
        assert_eq!(
            expanded(r"{{p|ubisoftconnect}}\savegames"),
            Expanded::Unknown
        );
        assert_eq!(
            expanded(r"<Oculus-Folder>\Software\Pistol Whip_Data"),
            Expanded::Unknown
        );
        // Documents and Saved Games written out after the profile are the known folders.
        assert_eq!(
            expanded(r"{{p|userprofile}}\Documents\My Games\Far Cry 3"),
            Expanded::Path(r"C:\Test\OneDrive\Documents\My Games\Far Cry 3".into())
        );
        assert_eq!(
            expanded(r"{{p|userprofile}}\My Documents\Eidos\Tomb Raider - Underworld"),
            Expanded::Path(r"C:\Test\OneDrive\Documents\Eidos\Tomb Raider - Underworld".into())
        );
        assert_eq!(
            expanded(r"{{p|userprofile}}\Saved Games\CD Projekt Red\Cyberpunk 2077"),
            Expanded::Path(r"C:\Test\User\Saved Games\CD Projekt Red\Cyberpunk 2077".into())
        );
        assert_eq!(
            expanded(r"{{p|userprofile}}\DocumentsOld\x"),
            Expanded::Path(r"C:\Test\User\DocumentsOld\x".into())
        );
        // A note holding a placeholder goes whole.
        assert_eq!(
            expanded(r"{{p|game}}\save{{Note|Or {{p|appdata}}\Old.}}"),
            Expanded::Path(r"D:\Games\The Witcher 3\save".into())
        );
        // A Steam library is the folder holding `steamapps`.
        let steam_game = game(
            Store::Steam,
            Path::new(r"E:\SteamLibrary\steamapps\common\Half-Life 2"),
        );
        assert_eq!(
            expand(
                r"{{p|steamlibrary}}\steamapps\common\Half-Life 2\hl2\save",
                &steam_game,
                &places
            ),
            Expanded::Path(r"E:\SteamLibrary\steamapps\common\Half-Life 2\hl2\save".into())
        );
    }

    #[test]
    fn folders_are_found_on_disk() {
        let dir = temp_dir("disk");
        let root = dir.to_string_lossy().into_owned();
        let saves = dir.join(r"Witcher\gamesaves");
        fs::create_dir_all(&saves).unwrap();
        fs::write(saves.join("save.sav"), b"").unwrap();
        // A folder, the folder of a pattern, the folder of a named file (there or not).
        let path = |relative: &str| format!(r"{root}\{relative}");
        assert_eq!(on_disk(&path(r"Witcher\gamesaves")), Some(saves.clone()));
        assert_eq!(
            on_disk(&path(r"Witcher\gamesaves\*.sav")),
            Some(saves.clone())
        );
        assert_eq!(
            on_disk(&path(r"Witcher\gamesaves\save.sav")),
            Some(saves.clone())
        );
        assert_eq!(
            on_disk(&path(r"Witcher\gamesaves\settings.cfg")),
            Some(saves.clone())
        );
        assert_eq!(on_disk(&path(r"Witcher\other")), None);
        assert_eq!(on_disk(&path(r"Missing\gamesaves\*.sav")), None);
        // One user id is that folder; several, the folder holding them.
        let userdata = dir.join(r"Steam\userdata");
        fs::create_dir_all(userdata.join(r"9035\292030\remote")).unwrap();
        assert_eq!(
            on_disk(&path(&format!(r"Steam\userdata\{UID}\292030\remote"))),
            Some(userdata.join(r"9035\292030\remote"))
        );
        fs::create_dir_all(userdata.join(r"7656\292030\remote")).unwrap();
        fs::create_dir_all(userdata.join(r"1111\440")).unwrap();
        assert_eq!(
            on_disk(&path(&format!(r"Steam\userdata\{UID}\292030\remote"))),
            Some(userdata.clone())
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_stores_own_paths_come_first_and_the_first_on_disk_wins() {
        let dir = temp_dir("order");
        let places = places(&dir);
        let install = dir.join(r"Games\Witcher");
        fs::create_dir_all(dir.join(r"OneDrive\Documents\The Witcher 3\gamesaves")).unwrap();
        fs::create_dir_all(dir.join(r"Steam\userdata\9035\292030\remote")).unwrap();
        let paths = [
            wiki(
                "Windows",
                r"{{p|userprofile\Documents}}\The Witcher 3\gamesaves\*.sav",
            ),
            wiki("Steam", r"{{p|steam}}\userdata\{{p|uid}}\292030\remote"),
            wiki("OS X", r"{{p|osxhome}}/Library/Witcher"),
        ];
        // A Steam game: Steam's cloud folder first.
        assert_eq!(
            locate(&paths, &game(Store::Steam, &install), &places),
            Folder::Found(dir.join(r"Steam\userdata\9035\292030\remote"))
        );
        // A GOG game: Windows' path, then the other stores'.
        assert_eq!(
            locate(&paths, &game(Store::Gog, &install), &places),
            Folder::Found(dir.join(r"OneDrive\Documents\The Witcher 3\gamesaves"))
        );
        fs::remove_dir_all(dir.join(r"OneDrive\Documents\The Witcher 3")).unwrap();
        assert_eq!(
            locate(&paths, &game(Store::Gog, &install), &places),
            Folder::Found(dir.join(r"Steam\userdata\9035\292030\remote"))
        );
        // Nothing on disk: the first path, as it would be.
        fs::remove_dir_all(dir.join("Steam")).unwrap();
        let missing = locate(&paths, &game(Store::Gog, &install), &places);
        assert_eq!(
            missing,
            Folder::Missing(format!(
                r"{}\OneDrive\Documents\The Witcher 3\gamesaves\*.sav",
                dir.display()
            ))
        );
        let in_steam = locate(&paths[1..], &game(Store::Steam, &install), &places);
        assert_eq!(
            in_steam,
            Folder::Missing(format!(
                r"{}\Steam\userdata\<user-id>\292030\remote",
                dir.display()
            ))
        );
        // Only a registry key, or nothing for Windows at all.
        let registry = [wiki(
            "Windows",
            r"{{p|hkcu}}\Software\IO Interactive\HITMAN3",
        )];
        assert_eq!(
            locate(&registry, &game(Store::Steam, &install), &places),
            Folder::Registry(r"HKEY_CURRENT_USER\Software\IO Interactive\HITMAN3".into())
        );
        assert_eq!(
            locate(&paths[2..], &game(Store::Steam, &install), &places),
            Folder::Unlisted
        );
        assert_eq!(
            locate(&[], &game(Store::Steam, &install), &places),
            Folder::Unlisted
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn patterns_match_folder_names() {
        assert!(glob("*", "1638"));
        assert!(!glob("*", ""));
        assert!(glob("profile_*", "profile_main"));
        assert!(glob("save?", "save1"));
        assert!(!glob("save?", "save12"));
        assert!(glob("abc", "abc") && !glob("abc", "abd"));
    }

    #[test]
    fn cached_paths_expire() {
        let found = Entry {
            fetched: 1_000,
            page: Some("Cyberpunk 2077".into()),
            ..Entry::default()
        };
        let day = 24 * 60 * 60;
        assert!(found.is_fresh(1_000 + 29 * day));
        assert!(!found.is_fresh(1_000 + 31 * day));
        let none = Entry {
            fetched: 1_000,
            ..Entry::default()
        };
        assert!(none.is_fresh(1_000 + 6 * day));
        assert!(!none.is_fresh(1_000 + 8 * day));
        // Written and read back by game id.
        let dir = temp_dir("cache");
        let file = dir.join("game-data.json");
        write_entry(&file, "epic:Ginger", found.clone());
        write_entry(&file, "steam:220", none.clone());
        assert_eq!(read_entry(&file, "epic:Ginger"), Some(found));
        assert_eq!(read_entry(&file, "steam:220"), Some(none));
        assert_eq!(read_entry(&file, "gog:1"), None);
        fs::remove_dir_all(dir).unwrap();
    }
}
