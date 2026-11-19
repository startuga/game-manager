//! The crash log. The release build aborts on a panic (`panic = "abort"`: nothing catches one), so
//! without this a crash leaves no trace. `install` adds a panic hook that writes what panicked,
//! where, on which thread and the backtrace to `%LOCALAPPDATA%\Game Manager\crash.log` before the
//! process ends (an elevated `--file-ops` run too); the next start `take`s it, shows its first line
//! once, and keeps it as `crash-shown.log` (replacing the one before) for a bug report.
//!
//! The release build is stripped, so its backtrace has addresses only; the message and the place
//! (`file:line:column`) are what tell a crash apart.

use std::backtrace::Backtrace;
use std::fs;
use std::panic::PanicHookInfo;
use std::path::PathBuf;

use crate::json_file;

const LOG: &str = "crash.log";
const SHOWN: &str = "crash-shown.log";

/// A crash from an earlier run: its first line (what panicked, and where) and the file now
/// holding the whole log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Crash {
    pub summary: String,
    pub file: PathBuf,
}

/// Writes the crash log on a panic, then lets the default hook run. Call it first thing in
/// `main`.
pub fn install() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(file) = json_file::local_file(LOG) {
            // Failing to write the log must not panic again: there is nothing else to do.
            let _ = fs::write(file, report(info, &Backtrace::force_capture().to_string()));
        }
        default(info);
    }));
}

/// The crash log an earlier run left, once: it is moved aside to `crash-shown.log` so the next
/// start doesn't show it again. `None` when there is none. Blocking (a small file).
pub fn take() -> Option<Crash> {
    let file = json_file::local_file(LOG).ok()?;
    take_from(file)
}

fn take_from(file: PathBuf) -> Option<Crash> {
    let text = fs::read_to_string(&file).ok()?;
    let shown = file.with_file_name(SHOWN);
    // Replaces the crash shown before (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`).
    let kept = if fs::rename(&file, &shown).is_ok() {
        shown
    } else {
        let _ = fs::remove_file(&file);
        file
    };
    Some(Crash {
        summary: text.lines().next().unwrap_or_default().to_owned(),
        file: kept,
    })
}

/// The log's text: a first line saying what panicked and where, then the thread and backtrace.
fn report(info: &PanicHookInfo<'_>, backtrace: &str) -> String {
    let message = info.payload_as_str().unwrap_or("(no message)");
    let place = info.location().map_or_else(
        || "an unknown place".to_owned(),
        |location| location.to_string(),
    );
    let thread = std::thread::current();
    format!(
        "{message} (at {place}, Game Manager {})\nThread: {}\n\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
        thread.name().unwrap_or("unnamed"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_crash_is_shown_once_and_kept_for_a_report() {
        let dir = std::env::temp_dir().join(format!("gm-crash-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(LOG);
        assert_eq!(take_from(file.clone()), None);
        fs::write(
            &file,
            "boom (at core\\src\\x.rs:1:2, Game Manager 0.1.0)\nThread: main\n",
        )
        .unwrap();
        fs::write(dir.join(SHOWN), "an older crash").unwrap();
        let crash = take_from(file.clone()).unwrap();
        assert_eq!(
            crash.summary,
            "boom (at core\\src\\x.rs:1:2, Game Manager 0.1.0)"
        );
        assert_eq!(crash.file, dir.join(SHOWN));
        assert!(
            fs::read_to_string(dir.join(SHOWN))
                .unwrap()
                .starts_with("boom")
        );
        // Shown once.
        assert_eq!(take_from(file), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_report_says_what_panicked_and_where() {
        let report = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let written = std::sync::Arc::clone(&report);
        let previous = std::panic::take_hook();
        // The hook is the process's: only this thread's panic is recorded (tests run in parallel).
        let here = std::thread::current().id();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == here {
                *written.lock().unwrap() = super::report(info, "backtrace here");
            }
        }));
        let _ = std::panic::catch_unwind(|| panic!("the cache is gone"));
        std::panic::set_hook(previous);
        let report = report.lock().unwrap().clone();
        let first = report.lines().next().unwrap();
        assert!(first.starts_with("the cache is gone (at core"), "{first}");
        assert!(first.contains("crash.rs:"), "{first}");
        assert!(report.contains("backtrace here"));
    }
}
