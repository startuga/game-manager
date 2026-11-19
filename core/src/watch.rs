//! New installs noticed: the places the store scanners read their games from, waited on in the
//! background, so the app scans again when one changes (a game installed or removed in its
//! store) without the user pressing Rescan.
//!
//! - **The places** (`places`), each from its scanner: Steam's own `steamapps`
//!   (`libraryfolders.vdf`) and each library's (its `appmanifest_*.acf`); the Epic launcher's
//!   data folder (`installations.jsonl`) and its legacy `Manifests`; the registry keys of GOG,
//!   the EA app and Ubisoft Connect (`HKLM`). Xbox games (packages) and games added by hand
//!   aren't watched: Rescan still finds them.
//! - **Waiting** (`wait`): one change notification per folder (file names and writes in it, not
//!   below) and per key (names and values, below it too), all waited on at once (at most 64),
//!   until one fires or the timeout passes, after which the caller gathers the places again (a
//!   library added). A place that can't be opened is left out. Each handle has an owner that
//!   closes it.
//! - **A change** says only that something changed: a store writes its files many times while
//!   installing, so the app waits for a quiet moment before scanning.

use crate::{ea, epic, gog, steam, ubisoft};
use std::path::PathBuf;
use std::time::Duration;
use windows::Win32::{
    CloseHandle, CreateEventW, FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
    FindCloseChangeNotification, FindFirstChangeNotificationW, HANDLE, HKEY, HKEY_LOCAL_MACHINE,
    INVALID_HANDLE_VALUE, KEY_NOTIFY, MAXIMUM_WAIT_OBJECTS, REG_NOTIFY_CHANGE_LAST_SET,
    REG_NOTIFY_CHANGE_NAME, RegCloseKey, RegNotifyChangeKeyValue, RegOpenKeyExW,
    WaitForMultipleObjects,
};
use windows::core::{HSTRING, PCWSTR};

/// A place a store keeps its games' list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Place {
    Folder(PathBuf),
    /// A key under `HKEY_LOCAL_MACHINE`.
    Key(String),
}

/// What ended a wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Woke {
    Changed,
    TimedOut,
}

/// The places the store scanners read, those on this PC. Blocking (reads Steam's library list).
pub fn places() -> Vec<Place> {
    let mut places: Vec<Place> = steam::watched_folders()
        .into_iter()
        .chain(epic::watched_folders())
        .filter(|folder| folder.is_dir())
        .map(Place::Folder)
        .collect();
    places.extend(
        gog::WATCHED_KEYS
            .iter()
            .chain(ea::WATCHED_KEYS)
            .chain(ubisoft::WATCHED_KEYS)
            .map(|key| Place::Key((*key).to_owned())),
    );
    places
}

/// Waits until one of `places` changes, or `timeout` passes. With none that can be opened it
/// only waits. Blocking.
pub fn wait(places: &[Place], timeout: Duration) -> Woke {
    let watches: Vec<Watch> = places
        .iter()
        .filter_map(Watch::open)
        .take(MAXIMUM_WAIT_OBJECTS as usize)
        .collect();
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
    if watches.is_empty() {
        std::thread::sleep(timeout);
        return Woke::TimedOut;
    }
    let handles: Vec<HANDLE> = watches.iter().map(Watch::handle).collect();
    // SAFETY: `handles` holds `len` handles that `watches` owns and keeps open over the call.
    let result =
        unsafe { WaitForMultipleObjects(handles.len() as u32, handles.as_ptr(), false, millis) };
    // WAIT_OBJECT_0 + n: the nth handle fired.
    if (result as usize) < handles.len() {
        Woke::Changed
    } else {
        Woke::TimedOut
    }
}

/// An open change notification, closed when dropped.
enum Watch {
    Folder(HANDLE),
    Key { key: HKEY, event: HANDLE },
}

impl Watch {
    fn open(place: &Place) -> Option<Self> {
        match place {
            Place::Folder(folder) => {
                let path = HSTRING::from(folder.as_os_str());
                // SAFETY: a null-terminated path; the handle is owned by the `Watch` made below.
                let handle = unsafe {
                    FindFirstChangeNotificationW(
                        &path,
                        false,
                        (FILE_NOTIFY_CHANGE_FILE_NAME | FILE_NOTIFY_CHANGE_LAST_WRITE) as u32,
                    )
                };
                (!handle.is_null() && handle != INVALID_HANDLE_VALUE)
                    .then_some(Watch::Folder(handle))
            }
            Place::Key(path) => {
                let name = HSTRING::from(path.as_str());
                let mut key: HKEY = std::ptr::null_mut();
                // SAFETY: a predefined key, a null-terminated name and a valid out-pointer.
                let opened = unsafe {
                    RegOpenKeyExW(HKEY_LOCAL_MACHINE, &name, None, KEY_NOTIFY as u32, &mut key)
                };
                if opened != 0 || key.is_null() {
                    return None;
                }
                // SAFETY: an unnamed manual-reset event, not signalled, default security.
                let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) };
                // Owned from here, so a failure below closes what is open.
                let watch = Watch::Key { key, event };
                if event.is_null() {
                    return None;
                }
                // SAFETY: a key opened with KEY_NOTIFY and an event, both owned by `watch`.
                let armed = unsafe {
                    RegNotifyChangeKeyValue(
                        key,
                        true,
                        (REG_NOTIFY_CHANGE_NAME | REG_NOTIFY_CHANGE_LAST_SET) as u32,
                        Some(event),
                        true,
                    )
                };
                (armed == 0).then_some(watch)
            }
        }
    }

    fn handle(&self) -> HANDLE {
        match self {
            Watch::Folder(handle) => *handle,
            Watch::Key { event, .. } => *event,
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        match self {
            Watch::Folder(handle) => {
                // SAFETY: a handle from FindFirstChangeNotificationW, closed once.
                let _ = unsafe { FindCloseChangeNotification(*handle) };
            }
            Watch::Key { key, event } => {
                // SAFETY: a key from RegOpenKeyExW, closed once; this also ends its notification.
                let _ = unsafe { RegCloseKey(*key) };
                if !event.is_null() {
                    // SAFETY: an event from CreateEventW, closed once after the key.
                    let _ = unsafe { CloseHandle(*event) };
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Instant;

    #[test]
    fn a_new_file_wakes_the_wait_and_quiet_times_out() {
        let dir = std::env::temp_dir().join(format!("GM-Watch-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let places = [Place::Folder(dir.clone())];

        let started = Instant::now();
        assert_eq!(wait(&places, Duration::from_millis(200)), Woke::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(150));

        let writer = {
            let file = dir.join("appmanifest_220.acf");
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(300));
                fs::write(file, b"\"AppState\" {}").unwrap();
            })
        };
        assert_eq!(wait(&places, Duration::from_secs(10)), Woke::Changed);
        writer.join().unwrap();

        // A place that doesn't exist is left out; with none, the wait only times out.
        let missing = [
            Place::Folder(dir.join("missing")),
            Place::Key(r"SOFTWARE\GM-No-Such-Key".into()),
        ];
        assert_eq!(wait(&missing, Duration::from_millis(50)), Woke::TimedOut);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn this_pcs_places_open() {
        // Read only: whatever stores this PC has, their places are listed and can be waited on.
        let places = places();
        assert_eq!(wait(&places, Duration::from_millis(10)), Woke::TimedOut);
    }
}
