//! Which graphics APIs a game uses (DirectX 8 to 12, OpenGL, Vulkan).
//!
//! PCGamingWiki's API table comes first: it is curated per game. When the wiki has no page for
//! the game, or cannot be reached, the game's files are read instead. Files are a weaker source:
//! engines link and probe several APIs (Unity and Unreal reference nearly all of them, GTA V's
//! program imports only `d3d9.dll` although the game runs on Direct3D 11), so only the evidence
//! that held up on real games is used: the main program's imports, bundled runtimes
//! (`D3D12Core.dll`) and renderer modules named after their API.
//!
//! Either answer is a suggestion: the user chooses the one API the game runs on in its settings
//! (`GameEdits::graphics_api`) when the answer lists several or is wrong.
//!
//! Answers are cached in `%LOCALAPPDATA%\Game Manager\cache\graphics.json` by game id: the
//! wiki's for 30 days, the files' for 7 days and only while the game's program is unchanged.

use crate::{Game, json_file, pcgamingwiki, pe};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum GraphicsApi {
    DirectX8,
    DirectX9,
    DirectX10,
    DirectX11,
    DirectX12,
    OpenGl,
    Vulkan,
}

impl GraphicsApi {
    pub const ALL: [GraphicsApi; 7] = [
        GraphicsApi::DirectX8,
        GraphicsApi::DirectX9,
        GraphicsApi::DirectX10,
        GraphicsApi::DirectX11,
        GraphicsApi::DirectX12,
        GraphicsApi::OpenGl,
        GraphicsApi::Vulkan,
    ];

    pub fn label(self) -> &'static str {
        match self {
            GraphicsApi::DirectX8 => "DirectX 8",
            GraphicsApi::DirectX9 => "DirectX 9",
            GraphicsApi::DirectX10 => "DirectX 10",
            GraphicsApi::DirectX11 => "DirectX 11",
            GraphicsApi::DirectX12 => "DirectX 12",
            GraphicsApi::OpenGl => "OpenGL",
            GraphicsApi::Vulkan => "Vulkan",
        }
    }
}

/// Where an answer came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GraphicsSource {
    /// The API table of this PCGamingWiki page.
    PcGamingWiki { page: String },
    /// The game's files: may include APIs the game does not use.
    GameFiles,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphicsInfo {
    /// In `GraphicsApi` order, without repeats.
    pub apis: Vec<GraphicsApi>,
    pub source: GraphicsSource,
}

/// The game's graphics APIs, from the cache, PCGamingWiki or the game's files; `None` when none
/// was found. Blocking: it may make web requests and read large files.
pub fn detect(game: &Game) -> Option<GraphicsInfo> {
    let stamp = program_stamp(game);
    let now = unix_seconds();
    if let Some(entry) = Cache::read(&game.id)
        && entry.is_fresh(now, stamp)
    {
        return entry.info;
    }
    let info = match pcgamingwiki::api_table(game) {
        Ok(Some(info)) => Some(info),
        Ok(None) => from_files(game),
        // Offline, or the wiki is busy: the files, without caching, so the wiki is asked again.
        Err(_) => return from_files(game),
    };
    Cache::write(
        &game.id,
        Entry {
            checked: now,
            info: info.clone(),
            program: stamp,
        },
    );
    info
}

/// The APIs the game's files show; `None` when they show none.
pub fn from_files(game: &Game) -> Option<GraphicsInfo> {
    let files = game
        .install_dir
        .as_deref()
        .map(binaries)
        .unwrap_or_default();
    let mut apis: BTreeSet<GraphicsApi> = main_programs(game, &files)
        .iter()
        .filter_map(|program| pe::imported_dlls(program))
        .flatten()
        .filter_map(|dll| dll.strip_suffix(".dll").and_then(api_of_dll))
        .chain(files.iter().filter_map(|file| named_api(&file.path)))
        .collect();
    // Next to Direct3D 11 or 12, Direct3D 9 and OpenGL are helpers (debug markers, probes, an
    // OpenGL renderer kept for other platforms), as in every Unreal game.
    if apis.contains(&GraphicsApi::DirectX11) || apis.contains(&GraphicsApi::DirectX12) {
        apis.remove(&GraphicsApi::DirectX9);
        apis.remove(&GraphicsApi::OpenGl);
    }
    (!apis.is_empty()).then(|| GraphicsInfo {
        apis: apis.into_iter().collect(),
        source: GraphicsSource::GameFiles,
    })
}

/// Below this size a game's program is a launcher or a stub that starts the real one (Unreal's
/// `<Game>.exe` starts `<Game>\Binaries\Win64\<Game>-Win64-Shipping.exe`).
const LAUNCHER_SIZE: u64 = 5 * 1024 * 1024;
const MAX_DEPTH: usize = 5;
const MAX_ENTRIES: usize = 2000;
/// Folders of installers, redistributables and third-party plug-ins, whose DLLs say nothing about
/// the game's own renderer.
const SKIPPED_FOLDERS: [&str; 9] = [
    "_commonredist",
    "redist",
    "vcredist",
    "directx",
    "__installer",
    "support",
    "plugins",
    "thirdparty",
    "installers",
];

struct Binary {
    path: PathBuf,
    size: u64,
}

/// The `.exe` and `.dll` files under the install folder, skipping `SKIPPED_FOLDERS`.
fn binaries(dir: &Path) -> Vec<Binary> {
    let mut found = Vec::new();
    let mut folders = vec![(dir.to_path_buf(), 0)];
    let mut entries = 0;
    while let Some((folder, depth)) = folders.pop() {
        let Ok(list) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in list.flatten() {
            entries += 1;
            if entries > MAX_ENTRIES {
                return found;
            }
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < MAX_DEPTH && !SKIPPED_FOLDERS.contains(&name.as_str()) {
                    folders.push((path, depth + 1));
                }
            } else if name.ends_with(".exe") || name.ends_with(".dll") {
                let size = entry.metadata().map_or(0, |metadata| metadata.len());
                found.push(Binary { path, size });
            }
        }
    }
    found
}

/// The game's program, and when that is small enough to be a launcher, the largest program in
/// the install folder too: the game itself, as a rule.
fn main_programs(game: &Game, files: &[Binary]) -> Vec<PathBuf> {
    let mut programs: Vec<PathBuf> = game.program.iter().cloned().collect();
    let small = game
        .program
        .as_deref()
        .and_then(|program| fs::metadata(program).ok())
        .is_none_or(|metadata| metadata.len() < LAUNCHER_SIZE);
    if small
        && let Some(largest) = files
            .iter()
            .filter(|file| has_extension(&file.path, "exe"))
            .max_by_key(|file| file.size)
        && !programs.contains(&largest.path)
    {
        programs.push(largest.path.clone());
    }
    programs
}

/// The API whose system DLL is called `stem` (`d3d11`, `vulkan-1`).
fn api_of_dll(stem: &str) -> Option<GraphicsApi> {
    Some(match stem {
        "d3d8" => GraphicsApi::DirectX8,
        "d3d9" => GraphicsApi::DirectX9,
        "d3d10" | "d3d10_1" => GraphicsApi::DirectX10,
        "d3d11" => GraphicsApi::DirectX11,
        "d3d12" => GraphicsApi::DirectX12,
        "opengl32" => GraphicsApi::OpenGl,
        "vulkan-1" => GraphicsApi::Vulkan,
        _ => return None,
    })
}

/// The API a file's name shows: a bundled runtime (`D3D12Core.dll`, `vulkan-1.dll`) or a renderer
/// module named after its API (`shaderapidx9.dll`, `rendersystemvulkan.dll`, `FC3_d3d11.dll`).
fn named_api(path: &Path) -> Option<GraphicsApi> {
    let stem = path.file_stem()?.to_string_lossy().to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|word| stem.contains(word));
    Some(if has(&["dx12", "d3d12", "directx12"]) {
        GraphicsApi::DirectX12
    } else if has(&["dx11", "d3d11", "directx11"]) {
        GraphicsApi::DirectX11
    } else if has(&["dx10", "d3d10", "directx10"]) {
        GraphicsApi::DirectX10
    } else if has(&["dx9", "d3d9", "directx9"]) {
        GraphicsApi::DirectX9
    } else if has(&["dx8", "d3d8", "directx8"]) {
        GraphicsApi::DirectX8
    } else if has(&["vulkan"]) {
        GraphicsApi::Vulkan
    } else if has(&["opengl"]) {
        GraphicsApi::OpenGl
    } else {
        return None;
    })
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
}

const WIKI_DAYS: u64 = 30;
const FILES_DAYS: u64 = 7;
const DAY: u64 = 24 * 60 * 60;
/// One detection at a time reads and writes the cache file, so none loses another's answer.
static CACHE: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
struct CacheFile {
    #[serde(default)]
    games: BTreeMap<String, Entry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct Entry {
    /// When the answer was found, in seconds since 1970.
    checked: u64,
    info: Option<GraphicsInfo>,
    /// The game program's size and modified time then.
    #[serde(default)]
    program: Option<(u64, u64)>,
}

impl Entry {
    fn is_fresh(&self, now: u64, program: Option<(u64, u64)>) -> bool {
        let age = now.saturating_sub(self.checked);
        match &self.info {
            Some(GraphicsInfo {
                source: GraphicsSource::PcGamingWiki { .. },
                ..
            }) => age < WIKI_DAYS * DAY,
            _ => age < FILES_DAYS * DAY && self.program == program,
        }
    }
}

struct Cache;

impl Cache {
    fn file() -> Option<PathBuf> {
        Some(json_file::local_file("cache").ok()?.join("graphics.json"))
    }

    fn read(id: &str) -> Option<Entry> {
        let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        read_entry(&Self::file()?, id)
    }

    /// A cache that cannot be written only means "not cached".
    fn write(id: &str, entry: Entry) {
        let _lock = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = Self::file() {
            write_entry(&file, id, entry);
        }
    }
}

fn read_entry(file: &Path, id: &str) -> Option<Entry> {
    json_file::load::<CacheFile>(file).ok()?.games.remove(id)
}

fn write_entry(file: &Path, id: &str, entry: Entry) {
    // An unreadable cache is replaced: it holds nothing that cannot be found again.
    let mut cache: CacheFile = json_file::load(file).unwrap_or_default();
    cache.games.insert(id.to_owned(), entry);
    let _ = json_file::save(file, &cache);
}

fn program_stamp(game: &Game) -> Option<(u64, u64)> {
    let metadata = fs::metadata(game.program.as_deref()?).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((metadata.len(), modified))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Launch, Store};
    use GraphicsApi::*;

    #[test]
    fn names_of_renderer_modules_and_runtimes() {
        let api = |name: &str| named_api(Path::new(name));
        assert_eq!(api(r"bin\x64\D3D12\D3D12Core.dll"), Some(DirectX12));
        assert_eq!(api("shaderapidx9.dll"), Some(DirectX9));
        assert_eq!(api("rendersystemdx11.dll"), Some(DirectX11));
        assert_eq!(api("FC3_d3d11.dll"), Some(DirectX11));
        assert_eq!(
            api("Engine.Render.Core2.PlatformPcDx12.dll"),
            Some(DirectX12)
        );
        assert_eq!(api("rendersystemvulkan.dll"), Some(Vulkan));
        assert_eq!(api("directx8tests.dll"), Some(DirectX8));
        assert_eq!(api("d3dcompiler_47.dll"), None);
        assert_eq!(api("dxgi.dll"), None);
        assert_eq!(api("game.exe"), None);
    }

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Graphics-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, relative: &str, bytes: &[u8]) {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn game(dir: &Path, program: &str) -> Game {
        Game {
            id: "custom:1".into(),
            name: "Test".into(),
            store: Store::Custom,
            install_dir: Some(dir.to_path_buf()),
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: Some(dir.join(program)),
            arch: None,
            edits: None,
            favorite: false,
        }
    }

    #[test]
    fn reads_the_game_files() {
        let dir = temp_dir("files");
        // A program importing Direct3D 9; DLLs in skipped folders do not count.
        write(
            &dir,
            "Game.exe",
            &pe::tests::program_importing(&["d3d9.dll"]),
        );
        write(&dir, r"_CommonRedist\vulkan-1.dll", b"");
        write(&dir, r"Plugins\Upscaler\upscaler_dx11.dll", b"");
        let found = from_files(&game(&dir, "Game.exe")).unwrap();
        assert_eq!(found.apis, [DirectX9]);
        assert_eq!(found.source, GraphicsSource::GameFiles);

        // A bundled Direct3D 12 runtime; next to it, Direct3D 9 is only a helper.
        write(&dir, r"bin\D3D12\D3D12Core.dll", b"");
        assert_eq!(
            from_files(&game(&dir, "Game.exe")).unwrap().apis,
            [DirectX12]
        );

        // A small launcher: the largest program is the game.
        let other = temp_dir("launcher");
        write(&other, "Launcher.exe", b"MZ");
        write(
            &other,
            r"Game\Binaries\Game-Shipping.exe",
            &pe::tests::program_importing(&["KERNEL32.dll", "OPENGL32.dll"]),
        );
        assert_eq!(
            from_files(&game(&other, "Launcher.exe")).unwrap().apis,
            [OpenGl]
        );
        // A renderer module named after its API.
        write(&other, r"Game\Binaries\RenderVulkan.dll", b"");
        assert_eq!(
            from_files(&game(&other, "Launcher.exe")).unwrap().apis,
            [OpenGl, Vulkan]
        );

        let empty = temp_dir("empty");
        assert_eq!(from_files(&game(&empty, "Missing.exe")), None);
        for dir in [dir, other, empty] {
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn cached_answers_expire() {
        let wiki = Entry {
            checked: 1000,
            info: Some(GraphicsInfo {
                apis: vec![DirectX11],
                source: GraphicsSource::PcGamingWiki {
                    page: "Grand Theft Auto V".into(),
                },
            }),
            program: Some((1, 2)),
        };
        assert!(wiki.is_fresh(1000 + 29 * DAY, Some((9, 9))));
        assert!(!wiki.is_fresh(1000 + 31 * DAY, Some((1, 2))));

        let files = Entry {
            info: None,
            ..wiki.clone()
        };
        assert!(files.is_fresh(1000 + 6 * DAY, Some((1, 2))));
        assert!(!files.is_fresh(1000 + 8 * DAY, Some((1, 2))));
        // The program changed (an update): read the files again.
        assert!(!files.is_fresh(1000, Some((1, 3))));

        let dir = temp_dir("cache");
        let file = dir.join("graphics.json");
        assert_eq!(read_entry(&file, "steam:271590"), None);
        write_entry(&file, "steam:271590", wiki.clone());
        write_entry(&file, "custom:1", files.clone());
        assert_eq!(read_entry(&file, "steam:271590"), Some(wiki));
        assert_eq!(read_entry(&file, "custom:1"), Some(files));
        fs::remove_dir_all(dir).unwrap();
    }
}
