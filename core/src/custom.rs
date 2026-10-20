//! Games the user added by hand: a program with a name and optional command-line arguments.
//!
//! They are saved in `%APPDATA%\Game Manager\custom-games.json` and listed with the store games,
//! but never merged with them: the user chose each one, even when a store reports the same folder.

use crate::{Game, IconRef, Launch, Store, json_file};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use windows::Win32::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::core::HSTRING;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomGame {
    /// Stable id of the form `custom:<number>`.
    pub id: String,
    pub name: String,
    /// The program started by Play, usually an `.exe`.
    pub program: PathBuf,
    /// Command-line arguments, passed as one string.
    #[serde(default)]
    pub args: String,
}

impl CustomGame {
    pub fn to_game(&self) -> Game {
        Game {
            id: self.id.clone(),
            name: self.name.clone(),
            store: Store::Custom,
            install_dir: self.program.parent().map(Path::to_path_buf),
            launch: Launch::Exe {
                path: self.program.clone(),
                args: self.args.clone(),
                working_dir: None,
            },
            alt_launches: Vec::new(),
            icon: Some(IconRef::file(&self.program)),
            program: Some(self.program.clone()),
            arch: None,
        }
    }
}

/// `custom-games.json`.
#[derive(Default, Serialize, Deserialize)]
struct LibraryFile {
    #[serde(default)]
    games: Vec<CustomGame>,
}

/// The saved custom games, as library games.
pub fn scan() -> Result<Vec<Game>, String> {
    Ok(load(&library_file()?)?
        .iter()
        .map(CustomGame::to_game)
        .collect())
}

/// Saves a new custom game and returns it.
pub fn add(name: &str, program: &Path, args: &str) -> Result<CustomGame, String> {
    add_to(&library_file()?, name, program, args, unix_millis())
}

/// Forgets a custom game. The game's files are not touched.
pub fn remove(id: &str) -> Result<(), String> {
    remove_from(&library_file()?, id)
}

/// A name for a newly chosen program: the product name or description from its version
/// information, else the file name without extension.
pub fn suggested_name(program: &Path) -> String {
    let mut version_info = VersionInfo::read(program);
    version_info
        .as_mut()
        .and_then(|info| {
            info.string("ProductName")
                .or_else(|| info.string("FileDescription"))
        })
        .unwrap_or_else(|| {
            program
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
}

fn library_file() -> Result<PathBuf, String> {
    json_file::user_file("custom-games.json")
}

fn load(file: &Path) -> Result<Vec<CustomGame>, String> {
    json_file::load::<LibraryFile>(file).map(|library| library.games)
}

fn save(file: &Path, games: Vec<CustomGame>) -> Result<(), String> {
    json_file::save(file, &LibraryFile { games })
}

fn add_to(
    file: &Path,
    name: &str,
    program: &Path,
    args: &str,
    now: u128,
) -> Result<CustomGame, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("a game needs a name".to_owned());
    }
    // Loading first also means a file that cannot be read is never overwritten.
    let mut games = load(file)?;
    let game = CustomGame {
        id: new_id(&games, now),
        name: name.to_owned(),
        program: program.to_path_buf(),
        args: args.trim().to_owned(),
    };
    games.push(game.clone());
    save(file, games)?;
    Ok(game)
}

fn remove_from(file: &Path, id: &str) -> Result<(), String> {
    let mut games = load(file)?;
    games.retain(|game| game.id != id);
    save(file, games)
}

/// `custom:<creation time in ms>`, made unique among the saved games.
fn new_id(games: &[CustomGame], now: u128) -> String {
    (now..)
        .map(|n| format!("custom:{n}"))
        .find(|id| !games.iter().any(|game| game.id == *id))
        .expect("an unused id")
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_millis())
}

/// A program's version-information block (the "Details" tab of its file properties).
struct VersionInfo {
    /// `u16` storage keeps the wide strings inside the block aligned.
    data: Vec<u16>,
}

impl VersionInfo {
    fn read(program: &Path) -> Option<Self> {
        let path = HSTRING::from(program.as_os_str());
        // SAFETY: `path` is a valid, null-terminated wide string.
        let size = unsafe { GetFileVersionInfoSizeW(&path, None) };
        if size == 0 {
            return None;
        }
        let mut data = vec![0u16; size.div_ceil(2) as usize];
        // SAFETY: `data` holds at least `size` bytes, the size the system asked for.
        unsafe { GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()) }
            .ok()
            .ok()?;
        Some(Self { data })
    }

    /// A string value such as `ProductName`, in the program's first listed language.
    fn string(&mut self, key: &str) -> Option<String> {
        // Language and code page pairs, e.g. 0x0409 0x04B0 for US English, Unicode.
        let mut languages: Vec<String> = self
            .query(r"\VarFileInfo\Translation")
            .map(|pairs| {
                pairs
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|[language, code_page]| format!("{language:04x}{code_page:04x}"))
                    .collect()
            })
            .unwrap_or_default();
        languages.push("040904b0".to_owned());
        languages.iter().find_map(|language| {
            let value = self.query(&format!(r"\StringFileInfo\{language}\{key}"))?;
            let text = String::from_utf16_lossy(value);
            let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            (!text.is_empty()).then(|| text.to_owned())
        })
    }

    /// The value at `sub_block` as `u16` units. Takes `&mut self` because Windows may convert
    /// strings in place, in the block's reserved space (16-bit programs), although the block is
    /// declared read-only.
    fn query(&mut self, sub_block: &str) -> Option<&[u16]> {
        let block = self.data.as_mut_ptr();
        let mut value = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `block` points to the buffer GetFileVersionInfoW filled, through a mutable
        // borrow, so any write Windows makes into it is allowed; the sub-block string lives until
        // the call returns and the out-pointers are valid locals.
        let found = unsafe {
            VerQueryValueW(
                block.cast_const().cast(),
                &HSTRING::from(sub_block),
                &mut value,
                &mut len,
            )
        };
        if !found.as_bool() || value.is_null() || len == 0 {
            return None;
        }
        // Strings report their length in characters, the binary Translation value in bytes.
        let units = if sub_block.starts_with(r"\StringFileInfo") {
            len as usize
        } else {
            len as usize / 2
        };
        let start = block as usize;
        let end = start + self.data.len() * 2;
        let value_start = value as usize;
        if value_start < start || value_start + units * 2 > end || !value_start.is_multiple_of(2) {
            return None;
        }
        // SAFETY: the value lies inside `data` (checked above), is u16-aligned, and is borrowed
        // for no longer than `self`.
        Some(unsafe { std::slice::from_raw_parts(value.cast::<u16>(), units) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_library(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Custom-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("custom-games.json")
    }

    #[test]
    fn missing_library_is_empty() {
        let file = temp_library("missing");
        assert_eq!(load(&file), Ok(Vec::new()));
    }

    #[test]
    fn adds_and_removes_games() {
        let file = temp_library("add-remove");
        let program = Path::new(r"C:\Games\Doom\gzdoom.exe");
        let doom = add_to(&file, " Doom ", program, " -iwad doom2.wad ", 1000).unwrap();
        assert_eq!(
            doom,
            CustomGame {
                id: "custom:1000".into(),
                name: "Doom".into(),
                program: program.to_path_buf(),
                args: "-iwad doom2.wad".into(),
            }
        );
        // Same millisecond: the id still has to be unique.
        let heretic = add_to(&file, "Heretic", program, "", 1000).unwrap();
        assert_eq!(heretic.id, "custom:1001");
        assert_eq!(load(&file).unwrap(), [doom.clone(), heretic.clone()]);

        remove_from(&file, &doom.id).unwrap();
        assert_eq!(load(&file).unwrap(), [heretic]);
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    #[test]
    fn rejects_empty_names() {
        let file = temp_library("empty-name");
        assert!(add_to(&file, "  ", Path::new(r"C:\x.exe"), "", 1).is_err());
        assert!(!file.exists());
    }

    #[test]
    fn unreadable_library_is_not_overwritten() {
        let file = temp_library("corrupt");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not json").unwrap();
        assert!(load(&file).is_err());
        assert!(add_to(&file, "Doom", Path::new(r"C:\x.exe"), "", 1).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "not json");
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    #[test]
    fn becomes_a_library_game() {
        let custom = CustomGame {
            id: "custom:1".into(),
            name: "Doom".into(),
            program: PathBuf::from(r"C:\Games\Doom\gzdoom.exe"),
            args: "-iwad doom2.wad".into(),
        };
        let game = custom.to_game();
        assert_eq!(game.store, Store::Custom);
        assert_eq!(game.install_dir, Some(PathBuf::from(r"C:\Games\Doom")));
        assert_eq!(
            game.launch,
            Launch::Exe {
                path: custom.program.clone(),
                args: custom.args.clone(),
                working_dir: None,
            }
        );
        assert_eq!(game.icon, Some(IconRef::file(&custom.program)));
    }

    #[test]
    fn suggests_names_from_version_info_or_file_name() {
        let notepad =
            PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\notepad.exe");
        // Windows' own programs carry version information.
        assert!(!suggested_name(&notepad).is_empty());
        assert_ne!(suggested_name(&notepad), "notepad");

        assert_eq!(
            suggested_name(Path::new(r"C:\does\not\exist\Some Game.exe")),
            "Some Game"
        );
    }
}
