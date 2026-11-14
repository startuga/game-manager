//! Files written whole: the JSON files (`json_file::save`), the cached covers and icons, and a
//! cover the user chose. Each is written to a temporary file beside it,
//! `<name>.<process>-<n>.tmp`, then renamed over the file, so a crash or a reader never sees one
//! half written. Each write has its own temporary name, so two writers (threads, or two copies of
//! the app) never write into the same one, and it is removed when the write or the rename fails.
//!
//! A cache's prune leaves temporary files younger than ten minutes (`is_stale_temp`), as they may
//! be a write still in progress, and removes older ones.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// How long a temporary file may be a write in progress; older ones are leftovers.
const TEMP_AGE: Duration = Duration::from_secs(10 * 60);

/// Writes `bytes` as `file` through a temporary file of its own beside it (see the module's doc),
/// which is removed when the write or the rename fails. The folder must exist.
pub(crate) fn write(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut name = file.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temp = file.with_file_name(name);
    let written = fs::write(&temp, bytes).and_then(|()| fs::rename(&temp, file));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Whether `path` is a temporary file left by an earlier write: a `.tmp` file last written more
/// than ten minutes ago. A younger one may be a write in progress.
pub(crate) fn is_stale_temp(path: &Path) -> bool {
    let is_temp = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tmp"));
    is_temp
        && fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|at| SystemTime::now().duration_since(at).ok())
            .is_some_and(|age| age > TEMP_AGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Whole-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_use_their_own_temporary_files_and_leave_none() {
        let dir = temp_dir("write");
        let file = dir.join("sample.json");
        // Threads writing the same file at once each finish, and one of them wins.
        let writers: Vec<_> = (0..8u8)
            .map(|n| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        // A rename can meet another's open file; that write only fails.
                        let _ = write(&file, &[n; 4096]);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let bytes = fs::read(&file).unwrap();
        assert!(bytes.len() == 4096 && bytes.iter().all(|byte| *byte == bytes[0]));
        let names: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(names, ["sample.json"], "no temporary file is left");

        // A write that can't be renamed (the file is a folder) removes its temporary file.
        let folder = dir.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("inside"), b"x").unwrap();
        assert!(write(&folder, b"bytes").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_old_temporary_files_are_stale() {
        let dir = temp_dir("stale");
        let fresh = dir.join("cover.jpg.1-0.tmp");
        let old = dir.join("cover.jpg.1-1.tmp");
        let old_other = dir.join("cover.jpg");
        for path in [&fresh, &old, &old_other] {
            fs::write(path, b"x").unwrap();
        }
        let hour_ago = SystemTime::now() - Duration::from_secs(60 * 60);
        for path in [&old, &old_other] {
            let handle = fs::File::options().write(true).open(path).unwrap();
            handle.set_modified(hour_ago).unwrap();
        }
        assert!(!is_stale_temp(&fresh));
        assert!(is_stale_temp(&old));
        assert!(!is_stale_temp(&old_other), "not a temporary file");
        assert!(!is_stale_temp(&dir.join("missing.tmp")));
        fs::remove_dir_all(&dir).unwrap();
    }
}
