//! Unpacks and packs archives with the 7-Zip command line that ships next to the app
//! (`7-Zip\7z.exe` and its `7z.dll`, GNU LGPL: `License.txt` ships with them). Blocking: it waits
//! for 7-Zip, at most `TIMEOUT` (an archive that makes it hang, or wait for a password, is stopped
//! then).
//!
//! The archive and the files asked for follow `--`, which ends 7-Zip's switches, so a name that
//! starts with `-` is never read as one. Exit code 1 is a warning (a file it could not set the
//! time of), not a failure.

use crate::{Error, Result, config};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use windows::Win32::CREATE_NO_WINDOW;

const SEVEN_ZIP: &str = r"7-Zip\7z.exe";
/// The longest 7-Zip may take; Streamline's 280 MB archive unpacks in seconds.
const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// The shipped `7z.exe`.
fn tool() -> Result<PathBuf> {
    config::shipped_file(Path::new(SEVEN_ZIP))
        .ok_or_else(|| Error::ToolMissing(PathBuf::from(SEVEN_ZIP)))
}

/// Unpacks `archive` into `dest`, keeping its folders. A `.tar.gz` is unpacked twice, the
/// compressed file and then the `.tar` inside it.
pub(crate) fn extract(archive: &Path, dest: &Path) -> Result<()> {
    let name = archive
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if !(name.ends_with(".tar.gz") || name.ends_with(".tgz")) {
        return unpack(archive, dest);
    }
    let inner = dest.join(".gzip");
    unpack(archive, &inner)?;
    let tar = fs::read_dir(&inner)
        .map_err(Error::io(&inner))?
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|ext| ext == "tar"))
        .ok_or_else(|| Error::NotInArchive {
            archive: archive.to_path_buf(),
            what: "TAR archive",
        })?;
    unpack(&tar, dest)?;
    fs::remove_dir_all(&inner).map_err(Error::io(&inner))
}

/// `7z x -y -o<dest> -- <archive>`.
fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    run(&["x"], dest, archive, &[]).map_err(|outcome| outcome.into_error(archive, None))
}

/// Unpacks only the files of `archive` that match `filters` (paths inside it, `*` matching any
/// text: `bin\x64\sl.*.dll`) into `dest`, without their folders.
pub(crate) fn extract_matching(archive: &Path, dest: &Path, filters: &[String]) -> Result<()> {
    let filters: Vec<&OsStr> = filters.iter().map(OsStr::new).collect();
    run(&["e"], dest, archive, &filters).map_err(|outcome| outcome.into_error(archive, None))
}

/// Packs `folder` (with its name) into the ZIP `archive`, replacing one that is there.
pub(crate) fn zip_folder(folder: &Path, archive: &Path) -> Result<()> {
    if archive.exists() {
        fs::remove_file(archive).map_err(Error::io(archive))?;
    }
    let tool = tool()?;
    let status = Command::new(&tool)
        .args(["a", "-tzip", "-y", "-bso0", "-bsp0", "-bse0", "--"])
        .arg(archive)
        .arg(folder)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW as u32)
        .spawn()
        .map_err(Error::io(&tool))
        .map_err(Outcome::Failed)
        .and_then(|child| wait(child, &tool));
    status.map_err(|outcome| outcome.into_error(archive, Some(folder)))
}

/// How a run of 7-Zip went wrong.
enum Outcome {
    /// It ended with this exit code (or none).
    Code(Option<i32>),
    /// It ran past `TIMEOUT` and was stopped.
    TimedOut,
    Failed(Error),
}

impl Outcome {
    /// The error for unpacking `archive`, or packing `folder` into it.
    fn into_error(self, archive: &Path, folder: Option<&Path>) -> Error {
        let code = match self {
            Outcome::Failed(error) => return error,
            Outcome::TimedOut => {
                return Error::TookTooLong {
                    what: match folder {
                        Some(folder) => format!("7-Zip packing {}", folder.display()),
                        None => format!("7-Zip unpacking {}", archive.display()),
                    },
                    minutes: TIMEOUT.as_secs() / 60,
                };
            }
            Outcome::Code(code) => code.unwrap_or(-1),
        };
        match folder {
            Some(folder) => Error::Pack {
                folder: folder.to_path_buf(),
                archive: archive.to_path_buf(),
                code,
            },
            None => Error::Extract {
                archive: archive.to_path_buf(),
                code,
            },
        }
    }
}

/// `7z <command> -y -o<dest> -- <archive> <names…>`, quietly and without a console window (7-Zip is
/// a console program, the app has none).
fn run(
    command: &[&str],
    dest: &Path,
    archive: &Path,
    names: &[&OsStr],
) -> std::result::Result<(), Outcome> {
    let tool = tool().map_err(Outcome::Failed)?;
    let mut out = OsString::from("-o");
    out.push(dest);
    let child = Command::new(&tool)
        .args(command)
        .args(["-y", "-bso0", "-bsp0", "-bse0"])
        .arg(out)
        .arg("--")
        .arg(archive)
        .args(names)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW as u32)
        .spawn()
        .map_err(|error| Outcome::Failed(Error::io(&tool)(error)))?;
    wait(child, &tool)
}

/// Waits for 7-Zip at most `TIMEOUT`, stopping it then. Exit codes 0 and 1 (a warning) succeed.
fn wait(mut child: std::process::Child, tool: &Path) -> std::result::Result<(), Outcome> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return match status.code() {
                    Some(0 | 1) => Ok(()),
                    code => Err(Outcome::Code(code)),
                };
            }
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Outcome::TimedOut);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(Outcome::Failed(Error::io(tool)(error))),
        }
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
        // A .tar.gz holding no .tar.
        fs::write(dir.join("note.txt"), b"no tar here").unwrap();
        seven_zip(&dir, &["a", "-tgzip", "empty.tar.gz", "note.txt"]);
        let error = extract(&dir.join("empty.tar.gz"), &dir.join("empty")).unwrap_err();
        assert!(
            matches!(
                error,
                Error::NotInArchive {
                    what: "TAR archive",
                    ..
                }
            ),
            "{error}"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_starting_with_a_dash_are_files() {
        // A folder and an archive whose names 7-Zip would read as switches without `--`.
        let dir = temp_dir("dash");
        let folder = dir.join("-shaders");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("a.fx"), b"fx").unwrap();
        let archive = dir.join("-packed.zip");
        zip_folder(&folder, &archive).unwrap();
        extract(&archive, &dir.join("-out")).unwrap();
        assert_eq!(fs::read(dir.join(r"-out\-shaders\a.fx")).unwrap(), b"fx");
        extract_matching(&archive, &dir.join("-one"), &[r"-shaders\a.*".to_owned()]).unwrap();
        assert_eq!(fs::read(dir.join(r"-one\a.fx")).unwrap(), b"fx");
        // An archive 7-Zip can't write (its folder is a file) fails as packing.
        let error = zip_folder(&folder, &folder.join(r"a.fx\packed.zip")).unwrap_err();
        assert!(matches!(error, Error::Pack { .. }), "{error}");
        assert!(error.to_string().contains("pack"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }
}
