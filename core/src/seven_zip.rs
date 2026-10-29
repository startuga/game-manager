//! Unpacks archives with the 7-Zip command line that ships next to the app (`7-Zip\7z.exe` and
//! its `7z.dll`, GNU LGPL: `License.txt` ships with them). Blocking: it waits for 7-Zip.

use crate::{Error, Result, config};
use std::ffi::OsString;
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use windows::Win32::CREATE_NO_WINDOW;

const SEVEN_ZIP: &str = r"7-Zip\7z.exe";

/// The shipped `7z.exe`.
fn tool() -> Result<PathBuf> {
    config::shipped_file(Path::new(SEVEN_ZIP))
        .ok_or_else(|| Error::ToolMissing(PathBuf::from(SEVEN_ZIP)))
}

/// Unpacks `archive` into `dest`, keeping its folders. A `.tar.gz` is unpacked twice, the
/// compressed file and then the `.tar` inside it.
pub(crate) fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let tool = tool()?;
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !(name.ends_with(".tar.gz") || name.ends_with(".tgz")) {
        return run(&tool, archive, dest);
    }
    let inner = dest.join(".gzip");
    run(&tool, archive, &inner)?;
    let tar = fs::read_dir(&inner)
        .map_err(Error::io(&inner))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "tar"))
        .ok_or_else(|| Error::Extract {
            archive: archive.to_path_buf(),
            code: 0,
        })?;
    run(&tool, &tar, dest)?;
    fs::remove_dir_all(&inner).map_err(Error::io(&inner))
}

/// `7z x -y -o<dest> <archive>`, quietly and without a console window (7-Zip is a console program,
/// the app has none). Exit code 1 is a warning (e.g. a file it could not set the time of).
fn run(tool: &Path, archive: &Path, dest: &Path) -> Result<()> {
    let mut out = OsString::from("-o");
    out.push(dest);
    let status = Command::new(tool)
        .args(["x", "-y", "-bso0", "-bsp0", "-bse0"])
        .arg(out)
        .arg(archive)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW as u32)
        .status()
        .map_err(Error::io(tool))?;
    match status.code() {
        Some(0 | 1) => Ok(()),
        code => Err(Error::Extract {
            archive: archive.to_path_buf(),
            code: code.unwrap_or(-1),
        }),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-7z-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Runs the shipped 7-Zip with `args` in `dir`, to build test archives.
    pub(crate) fn seven_zip(dir: &Path, args: &[&str]) {
        let status = Command::new(tool().unwrap())
            .args(args)
            .current_dir(dir)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "7z {args:?}");
    }

    #[test]
    fn unpacks_zip_and_tar_gz_archives() {
        let dir = temp_dir("formats");
        fs::create_dir_all(dir.join(r"build\x64")).unwrap();
        fs::write(dir.join(r"build\x64\d3d9.dll"), b"x64 d3d9").unwrap();
        seven_zip(&dir.join("build"), &["a", r"..\build.zip", "x64"]);
        seven_zip(&dir.join("build"), &["a", "-ttar", r"..\build.tar", "x64"]);
        seven_zip(&dir, &["a", "-tgzip", "build.tar.gz", "build.tar"]);

        extract(&dir.join("build.zip"), &dir.join("zip")).unwrap();
        assert_eq!(
            fs::read(dir.join(r"zip\x64\d3d9.dll")).unwrap(),
            b"x64 d3d9"
        );
        extract(&dir.join("build.tar.gz"), &dir.join("tgz")).unwrap();
        assert_eq!(
            fs::read(dir.join(r"tgz\x64\d3d9.dll")).unwrap(),
            b"x64 d3d9"
        );
        assert!(!dir.join(r"tgz\.gzip").exists());

        // Not an archive: 7-Zip's error code.
        fs::write(dir.join("broken.zip"), b"not a zip").unwrap();
        let error = extract(&dir.join("broken.zip"), &dir.join("broken")).unwrap_err();
        assert!(matches!(error, Error::Extract { code: 2, .. }), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }
}
