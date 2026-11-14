//! Mods installed into games' folders. A mod's files are copied next to the game's program by a
//! list of `FileOp`s, which `apply` runs itself, or through a copy of Game Manager running as
//! administrator when the folder is protected (`elevate`). `%APPDATA%\Game Manager\mods.json`
//! keeps, per game, each mod's settings and what was installed (the files with their size and hash,
//! and the game's own files set aside as `<name>.gm-backup`), so uninstalling removes exactly that
//! and puts the game's files back. Reset to detected in Game settings does not touch it.
//!
//! A run either does all its operations or none: a failure undoes those already done. Files to
//! delete are first set aside as `<name>.gm-delete` and only removed, with the folders that end
//! up empty, once everything succeeded. Folders of many files (ReShade's shaders) travel as one
//! `CopyTree` or `DeleteFiles`, so an elevated run's command line stays short.
//!
//! **As administrator** (`--file-ops`, and ReShade's `--reshade-ops`), the operations are
//! checked before any runs (`vet`): every path they read or change, the files a `CopyTree`
//! writes and each `DeleteFiles` name too, must be one work may reach (`elevate::check_paths`:
//! a whole local path, outside `%SystemRoot%`, through no junction a standard user made); a
//! `CopyTree`'s source may hold no junction or symbolic link; and `Writable` opens only
//! ReShade's own saved files (`ReShade.ini`, `ReShadePreset.ini`, `ReShade.log`) to the user.
//! Anything else and nothing runs: the exit code says why.
//!
//! **What it copies** is what Game Manager planned: the sources (the caches and staging in
//! `%LOCALAPPDATA%`, which a standard user can change) travel with their SHA-256, read before the
//! UAC prompt (`SourceHashes`). As administrator each source is read once and written from those
//! bytes only when they still have it, so a file swapped while Windows asked is copied nowhere:
//! that copy fails with `ERROR_DATA_CHECKSUM_ERROR` and the run is undone.

pub mod dlss_override;
pub mod dxvk;
pub mod laa;
pub mod reshade;
pub mod reshade_addons;
pub mod reshade_layer;
pub mod reshade_settings;
pub mod shader_packs;
pub mod upscalers;

pub use crate::elevate::FILE_OPS_ARG;
use crate::text::{fnv1a, percent_decode, percent_encode};
use crate::{Error, Result, crypto, elevate, http, json_file, security};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use windows::Win32::{
    E_FAIL, E_INVALIDARG, ERROR_ALREADY_EXISTS, ERROR_DATA_CHECKSUM_ERROR, ERROR_NOT_SUPPORTED,
    ERROR_REPARSE_POINT_ENCOUNTERED,
};
use windows::core::WIN32_ERROR;

const MODS_FILE: &str = "mods.json";
/// A game's file that a mod replaced is kept under its name with this suffix.
const BACKUP_SUFFIX: &str = ".gm-backup";
/// A file being deleted is first set aside under this suffix, so a later failure can put it back.
const ASIDE_SUFFIX: &str = ".gm-delete";
/// One change of `mods.json` at a time, so none loses another's.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// `mods.json`: each game's mods, by game id. Fields this version does not know are kept.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModsFile {
    #[serde(default)]
    pub games: BTreeMap<String, GameMods>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl ModsFile {
    /// The game's DXVK settings and installation, if any.
    pub fn dxvk(&self, id: &str) -> Option<&dxvk::DxvkState> {
        self.games.get(id)?.dxvk.as_ref()
    }

    /// The game's program as Game Manager patched it Large Address Aware, if it did.
    pub fn laa(&self, id: &str) -> Option<&Installed> {
        self.games.get(id)?.laa.as_ref()
    }

    /// The game's ReShade settings and installation, if any.
    pub fn reshade(&self, id: &str) -> Option<&reshade::ReshadeState> {
        self.games.get(id)?.reshade.as_ref()
    }

    /// The game's upscaler DLLs Game Manager swapped, one per place.
    pub fn upscalers(&self, id: &str) -> &[upscalers::Swap] {
        self.games.get(id).map_or(&[], |entry| &entry.upscalers)
    }

    /// The game's NVIDIA DLSS overrides, while Game Manager has set some.
    pub fn dlss_override(&self, id: &str) -> Option<&dlss_override::DlssOverride> {
        self.games.get(id)?.dlss_override.as_ref()
    }
}

/// One game's mods.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GameMods {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dxvk: Option<dxvk::DxvkState>,
    /// The program patched Large Address Aware, and its original kept as a backup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub laa: Option<Installed>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reshade: Option<reshade::ReshadeState>,
    /// Upscaler DLLs swapped for another version, one per place in the game's folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upscalers: Vec<upscalers::Swap>,
    /// NVIDIA's DLSS overrides in the game's driver profile, with what it held before.
    #[serde(
        default,
        rename = "dlssOverride",
        skip_serializing_if = "Option::is_none"
    )]
    pub dlss_override: Option<dlss_override::DlssOverride>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

impl GameMods {
    fn is_empty(&self) -> bool {
        self.dxvk.is_none()
            && self.laa.is_none()
            && self.reshade.is_none()
            && self.upscalers.is_empty()
            && self.dlss_override.is_none()
            && self.other.is_empty()
    }
}

/// What a mod put into a game's folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Installed {
    pub version: String,
    /// The web page of the build that was downloaded.
    pub page: String,
    /// The folder the files were copied into.
    pub folder: PathBuf,
    /// The files copied there; only those still unchanged are removed again.
    #[serde(default)]
    pub files: Vec<InstalledFile>,
    /// The game's files that were set aside as `<name>.gm-backup`.
    #[serde(default)]
    pub backups: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    pub name: String,
    pub size: u64,
    /// FNV-1a of the contents, in hex.
    pub hash: String,
}

impl InstalledFile {
    fn of(name: &str, path: &Path) -> Result<Self> {
        let bytes = fs::read(path).map_err(Error::io(path))?;
        Ok(Self {
            name: name.to_owned(),
            size: bytes.len() as u64,
            hash: hash(&bytes),
        })
    }

    /// An empty file, as `FileOp::Writable` creates.
    fn empty(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            size: 0,
            hash: hash(&[]),
        }
    }

    /// Whether `path` still holds this file, as it was copied.
    fn is_at(&self, path: &Path) -> bool {
        fs::read(path)
            .is_ok_and(|bytes| bytes.len() as u64 == self.size && hash(&bytes) == self.hash)
    }
}

fn hash(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a(bytes))
}

/// A mod build to download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// The version shown, which names the build in the cache: `3.1.1`, or `master-0cfe089` for a
    /// development build.
    pub version: String,
    /// The release page, or the commit it was built from.
    pub page: String,
    pub url: String,
    pub file_name: String,
    /// For a development build (a CI run's artifact, through nightly.link): the SHA-256 GitHub
    /// lists for it, which the download must have (`check_download`).
    pub sha256: Option<String>,
}

/// A run's artifact that has not expired and whose name `wanted` takes: its name and the
/// SHA-256 GitHub lists for it (`digest`, `sha256:<hex>`; `None` when it lists none).
fn run_artifact(
    artifacts: &serde_json::Value,
    wanted: impl Fn(&str) -> bool,
) -> Option<(String, Option<String>)> {
    artifacts["artifacts"]
        .as_array()?
        .iter()
        .filter(|artifact| !artifact["expired"].as_bool().unwrap_or(false))
        .find(|artifact| artifact["name"].as_str().is_some_and(&wanted))
        .map(|artifact| {
            let digest = artifact["digest"]
                .as_str()
                .and_then(|digest| digest.strip_prefix("sha256:"))
                .filter(|hex| crypto::is_sha256(hex))
                .map(str::to_ascii_lowercase);
            (
                artifact["name"].as_str().unwrap_or_default().to_owned(),
                digest,
            )
        })
}

/// Refuses a downloaded `file` that is not the one `build` names: a development build whose
/// SHA-256 isn't the one GitHub lists. Releases come from their projects' own servers over HTTPS.
/// Blocking: reads the file.
fn check_download(build: &Build, file: &Path) -> Result<()> {
    match &build.sha256 {
        Some(expected) if crypto::sha256_file(file)? != *expected => Err(Error::DigestMismatch {
            url: build.url.clone(),
        }),
        _ => Ok(()),
    }
}

/// `GET url` as JSON (GitHub's API).
fn get_json(url: &str) -> Result<serde_json::Value> {
    serde_json::from_slice(&http::get(url)?).map_err(|source| Error::WebJson {
        url: url.to_owned(),
        source,
    })
}

/// A GitHub Actions workflow's latest run, from its `runs` listing: its id and commit.
fn latest_run(runs: &serde_json::Value) -> Option<(u64, String)> {
    let run = &runs["workflow_runs"][0];
    Some((run["id"].as_u64()?, run["head_sha"].as_str()?.to_owned()))
}

/// The process exit code for an elevated run's outcome: 0, or the HRESULT of what failed.
fn exit_code(outcome: Result<()>) -> u32 {
    match outcome {
        Ok(()) => 0,
        Err(Error::Io { source, .. }) => source.raw_os_error().map_or(E_FAIL.0 as u32, |code| {
            WIN32_ERROR(code as u32).to_hresult().0 as u32
        }),
        Err(Error::Windows { source, .. }) => source.code().0 as u32,
        Err(_) => E_FAIL.0 as u32,
    }
}

/// The user's `mods.json`; empty when there is none yet.
pub fn load() -> Result<ModsFile> {
    json_file::load(&json_file::user_file(MODS_FILE)?)
}

/// Changes one game's entry in `mods.json`, dropping it once nothing is left, and returns the file
/// as saved.
fn update(id: &str, change: impl FnOnce(&mut GameMods)) -> Result<ModsFile> {
    update_in(&json_file::user_file(MODS_FILE)?, id, change)
}

fn update_in(file: &Path, id: &str, change: impl FnOnce(&mut GameMods)) -> Result<ModsFile> {
    let _lock = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    // Loading first also means a file that cannot be read is never overwritten.
    let mut mods: ModsFile = json_file::load(file)?;
    let entry = mods.games.entry(id.to_owned()).or_default();
    change(entry);
    if entry.is_empty() {
        mods.games.remove(id);
    }
    json_file::save(file, &mods)?;
    Ok(mods)
}

/// One change to a game's folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum FileOp {
    /// Copies `from` to `to`, which must not exist.
    Copy {
        from: PathBuf,
        to: PathBuf,
    },
    /// Renames `from` to `to`, which must not exist.
    Move {
        from: PathBuf,
        to: PathBuf,
    },
    Delete {
        path: PathBuf,
    },
    /// Copies every file under `from` to the same place under `to`, creating folders; none of
    /// the files may exist there yet. One operation for a whole folder of files (ReShade's
    /// shaders), so the command line of an elevated run stays short.
    CopyTree {
        from: PathBuf,
        to: PathBuf,
    },
    /// Deletes each `folder\<name>` that exists (names may hold subfolders), then the folders
    /// below `folder` that this leaves empty.
    DeleteFiles {
        folder: PathBuf,
        names: Vec<String>,
    },
    /// Deletes the folder `path` with everything in it.
    DeleteTree {
        path: PathBuf,
    },
    /// Creates `path` (empty) when it is missing and lets the user `sid` change it: ReShade's
    /// files in a protected folder, which ReShade saves into while the game runs.
    Writable {
        path: PathBuf,
        sid: String,
    },
}

/// The operations that replace `previous` (a mod's files in a game's folder, if any) by `files`
/// (name in the folder, source file) in `folder`, with the record of what they install. The game's
/// files in the way are set aside as backups, and `previous`'s backups are put back.
pub(crate) struct Plan {
    pub ops: Vec<FileOp>,
    pub files: Vec<InstalledFile>,
    pub backups: Vec<String>,
}

pub(crate) fn plan(
    previous: Option<&Installed>,
    folder: &Path,
    files: &[(String, PathBuf)],
) -> Result<Plan> {
    let key = |path: &Path| path.to_string_lossy().to_lowercase();
    let mut ops = Vec::new();
    let mut removed = HashSet::new();
    let mut restored = HashSet::new();
    if let Some(previous) = previous {
        for file in &previous.files {
            let path = previous.folder.join(&file.name);
            // A file the game has replaced since is the game's now.
            if file.is_at(&path) {
                removed.insert(key(&path));
                ops.push(FileOp::Delete { path });
            }
        }
        for name in &previous.backups {
            let path = previous.folder.join(name);
            let backup = with_suffix(&path, BACKUP_SUFFIX);
            if backup.exists() && (!path.exists() || removed.contains(&key(&path))) {
                restored.insert(key(&path));
                ops.push(FileOp::Move {
                    from: backup,
                    to: path,
                });
            }
        }
    }
    let mut installed = Vec::new();
    let mut backups = Vec::new();
    for (name, source) in files {
        let path = folder.join(name);
        let restored_here = restored.contains(&key(&path));
        if (path.exists() && !removed.contains(&key(&path))) || restored_here {
            let backup = with_suffix(&path, BACKUP_SUFFIX);
            // A restored file's backup was just moved back; any other is not ours to replace.
            if backup.exists() && !restored_here {
                return Err(Error::NotInstallable(format!(
                    "{} already exists: put it back or delete it first.",
                    backup.display()
                )));
            }
            ops.push(FileOp::Move {
                from: path.clone(),
                to: backup,
            });
            backups.push(name.clone());
        }
        installed.push(InstalledFile::of(name, source)?);
        ops.push(FileOp::Copy {
            from: source.clone(),
            to: path,
        });
    }
    Ok(Plan {
        ops,
        files: installed,
        backups,
    })
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

/// Runs `ops`: here, or as administrator when Game Manager may not write in one of the folders
/// they change, with their sources' SHA-256 (see the module's doc).
pub(crate) fn apply(ops: &[FileOp]) -> Result<()> {
    if all_writable(ops)? {
        run(ops)
    } else {
        let work = ElevatedOps {
            ops: ops.to_vec(),
            sources: SourceHashes::of(ops)?,
        };
        let json = serde_json::to_string(&work).map_err(Error::json("file operations"))?;
        elevate::run(FILE_OPS_ARG, &percent_encode(&json))
    }
}

/// Operations run as administrator (`--file-ops`), with the SHA-256 of every file they copy.
#[derive(Debug, Serialize, Deserialize)]
struct ElevatedOps {
    ops: Vec<FileOp>,
    sources: SourceHashes,
}

/// The SHA-256 (hex) of each file operations copy, by its path, as Game Manager read them before
/// asking for administrator rights (see the module's doc).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct SourceHashes(BTreeMap<PathBuf, String>);

impl SourceHashes {
    /// The sources of `ops`: each `Copy`'s, and every file under each `CopyTree`'s. Blocking:
    /// reads them.
    pub(crate) fn of(ops: &[FileOp]) -> Result<Self> {
        let mut sources = Self::default();
        for op in ops {
            match op {
                FileOp::Copy { from, .. } => sources.add(from)?,
                FileOp::CopyTree { from, .. } => {
                    for relative in files_under(from)? {
                        sources.add(&from.join(relative))?;
                    }
                }
                _ => {}
            }
        }
        Ok(sources)
    }

    /// Notes `file`'s SHA-256. Blocking: reads it.
    pub(crate) fn add(&mut self, file: &Path) -> Result<()> {
        let digest = crypto::sha256_file(file)?;
        self.0.insert(file.to_path_buf(), digest);
        Ok(())
    }

    /// Writes `from`'s bytes as `to` (replacing it), read once and only when they have the
    /// SHA-256 noted for `from`. A source not noted, or changed since, is refused with
    /// `ERROR_DATA_CHECKSUM_ERROR` and nothing is written.
    fn copy(&self, from: &Path, to: &Path) -> Result<()> {
        let changed = || Error::io(from)(io::Error::from_raw_os_error(ERROR_DATA_CHECKSUM_ERROR));
        let expected = self.0.get(from).ok_or_else(changed)?;
        let bytes = fs::read(from).map_err(Error::io(from))?;
        if crypto::hex(&crypto::sha256(&bytes)?) != *expected {
            return Err(changed());
        }
        fs::write(to, &bytes).map_err(Error::io(to))
    }
}

/// Copies `from` to `to`, replacing it: as it is, or with `sources` (as administrator) only when
/// it still has the SHA-256 noted for it.
pub(crate) fn copy_file(from: &Path, to: &Path, sources: Option<&SourceHashes>) -> Result<()> {
    match sources {
        Some(sources) => sources.copy(from, to),
        None => fs::copy(from, to).map(drop).map_err(Error::io(to)),
    }
}

/// Whether Game Manager may write in every folder `ops` change, without administrator rights.
pub(crate) fn all_writable(ops: &[FileOp]) -> Result<bool> {
    // Each changed file's folder, or the nearest parent that exists (a tree's folders may not
    // exist yet).
    let mut folders: Vec<PathBuf> = ops
        .iter()
        .flat_map(|op| match op {
            FileOp::Copy { to, .. } => vec![to.clone()],
            FileOp::Move { from, to } => vec![from.clone(), to.clone()],
            FileOp::Delete { path }
            | FileOp::Writable { path, .. }
            | FileOp::DeleteTree { path } => vec![path.clone()],
            FileOp::CopyTree { to, .. } => vec![to.join("file")],
            FileOp::DeleteFiles { folder, names } => {
                names.iter().map(|name| folder.join(name)).collect()
            }
        })
        .filter_map(|path| existing_folder(path.parent()?))
        .collect();
    folders.sort();
    folders.dedup();
    let mut writable = true;
    for folder in folders {
        writable &= can_write(&folder)?;
    }
    Ok(writable)
}

/// `path` or its nearest parent that exists.
fn existing_folder(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|folder| folder.is_dir())
        .map(Path::to_path_buf)
}

/// Whether Game Manager may write in `folder` (or the nearest parent that exists) without
/// administrator rights.
pub(crate) fn folder_writable(folder: &Path) -> Result<bool> {
    match existing_folder(folder) {
        Some(folder) => can_write(&folder),
        None => Ok(false),
    }
}

fn can_write(folder: &Path) -> Result<bool> {
    let probe = folder.join(".game-manager-write-test");
    match fs::File::create(&probe) {
        Ok(file) => {
            drop(file);
            let _ = fs::remove_file(&probe);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => Ok(false),
        Err(error) => Err(Error::io(folder)(error)),
    }
}

/// Runs the operations passed with `--file-ops` (see `elevate`), once `vet` has checked them,
/// copying only sources that still have their SHA-256. The result is the program's exit code: 0,
/// or the HRESULT of what failed or was refused.
pub fn run_encoded(encoded: &str) -> u32 {
    let work =
        percent_decode(encoded).and_then(|json| serde_json::from_str::<ElevatedOps>(&json).ok());
    let Some(work) = work else {
        return E_INVALIDARG.0 as u32;
    };
    exit_code(vet(&work.ops).and_then(|()| run_with(&work.ops, Some(&work.sources), || Ok(()))))
}

/// Checks, as administrator and before any of them runs, that `ops` reach only what Game
/// Manager's own work does (see the module's doc). The error's Windows code says why:
/// `ERROR_NOT_SUPPORTED` for a `Writable` of another file, else `elevate::check_paths`'s.
pub(crate) fn vet(ops: &[FileOp]) -> Result<()> {
    let refused = |path: &Path, code: i32| Err(Error::io(path)(io::Error::from_raw_os_error(code)));
    let mut paths = Vec::new();
    for op in ops {
        match op {
            FileOp::Copy { from, to } | FileOp::Move { from, to } => {
                paths.extend([from.clone(), to.clone()]);
            }
            FileOp::Delete { path } | FileOp::DeleteTree { path } => paths.push(path.clone()),
            FileOp::CopyTree { from, to } => {
                if link_under(from) {
                    return refused(from, ERROR_REPARSE_POINT_ENCOUNTERED);
                }
                paths.extend([from.clone(), to.clone()]);
                paths.extend(files_under(from)?.iter().map(|relative| to.join(relative)));
            }
            FileOp::DeleteFiles { folder, names } => {
                paths.push(folder.clone());
                paths.extend(names.iter().map(|name| folder.join(name)));
            }
            FileOp::Writable { path, .. } => {
                let saved = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        reshade::SAVED_FILES
                            .iter()
                            .any(|saved| saved.eq_ignore_ascii_case(name))
                    });
                if !saved {
                    return refused(path, ERROR_NOT_SUPPORTED);
                }
                paths.push(path.clone());
            }
        }
    }
    elevate::check_paths(paths.iter().map(PathBuf::as_path))
}

/// Whether a junction or symbolic link is anywhere under `folder` (none is followed).
fn link_under(folder: &Path) -> bool {
    let mut folders = vec![folder.to_path_buf()];
    while let Some(current) = folders.pop() {
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            match entry.file_type() {
                Ok(kind) if kind.is_symlink() => return true,
                Ok(kind) if kind.is_dir() => folders.push(entry.path()),
                _ => {}
            }
        }
    }
    false
}

/// What an operation did, to undo it.
enum Done {
    Copied(PathBuf),
    Moved {
        from: PathBuf,
        to: PathBuf,
    },
    SetAside {
        path: PathBuf,
        aside: PathBuf,
    },
    /// The files a `CopyTree` copied and the folders it created, in that order.
    CopiedTree {
        files: Vec<PathBuf>,
        folders: Vec<PathBuf>,
    },
    /// Files a `DeleteFiles` set aside; the folders they leave empty go once all succeeded.
    SetAsideIn {
        folder: PathBuf,
        files: Vec<(PathBuf, PathBuf)>,
    },
    /// A `Writable` file, and whether it was created.
    MadeWritable {
        path: PathBuf,
        created: bool,
    },
}

/// Runs `ops` in order. When one fails, those done are undone, last first, and the error names
/// the file.
pub(crate) fn run(ops: &[FileOp]) -> Result<()> {
    run_then(ops, || Ok(()))
}

/// Runs `ops` like `run`, then `then`; when `then` fails, the operations are undone too.
pub(crate) fn run_then(ops: &[FileOp], then: impl FnOnce() -> Result<()>) -> Result<()> {
    run_with(ops, None, then)
}

/// `run_then`, copying with `sources` when given (as administrator: see `SourceHashes`).
pub(crate) fn run_with(
    ops: &[FileOp],
    sources: Option<&SourceHashes>,
    then: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let mut done = Vec::new();
    let mut outcome = Ok(());
    for op in ops {
        match step(op, sources) {
            Ok(step) => done.push(step),
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    if outcome.is_ok() {
        outcome = then();
    }
    if outcome.is_err() {
        for step in done.iter().rev() {
            undo(step);
        }
        return outcome;
    }
    for step in &done {
        finish(step);
    }
    Ok(())
}

/// Removes what a successful run set aside, and the folders a `DeleteFiles` left empty.
fn finish(step: &Done) {
    match step {
        Done::SetAside { aside, .. } if aside.is_dir() => {
            let _ = fs::remove_dir_all(aside);
        }
        Done::SetAside { aside, .. } => {
            let _ = fs::remove_file(aside);
        }
        Done::SetAsideIn { folder, files } => {
            for (_, aside) in files {
                let _ = fs::remove_file(aside);
            }
            remove_empty_folders(folder, files.iter().map(|(path, _)| path.as_path()));
        }
        _ => {}
    }
}

/// Removes the folders of `files` below `top` that are empty, deepest first.
fn remove_empty_folders<'a>(top: &Path, files: impl Iterator<Item = &'a Path>) {
    let mut folders: Vec<&Path> = files
        .flat_map(|file| file.ancestors().skip(1))
        .filter(|folder| folder.starts_with(top) && *folder != top)
        .collect();
    folders.sort_by_key(|folder| std::cmp::Reverse(folder.components().count()));
    folders.dedup();
    for folder in folders {
        // Fails, as it should, while the folder holds anything.
        let _ = fs::remove_dir(folder);
    }
}

fn free(path: &Path) -> Result<()> {
    if path.exists() {
        let exists = io::Error::from_raw_os_error(ERROR_ALREADY_EXISTS);
        Err(Error::io(path)(exists))
    } else {
        Ok(())
    }
}

/// Every file under `folder`, with its path relative to it.
fn files_under(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut folders = vec![folder.to_path_buf()];
    while let Some(current) = folders.pop() {
        for entry in fs::read_dir(&current).map_err(Error::io(&current))? {
            let path = entry.map_err(Error::io(&current))?.path();
            if path.is_dir() {
                folders.push(path);
            } else if let Ok(relative) = path.strip_prefix(folder) {
                files.push(relative.to_path_buf());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn copy_tree(from: &Path, to: &Path, sources: Option<&SourceHashes>) -> Result<Done> {
    let mut files = Vec::new();
    let mut folders = Vec::new();
    let mut copy = || -> Result<()> {
        for relative in files_under(from)? {
            let target = to.join(&relative);
            free(&target)?;
            if let Some(parent) = target.parent() {
                // Each folder that does not exist yet is created, and remembered for undo.
                let missing: Vec<&Path> = parent
                    .ancestors()
                    .take_while(|folder| !folder.exists())
                    .collect();
                for folder in missing.into_iter().rev() {
                    fs::create_dir(folder).map_err(Error::io(folder))?;
                    folders.push(folder.to_path_buf());
                }
            }
            let source = from.join(&relative);
            // Noted first: it didn't exist, so a copy that fails half way is removed on undo.
            files.push(target.clone());
            copy_file(&source, &target, sources)?;
        }
        Ok(())
    };
    let outcome = copy();
    let done = Done::CopiedTree { files, folders };
    match outcome {
        Ok(()) => Ok(done),
        Err(error) => {
            undo(&done);
            Err(error)
        }
    }
}

fn delete_files(folder: &Path, names: &[String]) -> Result<Done> {
    let mut files = Vec::new();
    let mut delete = || -> Result<()> {
        for name in names {
            let path = folder.join(name);
            if !path.is_file() {
                continue;
            }
            let aside = set_aside(&path)?;
            files.push((path, aside));
        }
        Ok(())
    };
    let outcome = delete();
    let done = Done::SetAsideIn {
        folder: folder.to_path_buf(),
        files,
    };
    match outcome {
        Ok(()) => Ok(done),
        Err(error) => {
            undo(&done);
            Err(error)
        }
    }
}

/// Renames `path` (a file or folder) to `<path>.gm-delete`, replacing a stale one.
fn set_aside(path: &Path) -> Result<PathBuf> {
    let aside = with_suffix(path, ASIDE_SUFFIX);
    if aside.is_dir() {
        fs::remove_dir_all(&aside).map_err(Error::io(&aside))?;
    } else if aside.exists() {
        fs::remove_file(&aside).map_err(Error::io(&aside))?;
    }
    fs::rename(path, &aside).map_err(Error::io(path))?;
    Ok(aside)
}

fn make_writable(path: &Path, sid: &str) -> Result<Done> {
    let created = !path.exists();
    if created {
        fs::File::create(path).map_err(Error::io(path))?;
    }
    let done = Done::MadeWritable {
        path: path.to_path_buf(),
        created,
    };
    match security::grant(path, sid, security::MODIFY) {
        Ok(()) => Ok(done),
        Err(error) => {
            undo(&done);
            Err(error)
        }
    }
}

fn step(op: &FileOp, sources: Option<&SourceHashes>) -> Result<Done> {
    match op {
        FileOp::Copy { from, to } => {
            free(to)?;
            if let Err(error) = copy_file(from, to, sources) {
                // It didn't exist: what a failed copy left goes.
                let _ = fs::remove_file(to);
                return Err(error);
            }
            Ok(Done::Copied(to.clone()))
        }
        FileOp::Move { from, to } => {
            free(to)?;
            fs::rename(from, to).map_err(Error::io(from))?;
            Ok(Done::Moved {
                from: from.clone(),
                to: to.clone(),
            })
        }
        // A folder is set aside whole, like a file: undone by renaming it back.
        FileOp::Delete { path } | FileOp::DeleteTree { path } => Ok(Done::SetAside {
            path: path.clone(),
            aside: set_aside(path)?,
        }),
        FileOp::CopyTree { from, to } => copy_tree(from, to, sources),
        FileOp::DeleteFiles { folder, names } => delete_files(folder, names),
        FileOp::Writable { path, sid } => make_writable(path, sid),
    }
}

fn undo(step: &Done) {
    match step {
        Done::Copied(to) => {
            let _ = fs::remove_file(to);
        }
        Done::Moved { from, to } => {
            let _ = fs::rename(to, from);
        }
        Done::SetAside { path, aside } => {
            let _ = fs::rename(aside, path);
        }
        Done::CopiedTree { files, folders } => {
            for file in files.iter().rev() {
                let _ = fs::remove_file(file);
            }
            for folder in folders.iter().rev() {
                let _ = fs::remove_dir(folder);
            }
        }
        Done::SetAsideIn { files, .. } => {
            for (path, aside) in files.iter().rev() {
                let _ = fs::rename(aside, path);
            }
        }
        Done::MadeWritable { path, created } => {
            if *created {
                let _ = fs::remove_file(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Mods-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The folder's files and their contents, to compare before and after.
    fn listing(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|entry| entry.path().is_file())
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn a_failed_operation_undoes_the_others() {
        let dir = temp_dir("undo");
        fs::write(dir.join("source.dll"), b"new").unwrap();
        fs::write(dir.join("d3d9.dll"), b"game's own").unwrap();
        fs::write(dir.join("old.txt"), b"old").unwrap();
        let before = listing(&dir);
        let ops = [
            FileOp::Move {
                from: dir.join("d3d9.dll"),
                to: dir.join("d3d9.dll.gm-backup"),
            },
            FileOp::Delete {
                path: dir.join("old.txt"),
            },
            FileOp::Copy {
                from: dir.join("source.dll"),
                to: dir.join("d3d9.dll"),
            },
            FileOp::Copy {
                from: dir.join("missing.dll"),
                to: dir.join("d3d11.dll"),
            },
        ];
        let error = run(&ops).unwrap_err();
        assert!(error.to_string().contains("d3d11.dll"), "{error}");
        assert_eq!(listing(&dir), before);

        // Without the failing step, everything happens and the set-aside file is gone.
        run(&ops[..3]).unwrap();
        assert_eq!(fs::read(dir.join("d3d9.dll")).unwrap(), b"new");
        assert_eq!(
            fs::read(dir.join("d3d9.dll.gm-backup")).unwrap(),
            b"game's own"
        );
        assert!(!dir.join("old.txt").exists());
        assert!(!dir.join("old.txt.gm-delete").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    /// Every file (with its contents) and folder (`None`) under `dir`, by relative path.
    fn tree(dir: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
        let mut found = BTreeMap::new();
        let mut folders = vec![dir.to_path_buf()];
        while let Some(folder) = folders.pop() {
            for entry in fs::read_dir(&folder).unwrap().flatten() {
                let path = entry.path();
                let name = path
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                if path.is_dir() {
                    found.insert(name, None);
                    folders.push(path);
                } else {
                    found.insert(name, Some(fs::read(&path).unwrap()));
                }
            }
        }
        found
    }

    #[test]
    fn trees_are_copied_and_deleted_with_their_folders() {
        let dir = temp_dir("tree");
        let (game, staged) = (dir.join("game"), dir.join("staged"));
        let shaders = game.join(r"reshade-shaders\Shaders");
        fs::create_dir_all(&shaders).unwrap();
        fs::write(game.join("game.exe"), b"exe").unwrap();
        fs::write(shaders.join("Mine.fx"), b"the user's").unwrap();
        for (name, text) in [
            (r"Shaders\A.fx", "a"),
            (r"Shaders\Sub\B.fxh", "b"),
            (r"Textures\T.png", "t"),
        ] {
            let path = staged.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let before = tree(&game);
        let copy = FileOp::CopyTree {
            from: staged.clone(),
            to: game.join("reshade-shaders"),
        };
        let writable = FileOp::Writable {
            path: game.join("ReShade.ini"),
            sid: security::current_user_sid().unwrap(),
        };
        let failing = FileOp::Copy {
            from: dir.join("missing"),
            to: game.join("x"),
        };

        // A later failure undoes the copy and the created file, folders included.
        let error = run(&[copy.clone(), writable.clone(), failing.clone()]).unwrap_err();
        assert!(error.to_string().contains(r"game\x"), "{error}");
        assert_eq!(tree(&game), before);
        // So does a failing step after the operations.
        let refused = run_then(std::slice::from_ref(&copy), || Err(Error::ElevationRefused));
        assert!(matches!(refused, Err(Error::ElevationRefused)));
        assert_eq!(tree(&game), before);

        run(&[copy.clone(), writable]).unwrap();
        assert_eq!(fs::read(shaders.join(r"Sub\B.fxh")).unwrap(), b"b");
        assert_eq!(fs::read(game.join("ReShade.ini")).unwrap(), b"");
        // A file already there stops the copy.
        assert!(run(std::slice::from_ref(&copy)).is_err());

        let names: Vec<String> = [
            r"reshade-shaders\Shaders\A.fx",
            r"reshade-shaders\Shaders\Sub\B.fxh",
            r"reshade-shaders\Textures\T.png",
            r"reshade-shaders\Gone.fx",
        ]
        .map(String::from)
        .to_vec();
        let delete = FileOp::DeleteFiles {
            folder: game.clone(),
            names,
        };
        let with_ini = tree(&game);
        assert!(run(&[delete.clone(), failing.clone()]).is_err());
        assert_eq!(tree(&game), with_ini);

        // The emptied folders go; the user's file and its folder stay.
        run(&[delete]).unwrap();
        fs::remove_file(game.join("ReShade.ini")).unwrap();
        assert_eq!(tree(&game), before);

        // A whole folder: put back when a later step fails, else gone with all it holds.
        let delete_tree = FileOp::DeleteTree {
            path: game.join("reshade-shaders"),
        };
        assert!(run(&[delete_tree.clone(), failing]).is_err());
        assert_eq!(tree(&game), before);
        run(&[delete_tree]).unwrap();
        let left: Vec<String> = tree(&game).into_keys().collect();
        assert_eq!(left, ["game.exe"]);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn install_reinstall_and_uninstall_restore_the_folder() {
        let dir = temp_dir("plan");
        let (game, cache) = (dir.join("game"), dir.join("cache"));
        fs::create_dir_all(&game).unwrap();
        fs::create_dir_all(&cache).unwrap();
        fs::write(game.join("game.exe"), b"exe").unwrap();
        fs::write(game.join("dxgi.dll"), b"game's dxgi").unwrap();
        let before = listing(&game);
        let files = |version: &str| {
            ["d3d11.dll", "dxgi.dll"].map(|name| {
                let source = cache.join(format!("{version}-{name}"));
                fs::write(&source, format!("dxvk {name} {version}")).unwrap();
                (name.to_owned(), source)
            })
        };
        let install = |previous: Option<&Installed>, files: &[(String, PathBuf)]| {
            let plan = plan(previous, &game, files).unwrap();
            run(&plan.ops).unwrap();
            Installed {
                version: String::new(),
                page: String::new(),
                folder: game.clone(),
                files: plan.files,
                backups: plan.backups,
            }
        };

        let first = install(None, &files("v1"));
        assert_eq!(first.backups, ["dxgi.dll"]);
        assert_eq!(
            fs::read(game.join("dxgi.dll")).unwrap(),
            b"dxvk dxgi.dll v1"
        );
        assert_eq!(
            fs::read(game.join("dxgi.dll.gm-backup")).unwrap(),
            b"game's dxgi"
        );

        // Reinstalling replaces our files and keeps the game's backup.
        let second = install(Some(&first), &files("v2"));
        assert_eq!(second.backups, ["dxgi.dll"]);
        assert_eq!(
            fs::read(game.join("d3d11.dll")).unwrap(),
            b"dxvk d3d11.dll v2"
        );
        assert_eq!(
            fs::read(game.join("dxgi.dll.gm-backup")).unwrap(),
            b"game's dxgi"
        );

        // A file the game changed since stays; uninstalling puts the rest back as it was.
        fs::write(game.join("d3d11.dll"), b"patched by the game").unwrap();
        let uninstall = plan(Some(&second), &game, &[]).unwrap();
        run(&uninstall.ops).unwrap();
        assert_eq!(
            fs::read(game.join("d3d11.dll")).unwrap(),
            b"patched by the game"
        );
        fs::remove_file(game.join("d3d11.dll")).unwrap();
        assert_eq!(listing(&game), before);

        // Someone else's backup is never replaced.
        fs::write(game.join("dxgi.dll.gm-backup"), b"unknown").unwrap();
        let error = plan(None, &game, &files("v3")).err().unwrap();
        assert!(matches!(error, Error::NotInstallable(_)), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mods_file_keeps_unknown_fields_and_drops_empty_games() {
        let dir = temp_dir("file");
        let file = dir.join(MODS_FILE);
        fs::write(
            &file,
            r#"{ "games": { "steam:1": { "specialK": {} } }, "version": 1 }"#,
        )
        .unwrap();
        let saved = update_in(&file, "steam:7000", |mods| {
            mods.dxvk = Some(dxvk::DxvkState::default());
        })
        .unwrap();
        assert!(saved.dxvk("steam:7000").is_some());
        let json: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert!(json["games"]["steam:1"]["specialK"].is_object());
        assert_eq!(json["version"], 1);

        let saved = update_in(&file, "steam:7000", |mods| mods.dxvk = None).unwrap();
        assert!(!saved.games.contains_key("steam:7000"));

        fs::write(&file, "{ broken").unwrap();
        assert!(update_in(&file, "steam:7000", |_| {}).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ broken");
        fs::remove_dir_all(dir).unwrap();
    }

    /// `ops` as `apply` passes them to the copy running as administrator.
    fn elevated(ops: &[FileOp]) -> String {
        let work = ElevatedOps {
            ops: ops.to_vec(),
            sources: SourceHashes::of(ops).unwrap(),
        };
        percent_encode(&serde_json::to_string(&work).unwrap())
    }

    #[test]
    fn encoded_operations_run_as_passed() {
        let dir = temp_dir("encoded");
        fs::write(dir.join("a.txt"), b"a").unwrap();
        let ops = [FileOp::Copy {
            from: dir.join("a.txt"),
            to: dir.join("b c.txt"),
        }];
        let encoded = elevated(&ops);
        assert_eq!(run_encoded(&encoded), 0);
        assert_eq!(fs::read(dir.join("b c.txt")).unwrap(), b"a");
        // Again: the target exists now.
        assert_eq!(
            run_encoded(&encoded),
            WIN32_ERROR(ERROR_ALREADY_EXISTS as u32).to_hresult().0 as u32
        );
        assert_eq!(run_encoded("%zz"), E_INVALIDARG.0 as u32);
        // Operations without their sources' hashes (an older plan's form) are refused.
        let bare = percent_encode(&serde_json::to_string(&ops).unwrap());
        assert_eq!(run_encoded(&bare), E_INVALIDARG.0 as u32);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn as_administrator_only_sources_with_their_noted_hash_are_copied() {
        let dir = temp_dir("sources");
        let shaders = dir.join("staged");
        fs::create_dir_all(shaders.join(r"reshade-shaders\Shaders")).unwrap();
        fs::write(shaders.join(r"reshade-shaders\Shaders\a.fx"), b"fx").unwrap();
        fs::write(dir.join("dxgi.dll"), b"ReShade").unwrap();
        fs::write(dir.join("d3d9.dll"), b"ReShade 9").unwrap();
        let game = dir.join("Game");
        fs::create_dir(&game).unwrap();
        let ops = vec![
            FileOp::Copy {
                from: dir.join("dxgi.dll"),
                to: game.join("dxgi.dll"),
            },
            FileOp::CopyTree {
                from: shaders.clone(),
                to: game.clone(),
            },
            FileOp::Copy {
                from: dir.join("d3d9.dll"),
                to: game.join("d3d9.dll"),
            },
        ];
        let checksum = WIN32_ERROR(ERROR_DATA_CHECKSUM_ERROR as u32).to_hresult().0 as u32;
        // Noted, then swapped (as during the UAC prompt): that copy fails and the run is undone.
        let encoded = elevated(&ops);
        fs::write(dir.join("d3d9.dll"), b"something else").unwrap();
        assert_eq!(run_encoded(&encoded), checksum);
        assert!(
            fs::read_dir(&game).unwrap().next().is_none(),
            "nothing is left"
        );
        // A file added to the tree after it was noted.
        let encoded = elevated(&ops);
        fs::write(shaders.join(r"reshade-shaders\Shaders\b.fx"), b"added").unwrap();
        assert_eq!(run_encoded(&encoded), checksum);
        assert!(fs::read_dir(&game).unwrap().next().is_none());
        // As they were noted, everything is copied.
        let encoded = elevated(&ops);
        assert_eq!(run_encoded(&encoded), 0);
        assert_eq!(fs::read(game.join("d3d9.dll")).unwrap(), b"something else");
        assert_eq!(
            fs::read(game.join(r"reshade-shaders\Shaders\b.fx")).unwrap(),
            b"added"
        );
        // Not as administrator: copied as they are.
        let mut hashes = SourceHashes::default();
        hashes.add(&dir.join("dxgi.dll")).unwrap();
        let copy = dir.join("copy.dll");
        copy_file(&dir.join("dxgi.dll"), &copy, Some(&hashes)).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"ReShade");
        assert!(
            copy_file(&dir.join("d3d9.dll"), &copy, Some(&hashes)).is_err(),
            "not noted"
        );
        copy_file(&dir.join("d3d9.dll"), &copy, None).unwrap();
        assert_eq!(fs::read(&copy).unwrap(), b"something else");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "downloads DXVK's and ReShade's latest development builds (about 45 MB)"]
    fn live_development_builds_have_the_digest_github_lists() {
        let dir = temp_dir("live-digest");
        let builds = [
            dxvk::latest(dxvk::DxvkVariant::Development).unwrap(),
            reshade::latest(reshade::ReshadeVariant::Development).unwrap(),
        ];
        for build in builds {
            assert!(
                build.sha256.as_deref().is_some_and(crypto::is_sha256),
                "{build:?}"
            );
            let file = dir.join(&build.file_name);
            http::download(&build.url, &file).unwrap();
            check_download(&build, &file).unwrap_or_else(|error| panic!("{error}"));
            println!(
                "{} {}: {}",
                build.version,
                build.file_name,
                build.sha256.unwrap()
            );
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_development_build_must_have_the_digest_github_lists() {
        let dir = temp_dir("digest");
        let file = dir.join("build.zip");
        fs::write(&file, b"abc").unwrap();
        let build = |sha256: Option<&str>| Build {
            version: "master-0cfe089".into(),
            page: String::new(),
            url: "https://nightly.link/x/y.zip".into(),
            file_name: "build.zip".into(),
            sha256: sha256.map(str::to_owned),
        };
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(check_download(&build(Some(abc)), &file).is_ok());
        assert!(matches!(
            check_download(&build(Some(&"0".repeat(64))), &file),
            Err(Error::DigestMismatch { .. })
        ));
        // A release has no digest to check.
        assert!(check_download(&build(None), &file).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn elevated_work_reaches_only_where_game_managers_own_work_goes() {
        use windows::Win32::ERROR_BAD_PATHNAME;
        let dir = temp_dir("vet");
        let game = dir.join("Game");
        let shaders = dir.join("Shaders");
        fs::create_dir_all(game.join("bin")).unwrap();
        fs::create_dir_all(shaders.join("inner")).unwrap();
        fs::write(shaders.join(r"inner\a.fx"), b"fx").unwrap();
        fs::write(dir.join("dxgi.dll"), b"MZ").unwrap();
        let sid = security::current_user_sid().unwrap();
        let code = |error: i32| WIN32_ERROR(error as u32).to_hresult().0 as u32;
        let encoded = |ops: &[FileOp]| elevated(ops);

        // A mod's ordinary work passes.
        let ordinary = [
            FileOp::Copy {
                from: dir.join("dxgi.dll"),
                to: game.join(r"bin\dxgi.dll"),
            },
            FileOp::CopyTree {
                from: shaders.clone(),
                to: game.join("reshade-shaders"),
            },
            FileOp::Writable {
                path: game.join(r"bin\reshade.ini"),
                sid: sid.clone(),
            },
            FileOp::DeleteFiles {
                folder: game.join("reshade-shaders"),
                names: vec![r"inner\a.fx".to_owned()],
            },
        ];
        assert!(vet(&ordinary).is_ok());

        // Opening another file to the user, a name that climbs out, Windows' folder.
        let other = [FileOp::Writable {
            path: game.join(r"bin\dxgi.dll"),
            sid,
        }];
        assert_eq!(run_encoded(&encoded(&other)), code(ERROR_NOT_SUPPORTED));
        let climbs = [FileOp::DeleteFiles {
            folder: game.clone(),
            names: vec![r"..\..\dxgi.dll".to_owned()],
        }];
        assert_eq!(run_encoded(&encoded(&climbs)), code(ERROR_BAD_PATHNAME));
        let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let into_windows = [FileOp::Copy {
            from: dir.join("dxgi.dll"),
            to: system.join(r"System32\gm-test.dll"),
        }];
        assert_eq!(
            run_encoded(&encoded(&into_windows)),
            code(ERROR_BAD_PATHNAME)
        );

        // A junction a standard user made, on the way to a game or inside a tree to copy.
        if !security::is_elevated() {
            let link = dir.join("Linked");
            security::junction(&link, &game);
            let through = [
                FileOp::Copy {
                    from: dir.join("dxgi.dll"),
                    to: game.join("first.dll"),
                },
                FileOp::Copy {
                    from: dir.join("dxgi.dll"),
                    to: link.join("dxgi.dll"),
                },
            ];
            let reparse = code(ERROR_REPARSE_POINT_ENCOUNTERED);
            assert_eq!(run_encoded(&encoded(&through)), reparse);
            // Nothing ran: not even the operation before it.
            assert!(!game.join("first.dll").exists());
            let inside = shaders.join("linked");
            security::junction(&inside, &game);
            let tree = [FileOp::CopyTree {
                from: shaders.clone(),
                to: game.join("reshade-shaders"),
            }];
            assert_eq!(run_encoded(&encoded(&tree)), reparse);
            assert!(!game.join("reshade-shaders").exists());
            fs::remove_dir(&inside).unwrap();
            fs::remove_dir(&link).unwrap();
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
