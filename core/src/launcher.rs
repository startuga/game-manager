//! Starts games through the Windows shell, and opens their folders in File Explorer.

use crate::{Error, Launch, Result, folder_key, on_disk_case};
use std::path::Path;
use windows::Win32::{
    ILCreateFromPathW, ILFree, LPITEMIDLIST, SHOpenFolderAndSelectItems, SW_SHOWNORMAL,
    ShellExecuteW,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Launches a game. Store URIs (e.g. `steam://`) are handed to their registered client.
pub fn launch(launch: &Launch) -> Result<()> {
    let (target, args, working_dir) = match launch {
        Launch::Uri(uri) => (HSTRING::from(uri.as_str()), HSTRING::new(), HSTRING::new()),
        Launch::Exe {
            path,
            args,
            working_dir,
        } => {
            // Many games load their data relative to the current folder, so never inherit ours.
            let dir = working_dir.as_deref().or_else(|| path.parent());
            (
                HSTRING::from(path.as_os_str()),
                HSTRING::from(args.as_str()),
                dir.map(|d| HSTRING::from(d.as_os_str()))
                    .unwrap_or_default(),
            )
        }
    };
    let optional = |s: &HSTRING| {
        if s.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(s.as_ptr())
        }
    };

    // SAFETY: all string arguments are valid, null-terminated wide strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target,
            optional(&args),
            optional(&working_dir),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW reports success with a value greater than 32.
    let code = result as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(Error::Shell {
            action: "launch",
            target: target.to_string_lossy(),
            code,
        })
    }
}

/// Opens a game's folder in File Explorer, with `program` selected when it is directly in that
/// folder. Call it on the UI thread, like `launch`: the Shell needs COM, which is initialized there.
pub fn open_folder(dir: &Path, program: Option<&Path>) -> Result<()> {
    // Follows links: an Xbox game's `WindowsApps` package folder is a link to its real
    // `XboxGames\...\Content` folder, which File Explorer can show.
    let folder = on_disk_case(dir);
    if !folder.is_dir() {
        return Err(Error::NotFound(dir.to_path_buf()));
    }
    if let Some(name) = item_to_select(dir, program).and_then(Path::file_name)
        && select_in_folder(&folder.join(name))
    {
        return Ok(());
    }

    let path = HSTRING::from(folder.as_os_str());
    // SAFETY: `path` is a valid, null-terminated wide string that outlives the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = result as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(Error::Shell {
            action: "open",
            target: folder.display().to_string(),
            code,
        })
    }
}

/// Opens the folder holding `file` with `file` selected; `false` when the Shell could not.
fn select_in_folder(file: &Path) -> bool {
    let path = HSTRING::from(file.as_os_str());
    // SAFETY: `path` is a valid, null-terminated wide string that outlives the call.
    let pidl = unsafe { ILCreateFromPathW(&path) };
    if pidl.is_null() {
        return false;
    }
    let item = ItemIdList(pidl);
    // SAFETY: `item` holds a valid absolute item ID list from ILCreateFromPathW. Without child
    // items, the Shell opens the item's parent folder and selects the item.
    let result = unsafe { SHOpenFolderAndSelectItems(item.0.cast_const(), None, 0) };
    result.is_ok()
}

/// An item ID list from `ILCreateFromPathW`, freed when dropped.
struct ItemIdList(LPITEMIDLIST);

impl Drop for ItemIdList {
    fn drop(&mut self) {
        // SAFETY: allocated by ILCreateFromPathW, only this value owns it, and it is freed once,
        // here.
        unsafe { ILFree(Some(self.0)) };
    }
}

/// The program to select in the game's folder: only one directly inside it, so File Explorer
/// still shows the game folder itself (Witcher 3's `bin\x64_dx12\witcher3.exe` is not selected).
fn item_to_select<'a>(dir: &Path, program: Option<&'a Path>) -> Option<&'a Path> {
    program.filter(|program| {
        program
            .parent()
            .is_some_and(|parent| folder_key(parent) == folder_key(dir))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn selects_only_a_program_directly_in_the_folder() {
        let dir = Path::new(r"C:\Program Files\EA Games\Dead Space (2023)");
        let exe = PathBuf::from(r"C:\Program Files\EA Games\Dead Space (2023)\Dead Space.exe");
        assert_eq!(item_to_select(dir, Some(&exe)), Some(exe.as_path()));
        // Letter case and a trailing backslash do not matter.
        let other_case = Path::new(r"c:\program files\ea games\dead space (2023)\");
        assert_eq!(item_to_select(other_case, Some(&exe)), Some(exe.as_path()));

        let witcher = Path::new(r"C:\GOG Games\The Witcher 3");
        let nested = PathBuf::from(r"C:\GOG Games\The Witcher 3\bin\x64_dx12\witcher3.exe");
        assert_eq!(item_to_select(witcher, Some(&nested)), None);
        assert_eq!(item_to_select(witcher, Some(&exe)), None);
        assert_eq!(item_to_select(witcher, None), None);
    }

    #[test]
    fn missing_folders_are_reported_without_opening_anything() {
        let missing = Path::new(r"C:\does\not\exist\Special K");
        let error = open_folder(missing, None).unwrap_err();
        assert!(matches!(&error, Error::NotFound(dir) if dir == missing));
        assert_eq!(
            error.to_string(),
            r"C:\does\not\exist\Special K does not exist"
        );
    }
}
