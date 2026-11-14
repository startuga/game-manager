//! One window per copy of the app. A second start of the same `app.exe` brings the first one's
//! window to the front and ends, so two windows never change the same games' files, the mod
//! caches' work folders and `mods.json` at once (the core's locks are per process).
//!
//! - **Which start is first**: a mutex in the session's namespace named after the program's path,
//!   created by the first start, which keeps it until it ends. Another copy of the app (in another
//!   folder, such as the agent's `target\agent` build beside the user's) has its own name and runs
//!   on its own.
//! - **Its window**: a top-level window with the app's title whose process runs the same program.
//!   A first start that hasn't shown its window yet is left as it is.
//! - When the mutex can't be made or the program's path is unknown, the app starts as before.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows::Win32::{
    CloseHandle, CreateMutexW, FindWindowExW, GetWindowThreadProcessId, HANDLE, HWND, IsIconic,
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW, SW_RESTORE,
    SetForegroundWindow, ShowWindow,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// `ERROR_ALREADY_EXISTS`: the mutex was made by another start.
const ALREADY_EXISTS: i32 = 183;
/// Longest mutex name Windows takes (`MAX_PATH`), with room for the namespace.
const LONGEST_NAME: usize = 240;

/// The first start's claim, kept until the app ends.
pub struct Instance {
    // Held, not read: the mutex is the claim.
    _mutex: Handle,
}

/// A mutex or process handle, closed when dropped (null: none).
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: a handle from CreateMutexW or OpenProcess, owned here and closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// `Some` for the first start of this program in the session, which keeps it while it runs;
/// `None` when one is running already: its window (titled `title`) was brought to the front.
pub fn first(title: &str) -> Option<Instance> {
    let Ok(program) = std::env::current_exe() else {
        return Some(Instance {
            _mutex: Handle(std::ptr::null_mut()),
        });
    };
    let name = HSTRING::from(mutex_name(&program));
    // SAFETY: a null-terminated name; default security, and not taken: it only marks the start.
    let handle = unsafe { CreateMutexW(None, false, &name) };
    let existed = std::io::Error::last_os_error().raw_os_error() == Some(ALREADY_EXISTS);
    let instance = Instance {
        _mutex: Handle(handle),
    };
    if handle.is_null() || !existed {
        return Some(instance);
    }
    bring_forward(title, &program);
    None
}

/// `Local\Game Manager <program>`: the path in lower case, `\` as `/` (a name can't hold `\`
/// past its namespace), its end when it is long.
fn mutex_name(program: &Path) -> String {
    let path = program.to_string_lossy().to_lowercase().replace('\\', "/");
    let skip = path.chars().count().saturating_sub(LONGEST_NAME);
    let tail: String = path.chars().skip(skip).collect();
    format!(r"Local\Game Manager {tail}")
}

/// Shows the window titled `title` that `program` runs, restored if minimized.
fn bring_forward(title: &str, program: &Path) {
    let title = HSTRING::from(title);
    let mut after: Option<HWND> = None;
    loop {
        // SAFETY: a null-terminated title; top-level windows of any class, after the last one.
        let window = unsafe { FindWindowExW(None, after, PCWSTR::null(), &title) };
        if window.is_null() {
            return;
        }
        if program_of(window).is_some_and(|other| same_program(&other, program)) {
            // SAFETY: a plain query of a top-level window.
            if unsafe { IsIconic(window) }.as_bool() {
                // SAFETY: restores that window.
                let _ = unsafe { ShowWindow(window, SW_RESTORE) };
            }
            // SAFETY: this start came from the shell, so it may give the foreground away.
            let _ = unsafe { SetForegroundWindow(window) };
            return;
        }
        after = Some(window);
    }
}

/// The program the window's process runs.
fn program_of(window: HWND) -> Option<PathBuf> {
    let mut process_id = 0u32;
    // SAFETY: a window handle and a valid out-pointer.
    unsafe { GetWindowThreadProcessId(window, Some(&raw mut process_id)) };
    // SAFETY: asks only to query the process's name, closed by `process` below.
    let handle =
        unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION as u32, false, process_id) };
    if handle.is_null() {
        return None;
    }
    let process = Handle(handle);
    let mut buffer = vec![0u16; 32_768];
    let mut size = buffer.len() as u32;
    // SAFETY: `buffer` holds `size` units; Windows writes the path and its length.
    let named =
        unsafe { QueryFullProcessImageNameW(process.0, 0, PWSTR(buffer.as_mut_ptr()), &mut size) };
    let path = buffer.get(..size as usize)?;
    named
        .as_bool()
        .then(|| PathBuf::from(OsString::from_wide(path)))
}

fn same_program(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_program_has_its_own_name() {
        let user = mutex_name(Path::new(r"C:\Games\Game Manager\app.exe"));
        assert_eq!(user, r"Local\Game Manager c:/games/game manager/app.exe");
        let agent = mutex_name(Path::new(r"C:\dev\game-manager\target\agent\debug\app.exe"));
        assert_ne!(user, agent);
        assert_eq!(
            user,
            mutex_name(Path::new(r"c:\GAMES\Game Manager\APP.EXE"))
        );
        let long = format!(r"C:\{}\app.exe", "folder\\".repeat(60));
        let name = mutex_name(Path::new(&long));
        assert!(name.chars().count() <= LONGEST_NAME + 20 && name.ends_with("/app.exe"));
    }

    #[test]
    fn a_second_start_finds_the_first() {
        // This test's own program: the first claim makes the mutex, a second sees it.
        let first_claim = first("Game Manager test window that does not exist").unwrap();
        assert!(!first_claim._mutex.0.is_null());
        assert!(first("Game Manager test window that does not exist").is_none());
        drop(first_claim);
        let again = first("Game Manager test window that does not exist");
        assert!(again.is_some());
    }

    #[test]
    fn a_process_is_named_by_its_program() {
        assert!(same_program(
            Path::new(r"C:\Games\App.exe"),
            Path::new(r"c:\games\app.EXE")
        ));
        assert!(!same_program(
            Path::new(r"C:\Games\App.exe"),
            Path::new(r"C:\Other\App.exe")
        ));
    }
}
