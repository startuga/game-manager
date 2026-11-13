//! The files Game Manager keeps: the user's choices in `%APPDATA%\Game Manager` (roaming) and
//! caches in `%LOCALAPPDATA%\Game Manager` (this computer only, safe to delete). For the small JSON
//! files, a missing file reads as empty.
//!
//! **Whole files** (`write_whole`): the JSON files, the cached covers and icons are written to a
//! temporary file beside them, `<name>.<process>-<n>.tmp`, then renamed over the file, so a crash
//! or a reader never sees one half written. Each write has its own temporary name, so two writers
//! (threads, or two copies of the app) never write into the same one, and it is removed when the
//! write or the rename fails. A cache's prune leaves temporary files younger than ten minutes
//! (`is_stale_temp`), as they may be a write still in progress, and removes older ones.

use crate::error::{Error, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

/// How long a temporary file may be a write in progress; older ones are leftovers.
const TEMP_AGE: Duration = Duration::from_secs(10 * 60);

/// `%APPDATA%\Game Manager\<name>`.
pub(crate) fn user_file(name: &str) -> Result<PathBuf> {
    let app_data = std::env::var_os("APPDATA").ok_or(Error::MissingEnvVar("APPDATA"))?;
    Ok(PathBuf::from(app_data).join("Game Manager").join(name))
}

/// `%LOCALAPPDATA%\Game Manager\<name>`: files for this computer only, such as caches.
pub(crate) fn local_file(name: &str) -> Result<PathBuf> {
    let local_app_data =
        std::env::var_os("LOCALAPPDATA").ok_or(Error::MissingEnvVar("LOCALAPPDATA"))?;
    Ok(PathBuf::from(local_app_data)
        .join("Game Manager")
        .join(name))
}

/// The file's contents, or `T::default()` when it does not exist. A file that cannot be read or
/// parsed is an error, so a caller that loads before saving never overwrites it.
pub(crate) fn load<T: DeserializeOwned + Default>(file: &Path) -> Result<T> {
    match fs::read(file) {
        Ok(bytes) => {
            // Some editors save UTF-8 with a byte order mark, which JSON parsers reject.
            let json = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
            serde_json::from_slice(json).map_err(Error::json(file))
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(Error::io(file)(e)),
    }
}

/// Writes `value` as indented JSON, creating the folder if needed.
pub(crate) fn save<T: Serialize>(file: &Path, value: &T) -> Result<()> {
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(Error::io(dir))?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(Error::json(file))?;
    write_whole(file, &json).map_err(Error::io(file))
}

/// Writes `bytes` as `file` through a temporary file of its own beside it (see the module's
/// doc), which is removed when the write or the rename fails. The folder must exist.
pub(crate) fn write_whole(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
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
    use serde::Deserialize;
    use std::collections::BTreeMap;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Sample {
        #[serde(default)]
        values: BTreeMap<String, u32>,
    }

    fn temp_file(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Json-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("sample.json")
    }

    #[test]
    fn missing_file_reads_as_default_and_saves_round_trip() {
        let file = temp_file("round-trip");
        assert_eq!(load::<Sample>(&file).unwrap(), Sample::default());

        let sample = Sample {
            values: BTreeMap::from([("a".to_owned(), 1)]),
        };
        save(&file, &sample).unwrap();
        assert_eq!(load::<Sample>(&file).unwrap(), sample);
        let dir = file.parent().unwrap();
        assert_eq!(
            fs::read_dir(dir).unwrap().count(),
            1,
            "no temporary file is left"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn whole_writes_use_their_own_temporary_files_and_leave_none() {
        let file = temp_file("whole");
        let dir = file.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        // Threads writing the same file at once each finish, and one of them wins.
        let writers: Vec<_> = (0..8u8)
            .map(|n| {
                let file = file.clone();
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        // A rename can meet another's open file; that write only fails.
                        let _ = write_whole(&file, &[n; 4096]);
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
        assert!(write_whole(&folder, b"bytes").is_err());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_old_temporary_files_are_stale() {
        let file = temp_file("stale");
        let dir = file.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
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

    #[test]
    fn reads_files_saved_with_a_byte_order_mark() {
        let file = temp_file("bom");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"\xEF\xBB\xBF{ \"values\": { \"a\": 1 } }").unwrap();
        assert_eq!(
            load::<Sample>(&file).unwrap(),
            Sample {
                values: BTreeMap::from([("a".to_owned(), 1)]),
            }
        );
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    #[test]
    fn unparsable_file_is_an_error() {
        let file = temp_file("broken");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{ not json").unwrap();
        let error = load::<Sample>(&file).unwrap_err();
        assert!(matches!(&error, Error::Json { path, .. } if *path == file));
        assert!(
            error.to_string().starts_with(&file.display().to_string()),
            "{error}"
        );
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }
}
