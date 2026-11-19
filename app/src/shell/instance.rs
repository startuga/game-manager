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
//! - **A request for the first** (`--play <id>` from a desktop shortcut): instead of coming to the
//!   front, the second start hands it over as `WM_COPYDATA` text marked `COPY_DATA_MARK`
//!   (`hand_over`, waiting at most `HAND_OVER_WAIT` for a busy window), so the game, not Game
//!   Manager, takes the front. The shell gives the text to the app (`App::command`).
//! - When the mutex can't be made or the program's path is unknown, the app starts as before.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use windows::Win32::{
    COPYDATASTRUCT, CloseHandle, CreateMutexW, FindWindowExW, GetWindowThreadProcessId, HANDLE,
    HWND, IsIconic, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
    SMTO_ABORTIFHUNG, SW_RESTORE, SendMessageTimeoutW, SetForegroundWindow, ShowWindow,
    WM_COPYDATA,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// `ERROR_ALREADY_EXISTS`: the mutex was made by another start.
const ALREADY_EXISTS: i32 = 183;
/// Longest mutex name Windows takes (`MAX_PATH`), with room for the namespace.
const LONGEST_NAME: usize = 240;
/// Marks `WM_COPYDATA` text from another start of Game Manager ("GMCD").
pub const COPY_DATA_MARK: usize = 0x474D_4344;
/// How long a second start waits for the first's window to take its request, in milliseconds.
const HAND_OVER_WAIT: u32 = 5_000;

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
/// `None` when one is running already: its window (titled `title`) was given `request`, or
/// without one brought to the front.
pub fn first(title: &str, request: Option<&str>) -> Option<Instance> {
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
    if let Some(window) = find_window(title, &program) {
        match request {
            Some(text) => hand_over(window, text),
            None => bring_forward(window),
        }
    }
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

/// The window titled `title` that `program` runs.
fn find_window(title: &str, program: &Path) -> Option<HWND> {
    let title = HSTRING::from(title);
    let mut after: Option<HWND> = None;
    loop {
        // SAFETY: a null-terminated title; top-level windows of any class, after the last one.
        let window = unsafe { FindWindowExW(None, after, PCWSTR::null(), &title) };
        if window.is_null() {
            return None;
        }
        if program_of(window).is_some_and(|other| same_program(&other, program)) {
            return Some(window);
        }
        after = Some(window);
    }
}

/// Shows the window, restored if minimized.
fn bring_forward(window: HWND) {
    // SAFETY: a plain query of a top-level window.
    if unsafe { IsIconic(window) }.as_bool() {
        // SAFETY: restores that window.
        let _ = unsafe { ShowWindow(window, SW_RESTORE) };
    }
    // SAFETY: this start came from the shell, so it may give the foreground away.
    let _ = unsafe { SetForegroundWindow(window) };
}

/// Gives the window `text` as `WM_COPYDATA` (UTF-16, marked `COPY_DATA_MARK`), waiting at most
/// `HAND_OVER_WAIT` for it.
fn hand_over(window: HWND, text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    let data = COPYDATASTRUCT {
        dwData: COPY_DATA_MARK,
        cbData: (units.len() * 2) as u32,
        lpData: units.as_ptr().cast_mut().cast(),
    };
    // SAFETY: `data` and `units` outlive the call, which returns once the window has read them
    // (or the wait is over); Windows copies them into the other process.
    let _ = unsafe {
        SendMessageTimeoutW(
            window,
            WM_COPYDATA as u32,
            0,
            std::ptr::from_ref(&data) as isize,
            SMTO_ABORTIFHUNG as u32,
            HAND_OVER_WAIT,
            None,
        )
    };
}

/// The text another start handed over: `WM_COPYDATA`'s `lparam`, when it is marked as ours.
///
/// # Safety
/// `lparam` must be the `COPYDATASTRUCT` pointer of a `WM_COPYDATA` being handled.
pub unsafe fn handed_over(lparam: isize) -> Option<String> {
    // SAFETY: the caller's contract: a live COPYDATASTRUCT while the message is handled.
    let data = unsafe { &*(lparam as *const COPYDATASTRUCT) };
    if data.dwData != COPY_DATA_MARK || data.lpData.is_null() {
        return None;
    }
    let len = data.cbData as usize / 2;
    // SAFETY: Windows copied `cbData` bytes there for this message; UTF-16 units are read.
    let units = unsafe { std::slice::from_raw_parts(data.lpData.cast::<u16>(), len) };
    Some(String::from_utf16_lossy(units))
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
        let first_claim = first("Game Manager test window that does not exist", None).unwrap();
        assert!(!first_claim._mutex.0.is_null());
        assert!(first("Game Manager test window that does not exist", None).is_none());
        assert!(
            first(
                "Game Manager test window that does not exist",
                Some("play:x")
            )
            .is_none()
        );
        drop(first_claim);
        let again = first("Game Manager test window that does not exist", None);
        assert!(again.is_some());
    }

    #[test]
    fn handed_over_text_is_read_only_when_marked() {
        let units: Vec<u16> = "play:steam:220".encode_utf16().collect();
        let mut data = COPYDATASTRUCT {
            dwData: COPY_DATA_MARK,
            cbData: (units.len() * 2) as u32,
            lpData: units.as_ptr().cast_mut().cast(),
        };
        let lparam = std::ptr::from_ref(&data) as isize;
        // SAFETY: a live COPYDATASTRUCT, as a WM_COPYDATA would bring.
        let text = unsafe { handed_over(lparam) };
        assert_eq!(text.as_deref(), Some("play:steam:220"));
        data.dwData = 1;
        let lparam = std::ptr::from_ref(&data) as isize;
        // SAFETY: as above.
        let unmarked = unsafe { handed_over(lparam) };
        assert_eq!(unmarked, None);
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
