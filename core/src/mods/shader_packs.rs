//! ReShade's shader packs (the Shaders page): which packs there are, from the shipped
//! `reshade-shaders.json` (copied from `config/`), and laying out the `reshade-shaders` folder
//! that Game Manager deploys into games for the user's `Selection`.
//!
//! - **Cache**: a pack is downloaded once into `%LOCALAPPDATA%\Game Manager\mods\reshade\packs\<id>`
//!   as `Shaders` and `Textures`, with a `.source` file naming what it was made from: the
//!   download address and the release tag (a GitHub release) or ETag (an archive). With
//!   `refresh`, a pack whose source changed is downloaded again; offline, the cached one is used.
//! - **Layout**: packs come in many shapes. Their shallowest `Shaders` and `Textures` folders are
//!   kept; a pack without a `Shaders` folder gives its `.fx`/`.fxh` files to `Shaders` and its
//!   images and `.cube` LUTs to `Textures`, keeping their subfolders. `excludedFiles` (effects
//!   that do not compile) are dropped.
//! - **Selection**: whole packs, or some of a pack's effects (`.fx`). Its headers (`.fxh`) and
//!   textures always go with it, as the effects include them. A pack brings the packs it
//!   `requires`. Essential and `default` packs are selected until the user chooses; any pack can
//!   be left out.
//! - **Staging**: the shared headers (`ReShade.fxh`, `ReShadeUI.fxh` from `headersPack`) first,
//!   then the packs in the list's order (the first to supply a file keeps it), then the user's
//!   own shaders from `%APPDATA%\Game Manager\reshade\custom`.

use super::{files_under, get_json};
use crate::{Error, Result, config, http, json_file, seven_zip};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const CATALOG_FILE: &str = "reshade-shaders.json";
const SOURCE_FILE: &str = ".source";
pub const SHADERS: &str = "Shaders";
pub const TEXTURES: &str = "Textures";
/// Files that are effects' textures when a pack has no `Textures` folder.
const TEXTURE_EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "dds", "bmp", "tga", "cube"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    /// Selected until the user chooses (Lilium's HDR shaders).
    Essential,
    Recommended,
    Extra,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Essential => "Essential",
            Category::Recommended => "Recommended",
            Category::Extra => "Extra",
        }
    }
}

/// Where a pack is downloaded from.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Source {
    /// The latest release of `owner/repo`: its asset with this extension.
    GitHub { github: String, asset: String },
    /// An archive (a branch's ZIP).
    Url { url: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShaderPack {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub category: Category,
    pub source: Source,
    /// Selected until the user chooses.
    #[serde(default)]
    pub default: bool,
    /// Packs this one needs.
    #[serde(default)]
    pub requires: Vec<String>,
}

/// The shipped pack list.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    #[serde(default)]
    pub packs: Vec<ShaderPack>,
    #[serde(default)]
    excluded_files: Vec<String>,
    /// The pack whose `headers` every effect includes.
    #[serde(default)]
    headers_pack: Option<String>,
    #[serde(default)]
    headers: Vec<String>,
}

impl Catalog {
    pub fn pack(&self, id: &str) -> Option<&ShaderPack> {
        self.packs.iter().find(|pack| pack.id == id)
    }

    fn excluded(&self, name: &str) -> bool {
        self.excluded_files
            .iter()
            .any(|file| file.eq_ignore_ascii_case(name))
    }
}

/// The shipped pack list. Blocking: reads a file.
pub fn catalog() -> Result<Catalog> {
    let file = config::shipped_file(Path::new(CATALOG_FILE))
        .ok_or_else(|| Error::ToolMissing(PathBuf::from(CATALOG_FILE)))?;
    let text = fs::read_to_string(&file).map_err(Error::io(&file))?;
    serde_json::from_str(&text).map_err(Error::json(CATALOG_FILE))
}

/// What is picked of one pack (or of the user's own shaders).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    /// The effects picked (`Shaders\Bloom.fx`); `None` is the whole pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
}

impl Pick {
    /// Whether this pick deploys `file` (a path in the pack): every file but effects left out.
    pub fn takes(&self, file: &str) -> bool {
        let is_effect = file.to_ascii_lowercase().ends_with(".fx");
        match &self.files {
            Some(files) if is_effect => {
                files.iter().any(|picked| picked.eq_ignore_ascii_case(file))
            }
            _ => true,
        }
    }
}

/// The packs and effects to deploy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    #[serde(default)]
    pub packs: BTreeMap<String, Pick>,
    /// The user's own shaders; `None` leaves them out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom: Option<Pick>,
}

impl Selection {
    /// The `default` packs and the user's own shaders.
    pub fn defaults(catalog: &Catalog) -> Self {
        Self {
            packs: catalog
                .packs
                .iter()
                .filter(|pack| pack.default || pack.category == Category::Essential)
                .map(|pack| (pack.id.clone(), Pick::default()))
                .collect(),
            custom: Some(Pick::default()),
        }
    }

    /// The packs to deploy, in the list's order: the picked ones and the packs they require
    /// (whole).
    pub fn to_deploy<'a>(&self, catalog: &'a Catalog) -> Vec<(&'a ShaderPack, Pick)> {
        let mut wanted: HashSet<&str> = catalog
            .packs
            .iter()
            .filter(|pack| self.packs.contains_key(&pack.id))
            .map(|pack| pack.id.as_str())
            .collect();
        // Requirements, and theirs.
        loop {
            let more: Vec<&str> = catalog
                .packs
                .iter()
                .filter(|pack| wanted.contains(pack.id.as_str()))
                .flat_map(|pack| pack.requires.iter().map(String::as_str))
                .filter(|id| !wanted.contains(id))
                .collect();
            if more.is_empty() {
                break;
            }
            wanted.extend(more);
        }
        catalog
            .packs
            .iter()
            .filter(|pack| wanted.contains(pack.id.as_str()))
            .map(|pack| (pack, self.packs.get(&pack.id).cloned().unwrap_or_default()))
            .collect()
    }
}

/// What staging laid out, and the packs it could not get.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Staged {
    pub files: usize,
    /// "Name: what failed", for each pack left out.
    pub failed: Vec<String>,
}

/// Lays out `tree\reshade-shaders` for `selection`. Blocking: downloads the packs not in the
/// cache and, with `refresh`, those whose source changed. A pack that cannot be had is left out
/// and named in `Staged::failed`.
pub fn stage(
    catalog: &Catalog,
    selection: &Selection,
    tree: &Path,
    refresh: bool,
) -> Result<Staged> {
    let out = tree.join(super::reshade::SHADERS_FOLDER);
    fs::create_dir_all(&out).map_err(Error::io(&out))?;
    let mut staged = Staged::default();
    let headers = catalog
        .headers_pack
        .as_deref()
        .and_then(|id| catalog.pack(id));
    if let Some(pack) = headers {
        match ensure(catalog, pack, refresh) {
            Ok(dir) => {
                let names: Vec<String> = catalog
                    .headers
                    .iter()
                    .map(|header| format!(r"{SHADERS}\{header}"))
                    .collect();
                staged.files += copy_files(&dir, &out, |file| {
                    names.iter().any(|name| name.eq_ignore_ascii_case(file))
                })?;
            }
            Err(error) => staged.failed.push(format!("{}: {error}", pack.name)),
        }
    }
    for (pack, pick) in selection.to_deploy(catalog) {
        match ensure(catalog, pack, refresh) {
            Ok(dir) => staged.files += copy_files(&dir, &out, |file| pick.takes(file))?,
            Err(error) => staged.failed.push(format!("{}: {error}", pack.name)),
        }
    }
    if let Some(pick) = &selection.custom {
        let custom = custom_dir()?;
        if custom.is_dir() {
            staged.files += copy_files(&custom, &out, |file| pick.takes(file))?;
        }
    }
    Ok(staged)
}

/// Copies the `Shaders` and `Textures` files of `from` that `take` accepts into `to`, keeping
/// those already there; returns how many it copied.
pub(crate) fn copy_files(from: &Path, to: &Path, take: impl Fn(&str) -> bool) -> Result<usize> {
    let mut copied = 0;
    for relative in pack_files(from)? {
        let name = relative.to_string_lossy();
        let target = to.join(&relative);
        if !take(&name) || target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(Error::io(parent))?;
        }
        let source = from.join(&relative);
        fs::copy(&source, &target).map_err(Error::io(&target))?;
        copied += 1;
    }
    Ok(copied)
}

/// A pack folder's files under `Shaders` and `Textures`, relative to it (`Shaders\Bloom.fx`).
fn pack_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for top in [SHADERS, TEXTURES] {
        let folder = dir.join(top);
        if folder.is_dir() {
            files.extend(
                files_under(&folder)?
                    .into_iter()
                    .map(|file| Path::new(top).join(file)),
            );
        }
    }
    Ok(files)
}

/// A cached pack's effects (`Shaders\Bloom.fx`), for picking single ones; empty when it is not
/// downloaded. Blocking.
pub fn effects(id: &str) -> Vec<String> {
    pack_dir(id)
        .ok()
        .map(|dir| effects_in(&dir))
        .unwrap_or_default()
}

/// The user's own effects, in the custom folder. Blocking.
pub fn custom_effects() -> Vec<String> {
    custom_dir()
        .ok()
        .map(|dir| effects_in(&dir))
        .unwrap_or_default()
}

fn effects_in(dir: &Path) -> Vec<String> {
    pack_files(dir)
        .unwrap_or_default()
        .into_iter()
        .map(|file| file.to_string_lossy().into_owned())
        .filter(|file| file.to_ascii_lowercase().ends_with(".fx"))
        .collect()
}

/// `%APPDATA%\Game Manager\reshade\custom`: the user's own `Shaders`, `Textures` and `Addons`.
pub fn custom_dir() -> Result<PathBuf> {
    Ok(json_file::user_file("reshade")?.join("custom"))
}

fn packs_dir() -> Result<PathBuf> {
    Ok(json_file::local_file("mods")?.join("reshade").join("packs"))
}

fn pack_dir(id: &str) -> Result<PathBuf> {
    let id: String = id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_".contains(*c))
        .collect();
    Ok(packs_dir()?.join(id))
}

/// Whether the pack is in the cache.
pub fn is_cached(id: &str) -> bool {
    pack_dir(id).is_ok_and(|dir| dir.join(SOURCE_FILE).is_file())
}

/// One download into the pack cache at a time: they share its work folder.
static PACK_LOCK: Mutex<()> = Mutex::new(());

/// The pack's folder in the cache: downloaded when it is not there or, with `refresh`, when its
/// source changed (offline, the cached one stays). Blocking.
pub fn ensure(catalog: &Catalog, pack: &ShaderPack, refresh: bool) -> Result<PathBuf> {
    let _lock = PACK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = pack_dir(&pack.id)?;
    let cached = fs::read_to_string(dir.join(SOURCE_FILE)).ok();
    if cached.is_some() && !refresh {
        return Ok(dir);
    }
    let (url, file_name, version) = match latest(pack) {
        Ok(latest) => latest,
        Err(_) if cached.is_some() => return Ok(dir),
        Err(error) => return Err(error),
    };
    let source = format!("{url}\n{version}");
    if !version.is_empty() && cached.as_deref() == Some(source.as_str()) {
        return Ok(dir);
    }
    let Some(root) = dir.parent() else {
        return Err(Error::NoModBuild(dir));
    };
    let work = root.join("download");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(Error::io(&work))?;
    let download = work.join(&file_name);
    if let Err(error) = http::download(&url, &download) {
        let _ = fs::remove_dir_all(&work);
        return if cached.is_some() {
            Ok(dir)
        } else {
            Err(error)
        };
    }
    let kept = work.join("pack");
    if download
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("fx") || ext.eq_ignore_ascii_case("fxh"))
    {
        let to = kept.join(SHADERS).join(&file_name);
        fs::create_dir_all(kept.join(SHADERS)).map_err(Error::io(&kept))?;
        fs::copy(&download, &to).map_err(Error::io(&to))?;
    } else {
        let unpacked = work.join("unpacked");
        seven_zip::extract(&download, &unpacked)?;
        if lay_out(catalog, &unpacked, &kept)? == 0 {
            return Err(Error::NoModBuild(download));
        }
    }
    let source_file = kept.join(SOURCE_FILE);
    fs::write(&source_file, &source).map_err(Error::io(&source_file))?;
    // Moved into place last, so the cache never holds half a pack.
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(Error::io(&dir))?;
    }
    fs::rename(&kept, &dir).map_err(Error::io(&dir))?;
    let _ = fs::remove_dir_all(&work);
    Ok(dir)
}

/// The pack's download now: its address, a file name for it, and its version (the release tag,
/// or the archive's ETag; empty when it has none). Blocking: one web request.
fn latest(pack: &ShaderPack) -> Result<(String, String, String)> {
    match &pack.source {
        Source::GitHub { github, asset } => {
            let url = format!("https://api.github.com/repos/{github}/releases/latest");
            let release = get_json(&url)?;
            let tag = release["tag_name"].as_str().unwrap_or_default().to_owned();
            let found = release["assets"].as_array().and_then(|assets| {
                assets.iter().find_map(|item| {
                    let name = item["name"].as_str()?;
                    let link = item["browser_download_url"].as_str()?;
                    name.to_ascii_lowercase()
                        .ends_with(&asset.to_ascii_lowercase())
                        .then(|| (link.to_owned(), name.to_owned()))
                })
            });
            let (link, name) = found.ok_or(Error::NoRelease { url })?;
            Ok((link, name, tag))
        }
        Source::Url { url } => {
            let etag = http::etag(url)?.unwrap_or_default();
            Ok((url.clone(), "pack.zip".to_owned(), etag))
        }
    }
}

/// Lays an unpacked pack out as `kept\Shaders` and `kept\Textures`; returns how many files it
/// kept.
pub(crate) fn lay_out(catalog: &Catalog, unpacked: &Path, kept: &Path) -> Result<usize> {
    let mut count = 0;
    let mut copy = |from: &Path, relative: &Path, to: &Path| -> Result<()> {
        let excluded = relative
            .file_name()
            .is_some_and(|name| catalog.excluded(&name.to_string_lossy()));
        if excluded {
            return Ok(());
        }
        let target = to.join(relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(Error::io(parent))?;
        }
        fs::copy(from.join(relative), &target).map_err(Error::io(&target))?;
        count += 1;
        Ok(())
    };
    let shaders = shallowest_folder(unpacked, SHADERS);
    match &shaders {
        Some(folder) => {
            for relative in files_under(folder)? {
                copy(folder, &relative, &kept.join(SHADERS))?;
            }
        }
        // No Shaders folder: its effects, wherever they are (below a single top folder).
        None => {
            let root = single_top_folder(unpacked);
            for relative in files_under(&root)? {
                let extension = relative
                    .extension()
                    .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                if extension == "fx" || extension == "fxh" {
                    copy(&root, &relative, &kept.join(SHADERS))?;
                } else if TEXTURE_EXTENSIONS.contains(&extension.as_str()) {
                    copy(&root, &relative, &kept.join(TEXTURES))?;
                }
            }
        }
    }
    // With a Shaders folder, its Textures folder (outside it); without, textures were found above.
    if let Some(shaders) = &shaders
        && let Some(folder) =
            shallowest_folder(unpacked, TEXTURES).filter(|folder| !folder.starts_with(shaders))
    {
        for relative in files_under(&folder)? {
            copy(&folder, &relative, &kept.join(TEXTURES))?;
        }
    }
    Ok(count)
}

/// The shallowest folder called `name` under `root`, at most four levels down.
fn shallowest_folder(root: &Path, name: &str) -> Option<PathBuf> {
    let mut level = vec![root.to_path_buf()];
    for _ in 0..4 {
        let mut next = Vec::new();
        for folder in &level {
            let Ok(entries) = fs::read_dir(folder) else {
                continue;
            };
            let mut children: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .collect();
            children.sort();
            if let Some(found) = children.iter().find(|child| {
                child
                    .file_name()
                    .is_some_and(|child| child.eq_ignore_ascii_case(name))
            }) {
                return Some(found.clone());
            }
            next.extend(children);
        }
        level = next;
    }
    None
}

/// `root`'s only folder when it holds nothing else (an archive's `<repo>-<branch>`), else `root`.
fn single_top_folder(root: &Path) -> PathBuf {
    let entries: Vec<PathBuf> = fs::read_dir(root)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    match entries.as_slice() {
        [only] if only.is_dir() => only.clone(),
        _ => root.to_path_buf(),
    }
}

/// Zips the shaders of `selection` (from the cache; packs not there are downloaded) as
/// `%LOCALAPPDATA%\Game Manager\export\ReShade shaders.zip`, holding `reshade-shaders`, and
/// returns it with what could not be had. Blocking.
pub fn export(catalog: &Catalog, selection: &Selection) -> Result<(PathBuf, Vec<String>)> {
    let dir = json_file::local_file("export")?;
    let tree = dir.join("tree");
    let _ = fs::remove_dir_all(&tree);
    let staged = stage(catalog, selection, &tree, false)?;
    let archive = dir.join("ReShade shaders.zip");
    seven_zip::zip_folder(&tree.join(super::reshade::SHADERS_FOLDER), &archive)?;
    let _ = fs::remove_dir_all(&tree);
    Ok((archive, staged.failed))
}

/// Unpacks a ZIP of shaders (an export, or a pack) into the custom folder, below a folder named
/// after it (`Shaders\<name>`, `Textures\<name>`), and returns how many files it gave. Blocking.
pub fn import(archive: &Path) -> Result<usize> {
    import_into(archive, &custom_dir()?, &json_file::local_file("import")?)
}

/// `import` into the custom folder `custom`, unpacking in `work`.
fn import_into(archive: &Path, custom: &Path, work: &Path) -> Result<usize> {
    let name: String = archive
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| !r#"\/:*?"<>|"#.contains(*c))
        .collect();
    let name = if name.trim().is_empty() {
        "Imported".to_owned()
    } else {
        name
    };
    let _ = fs::remove_dir_all(work);
    let unpacked = work.join("unpacked");
    seven_zip::extract(archive, &unpacked)?;
    let kept = work.join("kept");
    if lay_out(&Catalog::default(), &unpacked, &kept)? == 0 {
        let _ = fs::remove_dir_all(work);
        return Err(Error::NotInstallable(format!(
            "{} holds no ReShade effects.",
            archive.display()
        )));
    }
    let mut copied = 0;
    for top in [SHADERS, TEXTURES] {
        let from = kept.join(top);
        if from.is_dir() {
            for relative in files_under(&from)? {
                let target = custom.join(top).join(&name).join(&relative);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(Error::io(parent))?;
                }
                fs::copy(from.join(&relative), &target).map_err(Error::io(&target))?;
                copied += 1;
            }
        }
    }
    let _ = fs::remove_dir_all(work);
    Ok(copied)
}

/// The packs listed by name, for showing a selection ("Lilium HDR Shaders, SweetFX by CeeJay.dk").
pub fn names(catalog: &Catalog, selection: &Selection) -> Vec<String> {
    selection
        .to_deploy(catalog)
        .into_iter()
        .map(|(pack, _)| pack.name.clone())
        .chain(
            selection
                .custom
                .as_ref()
                .map(|_| "your own shaders".to_owned()),
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seven_zip::tests::{seven_zip, temp_dir};

    fn shipped() -> Catalog {
        serde_json::from_str(include_str!("../../../config/reshade-shaders.json")).unwrap()
    }

    /// `cargo test -p gm-core live_packs -- --ignored --nocapture`: downloads every pack into
    /// Game Manager's cache and reports how many effects each gives, and which failed.
    #[test]
    #[ignore = "downloads every pack (hundreds of MB) from GitHub and GitLab"]
    fn live_packs_download() {
        let catalog = shipped();
        let mut failed = Vec::new();
        for pack in &catalog.packs {
            match ensure(&catalog, pack, false) {
                Ok(dir) => {
                    let files = pack_files(&dir).unwrap().len();
                    println!(
                        "{:<28} {:>4} effects {:>5} files",
                        pack.id,
                        effects_in(&dir).len(),
                        files
                    );
                    if effects_in(&dir).is_empty() {
                        failed.push(format!("{}: no effects", pack.id));
                    }
                }
                Err(error) => failed.push(format!("{}: {error}", pack.id)),
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn the_shipped_list_is_valid() {
        let catalog = shipped();
        assert!(catalog.packs.len() > 50, "{}", catalog.packs.len());
        let mut ids = HashSet::new();
        for pack in &catalog.packs {
            assert!(ids.insert(pack.id.as_str()), "{} twice", pack.id);
            assert!(!pack.description.is_empty(), "{}", pack.id);
            match &pack.source {
                Source::Url { url } => assert!(url.starts_with("https://"), "{url}"),
                Source::GitHub { github, asset } => {
                    assert_eq!(github.matches('/').count(), 1, "{github}");
                    assert!(asset.starts_with('.'), "{asset}");
                }
            }
        }
        for pack in &catalog.packs {
            for required in &pack.requires {
                assert!(
                    catalog.pack(required).is_some(),
                    "{} needs {required}",
                    pack.id
                );
            }
        }
        let headers = catalog.headers_pack.as_deref().unwrap();
        assert!(catalog.pack(headers).is_some());
        assert_eq!(catalog.headers, ["ReShade.fxh", "ReShadeUI.fxh"]);
        assert!(catalog.excluded("grainspread.FX"));
        // Lilium is the essential pack; Lilium and crosire's master are selected until the user
        // chooses.
        let defaults = Selection::defaults(&catalog);
        assert_eq!(
            defaults.packs.keys().collect::<Vec<_>>(),
            ["CrosireMaster", "Lilium"]
        );
        assert_eq!(
            catalog.pack("Lilium").unwrap().category,
            Category::Essential
        );
    }

    #[test]
    fn a_selection_brings_its_requirements() {
        let catalog = shipped();
        let mut selection = Selection::default();
        selection.packs.insert("Azen".into(), Pick::default());
        selection.packs.insert(
            "CrosireLegacy".into(),
            Pick {
                files: Some(vec![r"Shaders\Bloom.fx".into()]),
            },
        );
        let deployed: Vec<&str> = selection
            .to_deploy(&catalog)
            .iter()
            .map(|(pack, _)| pack.id.as_str())
            .collect();
        // In the list's order: what was picked and what it requires; Lilium only when picked.
        assert_eq!(
            deployed,
            [
                "CrosireMaster",
                "CrosireLegacy",
                "SmolbbsoopShaders",
                "Azen"
            ]
        );
        let legacy = selection.packs["CrosireLegacy"].clone();
        assert!(legacy.takes(r"Shaders\Bloom.fx"));
        assert!(legacy.takes(r"shaders\BLOOM.fx"));
        assert!(!legacy.takes(r"Shaders\Other.fx"));
        // Headers and textures always come with the picked effects.
        assert!(legacy.takes(r"Shaders\Common.fxh"));
        assert!(legacy.takes(r"Textures\Dirt.png"));
    }

    fn archive(dir: &Path, name: &str, files: &[&str]) -> PathBuf {
        let source = dir.join(format!("{name}-src"));
        for file in files {
            let path = source.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, file).unwrap();
        }
        seven_zip(&source, &["a", "-tzip", &format!(r"..\{name}.zip"), "*"]);
        let unpacked = dir.join(format!("{name}-unpacked"));
        seven_zip::extract(&dir.join(format!("{name}.zip")), &unpacked).unwrap();
        unpacked
    }

    fn laid_out(dir: &Path) -> Vec<String> {
        let mut files: Vec<String> = pack_files(dir)
            .unwrap()
            .into_iter()
            .map(|file| file.to_string_lossy().into_owned())
            .collect();
        files.sort();
        files
    }

    #[test]
    fn packs_of_every_shape_are_laid_out_alike() {
        let dir = temp_dir("shader-layout");
        let catalog = shipped();
        // A branch archive with Shaders and Textures folders.
        let tidy = archive(
            &dir,
            "tidy",
            &[
                r"repo-main\Shaders\Bloom.fx",
                r"repo-main\Shaders\Sub\Common.fxh",
                r"repo-main\Shaders\GrainSpread.fx",
                r"repo-main\Textures\Dirt.png",
                r"repo-main\README.md",
            ],
        );
        lay_out(&catalog, &tidy, &dir.join("tidy-kept")).unwrap();
        assert_eq!(
            laid_out(&dir.join("tidy-kept")),
            [
                r"Shaders\Bloom.fx",
                r"Shaders\Sub\Common.fxh",
                r"Textures\Dirt.png"
            ]
        );
        // Effects loose in the repository, with an image next to them.
        let loose = archive(
            &dir,
            "loose",
            &[
                r"repo-main\Effect.fx",
                r"repo-main\lib\Helpers.fxh",
                r"repo-main\lut.cube",
                r"repo-main\notes.txt",
            ],
        );
        lay_out(&catalog, &loose, &dir.join("loose-kept")).unwrap();
        assert_eq!(
            laid_out(&dir.join("loose-kept")),
            [
                r"Shaders\Effect.fx",
                r"Shaders\lib\Helpers.fxh",
                r"Textures\lut.cube"
            ]
        );
        // Nothing usable.
        let empty = archive(&dir, "empty", &[r"repo-main\README.md"]);
        assert_eq!(
            lay_out(&catalog, &empty, &dir.join("empty-kept")).unwrap(),
            0
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_export_imports_into_the_custom_folder() {
        let dir = temp_dir("shader-import");
        // An export: reshade-shaders with Shaders and Textures.
        let source = dir.join("src");
        for file in [
            r"reshade-shaders\Shaders\Mine.fx",
            r"reshade-shaders\Textures\Lut.png",
        ] {
            let path = source.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        let archive = dir.join("My look.zip");
        seven_zip::zip_folder(&source.join("reshade-shaders"), &archive).unwrap();
        let custom = dir.join("custom");
        assert_eq!(
            import_into(&archive, &custom, &dir.join("work")).unwrap(),
            2
        );
        assert!(custom.join(r"Shaders\My look\Mine.fx").exists());
        assert!(custom.join(r"Textures\My look\Lut.png").exists());
        assert_eq!(effects_in(&custom), [r"Shaders\My look\Mine.fx"]);
        // Nothing usable in it.
        fs::write(dir.join("readme.txt"), b"text").unwrap();
        let empty = dir.join("empty.zip");
        seven_zip::zip_folder(&dir.join("readme.txt"), &empty).unwrap();
        assert!(import_into(&empty, &custom, &dir.join("work")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn staging_keeps_the_first_file_and_the_picked_effects() {
        let dir = temp_dir("shader-stage");
        let write = |pack: &str, files: &[&str]| {
            for file in files {
                let path = dir.join(pack).join(file);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, format!("{pack} {file}")).unwrap();
            }
        };
        write(
            "first",
            &[r"Shaders\ReShade.fxh", r"Shaders\A.fx", r"Textures\T.png"],
        );
        write(
            "second",
            &[
                r"Shaders\A.fx",
                r"Shaders\B.fx",
                r"Shaders\C.fx",
                r"Shaders\C.fxh",
            ],
        );
        let out = dir.join("out");
        let all = Pick::default();
        let only_b = Pick {
            files: Some(vec![r"Shaders\B.fx".into()]),
        };
        assert_eq!(
            copy_files(&dir.join("first"), &out, |file| all.takes(file)).unwrap(),
            3
        );
        assert_eq!(
            copy_files(&dir.join("second"), &out, |file| only_b.takes(file)).unwrap(),
            2
        );
        assert_eq!(
            fs::read_to_string(out.join(r"Shaders\A.fx")).unwrap(),
            r"first Shaders\A.fx"
        );
        assert!(out.join(r"Shaders\B.fx").exists() && out.join(r"Shaders\C.fxh").exists());
        assert!(!out.join(r"Shaders\C.fx").exists());
        assert_eq!(effects_in(&dir.join("second")).len(), 3);
        fs::remove_dir_all(dir).unwrap();
    }
}
