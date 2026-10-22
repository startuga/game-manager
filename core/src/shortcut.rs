//! Reads Windows shortcuts (`.lnk`) with the Shell's own `ShellLink` object: the program a
//! shortcut starts, its arguments and its icon.

use crate::IconRef;
use crate::com::ComGuard;
use std::path::{Path, PathBuf};
use windows::Win32::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile, IShellLinkW, STGM_READ, ShellLink,
};
use windows::core::{HSTRING, Interface, PWSTR};

/// Longest path or argument string read from a shortcut, in UTF-16 units.
const TEXT_UNITS: usize = 32_768;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Shortcut {
    /// The file the shortcut opens; it may not exist any more.
    pub target: PathBuf,
    pub args: String,
    /// The shortcut's own icon, `None` when it shows the target's first icon.
    pub icon: Option<IconRef>,
}

/// Reads a `.lnk` file. A shortcut that opens no file (a Store app, a Control Panel item) is an
/// error.
pub(crate) fn read(lnk: &Path) -> Result<Shortcut, String> {
    let error = |e: windows::core::Error| format!("{}: {}", lnk.display(), e.message());
    let _com = ComGuard::new();
    // SAFETY: COM is initialized on this thread while `_com` lives, which outlasts `link`.
    let link: IShellLinkW =
        unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) }.map_err(error)?;
    let file = HSTRING::from(lnk.as_os_str());
    // SAFETY: `file` is a null-terminated wide string that outlives the call.
    unsafe {
        link.cast::<IPersistFile>()
            .map_err(error)?
            .Load(file.as_ptr(), STGM_READ as u32)
    }
    .ok()
    .map_err(error)?;

    let target = read_text(|buffer, len| {
        // SAFETY: `buffer` is writable for `len` units; the find data is optional (null).
        unsafe { link.GetPath(buffer, len, std::ptr::null_mut(), 0) }
    })
    .map_err(error)?;
    let args = read_text(|buffer, len| {
        // SAFETY: `buffer` is writable for `len` units.
        unsafe { link.GetArguments(buffer, len) }
    })
    .map_err(error)?;
    let mut index = 0;
    let icon_file = read_text(|buffer, len| {
        // SAFETY: `buffer` is writable for `len` units and `index` is a live i32.
        unsafe { link.GetIconLocation(buffer, len, &mut index) }
    })
    .map_err(error)?;

    // Store apps and other shell items have no file path.
    if target.trim().is_empty() {
        return Err(format!("{} does not start a program", shortcut_name(lnk)));
    }
    let target = PathBuf::from(target);
    let icon = Some(icon_file.trim())
        .filter(|file| !file.is_empty())
        .map(|file| IconRef {
            path: PathBuf::from(expand_env(file)),
            index,
        })
        .filter(|icon| !(icon.index == 0 && same_file(&icon.path, &target)));
    Ok(Shortcut {
        target,
        args: args.trim().to_owned(),
        icon,
    })
}

/// A string an `IShellLinkW` getter writes into a buffer it is given.
fn read_text(
    read: impl FnOnce(PWSTR, i32) -> windows::core::HRESULT,
) -> windows::core::Result<String> {
    let mut buffer = vec![0u16; TEXT_UNITS];
    read(PWSTR(buffer.as_mut_ptr()), buffer.len() as i32).ok()?;
    let len = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..len]))
}

/// The name a shortcut shows: its file name without `.lnk`.
pub(crate) fn shortcut_name(lnk: &Path) -> String {
    lnk.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Expands `%NAME%` environment variables, as Explorer does for icon locations such as
/// `%SystemRoot%\System32\shell32.dll`. Unknown variables are left as written.
fn expand_env(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(name).ok().filter(|_| !name.is_empty()) {
            Some(value) => out.push_str(&value),
            None => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

fn same_file(a: &Path, b: &Path) -> bool {
    a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Writes a `.lnk` with the Shell's own ShellLink object, as Explorer would.
    pub(crate) fn write(lnk: &Path, target: &Path, args: &str, icon: Option<(&str, i32)>) {
        let _com = ComGuard::new();
        // SAFETY: COM is initialized on this thread while `_com` lives; every string is a
        // null-terminated wide string that outlives its call.
        unsafe {
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            link.SetPath(&HSTRING::from(target.as_os_str()))
                .ok()
                .unwrap();
            link.SetArguments(&HSTRING::from(args)).ok().unwrap();
            if let Some((file, index)) = icon {
                link.SetIconLocation(&HSTRING::from(file), index)
                    .ok()
                    .unwrap();
            }
            let file = HSTRING::from(lnk.as_os_str());
            link.cast::<IPersistFile>()
                .unwrap()
                .Save(file.as_ptr(), true)
                .ok()
                .unwrap();
        }
    }

    pub(crate) fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Lnk-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_target_arguments_and_icon() {
        let dir = temp_dir("read");
        let target = dir.join("game.exe");
        std::fs::write(&target, b"MZ").unwrap();
        let lnk = dir.join("My Game.lnk");
        write(
            &lnk,
            &target,
            " -windowed ",
            Some((r"%SystemRoot%\System32\shell32.dll", 12)),
        );

        let shortcut = read(&lnk).unwrap();
        assert_eq!(shortcut.target, target);
        assert_eq!(shortcut.args, "-windowed");
        let system_root = std::env::var("SystemRoot").unwrap();
        assert_eq!(
            shortcut.icon,
            Some(IconRef {
                path: PathBuf::from(format!(r"{system_root}\System32\shell32.dll")),
                index: 12,
            })
        );
        assert_eq!(shortcut_name(&lnk), "My Game");

        // The target's own first icon is no icon of the shortcut's own.
        write(&lnk, &target, "", Some((&target.to_string_lossy(), 0)));
        assert_eq!(read(&lnk).unwrap().icon, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_file_that_is_not_a_shortcut_is_an_error() {
        let dir = temp_dir("broken");
        let lnk = dir.join("broken.lnk");
        std::fs::write(&lnk, b"not a shortcut").unwrap();
        assert!(read(&lnk).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn expands_known_environment_variables_only() {
        let system_root = std::env::var("SystemRoot").unwrap();
        assert_eq!(
            expand_env(r"%SystemRoot%\x.dll"),
            format!(r"{system_root}\x.dll")
        );
        assert_eq!(
            expand_env(r"%GM_UNKNOWN_VARIABLE%\x"),
            r"%GM_UNKNOWN_VARIABLE%\x"
        );
        assert_eq!(expand_env("100% sure"), "100% sure");
        assert_eq!(expand_env("%%"), "%%");
    }
}
