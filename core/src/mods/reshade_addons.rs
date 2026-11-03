//! ReShade's add-ons (ReShade ▸ Addons): `.addon32`/`.addon64` files ReShade's add-on
//! builds load from the game's folder. Three lists make the one shown:
//!
//! - **ReShade's own**, `Addons.ini` in crosire/reshade-shaders' `list` branch, read from the web
//!   (with the last copy kept for offline use), as ReShade's setup does. Its sections give a
//!   `PackageName`, `PackageDescription`, `DownloadUrl32`/`DownloadUrl64` or one ZIP in
//!   `DownloadUrl`, a `RepositoryUrl` and, for add-ons that bring effects, `EffectInstallPath`.
//!   Commented-out sections are not add-ons.
//! - **Extras** shipped in `reshade-addons.json` (copied from `config/`).
//! - **The user's own**, in `%APPDATA%\Game Manager\reshade\custom\Addons`.
//!
//! An add-on is downloaded once into `%LOCALAPPDATA%\Game Manager\mods\reshade\addons\<id>`: its
//! two files at the top, and its effects as `Shaders` and `Textures` (laid out as shader packs
//! are). A `.source` file names its version: the release tag, or the ETag of its download.

use super::shader_packs::{self, Catalog, SHADERS, TEXTURES};
use super::{files_under, get_json};
use crate::{Error, Result, config, http, json_file, seven_zip};
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const OFFICIAL_URL: &str =
    "https://raw.githubusercontent.com/crosire/reshade-shaders/list/Addons.ini";
const EXTRAS_FILE: &str = "reshade-addons.json";
const SOURCE_FILE: &str = ".source";
const OFFICIAL_COPY: &str = "Addons.ini";

/// Where an add-on comes from.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum AddonSource {
    /// The latest release of `owner/repo`: its `.addon32`/`.addon64` assets, or a ZIP.
    GitHub { github: String },
    /// A ZIP holding the add-on (and its effects).
    Zip { zip: String },
    /// Direct links to the two files (either may be missing).
    Files {
        #[serde(default)]
        url32: Option<String>,
        #[serde(default)]
        url64: Option<String>,
    },
    /// The user's own, in the custom folder.
    #[serde(skip)]
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Addon {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Its project page: "How to use".
    #[serde(default)]
    pub repository: Option<String>,
    pub source: AddonSource,
    /// Where its effects go below `reshade-shaders\Shaders` (`REST`); empty for the top.
    #[serde(default)]
    pub effects_folder: String,
}

#[derive(Deserialize)]
struct Extras {
    #[serde(default)]
    addons: Vec<Addon>,
}

/// Every add-on: ReShade's list (from the web, or its last copy), the shipped extras and the
/// user's own. Blocking: one web request.
pub fn list() -> Result<Vec<Addon>> {
    let copy = addons_dir()?.join(OFFICIAL_COPY);
    let official = match http::get(OFFICIAL_URL) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if let Some(parent) = copy.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = fs::write(&copy, &text);
            text
        }
        Err(error) => fs::read_to_string(&copy).map_err(|_| error)?,
    };
    let mut addons = parse_official(&official);
    addons.extend(extras()?);
    addons.extend(custom()?);
    Ok(addons)
}

fn extras() -> Result<Vec<Addon>> {
    let Some(file) = config::shipped_file(Path::new(EXTRAS_FILE)) else {
        return Ok(Vec::new());
    };
    let text = fs::read_to_string(&file).map_err(Error::io(&file))?;
    let extras: Extras = serde_json::from_str(&text).map_err(Error::json(EXTRAS_FILE))?;
    Ok(extras.addons)
}

/// The add-ons of ReShade's `Addons.ini`. Only `https` downloads are taken, and effects only
/// below `reshade-shaders\Shaders`.
fn parse_official(ini: &str) -> Vec<Addon> {
    let mut addons = Vec::new();
    let mut fields: Vec<(String, String)> = Vec::new();
    let take = |fields: &mut Vec<(String, String)>, addons: &mut Vec<Addon>| {
        let get = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };
        let https = |value: Option<String>| value.filter(|url| url.starts_with("https://"));
        let name = get("PackageName");
        let (url32, url64, url) = (
            https(get("DownloadUrl32")),
            https(get("DownloadUrl64")),
            https(get("DownloadUrl")),
        );
        let source = match (url32, url64, url) {
            (None, None, Some(url)) if url.to_ascii_lowercase().ends_with(".zip") => {
                Some(AddonSource::Zip { zip: url })
            }
            (None, None, Some(url)) => Some(AddonSource::Files {
                url32: None,
                url64: Some(url),
            }),
            (None, None, None) => None,
            (url32, url64, _) => Some(AddonSource::Files { url32, url64 }),
        };
        if let (Some(name), Some(source)) = (name, source) {
            let effects_folder = get("EffectInstallPath")
                .and_then(|path| effects_folder(&path))
                .unwrap_or_default();
            addons.push(Addon {
                id: format!("official-{}", slug(&name)),
                description: get("PackageDescription").unwrap_or_default(),
                repository: https(get("RepositoryUrl")),
                name,
                source,
                effects_folder,
            });
        }
        fields.clear();
    };
    for line in ini.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            take(&mut fields, &mut addons);
        } else if let Some((key, value)) = line.split_once('=') {
            fields.push((key.trim().to_owned(), value.trim().to_owned()));
        }
    }
    take(&mut fields, &mut addons);
    addons
}

/// `.\reshade-shaders\Shaders\REST` as `REST`; `None` outside `reshade-shaders\Shaders`.
fn effects_folder(path: &str) -> Option<String> {
    let path = path.replace('/', r"\");
    let rest = path
        .trim_start_matches(r".\")
        .get(r"reshade-shaders\Shaders".len()..)
        .filter(|_| {
            path.trim_start_matches(r".\")
                .to_ascii_lowercase()
                .starts_with(r"reshade-shaders\shaders")
        })?;
    let rest = rest.trim_start_matches('\\');
    let safe = rest
        .split('\\')
        .all(|part| !part.is_empty() && part != ".." && part != "." && !part.contains(':'));
    (rest.is_empty() || safe).then(|| rest.to_owned())
}

/// "Swap chain override by crosire" as `swap-chain-override-by-crosire`.
fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// The user's own add-ons: each name in `custom\Addons` with a `.addon32` and/or `.addon64`.
fn custom() -> Result<Vec<Addon>> {
    let folder = custom_dir()?;
    let mut names: Vec<String> = fs::read_dir(&folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| addon_bits(path).is_some())
                .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.dedup();
    Ok(names
        .into_iter()
        .map(|name| Addon {
            id: format!("custom-{name}"),
            description: "Your own add-on, from the custom folder.".to_owned(),
            repository: None,
            source: AddonSource::Custom,
            effects_folder: String::new(),
            name,
        })
        .collect())
}

/// `%APPDATA%\Game Manager\reshade\custom\Addons`, where the user's own add-ons go.
pub fn custom_dir() -> Result<PathBuf> {
    Ok(shader_packs::custom_dir()?.join("Addons"))
}

/// Whether `path` is a 64-bit (`.addon64`) or 32-bit (`.addon32`) add-on.
fn addon_bits(path: &Path) -> Option<bool> {
    let extension = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match extension.as_str() {
        "addon64" => Some(true),
        "addon32" => Some(false),
        _ => None,
    }
}

fn addons_dir() -> Result<PathBuf> {
    Ok(json_file::local_file("mods")?
        .join("reshade")
        .join("addons"))
}

fn addon_dir(addon: &Addon) -> Result<PathBuf> {
    if addon.source == AddonSource::Custom {
        return custom_dir();
    }
    let id: String = addon
        .id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "-_".contains(*c))
        .collect();
    Ok(addons_dir()?.join(id))
}

/// The cached add-on's version (release tag), when it is downloaded and has one.
pub fn version(addon: &Addon) -> Option<String> {
    let source = fs::read_to_string(addon_dir(addon).ok()?.join(SOURCE_FILE)).ok()?;
    let version = source.lines().nth(1)?.trim();
    // An ETag is no version to show.
    (!version.is_empty() && !version.starts_with('"') && !version.starts_with("W/"))
        .then(|| version.to_owned())
}

/// Whether the add-on is in the cache (the user's own always are).
pub fn is_cached(addon: &Addon) -> bool {
    addon.source == AddonSource::Custom
        || addon_dir(addon).is_ok_and(|dir| dir.join(SOURCE_FILE).is_file())
}

/// The add-on's file for a 64-bit or 32-bit game in its folder (`ensure`'s), if it has one.
pub fn file_for(addon: &Addon, dir: &Path, is_64_bit: bool) -> Option<(String, PathBuf)> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| addon_bits(path) == Some(is_64_bit))
        .filter(|path| {
            addon.source != AddonSource::Custom
                || path
                    .file_stem()
                    .is_some_and(|stem| stem.to_string_lossy() == addon.name)
        })
        .collect();
    found.sort();
    let path = found.into_iter().next()?;
    Some((path.file_name()?.to_string_lossy().into_owned(), path))
}

/// Copies the add-on's effects into `out` (a `reshade-shaders` folder), below its effects
/// folder, keeping files already there. Returns how many it copied.
pub fn stage_effects(addon: &Addon, dir: &Path, out: &Path) -> Result<usize> {
    if addon.source == AddonSource::Custom {
        return Ok(0);
    }
    let mut copied = 0;
    for (top, to) in [
        (SHADERS, out.join(SHADERS).join(&addon.effects_folder)),
        (TEXTURES, out.join(TEXTURES)),
    ] {
        let from = dir.join(top);
        if from.is_dir() {
            for relative in files_under(&from)? {
                let target = to.join(&relative);
                if target.exists() {
                    continue;
                }
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(Error::io(parent))?;
                }
                let source = from.join(&relative);
                fs::copy(&source, &target).map_err(Error::io(&target))?;
                copied += 1;
            }
        }
    }
    Ok(copied)
}

/// One download into the add-on cache at a time: they share its work folder.
static ADDON_LOCK: Mutex<()> = Mutex::new(());

/// The add-on's folder: downloaded into the cache when it is not there or, with `refresh`, when
/// its source changed (offline, the cached one stays). The user's own are in the custom folder.
/// Blocking.
pub fn ensure(addon: &Addon, refresh: bool) -> Result<PathBuf> {
    let dir = addon_dir(addon)?;
    if addon.source == AddonSource::Custom {
        return Ok(dir);
    }
    let _lock = ADDON_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let cached = fs::read_to_string(dir.join(SOURCE_FILE)).ok();
    if cached.is_some() && !refresh {
        return Ok(dir);
    }
    let (downloads, version) = match latest(addon) {
        Ok(latest) => latest,
        Err(_) if cached.is_some() => return Ok(dir),
        Err(error) => return Err(error),
    };
    let source = format!(
        "{}\n{version}",
        downloads
            .iter()
            .map(|(url, _)| url.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    );
    if !version.is_empty() && cached.as_deref() == Some(source.as_str()) {
        return Ok(dir);
    }
    let Some(root) = dir.parent() else {
        return Err(Error::NoModBuild(dir));
    };
    let work = root.join("download");
    let _ = fs::remove_dir_all(&work);
    let kept = work.join("addon");
    fs::create_dir_all(&kept).map_err(Error::io(&kept))?;
    // One of two links may be broken (ReShade's list has had such): the other still counts.
    let mut first_error = None;
    for (url, name) in &downloads {
        let download = work.join(name);
        if let Err(error) = http::download(url, &download) {
            first_error.get_or_insert(error);
            continue;
        }
        if addon_bits(&download).is_some() {
            let to = kept.join(name);
            fs::copy(&download, &to).map_err(Error::io(&to))?;
        } else {
            // A ZIP: its add-on files, and its effects laid out as a pack's.
            let unpacked = work.join("unpacked");
            seven_zip::extract(&download, &unpacked)?;
            for relative in files_under(&unpacked)? {
                let path = unpacked.join(&relative);
                if addon_bits(&path).is_some()
                    && let Some(file_name) = path.file_name()
                {
                    let to = kept.join(file_name);
                    fs::copy(&path, &to).map_err(Error::io(&to))?;
                }
            }
            shader_packs::lay_out(&Catalog::default(), &unpacked, &kept)?;
        }
    }
    let has_files = fs::read_dir(&kept)
        .map(|entries| {
            entries
                .flatten()
                .any(|entry| addon_bits(&entry.path()).is_some())
        })
        .unwrap_or(false);
    if !has_files {
        let _ = fs::remove_dir_all(&work);
        if cached.is_some() {
            return Ok(dir);
        }
        return Err(first_error.unwrap_or_else(|| Error::NoModBuild(PathBuf::from(&addon.name))));
    }
    let source_file = kept.join(SOURCE_FILE);
    fs::write(&source_file, &source).map_err(Error::io(&source_file))?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(Error::io(&dir))?;
    }
    fs::rename(&kept, &dir).map_err(Error::io(&dir))?;
    let _ = fs::remove_dir_all(&work);
    Ok(dir)
}

/// What to download now (address, file name) and the version. Blocking: one web request.
fn latest(addon: &Addon) -> Result<(Vec<(String, String)>, String)> {
    let named = |url: &str| {
        let name = url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or("addon.zip");
        (url.to_owned(), name.to_owned())
    };
    match &addon.source {
        AddonSource::GitHub { github } => {
            let url = format!("https://api.github.com/repos/{github}/releases/latest");
            let release = get_json(&url)?;
            let tag = release["tag_name"].as_str().unwrap_or_default().to_owned();
            let assets: Vec<(String, String)> = release["assets"]
                .as_array()
                .map(|assets| {
                    assets
                        .iter()
                        .filter_map(|asset| {
                            let name = asset["name"].as_str()?;
                            let link = asset["browser_download_url"].as_str()?;
                            Some((link.to_owned(), name.to_owned()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            let files: Vec<(String, String)> = assets
                .iter()
                .filter(|(_, name)| addon_bits(Path::new(name)).is_some())
                .cloned()
                .collect();
            let chosen = if files.is_empty() {
                assets
                    .into_iter()
                    .filter(|(_, name)| name.to_ascii_lowercase().ends_with(".zip"))
                    .take(1)
                    .collect()
            } else {
                files
            };
            if chosen.is_empty() {
                return Err(Error::NoRelease { url });
            }
            Ok((chosen, tag))
        }
        AddonSource::Zip { zip } => {
            let etag = http::etag(zip)?.unwrap_or_default();
            Ok((vec![named(zip)], etag))
        }
        AddonSource::Files { url32, url64 } => {
            // A plain `.addon` (which ReShade loads too) is named for the bitness its link is
            // for, so the game's is found.
            let with_bits = |url: &String, bits: &str| {
                let (url, name) = named(url);
                match name.strip_suffix(".addon") {
                    Some(stem) => (url, format!("{stem}.addon{bits}")),
                    None => (url, name),
                }
            };
            let urls: Vec<(String, String)> = [
                url32.as_ref().map(|url| with_bits(url, "32")),
                url64.as_ref().map(|url| with_bits(url, "64")),
            ]
            .into_iter()
            .flatten()
            .collect();
            let Some((last, _)) = urls.last() else {
                return Err(Error::NoRelease {
                    url: addon.name.clone(),
                });
            };
            let etag = http::etag(last)?.unwrap_or_default();
            Ok((urls, etag))
        }
        AddonSource::Custom => Ok((Vec::new(), String::new())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seven_zip::tests::{seven_zip, temp_dir};

    /// `cargo test -p gm-core live_addons -- --ignored --nocapture`: downloads every add-on into
    /// Game Manager's cache and reports what each holds.
    #[test]
    #[ignore = "downloads every add-on from GitHub"]
    fn live_addons_download() {
        let mut failed = Vec::new();
        for addon in list().unwrap() {
            match ensure(&addon, false) {
                Ok(dir) => {
                    let bits: Vec<&str> = [(false, "32"), (true, "64")]
                        .into_iter()
                        .filter(|(is_64_bit, _)| file_for(&addon, &dir, *is_64_bit).is_some())
                        .map(|(_, bits)| bits)
                        .collect();
                    let effects = files_under(&dir.join(SHADERS)).map_or(0, |files| files.len());
                    println!(
                        "{:<55} {:<6} effects {:>3} {}",
                        addon.name,
                        bits.join("+"),
                        effects,
                        version(&addon).unwrap_or_default()
                    );
                }
                Err(error) => failed.push(format!("{}: {error}", addon.name)),
            }
        }
        assert!(failed.is_empty(), "{failed:#?}");
    }

    #[test]
    fn reads_reshades_list_and_the_extras() {
        let ini = "[00]\r\nPackageName=Swap chain override by crosire\r\nPackageDescription=Forces windowed mode.\r\n\
            DownloadUrl32=https://x/swapchain_override.addon32\r\nDownloadUrl64=https://x/swapchain_override.addon64\r\n\
            RepositoryUrl=https://github.com/crosire/reshade\r\n\r\n\
            # [01]\r\n# PackageName=Framerate Limiter by crosire\r\n# DownloadUrl64=https://x/fps.addon64\r\n\
            [02]\r\nPackageName=ReshadeEffectShaderToggler (REST) by 4lex4nder\r\n\
            DownloadUrl=https://x/release.zip\r\nEffectInstallPath=.\\reshade-shaders\\Shaders\\REST\r\n\
            [03]\r\nPackageName=Nothing to download\r\n\
            [04]\r\nPackageName=Plain http\r\nDownloadUrl64=http://x/a.addon64\r\n\
            [05]\r\nPackageName=Escaping\r\nDownloadUrl=https://x/e.zip\r\nEffectInstallPath=..\\..\\Windows\r\n";
        let addons = parse_official(ini);
        let names: Vec<&str> = addons.iter().map(|addon| addon.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Swap chain override by crosire",
                "ReshadeEffectShaderToggler (REST) by 4lex4nder",
                "Escaping"
            ]
        );
        assert_eq!(addons[0].id, "official-swap-chain-override-by-crosire");
        assert!(matches!(
            &addons[0].source,
            AddonSource::Files {
                url32: Some(_),
                url64: Some(_)
            }
        ));
        assert_eq!(
            addons[1].source,
            AddonSource::Zip {
                zip: "https://x/release.zip".into()
            }
        );
        assert_eq!(addons[1].effects_folder, "REST");
        // An effects path outside reshade-shaders\Shaders is not taken.
        assert_eq!(addons[2].effects_folder, "");
        assert_eq!(
            effects_folder(r".\reshade-shaders\Shaders"),
            Some(String::new())
        );
        assert_eq!(effects_folder(r".\reshade-shaders\Shaders\..\x"), None);

        let extras: Extras =
            serde_json::from_str(include_str!("../../../config/reshade-addons.json")).unwrap();
        assert!(extras.addons.len() >= 7);
        for addon in &extras.addons {
            assert!(!addon.description.is_empty(), "{}", addon.id);
            assert!(!matches!(addon.source, AddonSource::Custom), "{}", addon.id);
        }
        let dlss = extras
            .addons
            .iter()
            .find(|addon| addon.id == "renodx-dlssfix")
            .unwrap();
        assert_eq!(
            dlss.source,
            AddonSource::Files {
                url32: None,
                url64: Some(
                    "https://github.com/clshortfuse/renodx/releases/download/snapshot/renodx-dlssfix.addon64"
                        .into()
                )
            }
        );
    }

    #[test]
    fn picks_the_file_for_the_games_bitness_and_its_effects() {
        let dir = temp_dir("addon-files");
        let addon_dir = dir.join("addon");
        fs::create_dir_all(addon_dir.join(r"Shaders\Sub")).unwrap();
        fs::write(addon_dir.join("REST.addon32"), b"32").unwrap();
        fs::write(addon_dir.join("REST.addon64"), b"64").unwrap();
        fs::write(addon_dir.join(r"Shaders\Sub\REST_FLIP.fx"), b"fx").unwrap();
        let addon = Addon {
            id: "rest".into(),
            name: "REST".into(),
            description: String::new(),
            repository: None,
            source: AddonSource::Zip { zip: String::new() },
            effects_folder: "REST".into(),
        };
        let (name, path) = file_for(&addon, &addon_dir, true).unwrap();
        assert_eq!(
            (name.as_str(), fs::read(path).unwrap()),
            ("REST.addon64", b"64".to_vec())
        );
        assert_eq!(
            file_for(&addon, &addon_dir, false).unwrap().0,
            "REST.addon32"
        );
        let out = dir.join("reshade-shaders");
        assert_eq!(stage_effects(&addon, &addon_dir, &out).unwrap(), 1);
        assert!(out.join(r"Shaders\REST\Sub\REST_FLIP.fx").exists());
        assert_eq!(slug("Undo/Redo by NotRayST"), "undo-redo-by-notrayst");

        // A ZIP like REST's release unpacks to its files and effects.
        let source = dir.join("zip-src");
        for file in [
            r"ReshadeEffectShaderToggler.addon64",
            r"reshade-shaders\Shaders\REST_NOOP.fx",
            "README.md",
        ] {
            let path = source.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        seven_zip(&source, &["a", "-tzip", r"..\release.zip", "*"]);
        seven_zip::extract(&dir.join("release.zip"), &dir.join("unpacked")).unwrap();
        let kept = dir.join("kept");
        shader_packs::lay_out(&Catalog::default(), &dir.join("unpacked"), &kept).unwrap();
        assert!(kept.join(r"Shaders\REST_NOOP.fx").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
