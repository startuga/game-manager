//! Save backups: ZIP copies of a game's saves folder (the one `game_data` finds on this PC), in
//! `Documents\Game Manager\Save backups\<game>\`, the newest `KEEP` of each game kept.
//!
//! - **Where**: Documents is Windows' known folder, wherever the user moved it (OneDrive). Each
//!   game gets a folder named after it once (`save-backups.json` in `%APPDATA%\Game Manager`, the
//!   characters Windows refuses left out, a number added when two games would share one), kept
//!   when the game is renamed.
//! - **A backup** is `YYYY-MM-DD HH-MM-SS.zip` on this PC's clock, the saves folder inside it
//!   under its own name, packed by the shipped 7-Zip into a `.part` file and renamed once whole,
//!   so one that stopped halfway never lists. A folder that is empty or over `LIMIT_GB` isn't
//!   packed. Folder links inside it aren't counted, and 7-Zip packs what they point to. Only
//!   files named as this module names them are ever pruned.
//! - **After each session** (`back_up_after_session`, on unless turned off for the game): the app
//!   backs up a game's saves when one of its sessions ends, only when a file in them is newer
//!   than the newest backup.
//! - **Restore**: the saves as they are now are backed up first ("before restore", `… before
//!   restore.zip`); the backup is unpacked beside the saves folder (`mods::scratch`, removed
//!   however it ends) and must hold one folder, which takes the saves folder's place through
//!   `<saves>.gm-old`, put back if the swap fails. The backup restored is never pruned by the one
//!   made before it. A game's store can bring its cloud copy back over a restore: the app says so
//!   when it asks.
//! - One backup or restore at a time (`WORK`), as a session's end and a click can meet.

use crate::local_time::{self, LocalTime};
use crate::{Error, Game, Result, game_data, json_file, mods, seven_zip, text};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

/// How many backups of a game are kept.
pub const KEEP: usize = 10;
/// The largest saves folder backed up.
const LIMIT_GB: u64 = 2;
const SETTINGS_FILE: &str = "save-backups.json";
const BEFORE_RESTORE: &str = " before restore";

/// One backup or restore at a time; the settings file is read and written under it too.
static WORK: Mutex<()> = Mutex::new(());

/// A backup of a game's saves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backup {
    pub file: PathBuf,
    /// When it was made, on this PC's clock (from its name).
    pub taken: LocalTime,
    pub size: u64,
    /// Made by a restore, of the saves it replaced.
    pub before_restore: bool,
}

/// A game's backups and their settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Backups {
    /// Where they are (it may not exist yet).
    pub folder: PathBuf,
    /// The newest first.
    pub list: Vec<Backup>,
    /// After each session.
    pub auto: bool,
}

/// `save-backups.json`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Settings {
    #[serde(default)]
    games: BTreeMap<String, GameEntry>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct GameEntry {
    /// The game's folder under the backups folder.
    folder: String,
    /// After each session; written only when off.
    #[serde(default = "on", skip_serializing_if = "is_on")]
    auto: bool,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

fn on() -> bool {
    true
}

fn is_on(value: &bool) -> bool {
    *value
}

/// Where backups and their settings go.
struct Places {
    root: PathBuf,
    settings: PathBuf,
}

impl Places {
    fn of_this_pc() -> Result<Self> {
        let documents = game_data::documents().ok_or(Error::NoDocumentsFolder)?;
        Ok(Self {
            root: documents.join("Game Manager").join("Save backups"),
            settings: json_file::user_file(SETTINGS_FILE)?,
        })
    }
}

/// The game's backups, newest first, where they go and whether they are made after each session.
/// Writes nothing. Blocking.
pub fn list(game: &Game) -> Result<Backups> {
    let _work = WORK.lock().unwrap_or_else(PoisonError::into_inner);
    list_in(&Places::of_this_pc()?, game)
}

/// Backs up the game's saves folder now. Blocking (7-Zip).
pub fn back_up(game: &Game, saves: &Path) -> Result<Backup> {
    let _work = WORK.lock().unwrap_or_else(PoisonError::into_inner);
    back_up_in(&Places::of_this_pc()?, game, saves, false, None)
}

/// After a session: backs up the game's saves when they are made after each session for it and
/// a file in them is newer than the newest backup; `None` otherwise. Blocking.
pub fn back_up_after_session(game: &Game, saves: &Path) -> Result<Option<Backup>> {
    let _work = WORK.lock().unwrap_or_else(PoisonError::into_inner);
    let places = Places::of_this_pc()?;
    if !entry(&places, game, false)?.auto {
        return Ok(None);
    }
    back_up_if_changed_in(&places, game, saves)
}

/// Whether the game's saves are backed up after each session (on unless turned off).
pub fn backs_up_after_sessions(game: &Game) -> Result<bool> {
    let _work = WORK.lock().unwrap_or_else(PoisonError::into_inner);
    Ok(entry(&Places::of_this_pc()?, game, false)?.auto)
}

/// Turns backing up after each session on or off for the game.
pub fn set_auto(game: &Game, auto: bool) -> Result<()> {
    let _work = WORK.lock().unwrap_or_else(PoisonError::into_inner);
    set_auto_in(&Places::of_this_pc()?, game, auto)
}

/// Puts a backup in place of the game's saves folder, the saves as they are first backed up
/// ("before restore"). Blocking (7-Zip).
pub fn restore(game: &Game, saves: &Path, backup: &Path) -> Result<()> {
    let _work = WORK.lock().unwrap_or_else(PoisonError::into_inner);
    restore_in(&Places::of_this_pc()?, game, saves, backup)
}

fn list_in(places: &Places, game: &Game) -> Result<Backups> {
    let entry = entry(places, game, false)?;
    let folder = places.root.join(&entry.folder);
    Ok(Backups {
        list: backups_in(&folder)?,
        folder,
        auto: entry.auto,
    })
}

/// The game's entry: the saved one, else one made from its name, saved when `keep`.
fn entry(places: &Places, game: &Game, keep: bool) -> Result<GameEntry> {
    let mut settings: Settings = json_file::load(&places.settings)?;
    if let Some(entry) = settings.games.get(&game.id) {
        return Ok(entry.clone());
    }
    let taken: Vec<&str> = settings
        .games
        .values()
        .map(|entry| entry.folder.as_str())
        .collect();
    let entry = GameEntry {
        folder: unique(text::file_name_for(&game.name, &game.id), &taken),
        auto: true,
        other: serde_json::Map::new(),
    };
    if keep {
        settings.games.insert(game.id.clone(), entry.clone());
        json_file::save(&places.settings, &settings)?;
    }
    Ok(entry)
}

fn set_auto_in(places: &Places, game: &Game, auto: bool) -> Result<()> {
    let entry = entry(places, game, true)?;
    let mut settings: Settings = json_file::load(&places.settings)?;
    settings
        .games
        .insert(game.id.clone(), GameEntry { auto, ..entry });
    json_file::save(&places.settings, &settings)
}

/// `base`, or `base 2`, `base 3`… when another game has it (Windows ignores case in names).
fn unique(base: String, taken: &[&str]) -> String {
    let free = |name: &str| !taken.iter().any(|other| other.eq_ignore_ascii_case(name));
    if free(&base) {
        return base;
    }
    let mut number = 2;
    loop {
        let name = format!("{base} {number}");
        if free(&name) {
            return name;
        }
        number += 1;
    }
}

/// A backup's file name: `2026-11-19 21-05-33.zip`, or `… before restore.zip`.
fn file_name(taken: LocalTime, before_restore: bool) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}-{:02}-{:02}{}.zip",
        taken.year,
        taken.month,
        taken.day,
        taken.hour,
        taken.minute,
        taken.second,
        if before_restore { BEFORE_RESTORE } else { "" }
    )
}

/// When a backup was taken and whether a restore made it, from its file name; `None` for a file
/// this module didn't name.
fn parse_name(name: &str) -> Option<(LocalTime, bool)> {
    let stem = name.strip_suffix(".zip")?;
    let (stamp, before_restore) = match stem.strip_suffix(BEFORE_RESTORE) {
        Some(stamp) => (stamp, true),
        None => (stem, false),
    };
    let bytes = stamp.as_bytes();
    let shape = bytes.len() == 19
        && bytes.iter().enumerate().all(|(i, &b)| match i {
            4 | 7 | 13 | 16 => b == b'-',
            10 => b == b' ',
            _ => b.is_ascii_digit(),
        });
    if !shape {
        return None;
    }
    let number = |range: std::ops::Range<usize>| stamp.get(range)?.parse::<u16>().ok();
    let small = |range| number(range).and_then(|n| u8::try_from(n).ok());
    Some((
        LocalTime {
            year: number(0..4)?,
            month: small(5..7)?,
            day: small(8..10)?,
            hour: small(11..13)?,
            minute: small(14..16)?,
            second: small(17..19)?,
        },
        before_restore,
    ))
}

/// The backups in a game's folder, newest first; none when it doesn't exist yet.
fn backups_in(folder: &Path) -> Result<Vec<Backup>> {
    let entries = match fs::read_dir(folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(Error::io(folder)(error)),
    };
    let mut backups: Vec<Backup> = entries
        .flatten()
        .filter_map(|entry| {
            let (taken, before_restore) = parse_name(&entry.file_name().to_string_lossy())?;
            let metadata = entry.metadata().ok().filter(fs::Metadata::is_file)?;
            Some(Backup {
                file: entry.path(),
                taken,
                size: metadata.len(),
                before_restore,
            })
        })
        .collect();
    backups.sort_by(|a, b| (b.taken, &b.file).cmp(&(a.taken, &a.file)));
    Ok(backups)
}

/// What a saves folder holds.
struct Contents {
    files: usize,
    bytes: u64,
    /// The latest a file in it was changed.
    newest: Option<SystemTime>,
}

/// The files under `folder`, folder links not followed.
fn contents(folder: &Path) -> Result<Contents> {
    if !folder.is_dir() {
        return Err(Error::NotFound(folder.to_path_buf()));
    }
    let mut found = Contents {
        files: 0,
        bytes: 0,
        newest: None,
    };
    let mut folders = vec![folder.to_path_buf()];
    while let Some(dir) = folders.pop() {
        for entry in fs::read_dir(&dir).map_err(Error::io(&dir))?.flatten() {
            let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                folders.push(entry.path());
            } else {
                found.files += 1;
                found.bytes += metadata.len();
                if let Ok(changed) = metadata.modified() {
                    found.newest = Some(found.newest.map_or(changed, |newest| newest.max(changed)));
                }
            }
        }
    }
    if found.files == 0 {
        return Err(Error::EmptyFolder(folder.to_path_buf()));
    }
    if found.bytes > LIMIT_GB << 30 {
        return Err(Error::FolderTooLarge {
            folder: folder.to_path_buf(),
            limit_gb: LIMIT_GB,
        });
    }
    Ok(found)
}

/// Packs `saves` into a new backup of the game, then prunes the oldest beyond `KEEP`, never
/// `keep`.
fn back_up_in(
    places: &Places,
    game: &Game,
    saves: &Path,
    before_restore: bool,
    keep: Option<&Path>,
) -> Result<Backup> {
    contents(saves)?;
    let entry = entry(places, game, true)?;
    let folder = places.root.join(&entry.folder);
    fs::create_dir_all(&folder).map_err(Error::io(&folder))?;
    let now = local_time::now();
    let taken = local_time::local(now).unwrap_or_else(|| local_time::utc(now));
    let name = file_name(taken, before_restore);
    let file = folder.join(&name);
    let part = folder.join(format!("{name}.part"));
    if let Err(error) = seven_zip::zip_folder(saves, &part) {
        let _ = fs::remove_file(&part);
        return Err(error);
    }
    fs::rename(&part, &file).map_err(Error::io(&file))?;
    prune(&folder, keep)?;
    let size = fs::metadata(&file).map_err(Error::io(&file))?.len();
    Ok(Backup {
        file,
        taken,
        size,
        before_restore,
    })
}

/// A new backup only when a file in `saves` is newer than the newest backup.
fn back_up_if_changed_in(places: &Places, game: &Game, saves: &Path) -> Result<Option<Backup>> {
    let newest_file = contents(saves)?.newest;
    let entry = entry(places, game, false)?;
    let newest_backup = backups_in(&places.root.join(&entry.folder))?
        .first()
        .and_then(|backup| fs::metadata(&backup.file).ok()?.modified().ok());
    if let (Some(file), Some(backup)) = (newest_file, newest_backup)
        && file <= backup
    {
        return Ok(None);
    }
    back_up_in(places, game, saves, false, None).map(Some)
}

/// Removes the oldest backups beyond `KEEP`, never `keep`.
fn prune(folder: &Path, keep: Option<&Path>) -> Result<()> {
    for old in backups_in(folder)?.iter().skip(KEEP) {
        if Some(old.file.as_path()) != keep {
            fs::remove_file(&old.file).map_err(Error::io(&old.file))?;
        }
    }
    Ok(())
}

/// `<saves><suffix>`, beside it.
fn beside(saves: &Path, suffix: &str) -> Result<PathBuf> {
    let name = saves
        .file_name()
        .ok_or_else(|| Error::NotFound(saves.to_path_buf()))?;
    let mut name = name.to_os_string();
    name.push(suffix);
    Ok(saves.with_file_name(name))
}

fn restore_in(places: &Places, game: &Game, saves: &Path, backup: &Path) -> Result<()> {
    if !backup.is_file() {
        return Err(Error::NotFound(backup.to_path_buf()));
    }
    let old = beside(saves, ".gm-old")?;
    if old.exists() {
        return Err(Error::InTheWay(old));
    }
    // The saves as they are first: a folder too large to back up isn't replaced.
    match contents(saves) {
        Ok(_) => {
            back_up_in(places, game, saves, true, Some(backup))?;
        }
        Err(Error::NotFound(_) | Error::EmptyFolder(_)) => {}
        Err(error) => return Err(error),
    }
    let unpacked = mods::scratch(beside(saves, ".gm-restore")?)?;
    seven_zip::extract(backup, &unpacked)?;
    let top = single_folder(&unpacked).ok_or_else(|| Error::NotInArchive {
        archive: backup.to_path_buf(),
        what: "saves folder",
    })?;
    let had = saves.exists();
    if had {
        fs::rename(saves, &old).map_err(Error::io(saves))?;
    }
    if let Err(error) = fs::rename(&top, saves) {
        if had {
            let _ = fs::rename(&old, saves);
        }
        return Err(Error::io(saves)(error));
    }
    if had {
        let _ = fs::remove_dir_all(&old);
    }
    Ok(())
}

/// The one folder in `dir`, when it holds exactly that.
fn single_folder(dir: &Path) -> Option<PathBuf> {
    let mut entries = fs::read_dir(dir).ok()?.flatten();
    let first = entries.next()?;
    (entries.next().is_none() && first.file_type().ok()?.is_dir()).then(|| first.path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Launch, Store};

    fn temp_dir(test: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("GM-SaveBackups-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn game(id: &str, name: &str) -> Game {
        Game {
            id: id.into(),
            name: name.into(),
            store: Store::Custom,
            install_dir: None,
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: None,
            arch: None,
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    fn places(dir: &Path) -> Places {
        Places {
            root: dir.join("Save backups"),
            settings: dir.join(SETTINGS_FILE),
        }
    }

    fn at(day: u8, second: u8) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 11,
            day,
            hour: 21,
            minute: 5,
            second,
        }
    }

    #[test]
    fn names_say_when_and_why() {
        assert_eq!(file_name(at(19, 3), false), "2026-11-19 21-05-03.zip");
        assert_eq!(
            file_name(at(19, 3), true),
            "2026-11-19 21-05-03 before restore.zip"
        );
        assert_eq!(
            parse_name("2026-11-19 21-05-03 before restore.zip"),
            Some((at(19, 3), true))
        );
        assert_eq!(
            parse_name("2026-11-19 21-05-03.zip"),
            Some((at(19, 3), false))
        );
        // Anything else in the folder is the user's: never listed, never pruned.
        for other in [
            "2026-11-19 21-05-03.zip.part",
            "notes.zip",
            "2026-11-19 21-05.zip",
            "2026-11-19T21-05-03.zip",
        ] {
            assert_eq!(parse_name(other), None, "{other}");
        }
    }

    #[test]
    fn folder_names_are_unique() {
        assert_eq!(unique("Doom".into(), &["doom", "Doom 2"]), "Doom 3");
        assert_eq!(unique("Quake".into(), &["doom"]), "Quake");
    }

    #[test]
    fn games_keep_their_folder_and_switch() {
        let dir = temp_dir("entries");
        let places = places(&dir);
        let first = game("steam:1", "Doom");
        let second = game("gog:1", "DOOM");
        // Listing writes nothing.
        assert_eq!(
            list_in(&places, &first).unwrap().folder,
            places.root.join("Doom")
        );
        assert!(!places.settings.exists());
        entry(&places, &first, true).unwrap();
        assert_eq!(entry(&places, &second, true).unwrap().folder, "DOOM 2");
        // A renamed game keeps its folder.
        let renamed = game("steam:1", "Doom (1993)");
        assert_eq!(
            list_in(&places, &renamed).unwrap().folder,
            places.root.join("Doom")
        );
        assert!(list_in(&places, &renamed).unwrap().auto);
        set_auto_in(&places, &renamed, false).unwrap();
        assert!(!list_in(&places, &first).unwrap().auto);
        let text = fs::read_to_string(&places.settings).unwrap();
        assert!(text.contains("\"auto\": false"), "{text}");
        set_auto_in(&places, &first, true).unwrap();
        assert!(
            !fs::read_to_string(&places.settings)
                .unwrap()
                .contains("auto")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn backs_up_keeps_ten_and_skips_unchanged_saves() {
        let dir = temp_dir("keep");
        let places = places(&dir);
        let saves = dir.join("Saves");
        fs::create_dir_all(saves.join("slot")).unwrap();
        fs::write(saves.join(r"slot\save1.sav"), b"level 1").unwrap();
        let game = game("custom:1", "Test Game");

        let backup = back_up_in(&places, &game, &saves, false, None).unwrap();
        assert!(backup.file.is_file() && backup.size > 0);
        assert_eq!(backup.file.parent().unwrap(), places.root.join("Test Game"));
        // Nothing changed since: no new backup after a session.
        assert_eq!(back_up_if_changed_in(&places, &game, &saves).unwrap(), None);

        // Older backups (made up, by name) and a file of the user's beside them.
        let folder = places.root.join("Test Game");
        for day in 1..=12 {
            fs::copy(&backup.file, folder.join(file_name(at(day, 0), false))).unwrap();
        }
        fs::write(folder.join("readme.txt"), b"mine").unwrap();
        prune(&folder, Some(&folder.join(file_name(at(1, 0), false)))).unwrap();
        let left = backups_in(&folder).unwrap();
        // The ten newest, and the oldest one kept as asked.
        assert_eq!(left.len(), KEEP + 1);
        assert_eq!(left.last().unwrap().taken, at(1, 0));
        assert_eq!(left[0].file, backup.file);
        assert!(folder.join("readme.txt").is_file());

        // An empty folder isn't backed up.
        let empty = dir.join("Empty");
        fs::create_dir_all(&empty).unwrap();
        assert!(matches!(
            back_up_in(&places, &game, &empty, false, None),
            Err(Error::EmptyFolder(_))
        ));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn restore_puts_a_backup_back_and_keeps_the_saves_it_replaced() {
        let dir = temp_dir("restore");
        let places = places(&dir);
        let saves = dir.join("My Games").join("Saves");
        fs::create_dir_all(&saves).unwrap();
        fs::write(saves.join("save1.sav"), b"level 1").unwrap();
        let game = game("custom:1", "Test Game");
        let backup = back_up_in(&places, &game, &saves, false, None).unwrap();

        fs::write(saves.join("save1.sav"), b"level 9").unwrap();
        fs::write(saves.join("save2.sav"), b"new").unwrap();
        restore_in(&places, &game, &saves, &backup.file).unwrap();
        assert_eq!(fs::read(saves.join("save1.sav")).unwrap(), b"level 1");
        assert!(!saves.join("save2.sav").exists());
        // Nothing left beside it.
        let beside: Vec<_> = fs::read_dir(dir.join("My Games"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(beside, ["Saves"]);
        // The saves it replaced, as a "before restore" backup.
        let list = list_in(&places, &game).unwrap().list;
        let before = list.iter().find(|backup| backup.before_restore).unwrap();
        let unpacked = dir.join("check");
        seven_zip::extract(&before.file, &unpacked).unwrap();
        assert_eq!(
            fs::read(unpacked.join(r"Saves\save1.sav")).unwrap(),
            b"level 9"
        );

        // A saves folder that isn't there any more is made again.
        fs::remove_dir_all(&saves).unwrap();
        restore_in(&places, &game, &saves, &backup.file).unwrap();
        assert_eq!(fs::read(saves.join("save1.sav")).unwrap(), b"level 1");

        // Something in the way, and an archive that isn't one folder: refused, nothing touched.
        let old = dir.join(r"My Games\Saves.gm-old");
        fs::create_dir_all(&old).unwrap();
        assert!(matches!(
            restore_in(&places, &game, &saves, &backup.file),
            Err(Error::InTheWay(_))
        ));
        fs::remove_dir_all(&old).unwrap();
        let loose = dir.join("loose");
        fs::create_dir_all(&loose).unwrap();
        fs::write(loose.join("a.sav"), b"a").unwrap();
        fs::write(loose.join("b.sav"), b"b").unwrap();
        let bad = dir.join("bad.zip");
        seven_zip::tests::seven_zip(
            &loose,
            &["a", "-tzip", bad.to_str().unwrap(), "a.sav", "b.sav"],
        );
        assert!(matches!(
            restore_in(&places, &game, &saves, &bad),
            Err(Error::NotInArchive { .. })
        ));
        assert_eq!(fs::read(saves.join("save1.sav")).unwrap(), b"level 1");
        assert!(!dir.join(r"My Games\Saves.gm-restore").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
