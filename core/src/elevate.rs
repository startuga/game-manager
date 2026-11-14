//! Runs work that needs administrator rights: file operations in protected game folders
//! (`C:\Program Files`, `--file-ops`), ReShade's Vulkan layer with the game folder's files
//! (`--reshade-ops`) and NVIDIA driver settings (`--nvidia-ops`). Game Manager starts a copy of
//! itself with the argument and the encoded work through the `runas` verb, which shows the UAC
//! prompt, and waits for it.
//!
//! **The work travels on the command line**, so the copy running as administrator reads nothing
//! that another program could change in between. Work longer than a command line holds (over
//! 30 000 characters: ReShade's Deploy to many games in protected folders) goes in a file instead,
//! `%LOCALAPPDATA%\Game Manager\elevated\<random>.txt`, and the command line carries the file's
//! SHA-256 and path, `file:<hash>:<path, percent-encoded>`. The copy as administrator
//! (`elevated_work`) reads the file once, as a plain file (a junction at its end is not followed,
//! and a named pipe can't act as the copy), and uses it only when its hash is the one given: so it
//! too uses exactly what Game Manager wrote. Game Manager deletes the file once the copy has
//! ended; the copy never does, as an administrator deleting a path a standard user can change is
//! how files get deleted elsewhere.
//!
//! **Where the work may go** (`check_paths`, run as administrator before any of it): every path
//! whole, on a local drive (`C:\…`: no `..`, no `\\?\`, no network path), outside `%SystemRoot%`
//! (compared as Windows reads the path, `GetFullPathNameW`), and through no junction or symbolic
//! link that a standard user made. One owned by Administrators, SYSTEM or TrustedInstaller is
//! followed (a game an administrator moved to another drive). Refused work ends with an exit code
//! that says why, before anything ran: `ERROR_BAD_PATHNAME` for a path's form or place,
//! `ERROR_REPARSE_POINT_ENCOUNTERED` for a junction, `ERROR_DATA_CHECKSUM_ERROR` for a work file
//! that changed.

use crate::crypto::{hex, random, sha256};
use crate::text::{percent_decode, percent_encode};
use crate::{Error, Result, json_file, security};
use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf, Prefix};
use windows::Win32::{
    CloseHandle, E_FAIL, E_INVALIDARG, ERROR_BAD_PATHNAME, ERROR_CANCELLED,
    ERROR_DATA_CHECKSUM_ERROR, ERROR_REPARSE_POINT_ENCOUNTERED, FILE_FLAG_OPEN_REPARSE_POINT,
    GetExitCodeProcess, HANDLE, INFINITE, SECURITY_IDENTIFICATION, SEE_MASK_NOASYNC,
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SW_HIDE, ShellExecuteExW, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, WIN32_ERROR, w};

/// The argument that makes `app.exe` run file operations instead of its window.
pub const FILE_OPS_ARG: &str = "--file-ops";
/// Work longer than this goes in a file: a command line holds 32 767 characters, the program's
/// path included.
const LONGEST_ON_COMMAND_LINE: usize = 30_000;
/// The largest work file read: far more than any work.
const LARGEST_WORK: u64 = 16 << 20;
/// Starts the argument of work in a file.
const FILE_MARK: &str = "file:";

/// Runs this program as administrator with `<arg> <encoded>` and waits for it to finish.
pub(crate) fn run(arg: &str, encoded: &str) -> Result<()> {
    let work = if fits(arg, encoded) {
        Work::on_command_line(encoded)
    } else {
        Work::in_file(&json_file::local_file("elevated")?, encoded)?
    };
    // `work` lives until the copy has ended, then deletes its file.
    start(arg, &work.argument)
}

/// Whether `<arg> <encoded>` goes on the command line itself.
fn fits(arg: &str, encoded: &str) -> bool {
    arg.len() + 1 + encoded.len() <= LONGEST_ON_COMMAND_LINE
}

fn start(arg: &str, argument: &str) -> Result<()> {
    let program = std::env::current_exe().map_err(Error::io("Game Manager"))?;
    let file = HSTRING::from(program.as_os_str());
    let parameters = HSTRING::from(format!("{arg} {argument}"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: (SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC) as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // SAFETY: `info` is initialized, its size is set, and its strings outlive the call.
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) }.ok() {
        return Err(
            if WIN32_ERROR::from_error(&error) == Some(WIN32_ERROR(ERROR_CANCELLED as u32)) {
                Error::ElevationRefused
            } else {
                Error::Windows {
                    context: "Game Manager as administrator".to_owned(),
                    source: error,
                }
            },
        );
    }
    let process = OwnedHandle(info.hProcess);
    // SAFETY: a live process handle, owned by `process`.
    unsafe { WaitForSingleObject(process.0, INFINITE) };
    let mut code = 0u32;
    // SAFETY: the process has ended; `code` is a valid out-pointer.
    unsafe { GetExitCodeProcess(process.0, &mut code) }
        .ok()
        .map_err(Error::windows("Game Manager as administrator"))?;
    match code {
        0 => Ok(()),
        code => Err(Error::Elevated { code }),
    }
}

/// Work as the command line carries it, and the file that holds it when it is long, deleted when
/// this is dropped.
struct Work {
    argument: String,
    file: Option<PathBuf>,
}

impl Work {
    fn on_command_line(encoded: &str) -> Self {
        Self {
            argument: encoded.to_owned(),
            file: None,
        }
    }

    /// `encoded` in a new file of a random name in `dir`, and the argument naming it with its
    /// SHA-256.
    fn in_file(dir: &Path, encoded: &str) -> Result<Self> {
        fs::create_dir_all(dir).map_err(Error::io(dir))?;
        let file = dir.join(format!("{}.txt", hex(&random()?)));
        let mut out = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&file)
            .map_err(Error::io(&file))?;
        let work = Self {
            argument: format!(
                "{FILE_MARK}{}:{}",
                hex(&sha256(encoded.as_bytes())?),
                percent_encode(&file.to_string_lossy())
            ),
            file: Some(file.clone()),
        };
        // Written once `work` owns the file, so a failed write deletes it.
        out.write_all(encoded.as_bytes())
            .map_err(Error::io(&file))?;
        Ok(work)
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        if let Some(file) = &self.file {
            let _ = fs::remove_file(file);
        }
    }
}

/// The work given to the copy running as administrator: `argument` itself, or what the file it
/// names holds when that has the SHA-256 it gives (see the module's doc). `Err` is the exit code
/// that says why not.
pub fn elevated_work(argument: &str) -> std::result::Result<String, u32> {
    let Some(rest) = argument.strip_prefix(FILE_MARK) else {
        return Ok(argument.to_owned());
    };
    let invalid = E_INVALIDARG.0 as u32;
    let (hash, path) = rest.split_once(':').ok_or(invalid)?;
    let path = PathBuf::from(percent_decode(path).ok_or(invalid)?);
    check_paths([path.as_path()]).map_err(|error| match error {
        Error::Io { source, .. } => io_code(&source),
        _ => E_FAIL.0 as u32,
    })?;
    let mut bytes = Vec::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT as u32)
        .security_qos_flags(SECURITY_IDENTIFICATION as u32)
        .open(&path)
        .and_then(|file| file.take(LARGEST_WORK + 1).read_to_end(&mut bytes))
        .map_err(|error| io_code(&error))?;
    let unchanged = bytes.len() as u64 <= LARGEST_WORK
        && sha256(&bytes).is_ok_and(|digest| hex(&digest).eq_ignore_ascii_case(hash));
    if !unchanged {
        return Err(WIN32_ERROR(ERROR_DATA_CHECKSUM_ERROR as u32).to_hresult().0 as u32);
    }
    String::from_utf8(bytes).map_err(|_| invalid)
}

/// The exit code for an I/O error: its Windows error as an HRESULT.
fn io_code(error: &io::Error) -> u32 {
    error.raw_os_error().map_or(E_FAIL.0 as u32, |code| {
        WIN32_ERROR(code as u32).to_hresult().0 as u32
    })
}

/// Refuses, as administrator, paths that work may not reach (see the module's doc). The error
/// is an I/O error of the path whose Windows error says why, so it becomes the exit code.
pub(crate) fn check_paths<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Result<()> {
    let lower = |path: &Path| PathBuf::from(path.to_string_lossy().to_lowercase());
    let system = std::env::var_os("SystemRoot")
        .and_then(|root| std::path::absolute(root).ok())
        .map(|root| lower(&root));
    // Folders already looked at, and so every folder above them.
    let mut seen = HashSet::new();
    for path in paths {
        let refused = |code: i32| Err(Error::io(path)(io::Error::from_raw_os_error(code)));
        let mut components = path.components();
        let local = matches!(
            components.next(),
            Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_))
        ) && components.next() == Some(Component::RootDir)
            && components.all(|component| matches!(component, Component::Normal(_)));
        if !local {
            return refused(ERROR_BAD_PATHNAME);
        }
        // As Windows reads it: GetFullPathNameW drops a trailing dot or space, as opening does.
        let full = std::path::absolute(path).map_err(Error::io(path))?;
        if system
            .as_ref()
            .is_some_and(|system| lower(&full).starts_with(system))
        {
            return refused(ERROR_BAD_PATHNAME);
        }
        for folder in full.ancestors() {
            if !seen.insert(folder.to_path_buf()) {
                break;
            }
            let linked =
                fs::symlink_metadata(folder).is_ok_and(|item| item.file_type().is_symlink());
            if linked && !security::owner_is_admin(folder).unwrap_or(false) {
                return refused(ERROR_REPARSE_POINT_ENCOUNTERED);
            }
        }
    }
    Ok(())
}

/// A process or token handle, closed when dropped.
pub(crate) struct OwnedHandle(pub(crate) HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from ShellExecuteExW, only this value owns it, and it is
            // closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Elevate-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn code(error: i32) -> u32 {
        WIN32_ERROR(error as u32).to_hresult().0 as u32
    }

    fn refusal(path: &Path) -> Option<i32> {
        match check_paths([path]) {
            Ok(()) => None,
            Err(Error::Io { source, .. }) => source.raw_os_error(),
            Err(error) => panic!("{error}"),
        }
    }

    #[test]
    fn short_work_stays_on_the_command_line() {
        assert!(fits(FILE_OPS_ARG, &"%5B".repeat(9_000)));
        assert!(!fits(FILE_OPS_ARG, &"%5B".repeat(10_000)));
        let work = Work::on_command_line("%5B%5D");
        assert_eq!(work.argument, "%5B%5D");
        assert_eq!(elevated_work(&work.argument), Ok("%5B%5D".to_owned()));
    }

    #[test]
    fn long_work_goes_through_a_file_with_its_hash() {
        let dir = temp_dir("file");
        let encoded = "%5B%7B%22Copy%22".repeat(2_600);
        assert!(encoded.len() > 40_000 && !fits(FILE_OPS_ARG, &encoded));
        let work = Work::in_file(&dir, &encoded).unwrap();
        let file = work.file.clone().unwrap();
        assert!(work.argument.starts_with(FILE_MARK) && work.argument.len() < 400);
        assert_eq!(elevated_work(&work.argument), Ok(encoded.clone()));

        // A byte changed after Game Manager wrote it.
        let mut changed = encoded.clone().into_bytes();
        changed[1000] = b'X';
        fs::write(&file, &changed).unwrap();
        assert_eq!(
            elevated_work(&work.argument),
            Err(code(ERROR_DATA_CHECKSUM_ERROR))
        );
        // Broken arguments.
        assert_eq!(elevated_work("file:abc"), Err(E_INVALIDARG.0 as u32));
        assert_eq!(elevated_work("file:abc:%zz"), Err(E_INVALIDARG.0 as u32));
        // Dropped, the work deletes its file.
        drop(work);
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_work_file_through_a_users_junction_is_refused() {
        let dir = temp_dir("junction-file");
        let real = dir.join("real");
        fs::create_dir(&real).unwrap();
        let work = Work::in_file(&real, "%5B%5D").unwrap();
        let link = dir.join("link");
        security::junction(&link, &real);
        let name = work.file.as_ref().unwrap().file_name().unwrap();
        let (hash, _) = work.argument[FILE_MARK.len()..].split_once(':').unwrap();
        let through = format!(
            "{FILE_MARK}{hash}:{}",
            percent_encode(&link.join(name).to_string_lossy())
        );
        let expected = if security::is_elevated() {
            Ok("%5B%5D".to_owned())
        } else {
            Err(code(ERROR_REPARSE_POINT_ENCOUNTERED))
        };
        assert_eq!(elevated_work(&through), expected);
        drop(work);
        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_whole_local_paths_outside_windows_are_accepted() {
        let dir = temp_dir("paths");
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        assert_eq!(refusal(&dir.join(r"Game\ReShade.ini")), None);
        for refused in [
            PathBuf::from(r"Game\ReShade.ini"),
            PathBuf::from(r"C:Game\ReShade.ini"),
            PathBuf::from(r"\Game\ReShade.ini"),
            dir.join(r"Game\..\..\ReShade.ini"),
            PathBuf::from(format!(r"\\?\{}", dir.display())),
            PathBuf::from(r"\\.\C:\Games"),
            PathBuf::from(r"\\localhost\C$\Games"),
            system.join(r"System32\dxgi.dll"),
            PathBuf::from(format!(r"{}.\System32\dxgi.dll", system.display())),
            PathBuf::from(system.to_string_lossy().to_uppercase()),
        ] {
            assert_eq!(
                refusal(&refused),
                Some(ERROR_BAD_PATHNAME),
                "{}",
                refused.display()
            );
        }
        // A junction a standard user made, anywhere along the path.
        let target = dir.join("target");
        fs::create_dir(&target).unwrap();
        let link = dir.join("link");
        security::junction(&link, &target);
        let expected = (!security::is_elevated()).then_some(ERROR_REPARSE_POINT_ENCOUNTERED);
        assert_eq!(refusal(&link), expected);
        assert_eq!(refusal(&link.join(r"Game\ReShade.ini")), expected);
        assert_eq!(refusal(&target.join(r"Game\ReShade.ini")), None);
        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }
}
