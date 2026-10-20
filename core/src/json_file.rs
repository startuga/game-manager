//! The small JSON files Game Manager keeps for the user in `%APPDATA%\Game Manager`: a missing file
//! reads as empty, and files are written through a temporary file, so a crash never leaves one
//! half written.

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// `%APPDATA%\Game Manager\<name>`.
pub(crate) fn user_file(name: &str) -> Result<PathBuf, String> {
    let app_data = std::env::var_os("APPDATA").ok_or("APPDATA is not set")?;
    Ok(PathBuf::from(app_data).join("Game Manager").join(name))
}

/// The file's contents, or `T::default()` when it does not exist. A file that cannot be read or
/// parsed is an error, so a caller that loads before saving never overwrites it.
pub(crate) fn load<T: DeserializeOwned + Default>(file: &Path) -> Result<T, String> {
    match fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Writes `value` as indented JSON, creating the folder if needed.
pub(crate) fn save<T: Serialize>(file: &Path, value: &T) -> Result<(), String> {
    let error = |e: std::io::Error| format!("{}: {e}", file.display());
    if let Some(dir) = file.parent() {
        fs::create_dir_all(dir).map_err(error)?;
    }
    let json = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    // Written next to the file first, so a crash never leaves it half written.
    let temp = file.with_extension("json.tmp");
    fs::write(&temp, json).map_err(error)?;
    fs::rename(&temp, file).map_err(error)
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
        assert_eq!(load::<Sample>(&file), Ok(Sample::default()));

        let sample = Sample {
            values: BTreeMap::from([("a".to_owned(), 1)]),
        };
        save(&file, &sample).unwrap();
        assert_eq!(load::<Sample>(&file), Ok(sample));
        assert!(!file.with_extension("json.tmp").exists());
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    #[test]
    fn unparsable_file_is_an_error() {
        let file = temp_file("broken");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "{ not json").unwrap();
        let error = load::<Sample>(&file).unwrap_err();
        assert!(error.starts_with(&file.display().to_string()), "{error}");
        fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }
}
