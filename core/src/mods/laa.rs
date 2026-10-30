//! Large Address Aware (LAA): a 32-bit program marked so in its PE header may use 4 GB of memory
//! on 64-bit Windows instead of 2 GB. Game Manager sets the flag in a copy of the game's program
//! (`pe::set_large_address_aware`, which keeps the checksum valid) and swaps it in with the mods
//! machinery: the original is kept as `<program>.gm-backup` and `mods.json` records the patched
//! file, so ✕ puts the original back byte for byte. Only for 32-bit x86 programs.
//!
//! Programs wrapped by Steam's DRM have a `.bind` section; a changed one may not start, which the
//! app warns about. A game update replaces the patched program: the backup of the old version is
//! then stale, and enabling LAA again or ✕ removes it.

use super::{
    BACKUP_SUFFIX, FileOp, Installed, InstalledFile, ModsFile, apply, plan, update, with_suffix,
};
use crate::mods::dxvk::Blocked;
use crate::{Error, Game, Result, json_file, pe};
use std::fs;
use std::path::{Path, PathBuf};

/// What the Info dialog says about LAA.
pub const LAA_INFO: &str = "Large Address Aware (LAA) lets a 32-bit game use up to 4 GB of \
memory on 64-bit Windows instead of 2 GB. It helps against out-of-memory crashes in games with \
DXVK, ReShade, texture packs or many mods.

Enable LAA sets this flag in the game's program and keeps the original next to it as \
<program>.gm-backup; ✕ puts it back.

⚠ The program's digital signature no longer matches.
⚠ Steam's DRM wrapper or anti-cheat may refuse a changed program.
⚠ A game update puts an unpatched program back: enable LAA again then.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaaStatus {
    /// Not Large Address Aware.
    Off,
    /// Patched by Game Manager, and still the patched file.
    Patched,
    /// Large Address Aware as its developer built it.
    OnByDeveloper,
    /// Game Manager patched it, but the game has replaced the program since (an update).
    Updated,
}

/// A game's program and its LAA status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laa {
    /// The program's file name.
    pub exe: String,
    pub status: LaaStatus,
    /// The program is wrapped by Steam's DRM (a `.bind` section): a patched copy may not start.
    pub steam_drm: bool,
}

/// The game's LAA status. Blocking: reads the program's headers, and the whole file when Game
/// Manager patched it.
pub fn status(game: &Game, mods: &ModsFile) -> Result<Laa> {
    status_of(&program(game)?, mods.laa(&game.id))
}

/// Marks the game's program Large Address Aware, keeping the original as its backup, and returns
/// `mods.json` as saved. Blocking: reads and copies the program (as administrator when its folder
/// needs it).
pub fn enable(game: &Game) -> Result<ModsFile> {
    let program = program(game)?;
    let mods = super::load()?;
    let record = mods.laa(&game.id);
    if record.is_some_and(is_patched) {
        return Ok(mods);
    }
    let staging = json_file::local_file("mods")?.join("staging");
    let (ops, installed) = enable_ops(&program, record, &staging)?;
    apply(&ops)?;
    update(&game.id, |entry| entry.laa = Some(installed))
}

/// Puts the original program back (or, after an update, removes the stale backup) and returns
/// `mods.json` as saved.
pub fn restore(game: &Game) -> Result<ModsFile> {
    let mods = super::load()?;
    if let Some(record) = mods.laa(&game.id) {
        apply(&restore_ops(record)?)?;
    }
    update(&game.id, |entry| entry.laa = None)
}

fn program(game: &Game) -> Result<PathBuf> {
    game.program
        .clone()
        .ok_or_else(|| Error::NotInstallable(Blocked::NoProgram.to_string()))
}

fn file_name(program: &Path) -> String {
    program
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn not_a_program(exe: &str) -> Error {
    Error::NotInstallable(format!("{exe} is not a program Game Manager can read."))
}

fn status_of(program: &Path, record: Option<&Installed>) -> Result<Laa> {
    let exe = file_name(program);
    let on = pe::large_address_aware(program).ok_or_else(|| not_a_program(&exe))?;
    let status = match record {
        Some(record) if is_patched(record) => LaaStatus::Patched,
        Some(_) => LaaStatus::Updated,
        None if on => LaaStatus::OnByDeveloper,
        None => LaaStatus::Off,
    };
    let steam_drm = pe::section_names(program)
        .unwrap_or_default()
        .iter()
        .any(|name| name == ".bind");
    Ok(Laa {
        exe,
        status,
        steam_drm,
    })
}

/// The patched program is still in place.
fn is_patched(record: &Installed) -> bool {
    record
        .files
        .first()
        .is_some_and(|file| file.is_at(&record.folder.join(&file.name)))
}

/// The record's backups that are still there.
fn backups(record: &Installed) -> Vec<PathBuf> {
    record
        .backups
        .iter()
        .map(|name| with_suffix(&record.folder.join(name), BACKUP_SUFFIX))
        .filter(|path| path.exists())
        .collect()
}

/// The operations that swap in a patched copy of `program` (written to `staging`), and the record
/// of it. `record` is an earlier patch the game has replaced since: its backup goes.
fn enable_ops(
    program: &Path,
    record: Option<&Installed>,
    staging: &Path,
) -> Result<(Vec<FileOp>, Installed)> {
    let exe = file_name(program);
    let folder = program.parent().ok_or_else(|| not_a_program(&exe))?;
    let mut bytes = fs::read(program).map_err(Error::io(program))?;
    pe::set_large_address_aware(&mut bytes)
        .ok_or_else(|| Error::NotInstallable(format!("{exe} is not a 32-bit x86 program.")))?;
    fs::create_dir_all(staging).map_err(Error::io(staging))?;
    let staged = staging.join(&exe);
    fs::write(&staged, &bytes).map_err(Error::io(&staged))?;

    let stale = record.map(backups).unwrap_or_default();
    let backup = with_suffix(program, BACKUP_SUFFIX);
    if backup.exists() && !stale.contains(&backup) {
        return Err(Error::NotInstallable(format!(
            "{} already exists: put it back or delete it first.",
            backup.display()
        )));
    }
    let mut ops: Vec<FileOp> = stale
        .into_iter()
        .map(|path| FileOp::Delete { path })
        .collect();
    ops.push(FileOp::Move {
        from: program.to_path_buf(),
        to: backup,
    });
    ops.push(FileOp::Copy {
        from: staged.clone(),
        to: program.to_path_buf(),
    });
    let installed = Installed {
        version: "LAA".to_owned(),
        page: String::new(),
        folder: folder.to_path_buf(),
        files: vec![InstalledFile::of(&exe, &staged)?],
        backups: vec![exe],
    };
    Ok((ops, installed))
}

fn restore_ops(record: &Installed) -> Result<Vec<FileOp>> {
    if is_patched(record) {
        return Ok(plan(Some(record), &record.folder, &[])?.ops);
    }
    // An update replaced the patched program: it is the game's now, only the old backup goes.
    Ok(backups(record)
        .into_iter()
        .map(|path| FileOp::Delete { path })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::run;
    use std::collections::BTreeMap;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Laa-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn listing(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().is_file())
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                (name, fs::read(entry.path()).unwrap())
            })
            .collect()
    }

    /// A 32-bit program that is not Large Address Aware: Windows' own Notepad, copied.
    fn game_program(dir: &Path) -> Option<PathBuf> {
        let windows = PathBuf::from(std::env::var_os("SystemRoot")?);
        let notepad = windows.join(r"SysWOW64\notepad.exe");
        if pe::large_address_aware(&notepad) != Some(false) {
            return None;
        }
        let program = dir.join("game").join("game.exe");
        fs::create_dir_all(program.parent()?).ok()?;
        fs::copy(&notepad, &program).ok()?;
        Some(program)
    }

    #[test]
    fn patches_and_puts_the_original_back() {
        let dir = temp_dir("patch");
        let Some(program) = game_program(&dir) else {
            return;
        };
        let (game, staging) = (dir.join("game"), dir.join("staging"));
        let before = listing(&game);
        assert_eq!(status_of(&program, None).unwrap().status, LaaStatus::Off);

        let (ops, record) = enable_ops(&program, None, &staging).unwrap();
        run(&ops).unwrap();
        assert_eq!(pe::large_address_aware(&program), Some(true));
        assert_eq!(
            fs::read(game.join("game.exe.gm-backup")).unwrap(),
            before["game.exe"]
        );
        let laa = status_of(&program, Some(&record)).unwrap();
        assert_eq!(
            (laa.exe.as_str(), laa.status),
            ("game.exe", LaaStatus::Patched)
        );
        assert!(!laa.steam_drm);

        run(&restore_ops(&record).unwrap()).unwrap();
        assert_eq!(listing(&game), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_update_leaves_a_stale_backup_that_goes() {
        let dir = temp_dir("update");
        let Some(program) = game_program(&dir) else {
            return;
        };
        let (game, staging) = (dir.join("game"), dir.join("staging"));
        let (ops, record) = enable_ops(&program, None, &staging).unwrap();
        run(&ops).unwrap();

        // The game updates its program (unpatched, and different from the old one).
        let mut updated = fs::read(game.join("game.exe.gm-backup")).unwrap();
        updated.extend_from_slice(b"version 2");
        fs::write(&program, &updated).unwrap();
        assert_eq!(
            status_of(&program, Some(&record)).unwrap().status,
            LaaStatus::Updated
        );

        // Enabling again replaces the stale backup with the new version.
        let (ops, again) = enable_ops(&program, Some(&record), &staging).unwrap();
        run(&ops).unwrap();
        assert_eq!(fs::read(game.join("game.exe.gm-backup")).unwrap(), updated);
        assert_eq!(
            status_of(&program, Some(&again)).unwrap().status,
            LaaStatus::Patched
        );

        // Another update, then ✕: the program stays, only the stale backup goes.
        fs::write(&program, b"version 3").unwrap();
        run(&restore_ops(&again).unwrap()).unwrap();
        assert_eq!(fs::read(&program).unwrap(), b"version 3");
        assert!(!game.join("game.exe.gm-backup").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_other_programs_and_foreign_backups() {
        let dir = temp_dir("refuse");
        let Some(program) = game_program(&dir) else {
            return;
        };
        let staging = dir.join("staging");
        fs::write(dir.join(r"game\game.exe.gm-backup"), b"someone else's").unwrap();
        let error = enable_ops(&program, None, &staging).unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");

        let text = dir.join(r"game\readme.exe");
        fs::write(&text, b"not a program").unwrap();
        assert!(status_of(&text, None).is_err());
        assert!(enable_ops(&text, None, &staging).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
