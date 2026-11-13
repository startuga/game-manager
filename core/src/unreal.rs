//! Unreal Engine's packaged games: a small launcher at the game's root, and the program the game
//! runs as in `<Project>\Binaries\<Platform>`.
//!
//! Unreal's packaging copies its `BootstrapPackagedGame` program (a few hundred KB) to the game's
//! root under the game's name, beside the `Engine` folder, and writes into it, as `RCDATA`
//! resources, the real program's path relative to the launcher's folder (201, `IDI_EXEC_FILE`,
//! UTF-16) and its arguments (202, the project's folder). The launcher installs the prerequisites
//! and starts that program.
//!
//! Stores' launches and icons name the launcher, but mods (ReShade, DXVK, the upscalers), the
//! graphics API and the NVIDIA profile need the program the game runs as. `packaged_program`
//! reads it from the launcher, mapped as data (none of its code runs). Nothing is guessed from
//! names: on the five Unreal games of the PC this was written on, `GWT.exe` started
//! `Snowfall\Binaries\Win64\GWT.exe` and `HogwartsLegacy.exe`
//! `Phoenix\Binaries\Win64\HogwartsLegacy.exe`, and only two of the five were `*-Shipping.exe`.
//! Unreal Engine 3 games have no such launcher.

use crate::data_module::{DataModule, int_resource};
use std::path::{Component, Path, PathBuf};

/// `RT_RCDATA`, and the resource holding the program the launcher starts.
const RT_RCDATA: u16 = 10;
const EXEC_FILE: u16 = 201;

/// The program an Unreal launcher starts, when `exe` is one (beside an `Engine` folder, with the
/// resource) and the program it names is there. `None` for any other program. Blocking.
pub(crate) fn packaged_program(exe: &Path) -> Option<PathBuf> {
    let folder = exe.parent()?;
    if !folder.join("Engine").is_dir() {
        return None;
    }
    let module = DataModule::load(exe)?;
    let bytes = module.resource(int_resource(EXEC_FILE), int_resource(RT_RCDATA))?;
    let program = folder.join(relative_program(bytes)?);
    let is_exe = program
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"));
    (is_exe && program.is_file()).then_some(program)
}

/// The launcher's resource as a path inside its folder: UTF-16, NULs and spaces trimmed, `/`
/// taken as `\`. `None` when it is empty, absolute or climbs out with `..`.
fn relative_program(bytes: &[u8]) -> Option<PathBuf> {
    let (pairs, rest) = bytes.as_chunks::<2>();
    if !rest.is_empty() {
        return None;
    }
    let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
    let text = String::from_utf16(&units).ok()?;
    let text = text.trim_matches(['\0', ' ']).replace('/', "\\");
    let path = PathBuf::from(text);
    let inside = path.components().next().is_some()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    inside.then_some(path)
}

/// Builds an Unreal launcher for tests: a copy of a small Windows program at `launcher`, with
/// `resource` written into it as `RCDATA 201`.
#[cfg(test)]
pub(crate) fn make_launcher(launcher: &Path, resource: &[u8]) {
    use windows::Win32::{BeginUpdateResourceW, EndUpdateResourceW, UpdateResourceW};
    use windows::core::HSTRING;
    let system =
        PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join(r"System32\hostname.exe");
    std::fs::copy(system, launcher).unwrap();
    let path = HSTRING::from(launcher.as_os_str());
    // SAFETY: `path` is a valid wide string; the existing resources are kept.
    let update = unsafe { BeginUpdateResourceW(&path, false) };
    assert!(!update.is_null(), "BeginUpdateResourceW");
    // SAFETY: `update` came from BeginUpdateResourceW; `resource` lives for the call, which copies
    // its `len` bytes.
    let added = unsafe {
        UpdateResourceW(
            update,
            int_resource(RT_RCDATA),
            int_resource(EXEC_FILE),
            0,
            Some(resource.as_ptr().cast_mut().cast()),
            resource.len() as u32,
        )
    };
    assert!(added.as_bool(), "UpdateResourceW");
    // SAFETY: ends the update begun above, writing it to the file.
    let written = unsafe { EndUpdateResourceW(update, false) };
    assert!(written.as_bool(), "EndUpdateResourceW");
}

/// `text` as the launcher stores it: UTF-16 LE with its terminating NUL.
#[cfg(test)]
pub(crate) fn wide(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn the_resource_is_a_path_inside_the_launchers_folder() {
        let expected = PathBuf::from(r"Sifu\Binaries\Win64\Sifu-Win64-Shipping.exe");
        assert_eq!(
            relative_program(&wide(r"Sifu\Binaries\Win64\Sifu-Win64-Shipping.exe")),
            Some(expected.clone())
        );
        // NUL padding, spaces and forward slashes.
        let mut padded = wide(" Sifu/Binaries/Win64/Sifu-Win64-Shipping.exe ");
        padded.extend([0, 0, 0, 0]);
        assert_eq!(relative_program(&padded), Some(expected));
        // Nothing, outside the folder, or not UTF-16.
        for refused in [
            wide(""),
            wide(r"C:\Windows\notepad.exe"),
            wide(r"\Windows\notepad.exe"),
            wide(r"..\Other\Game.exe"),
            wide(r"Game\..\..\Game.exe"),
        ] {
            assert_eq!(relative_program(&refused), None, "{refused:?}");
        }
        assert_eq!(relative_program(&[0x41, 0x00, 0x42]), None);
        assert_eq!(relative_program(&[0x00, 0xD8]), None);
    }

    #[test]
    fn a_launcher_names_the_program_the_game_runs_as() {
        let dir = std::env::temp_dir().join(format!("GM-Unreal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let binaries = dir.join(r"Phoenix\Binaries\Win64");
        fs::create_dir_all(&binaries).unwrap();
        fs::create_dir_all(dir.join("Engine")).unwrap();
        let real = binaries.join("HogwartsLegacy.exe");
        fs::write(&real, b"MZ").unwrap();
        let launcher = dir.join("HogwartsLegacy.exe");
        make_launcher(
            &launcher,
            &wide(r"Phoenix\Binaries\Win64\HogwartsLegacy.exe"),
        );
        assert_eq!(packaged_program(&launcher), Some(real.clone()));

        // Without the `Engine` folder beside it, it isn't looked into.
        fs::remove_dir(dir.join("Engine")).unwrap();
        assert_eq!(packaged_program(&launcher), None);
        fs::create_dir(dir.join("Engine")).unwrap();
        // The program it names is gone.
        fs::remove_file(&real).unwrap();
        assert_eq!(packaged_program(&launcher), None);
        // It names something that isn't a program.
        let other = dir.join("Other.exe");
        fs::write(binaries.join("readme.txt"), b"text").unwrap();
        make_launcher(&other, &wide(r"Phoenix\Binaries\Win64\readme.txt"));
        assert_eq!(packaged_program(&other), None);
        // An ordinary program, and no file at all.
        let plain = dir.join("Plain.exe");
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        fs::copy(system.join(r"System32\hostname.exe"), &plain).unwrap();
        assert_eq!(packaged_program(&plain), None);
        assert_eq!(packaged_program(&dir.join("Missing.exe")), None);
        fs::remove_dir_all(&dir).unwrap();
    }
}
