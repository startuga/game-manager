//! Which games are running: one look at Windows' processes, matched to the games' programs.
//!
//! - **The watchlist** (`Watchlist::new`, built in the background): each game with a program, by
//!   the program's file name, and each open session's program too (a scan may have lost its
//!   game). The stores' own programs (each scanner's `CLIENT_PROGRAMS`) are never watched.
//! - **One look** (`running`): a Toolhelp snapshot of every process lists their file names;
//!   only those whose name a game's program has are opened (`PROCESS_QUERY_LIMITED_INFORMATION`)
//!   for their full path and creation time, and closed at once. A process is a game's when its
//!   path is the game's program, else when it has the program's file name inside the game's
//!   folder. When Windows won't open it (a protected game, an anti-cheat), its file name alone
//!   decides, and only when a single game has that name.
//! - A look that fails is an error, and the app ends no session on it.

use crate::elevate::OwnedHandle;
use crate::{Error, Game, Result, folder_key};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows::Win32::{
    CreateToolhelp32Snapshot, FILETIME, GetProcessTimes, INVALID_HANDLE_VALUE, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESSENTRY32W, Process32FirstW, Process32NextW,
    QueryFullProcessImageNameW, TH32CS_SNAPPROCESS,
};
use windows::core::PWSTR;

use super::OpenSession;

/// A running process of a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub game: String,
    pub pid: u32,
    /// When the process started, in seconds since 1970; 0 when Windows wouldn't say.
    pub created: u64,
    /// The game's program it was matched to.
    pub program: PathBuf,
}

/// A program to look for.
#[derive(Clone, Debug)]
struct Target {
    game: String,
    program: PathBuf,
    program_key: String,
    /// The game's folder: the program's file name anywhere in it counts.
    dir_key: Option<String>,
}

/// The games' programs, by lower-case file name.
#[derive(Debug, Default)]
pub struct Watchlist {
    by_name: HashMap<String, Vec<Target>>,
}

impl Watchlist {
    /// The programs of `games`, and of the sessions still `open`.
    pub fn new(games: &[Game], open: &BTreeMap<String, OpenSession>) -> Self {
        let clients: HashSet<String> = crate::store_client_programs()
            .map(str::to_lowercase)
            .collect();
        let mut watch = Self::default();
        for game in games {
            if let Some(program) = &game.program {
                watch.add(&game.id, program, game.install_dir.as_deref(), &clients);
            }
        }
        for (id, session) in open {
            if !watch.watches(id) {
                watch.add(id, &session.program, session.program.parent(), &clients);
            }
        }
        watch
    }

    fn add(&mut self, game: &str, program: &Path, dir: Option<&Path>, clients: &HashSet<String>) {
        let Some(name) = program.file_name() else {
            return;
        };
        let name = name.to_string_lossy().to_lowercase();
        if clients.contains(&name) {
            return;
        }
        self.by_name.entry(name).or_default().push(Target {
            game: game.to_owned(),
            program: program.to_path_buf(),
            program_key: folder_key(program),
            dir_key: dir.map(folder_key),
        });
    }

    /// Whether there is nothing to look for.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Whether the game's program is looked for.
    pub fn watches(&self, game: &str) -> bool {
        self.by_name
            .values()
            .flatten()
            .any(|target| target.game == game)
    }
}

/// The watched games' processes running now. Blocking (a few milliseconds).
pub fn running(watch: &Watchlist) -> Result<Vec<Seen>> {
    // SAFETY: a snapshot of every process; `snapshot` below closes it.
    let handle = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS as u32, 0) };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(Error::windows("The running programs")(
            windows::core::Error::from_thread(),
        ));
    }
    let snapshot = OwnedHandle(handle);
    let own = std::process::id();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    let mut seen = Vec::new();
    // SAFETY: the snapshot's handle and an entry whose size is set.
    let mut more = unsafe { Process32FirstW(snapshot.0, &mut entry) }.as_bool();
    while more {
        let pid = entry.th32ProcessID;
        // 0 and 4 are the idle process and the system.
        if pid > 4
            && pid != own
            && let Some(targets) = watch.by_name.get(&file_name(&entry.szExeFile))
        {
            let process = query(pid);
            for target in matches(targets, process.path.as_deref()) {
                seen.push(Seen {
                    game: target.game.clone(),
                    pid,
                    created: process.created,
                    program: target.program.clone(),
                });
            }
        }
        // SAFETY: as for the first entry.
        more = unsafe { Process32NextW(snapshot.0, &mut entry) }.as_bool();
    }
    Ok(seen)
}

/// The games among `targets` (all with the process's file name) a process at `path` is: the one
/// whose program it is, else those with that file name inside their folder; with no path (Windows
/// wouldn't open the process), the one game with this file name, when only one has it.
fn matches<'a>(targets: &'a [Target], path: Option<&Path>) -> Vec<&'a Target> {
    let Some(path) = path else {
        return if targets.len() == 1 {
            targets.iter().collect()
        } else {
            Vec::new()
        };
    };
    let key = folder_key(path);
    let exact: Vec<&Target> = targets
        .iter()
        .filter(|target| target.program_key == key)
        .collect();
    if !exact.is_empty() {
        return exact;
    }
    targets
        .iter()
        .filter(|target| {
            target
                .dir_key
                .as_deref()
                .and_then(|dir| key.strip_prefix(dir))
                .is_some_and(|rest| rest.starts_with('\\'))
        })
        .collect()
}

/// A Toolhelp entry's file name, lower-case.
fn file_name(name: &[u16]) -> String {
    let end = name
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..end]).to_lowercase()
}

/// What Windows tells of a process: its program and when it started.
#[derive(Debug, Default)]
struct Process {
    path: Option<PathBuf>,
    created: u64,
}

/// Opens a process to read its program and start time, and closes it; empty when Windows won't.
fn query(pid: u32) -> Process {
    // SAFETY: asks only to read the process's name and times; `process` below closes it.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION as u32, false, pid) };
    if handle.is_null() {
        return Process::default();
    }
    let process = OwnedHandle(handle);
    let mut buffer = vec![0u16; 32_768];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` holds `size` units; Windows writes the path and its length.
    let named =
        unsafe { QueryFullProcessImageNameW(process.0, 0, PWSTR(buffer.as_mut_ptr()), &mut size) };
    let path = named
        .as_bool()
        .then(|| buffer.get(..size as usize))
        .flatten()
        .map(|wide| PathBuf::from(OsString::from_wide(wide)));
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    // SAFETY: the open process and four valid out-pointers.
    let timed = unsafe { GetProcessTimes(process.0, created, exited, kernel, user) };
    Process {
        path,
        created: if timed.as_bool() {
            unix_seconds(times[0])
        } else {
            0
        },
    }
}

/// A `FILETIME` (100-nanosecond steps since 1601) in seconds since 1970.
fn unix_seconds(time: FILETIME) -> u64 {
    let steps = (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime);
    (steps / 10_000_000).saturating_sub(11_644_473_600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Launch, Store};

    fn game(id: &str, program: Option<&str>, dir: Option<&str>) -> Game {
        Game {
            id: id.into(),
            name: id.into(),
            store: Store::Custom,
            install_dir: dir.map(PathBuf::from),
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: program.map(PathBuf::from),
            arch: None,
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    #[test]
    fn this_test_program_is_seen_running() {
        let exe = std::env::current_exe().unwrap();
        let mut me = game("custom:me", None, None);
        me.program = Some(exe.clone());
        me.install_dir = exe.parent().map(Path::to_path_buf);
        // The test runs as its own process: look from a copy of the watchlist that skips nothing.
        let watch = Watchlist::new(&[me], &BTreeMap::new());
        assert!(watch.watches("custom:me"));
        let process = query(std::process::id());
        assert_eq!(
            process.path.as_deref().map(folder_key),
            Some(folder_key(&exe))
        );
        assert!(process.created > 1_700_000_000, "{}", process.created);
        let targets = &watch.by_name[&file_name(
            &exe.file_name()
                .unwrap()
                .to_string_lossy()
                .encode_utf16()
                .collect::<Vec<_>>(),
        )];
        assert_eq!(matches(targets, process.path.as_deref()).len(), 1);
        // A look at every process works (this one is left out: it is Game Manager's own).
        assert!(running(&watch).is_ok());
    }

    #[test]
    fn programs_match_by_path_then_by_name_in_their_folder() {
        let games = [
            game("a", Some(r"C:\Games\A\bin\game.exe"), Some(r"C:\Games\A")),
            game("b", Some(r"D:\Other\B\game.exe"), Some(r"D:\Other\B")),
        ];
        let watch = Watchlist::new(&games, &BTreeMap::new());
        let targets = &watch.by_name["game.exe"];
        let ids = |path: Option<&str>| -> Vec<String> {
            matches(targets, path.map(Path::new))
                .iter()
                .map(|target| target.game.clone())
                .collect()
        };
        assert_eq!(ids(Some(r"c:\games\a\BIN\Game.exe")), ["a"]);
        // Moved inside its folder: still A's.
        assert_eq!(ids(Some(r"C:\Games\A\win64\game.exe")), ["a"]);
        // A folder whose name only starts the same isn't the game's.
        assert!(ids(Some(r"C:\Games\Another\game.exe")).is_empty());
        // No path, and two games with the name: neither.
        assert!(ids(None).is_empty());
        let one = Watchlist::new(&games[..1], &BTreeMap::new());
        assert_eq!(matches(&one.by_name["game.exe"], None).len(), 1);
    }

    #[test]
    fn store_clients_and_games_without_programs_are_not_watched() {
        let games = [
            game("steam:1", Some(r"C:\Steam\steam.exe"), Some(r"C:\Steam")),
            game("custom:2", None, Some(r"C:\Games\Two")),
        ];
        assert!(Watchlist::new(&games, &BTreeMap::new()).is_empty());
    }

    #[test]
    fn filetimes_become_unix_seconds() {
        // 2026-11-18 11:06:40 UTC.
        let steps = (1_795_000_000u64 + 11_644_473_600) * 10_000_000;
        let time = FILETIME {
            dwLowDateTime: steps as u32,
            dwHighDateTime: (steps >> 32) as u32,
        };
        assert_eq!(unix_seconds(time), 1_795_000_000);
    }
}
