//! The upscaler DLLs a game ships, which Game Manager swaps for another version without a game
//! update: DLSS (Super Resolution, Ray Reconstruction, Frame Generation), NVIDIA Streamline, FSR
//! 3.1 and the FidelityFX SDK 2, and XeSS (Super Resolution, its DirectX 11 build, Frame
//! Generation, Xe Low Latency).
//!
//! - **Kinds** come from the shipped `upscalers.json` (copied from `config/`): technologies, never
//!   games. A kind is one DLL, or a set swapped together whose first file is the one looked for:
//!   Streamline's `sl.*.dll`, whose plugins must match its interposer, and FidelityFX SDK 2's
//!   loader with its effects.
//! - **Compatible versions**: a kind marked `sameMajor` takes only versions of the game's own
//!   major version (`Kind::fits`): a Streamline 1 game can't load Streamline 2. The game's version
//!   is its DLL's, or for a row already swapped the one it had before (`Found::game_version`).
//! - **Versions**: only the vendors' own DLLs, from their GitHub repositories, within each
//!   kind's layouts (the folder that holds them from one tag to another).
//!   - `files`: every DLL sits in a folder of the repository at each tag, so one is downloaded on
//!     its own from raw.githubusercontent.com, which GitHub's API limit does not count.
//!   - `release`: the DLLs are inside a release's archive (Streamline's, about 280 MB, since 2.7.32
//!     keeps no binaries in its repository); only those matching the kind's files are unpacked.
//!   - The lists (tags, or releases with their assets) come from GitHub's API, 60 anonymous
//!     requests an hour, and are kept for a day in `cache\upscaler-versions.json`.
//! - **Cache**: `%LOCALAPPDATA%\Game Manager\mods\upscalers\<kind>\<version>\`, its DLLs as the
//!   vendor names them (lowercase), with a `.source` file naming the repository and tag. Every DLL
//!   is checked before it is kept: 64-bit, with a valid Authenticode signature by the kind's
//!   vendor (`signature::signer`, the catalog's `signer`), not merely by someone Windows trusts.
//! - **Detection** (`scan`): the kinds' first files in the game's folder and below, each place a
//!   `Found`, with the DLL's own fixed file version (`310.9.16`). Files set aside by mods
//!   (`.gm-backup`, `.gm-delete`) are not looked at.
//! - **Swap and restore**: `mods::plan` and `apply`, as administrator for a protected folder. The
//!   game's DLLs are kept as `<name>.gm-backup`; `mods.json` records each swapped place (`Swap`):
//!   the kind, the game's own version and what was copied. A game update or a check of its files
//!   puts the game's DLL back: the record no longer matches (`State::Replaced`), and Restore then
//!   removes only the stale backups.

use super::{
    BACKUP_SUFFIX, FileOp, Installed, InstalledFile, ModsFile, Plan, apply, get_json, plan, update,
    with_suffix,
};
use crate::{Arch, Error, Game, Result, config, http, json_file, pe, seven_zip, signature};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CATALOG_FILE: &str = "upscalers.json";
const LISTINGS_FILE: &str = "upscaler-versions.json";
/// How long the version lists are kept before GitHub is asked again.
const LISTINGS_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const SOURCE_FILE: &str = ".source";
/// Where downloads are unpacked and checked, inside the cache.
const WORK_FOLDER: &str = ".download";
/// Folders deeper than this below the game's are not searched.
const MAX_DEPTH: usize = 16;

pub const INFO: &str = "DLSS, FSR, XeSS and Streamline ship as DLLs next to a game's program. \
Swapping one for another version changes the upscaler without a game update: a newer one can look \
sharper or ghost less, an older one can undo a regression.

The versions are NVIDIA's, AMD's and Intel's own, from their GitHub repositories, and each DLL \
must carry a valid signature. Swap keeps the game's DLL as <name>.gm-backup; Restore puts it back.

⚠ An online game's anti-cheat may object to changed files.
⚠ A newer DLSS may need its preset chosen in the NVIDIA app to use its newest model.
⚠ A game update or a check of its files puts the game's own DLL back: swap it again then.";

/// The shipped kinds.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub kinds: Vec<Kind>,
}

impl Catalog {
    pub fn kind(&self, id: &str) -> Option<&Kind> {
        self.kinds.iter().find(|kind| kind.id == id)
    }
}

/// An upscaler DLL, or a set of DLLs swapped together.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Kind {
    pub id: String,
    pub name: String,
    pub vendor: String,
    /// The name on the vendor's code-signing certificate ("NVIDIA Corporation"): every DLL must
    /// be signed by it (`signature::signer`), whatever else signed it.
    signer: String,
    /// The first is looked for in games; with `set`, every file of its folder that matches one of
    /// them goes with it (`*` matches any text).
    pub files: Vec<String>,
    #[serde(default)]
    pub set: bool,
    /// The GitHub repository (`NVIDIA/DLSS`).
    pub repo: String,
    #[serde(default)]
    source: Source,
    /// A release's asset, `{version}` standing for the version (`source: release`).
    #[serde(default)]
    asset: String,
    layouts: Vec<Layout>,
    /// For a set: what the library downloads without a game to say which files it uses.
    #[serde(default)]
    download: Vec<String>,
    /// Only versions of the game's own major version work in it.
    #[serde(default, rename = "sameMajor")]
    same_major: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Source {
    /// Each DLL in the repository at a tag.
    #[default]
    Files,
    /// The DLLs inside a release's asset.
    Release,
}

/// The tags (from `since`, up to `until`) whose DLLs are in `folder`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
struct Layout {
    since: String,
    #[serde(default)]
    until: Option<String>,
    folder: String,
}

impl Layout {
    fn covers(&self, version: &Version) -> bool {
        let at_least = Version::parse(&self.since).is_some_and(|since| *version >= since);
        let at_most = self
            .until
            .as_deref()
            .is_none_or(|until| Version::parse(until).is_some_and(|until| *version <= until));
        at_least && at_most
    }
}

impl Kind {
    /// The file looked for in games.
    pub fn anchor(&self) -> &str {
        self.files.first().map_or("", String::as_str)
    }

    /// Whether this kind swaps a file called `name` (its first file, or for a set any of them).
    fn takes(&self, name: &str) -> bool {
        if self.set {
            self.files.iter().any(|pattern| matches(pattern, name))
        } else {
            matches(self.anchor(), name)
        }
    }

    /// The repository's folder holding the DLLs at `version`, when a layout covers it.
    fn folder(&self, version: &Version) -> Option<&str> {
        self.layouts
            .iter()
            .find(|layout| layout.covers(version))
            .map(|layout| layout.folder.as_str())
    }

    /// What a download without a game fetches: the set's `download` list, else the first file.
    fn download_files(&self) -> Vec<String> {
        if self.download.is_empty() {
            vec![self.anchor().to_owned()]
        } else {
            self.download.clone()
        }
    }

    /// Whether `available` can replace a DLL whose game version is `game`: any, unless the kind
    /// needs the game's own major version (Streamline 1 games can't load Streamline 2), which an
    /// unknown version can't be checked against.
    pub fn fits(&self, game: Option<&str>, available: &Available) -> bool {
        !self.same_major
            || game
                .and_then(Version::parse)
                .is_some_and(|game| game.major() == available.version.major())
    }

    /// The versions among `versions` that fit `found`, in their order.
    pub fn offered<'a>(&self, found: &Found, versions: &'a [Available]) -> Vec<&'a Available> {
        versions
            .iter()
            .filter(|available| self.fits(found.game_version(), available))
            .collect()
    }

    /// The release asset holding `version`'s DLLs.
    fn asset_name(&self, version: &Version) -> String {
        self.asset.replace("{version}", &version.to_string())
    }

    /// Where a version's DLLs come from, to show.
    fn page(&self, available: &Available) -> String {
        let tag = &available.tag;
        match (self.source, self.folder(&available.version)) {
            (Source::Files, Some(folder)) => {
                format!("https://github.com/{}/tree/{tag}/{folder}", self.repo)
            }
            _ => format!("https://github.com/{}/releases/tag/{tag}", self.repo),
        }
    }

    /// The key of this kind's list in `upscaler-versions.json`.
    fn listing_key(&self) -> String {
        match self.source {
            Source::Files => format!("{} tags", self.repo),
            Source::Release => format!("{} releases", self.repo),
        }
    }
}

/// Whether `name` matches `pattern`, ignoring case; one `*` matches any text.
fn matches(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.to_ascii_lowercase(), name.to_ascii_lowercase());
    match pattern.split_once('*') {
        Some((head, tail)) => {
            name.len() >= head.len() + tail.len() && name.starts_with(head) && name.ends_with(tail)
        }
        None => pattern == name,
    }
}

/// The shipped kinds. Blocking: reads a file.
pub fn catalog() -> Result<Catalog> {
    let file = config::shipped_file(Path::new(CATALOG_FILE))
        .ok_or_else(|| Error::ToolMissing(PathBuf::from(CATALOG_FILE)))?;
    let text = fs::read_to_string(&file).map_err(Error::io(&file))?;
    serde_json::from_str(&text).map_err(Error::json(CATALOG_FILE))
}

/// A version as vendors tag it: numbers and an optional letter (`3.1.0a`), ordered by number.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    numbers: Vec<u32>,
    suffix: String,
}

impl Version {
    /// Its first number: 2 for `2.14.16`.
    pub fn major(&self) -> Option<u32> {
        self.numbers.first().copied()
    }

    /// `v310.9.16`, `3.1.0a`; `None` for other tags (`fsr3-v3.0.4`).
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let end = text
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(text.len());
        let (numbers, suffix) = text.split_at(end);
        if !suffix.chars().all(|c| c.is_ascii_lowercase()) {
            return None;
        }
        let numbers = numbers
            .split('.')
            .map(|number| number.parse().ok())
            .collect::<Option<Vec<u32>>>()?;
        Some(Self {
            numbers,
            suffix: suffix.to_owned(),
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, number) in self.numbers.iter().enumerate() {
            if index > 0 {
                f.write_str(".")?;
            }
            write!(f, "{number}")?;
        }
        f.write_str(&self.suffix)
    }
}

/// A DLL's fixed file version as shown: `310.9.16`, its last number only when it is not 0.
fn file_version_label(file: &Path) -> Option<String> {
    let [major, minor, build, revision] = crate::version_info::file_version(file)?;
    Some(if revision == 0 {
        format!("{major}.{minor}.{build}")
    } else {
        format!("{major}.{minor}.{build}.{revision}")
    })
}

/// A version of a kind that can be downloaded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Available {
    pub version: Version,
    /// The repository's tag (`v310.9.16`).
    pub tag: String,
    /// A release asset's address and size: the download, for a kind in a release's archive.
    pub asset: Option<(String, u64)>,
}

/// The repositories' tags and releases, as GitHub listed them.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Listings {
    /// When they were listed, in seconds since 1970.
    #[serde(default)]
    fetched: u64,
    #[serde(default)]
    lists: BTreeMap<String, Vec<Listed>>,
}

/// A tag, or a release with its assets.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Listed {
    tag: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    assets: Vec<ListedAsset>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ListedAsset {
    name: String,
    size: u64,
    url: String,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Every kind's versions, newest first, and what could not be listed. The lists are kept for a
/// day unless `refresh`; offline, the last ones are used. Blocking: one web request per
/// repository when they are not kept.
pub fn versions(
    catalog: &Catalog,
    refresh: bool,
) -> (BTreeMap<String, Vec<Available>>, Vec<String>) {
    let file = json_file::local_file("cache").map(|cache| cache.join(LISTINGS_FILE));
    let mut listings: Listings = file
        .as_ref()
        .ok()
        .and_then(|file| json_file::load(file).ok())
        .unwrap_or_default();
    let mut keys: Vec<(String, &Kind)> = catalog
        .kinds
        .iter()
        .map(|kind| (kind.listing_key(), kind))
        .collect();
    keys.sort_by(|a, b| a.0.cmp(&b.0));
    keys.dedup_by(|a, b| a.0 == b.0);
    let fresh = !refresh
        && now().saturating_sub(listings.fetched) < LISTINGS_AGE.as_secs()
        && keys.iter().all(|(key, _)| listings.lists.contains_key(key));
    let mut failed = Vec::new();
    if !fresh {
        for (key, kind) in &keys {
            match list(kind) {
                Ok(listed) => {
                    listings.lists.insert(key.clone(), listed);
                }
                Err(error) => failed.push(format!("{}: {error}", kind.repo)),
            }
        }
        if failed.is_empty() {
            listings.fetched = now();
        }
        if let Ok(file) = &file {
            let _ = json_file::save(file, &listings);
        }
    }
    let versions = catalog
        .kinds
        .iter()
        .map(|kind| {
            let listed = listings
                .lists
                .get(&kind.listing_key())
                .map(Vec::as_slice)
                .unwrap_or_default();
            (kind.id.clone(), available(kind, listed))
        })
        .collect();
    (versions, failed)
}

/// The kind's versions among `listed`, newest first.
fn available(kind: &Kind, listed: &[Listed]) -> Vec<Available> {
    let mut versions: Vec<Available> = listed
        .iter()
        .filter_map(|listed| {
            let version = Version::parse(&listed.tag)?;
            kind.folder(&version)?;
            let asset = match kind.source {
                Source::Files => None,
                Source::Release => {
                    let name = kind.asset_name(&version);
                    let asset = listed.assets.iter().find(|asset| asset.name == name)?;
                    Some((asset.url.clone(), asset.size))
                }
            };
            Some(Available {
                version,
                tag: listed.tag.clone(),
                asset,
            })
        })
        .collect();
    versions.sort_by(|a, b| b.version.cmp(&a.version));
    versions.dedup_by(|a, b| a.version == b.version);
    versions
}

/// The kind's repository's tags, or releases with their assets. Blocking: one web request.
fn list(kind: &Kind) -> Result<Vec<Listed>> {
    let base = format!("https://api.github.com/repos/{}", kind.repo);
    let listed = match kind.source {
        Source::Files => get_json(&format!("{base}/tags?per_page=100"))?
            .as_array()
            .map(|tags| {
                tags.iter()
                    .filter_map(|tag| {
                        Some(Listed {
                            tag: tag["name"].as_str()?.to_owned(),
                            assets: Vec::new(),
                        })
                    })
                    .collect()
            }),
        Source::Release => get_json(&format!("{base}/releases?per_page=100"))?
            .as_array()
            .map(|releases| {
                releases
                    .iter()
                    .filter(|release| !release["draft"].as_bool().unwrap_or(false))
                    .filter_map(|release| {
                        let assets = release["assets"]
                            .as_array()?
                            .iter()
                            .filter_map(|asset| {
                                Some(ListedAsset {
                                    name: asset["name"].as_str()?.to_owned(),
                                    size: asset["size"].as_u64()?,
                                    url: asset["browser_download_url"].as_str()?.to_owned(),
                                })
                            })
                            .collect();
                        Some(Listed {
                            tag: release["tag_name"].as_str()?.to_owned(),
                            assets,
                        })
                    })
                    .collect()
            }),
    };
    listed.ok_or(Error::NoRelease { url: base })
}

/// A version in the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Downloaded {
    pub kind: String,
    pub version: String,
    /// Its DLLs, as the vendor names them.
    pub files: Vec<String>,
    /// Their size, in bytes.
    pub size: u64,
    /// The first DLL's own version, when it differs from the tag's (FidelityFX's SDK version is
    /// not its DLLs').
    pub dll_version: Option<String>,
}

/// `%LOCALAPPDATA%\Game Manager\mods\upscalers`.
pub fn cache_folder() -> Result<PathBuf> {
    Ok(json_file::local_file("mods")?.join("upscalers"))
}

/// What the cache holds, by kind and newest version first. Blocking: lists folders and reads
/// each version's first DLL.
pub fn downloaded() -> Vec<Downloaded> {
    let Ok(root) = cache_folder() else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for kind in folders_in(&root) {
        let kind_name = name_of(&kind);
        if kind_name == WORK_FOLDER {
            continue;
        }
        for version in folders_in(&kind) {
            let files = dlls_in(&version);
            if files.is_empty() {
                continue;
            }
            let size = files
                .iter()
                .filter_map(|file| fs::metadata(version.join(file)).ok())
                .map(|metadata| metadata.len())
                .sum();
            let version_name = name_of(&version);
            let dll_version = files
                .first()
                .and_then(|file| file_version_label(&version.join(file)))
                .filter(|label| Version::parse(label) != Version::parse(&version_name));
            found.push(Downloaded {
                kind: kind_name.clone(),
                version: version_name,
                files,
                size,
                dll_version,
            });
        }
    }
    found.sort_by(|a, b| {
        a.kind
            .cmp(&b.kind)
            .then_with(|| Version::parse(&b.version).cmp(&Version::parse(&a.version)))
    });
    found
}

/// Downloads `available` into the cache, without a game: a set's `download` list, else its first
/// file. Blocking: downloads, and 7-Zip for a release.
pub fn download(kind: &Kind, available: &Available) -> Result<()> {
    ensure(kind, available, &kind.download_files()).map(drop)
}

/// Removes a version from the cache; the games it was swapped into keep their copies. Blocking.
pub fn delete_download(kind: &str, version: &str) -> Result<()> {
    // A folder of the cache, never a path out of it.
    let is_name = |name: &str| !name.is_empty() && !name.contains(['\\', '/', ':']) && name != "..";
    if !is_name(kind) || !is_name(version) || kind == WORK_FOLDER {
        return Err(Error::NotInstallable(format!(
            "{kind} {version} is not a download."
        )));
    }
    let folder = cache_folder()?.join(kind).join(version);
    if folder.is_dir() {
        fs::remove_dir_all(&folder).map_err(Error::io(&folder))?;
    }
    Ok(())
}

fn folders_in(folder: &Path) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect()
        })
        .unwrap_or_default();
    folders.sort();
    folders
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The DLLs directly in `folder`, by name.
fn dlls_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.to_ascii_lowercase().ends_with(".dll"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The file called `name` (ignoring case) in `folder`.
fn find_in(folder: &Path, name: &str) -> Option<PathBuf> {
    dlls_in(folder)
        .into_iter()
        .find(|file| file.eq_ignore_ascii_case(name))
        .map(|file| folder.join(file))
}

/// One download into the cache at a time: they share its work folder.
static DOWNLOAD_LOCK: Mutex<()> = Mutex::new(());

/// The cache's folder of `kind` at `available`, holding `names` (downloaded and checked when
/// missing). Blocking: downloads and, for a release, 7-Zip.
fn ensure(kind: &Kind, available: &Available, names: &[String]) -> Result<PathBuf> {
    let _lock = DOWNLOAD_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let root = cache_folder()?;
    let dir = root.join(&kind.id).join(available.version.to_string());
    let missing: Vec<&String> = names
        .iter()
        .filter(|name| find_in(&dir, name).is_none())
        .collect();
    if missing.is_empty() {
        return Ok(dir);
    }
    let folder = kind.folder(&available.version).ok_or_else(|| {
        Error::NotInstallable(format!(
            "{} {} is not offered.",
            kind.name, available.version
        ))
    })?;
    let work = root.join(WORK_FOLDER);
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(Error::io(&work))?;
    let checked = work.join("checked");
    fs::create_dir_all(&checked).map_err(Error::io(&checked))?;
    match kind.source {
        Source::Files => {
            for name in missing {
                let name = name.to_ascii_lowercase();
                let url = format!(
                    "https://raw.githubusercontent.com/{}/{}/{folder}/{name}",
                    kind.repo, available.tag
                );
                let file = checked.join(&name);
                http::download(&url, &file).map_err(|error| match error {
                    Error::Http { status: 404, .. } => Error::NotInstallable(format!(
                        "{} {} has no {name}.",
                        kind.name, available.version
                    )),
                    error => error,
                })?;
            }
        }
        Source::Release => {
            let (url, _) = available.asset.as_ref().ok_or_else(|| Error::NoRelease {
                url: format!("https://github.com/{}/releases", kind.repo),
            })?;
            let archive = work.join(kind.asset_name(&available.version));
            http::download(url, &archive)?;
            let inside = folder.replace('/', "\\");
            let filters: Vec<String> = kind
                .files
                .iter()
                .map(|pattern| format!("{inside}\\{pattern}"))
                .collect();
            seven_zip::extract_matching(&archive, &checked, &filters)?;
            fs::remove_file(&archive).map_err(Error::io(&archive))?;
        }
    }
    // Every DLL is checked before any goes into the cache.
    let files = dlls_in(&checked);
    for file in &files {
        check(&checked.join(file), kind, available)?;
    }
    fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
    for file in &files {
        let to = dir.join(file.to_ascii_lowercase());
        if to.exists() {
            fs::remove_file(&to).map_err(Error::io(&to))?;
        }
        fs::rename(checked.join(file), &to).map_err(Error::io(&to))?;
    }
    let source = dir.join(SOURCE_FILE);
    fs::write(&source, format!("{}\n{}\n", kind.repo, available.tag))
        .map_err(Error::io(&source))?;
    let _ = fs::remove_dir_all(&work);
    Ok(dir)
}

/// Rejects a downloaded file that is not a 64-bit Windows DLL signed by the kind's vendor.
fn check(file: &Path, kind: &Kind, available: &Available) -> Result<()> {
    let name = name_of(file);
    let from = || format!("{name} of {} {}", kind.name, available.version);
    if pe::architecture(file) != Some(Arch::X64) {
        return Err(Error::NotInstallable(format!(
            "{} is not a 64-bit Windows DLL, so it was not kept.",
            from()
        )));
    }
    match signature::signer(file) {
        Some(signer) if signature::is_signer(&signer, &kind.signer) => Ok(()),
        Some(signer) => Err(Error::NotInstallable(format!(
            "{} is signed by {signer}, not {}, so it was not kept.",
            from(),
            kind.signer
        ))),
        None => Err(Error::NotInstallable(format!(
            "{} has no valid signature, so it was not kept.",
            from()
        ))),
    }
}

/// A kind's DLLs found in a game's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub kind: String,
    pub folder: PathBuf,
    /// The kind's DLLs there, as the game names them: several for a set, the first file first.
    pub files: Vec<String>,
    /// The first file's own version (`310.9.16`).
    pub version: Option<String>,
    /// Whether the first file is a 64-bit DLL: only 64-bit builds are offered.
    pub x64: bool,
    pub state: State,
}

impl Found {
    /// The game's own version: its DLL's, or for a row Game Manager swapped, the one it had
    /// before.
    pub fn game_version(&self) -> Option<&str> {
        match &self.state {
            State::Swapped { original, .. } => original.as_deref(),
            State::Game | State::Replaced => self.version.as_deref(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// The game's own DLL.
    Game,
    /// Swapped by Game Manager for `version` (the tag's, which can differ from the DLL's own); the
    /// game's own version (`original`) is kept as a backup.
    Swapped {
        version: String,
        original: Option<String>,
    },
    /// Swapped once, but the game has put a DLL of its own back since (an update, or a check of
    /// its files); the backup of the old one is still there.
    Replaced,
}

/// A swap in `mods.json`: the kind, the game's own version, and what Game Manager copied (its
/// folder, files and the game's files kept as backups).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Swap {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original: Option<String>,
    pub installed: Installed,
}

impl Swap {
    /// Whether the DLLs Game Manager copied are still in place.
    fn is_current(&self) -> bool {
        !self.installed.files.is_empty()
            && self
                .installed
                .files
                .iter()
                .all(|file| file.is_at(&self.installed.folder.join(&file.name)))
    }

    /// The record's backups that are still there.
    fn backups(&self) -> Vec<PathBuf> {
        self.installed
            .backups
            .iter()
            .map(|name| with_suffix(&self.installed.folder.join(name), BACKUP_SUFFIX))
            .filter(|path| path.exists())
            .collect()
    }
}

fn same_folder(a: &Path, b: &Path) -> bool {
    a.to_string_lossy().to_lowercase().trim_end_matches('\\')
        == b.to_string_lossy().to_lowercase().trim_end_matches('\\')
}

/// The game's swap of `kind` in `folder`, if any.
fn record<'a>(mods: &'a ModsFile, id: &str, kind: &str, folder: &Path) -> Option<&'a Swap> {
    mods.upscalers(id)
        .iter()
        .find(|swap| swap.kind == kind && same_folder(&swap.installed.folder, folder))
}

/// The catalog's DLLs in the game's folder (its install folder, else its program's) and below,
/// by kind and place. Blocking: walks the folder and reads each DLL's version.
pub fn scan(game: &Game, catalog: &Catalog, mods: &ModsFile) -> Vec<Found> {
    let root = game
        .install_dir
        .clone()
        .filter(|dir| dir.is_dir())
        .or_else(|| game.program.as_ref()?.parent().map(Path::to_path_buf));
    let Some(root) = root else {
        return Vec::new();
    };
    let mut found = scan_folder(&root, catalog);
    for item in &mut found {
        if let Some(swap) = record(mods, &game.id, &item.kind, &item.folder) {
            item.state = if swap.is_current() {
                State::Swapped {
                    version: swap.installed.version.clone(),
                    original: swap.original.clone(),
                }
            } else {
                State::Replaced
            };
        }
    }
    found
}

/// The catalog's DLLs under `root`, each kind's place once, in the catalog's order.
fn scan_folder(root: &Path, catalog: &Catalog) -> Vec<Found> {
    let mut found = Vec::new();
    let mut folders = vec![(root.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        let mut names = Vec::new();
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // Links and junctions could lead back up, or out of the game.
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                if depth < MAX_DEPTH {
                    folders.push((entry.path(), depth + 1));
                }
            } else {
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        names.sort();
        for kind in &catalog.kinds {
            let Some(anchor) = names.iter().find(|name| matches(kind.anchor(), name)) else {
                continue;
            };
            let mut files = vec![anchor.clone()];
            if kind.set {
                files.extend(
                    names
                        .iter()
                        .filter(|name| *name != anchor && kind.takes(name))
                        .cloned(),
                );
            }
            let first = folder.join(anchor);
            found.push(Found {
                kind: kind.id.clone(),
                version: file_version_label(&first),
                x64: pe::architecture(&first) == Some(Arch::X64),
                folder: folder.clone(),
                files,
                state: State::Game,
            });
        }
    }
    let order = |id: &str| catalog.kinds.iter().position(|kind| kind.id == id);
    found.sort_by(|a, b| {
        order(&a.kind)
            .cmp(&order(&b.kind))
            .then_with(|| a.folder.cmp(&b.folder))
    });
    found
}

/// Swaps the DLLs of `found` for `available`'s (downloading what the cache lacks), keeping the
/// game's as backups, and returns `mods.json` as saved. Swapped before, Game Manager's copies are
/// replaced and the game's backups stay. Blocking: downloads, and copies as administrator when the
/// folder needs it.
pub fn swap(game: &Game, found: &Found, available: &Available) -> Result<ModsFile> {
    let catalog = catalog()?;
    let kind = catalog
        .kind(&found.kind)
        .ok_or_else(|| Error::NotInstallable(format!("{} is not an upscaler.", found.kind)))?;
    if !found.x64 {
        return Err(Error::NotInstallable(format!(
            "This game's {} is not a 64-bit DLL; only 64-bit builds are offered.",
            kind.name
        )));
    }
    if !kind.fits(found.game_version(), available) {
        return Err(Error::NotInstallable(format!(
            "This game's {} is {}, which only versions of its own major version can replace; {} \
             is not one.",
            kind.name,
            found.game_version().unwrap_or("of an unknown version"),
            available.version
        )));
    }
    let mods = super::load()?;
    let record = record(&mods, &game.id, &kind.id, &found.folder);
    let cache = ensure(kind, available, &found.files)?;
    let mut files = Vec::new();
    for name in &found.files {
        let source = find_in(&cache, name).ok_or_else(|| {
            Error::NotInstallable(format!(
                "{} {} has no {name}, which this game uses.",
                kind.name, available.version
            ))
        })?;
        files.push((name.clone(), source));
    }
    // Before anything changes: an earlier swap still in place keeps the game's version it noted.
    let original = match record {
        Some(swap) if swap.is_current() => swap.original.clone(),
        _ => found.version.clone(),
    };
    let planned = swap_plan(record, &found.folder, &files)?;
    apply(&planned.ops)?;
    let swap = Swap {
        kind: kind.id.clone(),
        original,
        installed: Installed {
            version: available.version.to_string(),
            page: kind.page(available),
            folder: found.folder.clone(),
            files: planned.files,
            backups: planned.backups,
        },
    };
    update(&game.id, |entry| {
        entry.upscalers.retain(|other| {
            !(other.kind == swap.kind
                && same_folder(&other.installed.folder, &swap.installed.folder))
        });
        entry.upscalers.push(swap);
    })
}

/// Puts the game's own DLLs of `kind` in `folder` back (or, after the game replaced them, only
/// removes the stale backups) and returns `mods.json` as saved. Blocking: copies, as administrator
/// when the folder needs it.
pub fn restore(game: &Game, kind: &str, folder: &Path) -> Result<ModsFile> {
    let mods = super::load()?;
    if let Some(swap) = record(&mods, &game.id, kind, folder) {
        apply(&restore_ops(swap)?)?;
    }
    update(&game.id, |entry| {
        entry
            .upscalers
            .retain(|other| !(other.kind == kind && same_folder(&other.installed.folder, folder)));
    })
}

/// The operations that put `files` (name in `folder`, source) in place of the DLLs there, with
/// the record of them. `record` is an earlier swap here: still in place, it is replaced and the
/// game's backups stay; replaced by the game since, its stale backups go and the game's new DLLs
/// are kept instead.
fn swap_plan(record: Option<&Swap>, folder: &Path, files: &[(String, PathBuf)]) -> Result<Plan> {
    match record {
        Some(swap) if swap.is_current() => plan(Some(&swap.installed), folder, files),
        Some(swap) => {
            let stale = swap.backups();
            let mut ops: Vec<FileOp> = stale
                .iter()
                .map(|path| FileOp::Delete { path: path.clone() })
                .collect();
            let mut installed = Vec::new();
            let mut backups = Vec::new();
            for (name, source) in files {
                let path = folder.join(name);
                let backup = with_suffix(&path, BACKUP_SUFFIX);
                if backup.exists() && !stale.contains(&backup) {
                    return Err(Error::NotInstallable(format!(
                        "{} already exists: put it back or delete it first.",
                        backup.display()
                    )));
                }
                if path.exists() {
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
        None => plan(None, folder, files),
    }
}

fn restore_ops(swap: &Swap) -> Result<Vec<FileOp>> {
    if swap.is_current() {
        return Ok(plan(Some(&swap.installed), &swap.installed.folder, &[])?.ops);
    }
    // The game has put DLLs of its own back: they stay, only the old backups go.
    Ok(swap
        .backups()
        .into_iter()
        .map(|path| FileOp::Delete { path })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::run;

    fn shipped() -> Catalog {
        serde_json::from_str(include_str!("../../../config/upscalers.json")).unwrap()
    }

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Upscalers-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn listed(tags: &[&str]) -> Vec<Listed> {
        tags.iter()
            .map(|tag| Listed {
                tag: (*tag).to_owned(),
                assets: Vec::new(),
            })
            .collect()
    }

    #[test]
    fn the_shipped_catalog_is_valid() {
        let catalog = shipped();
        assert!(catalog.kinds.len() >= 11);
        for kind in &catalog.kinds {
            assert!(!kind.files.is_empty(), "{}", kind.id);
            assert!(!kind.layouts.is_empty(), "{}", kind.id);
            for layout in &kind.layouts {
                assert!(Version::parse(&layout.since).is_some(), "{}", kind.id);
            }
            if kind.source == Source::Release {
                assert!(kind.asset.contains("{version}"), "{}", kind.id);
            }
            let ids = catalog
                .kinds
                .iter()
                .filter(|other| other.id == kind.id)
                .count();
            assert_eq!(ids, 1, "{} is listed once", kind.id);
            assert!(
                !kind.signer.trim().is_empty(),
                "{} names its signer",
                kind.id
            );
        }
    }

    #[test]
    fn a_dll_signed_by_another_company_is_not_kept() {
        // Windows' Visual C++ runtime: a valid 64-bit DLL, but Microsoft's, not NVIDIA's.
        let runtime = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"System32\vcruntime140.dll");
        if !runtime.is_file() {
            return;
        }
        let catalog = shipped();
        let dlss = catalog.kind("dlss").unwrap();
        let available = Available {
            version: Version::parse("310.9.16").unwrap(),
            tag: "v310.9.16".to_owned(),
            asset: None,
        };
        let error = check(&runtime, dlss, &available).unwrap_err().to_string();
        assert!(
            error.contains("is signed by Microsoft") && error.contains("not NVIDIA Corporation"),
            "{error}"
        );
    }

    #[test]
    fn versions_order_by_number() {
        let v = |text| Version::parse(text).unwrap();
        assert!(v("v310.1.0") > v("v3.7.20"));
        assert!(v("v3.1.0a") > v("v3.1.0"));
        assert!(v("3.1.10") > v("3.1.2"));
        assert_eq!(v("v310.9.16").to_string(), "310.9.16");
        assert_eq!(v("v3.1.0a").to_string(), "3.1.0a");
        assert_eq!(Version::parse("fsr3-v3.0.4"), None);
        assert_eq!(Version::parse("v2.0.0-beta"), None);
    }

    #[test]
    fn a_kind_offers_the_tags_its_layouts_cover() {
        let catalog = shipped();
        let tags = listed(&[
            "v2.4.0",
            "v2.1.0",
            "v2.0.0",
            "v1.1.4",
            "v1.1.0",
            "v1.0.0",
            "fsr3-v3.0.4",
        ]);
        let versions = |id: &str| -> Vec<String> {
            available(catalog.kind(id).unwrap(), &tags)
                .iter()
                .map(|available| available.version.to_string())
                .collect()
        };
        assert_eq!(versions("fsr_dx12"), ["1.1.4", "1.1.0"]);
        assert_eq!(versions("fsr_sdk2"), ["2.4.0", "2.1.0", "2.0.0"]);
        let dlss = listed(&["v310.4.0", "v310.1.0", "v3.7.20", "v2.2.1", "v2.2.0"]);
        let of = |id: &str| -> Vec<String> {
            available(catalog.kind(id).unwrap(), &dlss)
                .iter()
                .map(|available| available.version.to_string())
                .collect()
        };
        assert_eq!(of("dlss"), ["310.4.0", "310.1.0", "3.7.20", "2.2.1"]);
        assert_eq!(of("dlss_rr"), ["310.4.0", "310.1.0"]);
        assert_eq!(of("dlss_fg"), ["310.4.0"]);
        // A release kind needs its asset.
        let streamline = catalog.kind("streamline").unwrap();
        let releases = vec![
            Listed {
                tag: "v2.14.16".to_owned(),
                assets: vec![ListedAsset {
                    name: "streamline-sdk-v2.14.16.zip".to_owned(),
                    size: 276_016_170,
                    url: "https://example.com/a.zip".to_owned(),
                }],
            },
            Listed {
                tag: "v2.14.1".to_owned(),
                assets: Vec::new(),
            },
        ];
        let offered = available(streamline, &releases);
        assert_eq!(offered.len(), 1);
        assert_eq!(
            offered[0].asset,
            Some(("https://example.com/a.zip".to_owned(), 276_016_170))
        );
    }

    #[test]
    fn streamline_takes_only_versions_of_the_games_major_version() {
        let catalog = shipped();
        let versions: Vec<Available> = ["v2.14.16", "v2.7.32"]
            .iter()
            .map(|tag| Available {
                version: Version::parse(tag).unwrap(),
                tag: (*tag).to_owned(),
                asset: None,
            })
            .collect();
        let found = |version: Option<&str>, state: State| Found {
            kind: "streamline".to_owned(),
            folder: PathBuf::from(r"C:\Game"),
            files: vec!["sl.interposer.dll".to_owned()],
            version: version.map(str::to_owned),
            x64: true,
            state,
        };
        let offered = |found: &Found| -> Vec<String> {
            catalog
                .kind("streamline")
                .unwrap()
                .offered(found, &versions)
                .iter()
                .map(|available| available.version.to_string())
                .collect()
        };
        // Streamline 1 can't load Streamline 2.
        assert!(offered(&found(Some("1.0.0.0"), State::Game)).is_empty());
        assert_eq!(
            offered(&found(Some("2.10.3"), State::Game)),
            ["2.14.16", "2.7.32"]
        );
        // An unknown version can't be checked: nothing is offered.
        assert!(offered(&found(None, State::Game)).is_empty());
        // Swapped already, the game's own version counts, not the one swapped in.
        let swapped = State::Swapped {
            version: "2.7.32".to_owned(),
            original: Some("1.0.0.0".to_owned()),
        };
        assert!(offered(&found(Some("2.7.32"), swapped)).is_empty());
        // DLSS takes any version.
        let dlss = catalog.kind("dlss").unwrap();
        assert!(dlss.fits(Some("2.2.16"), &versions[0]));
    }

    #[test]
    fn patterns_match_ignoring_case() {
        assert!(matches("sl.*.dll", "sl.interposer.dll"));
        assert!(matches("sl.*.dll", "SL.DLSS_G.DLL"));
        assert!(!matches("sl.*.dll", "nvngx_dlss.dll"));
        assert!(matches("nvngx_dlss.dll", "NVNGX_DLSS.dll"));
        assert!(!matches("nvngx_dlss.dll", "nvngx_dlssg.dll"));
        assert!(matches(
            "amd_fidelityfx_*_dx12.dll",
            "amd_fidelityfx_upscaler_dx12.dll"
        ));
    }

    #[test]
    fn scanning_finds_each_place_and_groups_sets() {
        let dir = temp_dir("scan");
        write(&dir.join(r"bin\x64\nvngx_dlss.dll"), "dlss");
        write(&dir.join(r"bin\x64\nvngx_dlssg.dll"), "fg");
        write(&dir.join(r"bin\x64\sl.interposer.dll"), "sl");
        write(&dir.join(r"bin\x64\sl.common.dll"), "sl");
        write(&dir.join(r"bin\x64\sl.dlss_g.dll"), "sl");
        write(&dir.join(r"bin\x64\nvngx_dlss.dll.gm-backup"), "old");
        write(&dir.join(r"Engine\Plugins\XeSS\libxess.dll"), "xess");
        // Plugins alone, without the interposer, are no Streamline to swap.
        write(&dir.join(r"tools\sl.common.dll"), "sl");
        let found = scan_folder(&dir, &shipped());
        let summary: Vec<(String, String, Vec<String>)> = found
            .iter()
            .map(|item| {
                (
                    item.kind.clone(),
                    item.folder
                        .strip_prefix(&dir)
                        .unwrap()
                        .display()
                        .to_string(),
                    item.files.clone(),
                )
            })
            .collect();
        let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(
            summary,
            vec![
                (
                    "dlss".to_owned(),
                    r"bin\x64".to_owned(),
                    names(&["nvngx_dlss.dll"])
                ),
                (
                    "dlss_fg".to_owned(),
                    r"bin\x64".to_owned(),
                    names(&["nvngx_dlssg.dll"])
                ),
                (
                    "streamline".to_owned(),
                    r"bin\x64".to_owned(),
                    names(&["sl.interposer.dll", "sl.common.dll", "sl.dlss_g.dll"])
                ),
                (
                    "xess".to_owned(),
                    r"Engine\Plugins\XeSS".to_owned(),
                    names(&["libxess.dll"])
                ),
            ]
        );
        // Not real DLLs: no version, not 64-bit.
        assert!(found.iter().all(|item| item.version.is_none() && !item.x64));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn swapping_again_and_restoring_give_the_game_its_dlls_back() {
        let dir = temp_dir("swap");
        let game = dir.join("game");
        write(&game.join("nvngx_dlss.dll"), "the game's 3.7");
        write(&dir.join(r"cache\310\nvngx_dlss.dll"), "310");
        write(&dir.join(r"cache\311\nvngx_dlss.dll"), "311");
        let file = |version: &str| {
            vec![(
                "nvngx_dlss.dll".to_owned(),
                dir.join("cache").join(version).join("nvngx_dlss.dll"),
            )]
        };
        let record = |plan: Plan| Swap {
            kind: "dlss".to_owned(),
            original: Some("3.7".to_owned()),
            installed: Installed {
                version: String::new(),
                page: String::new(),
                folder: game.clone(),
                files: plan.files,
                backups: plan.backups,
            },
        };
        let read = |name: &str| fs::read_to_string(game.join(name)).unwrap();

        // First swap: the game's DLL is kept.
        let planned = swap_plan(None, &game, &file("310")).unwrap();
        run(&planned.ops).unwrap();
        let swapped = record(planned);
        assert_eq!(read("nvngx_dlss.dll"), "310");
        assert_eq!(read("nvngx_dlss.dll.gm-backup"), "the game's 3.7");
        assert!(swapped.is_current());

        // Swapping again replaces Game Manager's copy; the game's backup stays.
        let planned = swap_plan(Some(&swapped), &game, &file("311")).unwrap();
        run(&planned.ops).unwrap();
        let swapped = record(planned);
        assert_eq!(read("nvngx_dlss.dll"), "311");
        assert_eq!(read("nvngx_dlss.dll.gm-backup"), "the game's 3.7");

        // Restore: the game's own DLL, byte for byte, and no backup.
        run(&restore_ops(&swapped).unwrap()).unwrap();
        assert_eq!(read("nvngx_dlss.dll"), "the game's 3.7");
        assert!(!game.join("nvngx_dlss.dll.gm-backup").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn after_the_game_replaced_a_swap_only_its_stale_backup_goes() {
        let dir = temp_dir("replaced");
        let game = dir.join("game");
        write(&game.join("libxess.dll"), "the game's 1.3");
        write(&dir.join(r"cache\libxess.dll"), "2.1");
        let files = vec![("libxess.dll".to_owned(), dir.join(r"cache\libxess.dll"))];
        let planned = swap_plan(None, &game, &files).unwrap();
        run(&planned.ops).unwrap();
        let swapped = Swap {
            kind: "xess".to_owned(),
            original: Some("1.3".to_owned()),
            installed: Installed {
                version: "2.1".to_owned(),
                page: String::new(),
                folder: game.clone(),
                files: planned.files,
                backups: planned.backups,
            },
        };
        // A game update puts a DLL of its own back.
        fs::write(game.join("libxess.dll"), "the game's 1.4").unwrap();
        assert!(!swapped.is_current());

        // Swapping again keeps the update's DLL as the backup, not the stale one.
        let planned = swap_plan(Some(&swapped), &game, &files).unwrap();
        run(&planned.ops).unwrap();
        assert_eq!(fs::read_to_string(game.join("libxess.dll")).unwrap(), "2.1");
        assert_eq!(
            fs::read_to_string(game.join("libxess.dll.gm-backup")).unwrap(),
            "the game's 1.4"
        );

        // Restoring a replaced swap leaves the game's DLL and removes the stale backup.
        fs::write(game.join("libxess.dll"), "the game's 1.5").unwrap();
        let stale = Swap {
            installed: Installed {
                files: vec![
                    InstalledFile::of("libxess.dll", &dir.join(r"cache\libxess.dll")).unwrap(),
                ],
                backups: vec!["libxess.dll".to_owned()],
                ..swapped.installed.clone()
            },
            ..swapped
        };
        run(&restore_ops(&stale).unwrap()).unwrap();
        assert_eq!(
            fs::read_to_string(game.join("libxess.dll")).unwrap(),
            "the game's 1.5"
        );
        assert!(!game.join("libxess.dll.gm-backup").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[ignore = "downloads one version of every kind from NVIDIA's, AMD's and Intel's repositories"]
    fn live_upscalers_download() {
        let catalog = catalog().unwrap();
        let (versions, failed) = versions(&catalog, true);
        assert!(failed.is_empty(), "{failed:?}");
        for kind in &catalog.kinds {
            let offered = &versions[&kind.id];
            assert!(!offered.is_empty(), "{} lists no version", kind.id);
            // The oldest is the smallest download, and the likeliest to be laid out differently.
            let available = offered.last().unwrap();
            let names = match kind.id.as_str() {
                "streamline" => vec!["sl.interposer.dll".to_owned(), "sl.common.dll".to_owned()],
                "fsr_sdk2" => vec![
                    "amd_fidelityfx_loader_dx12.dll".to_owned(),
                    "amd_fidelityfx_upscaler_dx12.dll".to_owned(),
                ],
                _ => vec![kind.anchor().to_owned()],
            };
            let dir = ensure(kind, available, &names)
                .unwrap_or_else(|error| panic!("{} {}: {error}", kind.id, available.version));
            for name in &names {
                let file = find_in(&dir, name).unwrap();
                let signer = signature::signer(&file).unwrap_or_default();
                assert!(
                    signature::is_signer(&signer, &kind.signer),
                    "{} is signed by {signer:?}",
                    file.display()
                );
                assert_eq!(pe::architecture(&file), Some(Arch::X64));
                println!(
                    "{} {}: {name} {}",
                    kind.id,
                    available.version,
                    file_version_label(&file).unwrap_or_default()
                );
            }
        }
    }
}
