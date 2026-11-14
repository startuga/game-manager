//! ReShade (https://reshade.me): post-processing effects injected into a game, installed as one
//! of its graphics DLLs, or through ReShade's Vulkan layer (`reshade_layer`) in Vulkan games and
//! on top of DXVK. Three variants:
//!
//! - **Stable**: the setup "with full add-on support" from reshade.me,
//!   `downloads/ReShade_Setup_<version>_Addon.exe`, found on its home page.
//! - **No Addons**: the plain setup from the same page, `ReShade_Setup_<version>.exe`. Its build
//!   (`RESHADE_ADDON=1`) loads no add-ons, which some online games' anti-cheat requires.
//! - **Development**: the latest build of `main` by crosire/reshade's `build.yml` workflow, its
//!   "ReShade Setup" artifact through nightly.link, checked against the SHA-256 GitHub lists for it
//!   (as for DXVK). It is built with
//!   `RESHADE_ADDON=2`, full add-on support. Artifacts expire after 90 days.
//!
//! Every setup carries the DLLs in a ZIP appended to the program, whose offsets count from the
//! ZIP's own start: `appended_zip` cuts it out and 7-Zip unpacks it. The build is cached in
//! `%LOCALAPPDATA%\Game Manager\mods\reshade\<variant>-<version>` (`LAYER_FILES`).
//!
//! What goes into the game's folder, next to its program, follows ReShade's own setup:
//!
//! - **The DLL**: `ReShade32.dll` or `ReShade64.dll`, named after the API: `d3d9.dll` for
//!   Direct3D 9, `dxgi.dll` for 10, 11 and 12, `opengl32.dll` for OpenGL. Vulkan games, and
//!   games with DXVK (found in `mods.json`, or by a DLL whose product name is "DXVK"), use the
//!   Vulkan layer instead. Direct3D 8 needs d3d8to9 or DXVK, so it is refused without DXVK.
//! - **`ReShade.ini`**, only when there is none: the shader paths, the overlay key (Home) and no
//!   proxy DLL. ReShade saves its settings into it, so Reinstall keeps it once changed.
//! - **Effects**: the shaders chosen in ReShade ▸ Shaders (`shader_packs`, `reshade_settings`),
//!   laid out as `reshade-shaders\Shaders` and `\Textures`. That folder is Game Manager's: a
//!   game's own one is set aside as `reshade-shaders.gm-backup` on the first install and put back
//!   on uninstall, and Reinstall or Deploy replaces the whole folder (`DeleteTree` then
//!   `CopyTree`, two short operations a game).
//! - **Add-ons** chosen in ReShade ▸ Add-ons go next to the program, for its bitness, in builds
//!   that load add-ons.
//! - **Whose choice**: each game gets its own shaders or add-ons when it has them
//!   (`reshade_settings`), else the shared ones. `deploy` brings a changed shared choice to the
//!   games that follow it, or a game's own choice to that game.
//! - **In a protected folder**, `ReShade.ini`, `ReShadePreset.ini` and `ReShade.log` are created
//!   and given to the user, as ReShade's setup does, so ReShade can save into them.
//!
//! Uninstall removes all of it: the DLL (putting the game's own back), `ReShade.ini`,
//! `ReShadePreset.ini` and `ReShade.log` whoever wrote them, and the `reshade-shaders` folder
//! (putting a game's own back). That is more than ReShade's setup removes (it keeps the preset
//! and the textures).
//!
//! An install that changes the Vulkan layer runs as one `ReshadeJob` in a copy of Game Manager
//! started as administrator with `--reshade-ops`: the folder's files first, then the layer; a
//! failing layer change undoes the files.

use super::reshade_addons::{self, Addon};
use super::reshade_layer::{self, LAYER_FILES, LayerChange, LayerStatus};
use super::reshade_settings::{Part, ReshadeSettings};
use super::shader_packs::{Catalog, Selection};
use super::{
    BACKUP_SUFFIX, Build, FileOp, Installed, InstalledFile, ModsFile, SourceHashes, check_download,
    files_under, folder_writable, get_json, latest_run, plan, reshade_settings, run_artifact,
    run_then, shader_packs, update, with_suffix,
};
use crate::graphics::GraphicsApi;
use crate::text::{percent_decode, percent_encode};
use crate::{
    Arch, Error, Game, Result, elevate, http, json_file, security, seven_zip, version_info,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const HOME_PAGE: &str = "https://reshade.me/";
const RELEASES_PAGE: &str = "https://reshade.me/releases";
const API_URL: &str = "https://api.github.com/repos/crosire/reshade";
/// The development build's artifact.
const SETUP_ARTIFACT: &str = "ReShade Setup";
const CONFIG_FILE: &str = "ReShade.ini";
/// The files ReShade saves into while the game runs.
pub(crate) const SAVED_FILES: [&str; 3] = [CONFIG_FILE, "ReShadePreset.ini", "ReShade.log"];
/// The effects' folder in the game's folder.
pub(crate) const SHADERS_FOLDER: &str = "reshade-shaders";

/// The argument that makes `app.exe` run a `ReshadeJob` instead of its window.
pub const RESHADE_OPS_ARG: &str = "--reshade-ops";

/// What the Info dialog says about ReShade.
pub const INFO: &str = "ReShade adds post-processing effects to games: sharpening, color \
grading, ambient occlusion, depth of field, HDR analysis and more. Press Home in the game to \
open its overlay.

Variants:
• Stable: the latest release with full add-on support.
• Development: the latest build of ReShade's main branch, also with add-ons.
• No Addons: the latest release without add-on support, for online games whose anti-cheat \
accepts ReShade.

Install also adds the shaders and add-ons chosen in the ReShade tab's Shaders and Add-ons; \
Deploy in Shaders brings a new selection to every game with ReShade.

Vulkan games, and games with DXVK, use ReShade's Vulkan layer: one install for the whole \
computer, shared with ReShade's own setup; installing updates it for every game on its list.

Uninstall removes ReShade with its settings: ReShade.ini, ReShadePreset.ini, ReShade.log and \
the reshade-shaders folder (back up a preset you want to keep).

⚠ Add-on builds are for single-player games: anti-cheat may ban players who use them online.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReshadeVariant {
    #[default]
    Stable,
    Development,
    NoAddons,
}

impl ReshadeVariant {
    pub const ALL: [ReshadeVariant; 3] = [
        ReshadeVariant::Stable,
        ReshadeVariant::Development,
        ReshadeVariant::NoAddons,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ReshadeVariant::Stable => "Stable",
            ReshadeVariant::Development => "Development",
            ReshadeVariant::NoAddons => "No Addons",
        }
    }

    /// The name with where builds come from.
    pub fn label(self) -> &'static str {
        match self {
            ReshadeVariant::Stable => "Stable (reshade.me, with add-ons)",
            ReshadeVariant::Development => {
                "Development (latest main build, via nightly.link, with add-ons)"
            }
            ReshadeVariant::NoAddons => "No Addons (reshade.me, for online games)",
        }
    }

    /// Names the variant's builds in the cache.
    fn key(self) -> &'static str {
        match self {
            ReshadeVariant::Stable => "stable",
            ReshadeVariant::Development => "development",
            ReshadeVariant::NoAddons => "noaddons",
        }
    }
}

/// A game's ReShade in `mods.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReshadeState {
    #[serde(default)]
    pub variant: ReshadeVariant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<Installed>,
    /// Set while the game uses ReShade's Vulkan layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<LayerUse>,
    /// Game Manager's `reshade-shaders` folder in the game, while it is there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effects: Option<Effects>,
    /// The variant of the installed build (`variant` is the next install's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built: Option<ReshadeVariant>,
    /// The add-on files (ReShade ▸ Add-ons) Game Manager put next to the program.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addons: Option<Installed>,
}

/// The `reshade-shaders` folder Game Manager put into a game.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Effects {
    /// How many files it holds.
    pub files: usize,
    /// The game had a `reshade-shaders` of its own, kept as `reshade-shaders.gm-backup`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backup: bool,
}

impl ReshadeState {
    /// Whether the installed build loads add-ons: not the No Addons variant. An earlier
    /// version's record says so only in its version ("6.8.0 (no add-ons)").
    pub fn loads_addons(&self) -> bool {
        match self.built {
            Some(variant) => variant != ReshadeVariant::NoAddons,
            None => self
                .installed
                .as_ref()
                .is_some_and(|installed| !installed.version.ends_with("(no add-ons)")),
        }
    }

    /// Whether the game's `reshade-shaders` is Game Manager's: recorded as such, or (an earlier
    /// version's record) holding the effect files it listed.
    fn owns_effects(&self) -> bool {
        self.effects.is_some()
            || self
                .installed
                .as_ref()
                .is_some_and(|installed| !split_effects(Some(installed)).0.is_empty())
    }

    /// The DLL ReShade is installed as, when it is installed as one (not through the layer).
    pub fn dll(&self) -> Option<&str> {
        let installed = self.installed.as_ref().filter(|_| self.layer.is_none())?;
        installed
            .files
            .iter()
            .map(|file| file.name.as_str())
            .find(|name| Mode::DLLS.iter().any(|dll| dll.eq_ignore_ascii_case(name)))
    }
}

/// A game on ReShade's Vulkan layer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerUse {
    /// The program on the layer's list.
    pub program: PathBuf,
    /// Game Manager put it there (it was not listed before), so uninstalling takes it off.
    pub added: bool,
}

/// How ReShade gets into the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// As this DLL next to the program.
    Dll(&'static str),
    /// Through the Vulkan layer.
    Layer,
}

impl Mode {
    const DLLS: [&'static str; 3] = ["d3d9.dll", "dxgi.dll", "opengl32.dll"];
}

/// Where and how ReShade installs for a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub is_64_bit: bool,
    pub api: GraphicsApi,
    pub mode: Mode,
    pub program: PathBuf,
    /// The folder of the game's program.
    pub folder: PathBuf,
}

impl Target {
    /// "ReShade64.dll as dxgi.dll for DirectX 11", or "the Vulkan layer for Vulkan".
    pub fn summary(&self) -> String {
        match self.mode {
            Mode::Dll(dll) => format!("{} as {dll} for {}", self.module(), self.api.label()),
            Mode::Layer => format!("ReShade's Vulkan layer for {}", self.api.label()),
        }
    }

    fn module(&self) -> &'static str {
        if self.is_64_bit {
            "ReShade64.dll"
        } else {
            "ReShade32.dll"
        }
    }
}

/// Why ReShade cannot be installed in a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    NoProgram,
    UnknownBitness,
    Arm,
    NoApi,
    /// Direct3D 8 without DXVK.
    Direct3D8,
    /// The Vulkan layer cannot list a program whose path has a comma.
    Comma(String),
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocked::NoProgram => f.write_str("Choose the game program in Game settings first."),
            Blocked::UnknownBitness => f.write_str("Choose the game's bitness in Game settings."),
            Blocked::Arm => f.write_str("ReShade has no build for ARM programs."),
            Blocked::NoApi => f.write_str("Choose the game's graphics API in Game settings."),
            Blocked::Direct3D8 => f.write_str(
                "Direct3D 8 needs d3d8to9 or DXVK: install DXVK, then ReShade uses its Vulkan \
                 layer.",
            ),
            Blocked::Comma(program) => f.write_str(&reshade_layer::comma_message(program)),
        }
    }
}

/// What ReShade installs for this game on `api`, or why it can't; `dxvk` is whether DXVK is in
/// the game's folder.
pub fn target(game: &Game, api: Option<GraphicsApi>, dxvk: bool) -> Result<Target, Blocked> {
    let program = game.program.clone().ok_or(Blocked::NoProgram)?;
    let folder = program.parent().ok_or(Blocked::NoProgram)?.to_path_buf();
    let is_64_bit = match game.arch.ok_or(Blocked::UnknownBitness)? {
        Arch::X86 => false,
        Arch::X64 | Arch::AnyCpu => true,
        Arch::Arm64 | Arch::Arm => return Err(Blocked::Arm),
    };
    let api = api.ok_or(Blocked::NoApi)?;
    let mode = match api {
        _ if dxvk => Mode::Layer,
        GraphicsApi::Vulkan => Mode::Layer,
        GraphicsApi::DirectX8 => return Err(Blocked::Direct3D8),
        GraphicsApi::DirectX9 => Mode::Dll("d3d9.dll"),
        GraphicsApi::DirectX10 | GraphicsApi::DirectX11 | GraphicsApi::DirectX12 => {
            Mode::Dll("dxgi.dll")
        }
        GraphicsApi::OpenGl => Mode::Dll("opengl32.dll"),
    };
    if mode == Mode::Layer && program.to_string_lossy().contains(',') {
        return Err(Blocked::Comma(program.display().to_string()));
    }
    Ok(Target {
        is_64_bit,
        api,
        mode,
        program,
        folder,
    })
}

/// What the ReShade row needs that takes reading the disk: whether DXVK is in the game's folder
/// (which the game's graphics line shows too), and the Vulkan layer.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReshadeStatus {
    pub dxvk: bool,
    pub layer: LayerStatus,
}

/// The game's `ReshadeStatus`. Blocking: reads DLLs' version information and the layer.
pub fn status(game: &Game, mods: &ModsFile) -> ReshadeStatus {
    ReshadeStatus {
        dxvk: dxvk_present(game, mods),
        layer: reshade_layer::status(),
    }
}

/// DXVK is installed in the game's folder: by Game Manager, or by hand (a Direct3D DLL whose
/// product name is "DXVK", as ReShade's setup checks).
fn dxvk_present(game: &Game, mods: &ModsFile) -> bool {
    if mods
        .dxvk(&game.id)
        .is_some_and(|dxvk| dxvk.installed.is_some())
    {
        return true;
    }
    let Some(folder) = game.program.as_deref().and_then(Path::parent) else {
        return false;
    };
    ["d3d8.dll", "d3d9.dll", "d3d11.dll", "dxgi.dll"]
        .iter()
        .filter_map(|dll| version_info::product_name(&folder.join(dll)))
        .any(|name| name.eq_ignore_ascii_case("DXVK"))
}

/// Why DXVK cannot go where ReShade is installed as `dll`.
pub fn blocks_dxvk(dll: &str) -> String {
    format!(
        "ReShade is installed as {dll}: uninstall it, install DXVK, then install ReShade again \
         (it then uses the Vulkan layer)."
    )
}

/// The latest build of the variant. Blocking: one or two web requests.
pub fn latest(variant: ReshadeVariant) -> Result<Build> {
    match variant {
        ReshadeVariant::Stable | ReshadeVariant::NoAddons => {
            let add_ons = variant == ReshadeVariant::Stable;
            let page = String::from_utf8_lossy(&http::get(HOME_PAGE)?).into_owned();
            let version = setup_version(&page, add_ons).ok_or_else(|| Error::NoRelease {
                url: HOME_PAGE.to_owned(),
            })?;
            let file_name = if add_ons {
                format!("ReShade_Setup_{version}_Addon.exe")
            } else {
                format!("ReShade_Setup_{version}.exe")
            };
            Ok(Build {
                url: format!("{HOME_PAGE}downloads/{file_name}"),
                version,
                page: RELEASES_PAGE.to_owned(),
                file_name,
                sha256: None,
            })
        }
        ReshadeVariant::Development => {
            let url = format!(
                "{API_URL}/actions/workflows/build.yml/runs?branch=main&status=success&\
                 event=push&per_page=1"
            );
            let (run, commit) = latest_run(&get_json(&url)?).ok_or(Error::NoRelease { url })?;
            let url = format!("{API_URL}/actions/runs/{run}/artifacts");
            let (_, digest) = run_artifact(&get_json(&url)?, |name| name == SETUP_ARTIFACT)
                .ok_or(Error::NoRelease { url: url.clone() })?;
            let sha256 = Some(digest.ok_or(Error::NoDigest { url })?);
            Ok(Build {
                version: format!("main-{}", commit.get(..7).unwrap_or(&commit)),
                page: format!("https://github.com/crosire/reshade/commit/{commit}"),
                url: format!(
                    "https://nightly.link/crosire/reshade/actions/runs/{run}/{}.zip",
                    SETUP_ARTIFACT.replace(' ', "%20")
                ),
                file_name: format!("{SETUP_ARTIFACT}.zip"),
                sha256,
            })
        }
    }
}

/// The version of the setup reshade.me's page links to: `ReShade_Setup_<version>_Addon.exe` with
/// `add_ons`, else `ReShade_Setup_<version>.exe`.
fn setup_version(page: &str, add_ons: bool) -> Option<String> {
    page.split("ReShade_Setup_").skip(1).find_map(|rest| {
        let name = &rest[..rest.find(".exe")?];
        let version = if add_ons {
            name.strip_suffix("_Addon")?
        } else {
            name
        };
        let valid = !version.is_empty()
            && version.chars().all(|c| c.is_ascii_digit() || c == '.')
            && version.chars().next().is_some_and(|c| c.is_ascii_digit());
        valid.then(|| version.to_owned())
    })
}

/// The ZIP appended to a ReShade setup program. Its offsets count from its own start: that is
/// where its central directory is, minus the offset the directory states.
fn appended_zip(bytes: &[u8]) -> Option<&[u8]> {
    const END: &[u8] = b"PK\x05\x06";
    const LOCAL_FILE: &[u8] = b"PK\x03\x04";
    let u32_at = |at: usize| -> Option<usize> {
        let field: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
        Some(u32::from_le_bytes(field) as usize)
    };
    let last = bytes.len().checked_sub(22)?;
    // The end record is 22 bytes plus a comment of at most 65 535.
    let first = last.saturating_sub(u16::MAX as usize);
    let end = (first..=last).rev().find(|&at| &bytes[at..at + 4] == END)?;
    let directory = end.checked_sub(u32_at(end + 12)?)?;
    let start = directory.checked_sub(u32_at(end + 16)?)?;
    let zip = &bytes[start..];
    zip.starts_with(LOCAL_FILE).then_some(zip)
}

/// One download into the cache at a time.
static BUILD_LOCK: Mutex<()> = Mutex::new(());
/// The cache folder's layout: `LAYER_FILES` at the top. A folder without this number in its
/// `.format` file is downloaded again.
const CACHE_FORMAT: &str = "1";
const FORMAT_FILE: &str = ".format";

/// Where a variant's version is cached.
fn build_dir(variant: ReshadeVariant, version: &str) -> Result<PathBuf> {
    let version: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c))
        .collect();
    Ok(json_file::local_file("mods")?
        .join("reshade")
        .join(format!("{}-{version}", variant.key())))
}

/// The build's folder in the cache, holding `LAYER_FILES`: downloaded and unpacked the first
/// time. Blocking.
pub(crate) fn cached_build(variant: ReshadeVariant, build: &Build) -> Result<PathBuf> {
    let _lock = BUILD_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = build_dir(variant, &build.version)?;
    let complete =
        fs::read_to_string(dir.join(FORMAT_FILE)).is_ok_and(|format| format.trim() == CACHE_FORMAT);
    if complete {
        return Ok(dir);
    }
    let Some(root) = dir.parent() else {
        return Err(Error::NoModBuild(dir));
    };
    let work = root.join("download");
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(Error::io(&work))?;
    let download = work.join(&build.file_name);
    http::download(&build.url, &download)?;
    check_download(build, &download)?;
    // A development build comes as a ZIP holding the setup.
    let setup = if build.file_name.to_ascii_lowercase().ends_with(".zip") {
        let artifact = work.join("artifact");
        seven_zip::extract(&download, &artifact)?;
        files_with_extension(&artifact, "exe")
            .into_iter()
            .next()
            .ok_or_else(|| Error::NoModBuild(download.clone()))?
    } else {
        download
    };
    let bytes = fs::read(&setup).map_err(Error::io(&setup))?;
    let zip = appended_zip(&bytes).ok_or_else(|| Error::NoModBuild(setup.clone()))?;
    let zip_file = work.join("setup.zip");
    fs::write(&zip_file, zip).map_err(Error::io(&zip_file))?;
    let unpacked = work.join("unpacked");
    seven_zip::extract(&zip_file, &unpacked)?;
    // Moved into place last, so the cache never holds half a build.
    let kept = work.join("build");
    fs::create_dir_all(&kept).map_err(Error::io(&kept))?;
    for name in LAYER_FILES {
        let (from, to) = (unpacked.join(name), kept.join(name));
        if !from.is_file() {
            return Err(Error::NoModBuild(setup));
        }
        fs::copy(&from, &to).map_err(Error::io(&to))?;
    }
    let format = kept.join(FORMAT_FILE);
    fs::write(&format, CACHE_FORMAT).map_err(Error::io(&format))?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(Error::io(&dir))?;
    }
    fs::rename(&kept, &dir).map_err(Error::io(&dir))?;
    let _ = fs::remove_dir_all(&work);
    Ok(dir)
}

fn files_with_extension(folder: &Path, extension: &str) -> Vec<PathBuf> {
    fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case(extension))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `ReShade.ini` Game Manager writes when the game has none, as ReShade's setup does.
fn config_text() -> String {
    [
        "[GENERAL]",
        r"EffectSearchPaths=.\reshade-shaders\Shaders\**",
        r"TextureSearchPaths=.\reshade-shaders\Textures\**",
        "",
        "[INPUT]",
        "KeyOverlay=36,0,0,0",
        "GamepadNavigation=0",
        "",
        "[PROXY]",
        "EnableProxyLibrary=0",
        "ProxyLibrary=",
        "",
    ]
    .join("\r\n")
}

/// Work for the folder and the Vulkan layer, run as administrator as one (`--reshade-ops`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReshadeJob {
    ops: Vec<FileOp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer: Option<LayerChange>,
    /// The SHA-256 of every file the job copies, the layer's DLLs too: filled in before asking
    /// for administrator rights (see `super::SourceHashes`).
    #[serde(default)]
    sources: SourceHashes,
}

/// Runs `job`: here when it needs no rights Game Manager lacks, else in a copy of Game Manager
/// running as administrator. Blocking.
fn run_job(job: &ReshadeJob) -> Result<()> {
    match &job.layer {
        None => super::apply(&job.ops),
        Some(change) if security::is_elevated() => {
            run_then(&job.ops, || reshade_layer::apply(change, None))
        }
        Some(change) => {
            // Said here, with the folder's name; the copy as administrator checks it again.
            reshade_layer::check_system_folder()?;
            let mut job = job.clone();
            job.sources = SourceHashes::of(&job.ops)?;
            if let Some(build) = &change.install_from {
                for name in reshade_layer::LAYER_FILES {
                    job.sources.add(&build.join(name))?;
                }
            }
            let json = serde_json::to_string(&job).map_err(Error::json("ReShade's changes"))?;
            elevate::run(RESHADE_OPS_ARG, &percent_encode(&json))
        }
    }
}

/// Runs the job passed with `--reshade-ops`, once its operations and the layer build it installs
/// from are checked (`super::vet`, `elevate::check_paths`). The result is the program's exit
/// code: 0, or the HRESULT of what failed or was refused.
pub fn run_encoded(encoded: &str) -> u32 {
    let job =
        percent_decode(encoded).and_then(|json| serde_json::from_str::<ReshadeJob>(&json).ok());
    let Some(job) = job else {
        return windows::Win32::E_INVALIDARG.0 as u32;
    };
    let build = job
        .layer
        .as_ref()
        .and_then(|change| change.install_from.as_deref());
    let vetted = super::vet(&job.ops).and_then(|()| match build {
        Some(build) => {
            let files: Vec<PathBuf> = reshade_layer::LAYER_FILES
                .iter()
                .map(|name| build.join(name))
                .collect();
            elevate::check_paths(files.iter().map(PathBuf::as_path))
        }
        None => Ok(()),
    });
    let sources = Some(&job.sources);
    super::exit_code(vetted.and_then(|()| {
        super::run_with(&job.ops, sources, || match &job.layer {
            Some(change) => reshade_layer::apply(change, sources),
            None => Ok(()),
        })
    }))
}

/// What an install puts where: the job, and what to record.
struct Prepared {
    job: ReshadeJob,
    installed: Installed,
    layer: Option<LayerUse>,
    effects: Effects,
}

/// Where the install gets its files: the cached build, and the effects laid out as they go into
/// the game's folder (a folder holding `reshade-shaders`).
struct Sources<'a> {
    build: &'a Path,
    effects: &'a Path,
    /// For the files Game Manager writes (`ReShade.ini`).
    staging: &'a Path,
}

/// The operations that put the effects laid out in `effects` (a folder holding
/// `reshade-shaders`) into `folder` over `previous`'s, and the record of them. Game Manager's
/// earlier folder goes whole; a game's own one is set aside as `reshade-shaders.gm-backup`.
fn effects_ops(
    previous: &ReshadeState,
    folder: &Path,
    effects: &Path,
) -> Result<(Vec<FileOp>, Effects)> {
    let mut ops = Vec::new();
    let old_folder = previous.installed.as_ref().map(|old| old.folder.as_path());
    let here = old_folder == Some(folder);
    let ours = previous.owns_effects();
    let mut backup = here
        && previous
            .effects
            .as_ref()
            .is_some_and(|effects| effects.backup);
    // An install in another folder (the program moved): its effects go there.
    if let Some(old) = old_folder.filter(|_| ours && !here) {
        ops.extend(effects_removal(previous, old));
    }
    let shaders = folder.join(SHADERS_FOLDER);
    if shaders.exists() {
        if ours && here {
            ops.push(FileOp::DeleteTree {
                path: shaders.clone(),
            });
        } else {
            let aside = with_suffix(&shaders, BACKUP_SUFFIX);
            if aside.exists() {
                return Err(Error::NotInstallable(format!(
                    "{} already exists: put it back or delete it first.",
                    aside.display()
                )));
            }
            ops.push(FileOp::Move {
                from: shaders,
                to: aside,
            });
            backup = true;
        }
    }
    let files = files_under(&effects.join(SHADERS_FOLDER)).map_or(0, |files| files.len());
    if files > 0 {
        ops.push(FileOp::CopyTree {
            from: effects.to_path_buf(),
            to: folder.to_path_buf(),
        });
    }
    Ok((ops, Effects { files, backup }))
}

/// The operations that remove the `reshade-shaders` folder from `folder`, and put back a game's
/// own one Game Manager set aside.
fn effects_removal(state: &ReshadeState, folder: &Path) -> Vec<FileOp> {
    let shaders = folder.join(SHADERS_FOLDER);
    let aside = with_suffix(&shaders, BACKUP_SUFFIX);
    let mut ops = Vec::new();
    if shaders.is_dir() {
        ops.push(FileOp::DeleteTree {
            path: shaders.clone(),
        });
    }
    if state.effects.as_ref().is_some_and(|effects| effects.backup) && aside.is_dir() {
        ops.push(FileOp::Move {
            from: aside,
            to: shaders,
        });
    }
    ops
}

/// Plans installing `target` over `previous`. `sid` is the user's, when the folder is protected
/// and ReShade's saved files must be given to them.
fn prepare(
    previous: &ReshadeState,
    target: &Target,
    sources: &Sources,
    build: &Build,
    layer: &LayerStatus,
    sid: Option<&str>,
) -> Result<Prepared> {
    let folder = &target.folder;
    let old = previous.installed.as_ref();
    // The effects an earlier version recorded file by file go with their folder (`effects_ops`).
    let (_, old_files) = split_effects(old);
    // Game Manager's file `name` in this folder, unchanged, which the plan below removes.
    let ours_here = |name: &str| {
        old.is_some_and(|old| old.folder == *folder)
            && old_files
                .files
                .iter()
                .any(|file| file.name.eq_ignore_ascii_case(name) && file.is_at(&folder.join(name)))
    };

    // The DLL and ReShade.ini, through `plan` (which also removes the earlier ones, wherever
    // they were).
    let mut files = Vec::new();
    if let Mode::Dll(dll) = target.mode {
        files.push((dll.to_owned(), sources.build.join(target.module())));
    }
    if !folder.join(CONFIG_FILE).exists() || ours_here(CONFIG_FILE) {
        let staged = sources.staging.join(CONFIG_FILE);
        fs::write(&staged, config_text()).map_err(Error::io(&staged))?;
        files.push((CONFIG_FILE.to_owned(), staged));
    }
    let plan = plan(old.map(|_| &old_files), folder, &files)?;
    let mut ops = plan.ops;
    let mut recorded = plan.files;

    // Effects: the whole reshade-shaders folder.
    let (effect_ops, effects) = effects_ops(previous, folder, sources.effects)?;
    ops.extend(effect_ops);

    // In a protected folder, ReShade's saved files are given to the user (created when missing).
    if let Some(sid) = sid {
        for name in SAVED_FILES {
            let path = folder.join(name);
            let coming = recorded
                .iter()
                .any(|file| file.name.eq_ignore_ascii_case(name));
            // Created by the operation: recorded, so an untouched one goes on uninstall.
            if !coming && (!path.exists() || ours_here(name)) {
                recorded.push(InstalledFile::empty(name));
            }
            ops.push(FileOp::Writable {
                path,
                sid: sid.to_owned(),
            });
        }
    }

    // The Vulkan layer: added to (or updated) for this game, or left.
    let mut change = LayerChange::default();
    let mut layer_use = None;
    if target.mode == Mode::Layer {
        change.install_from = Some(sources.build.to_path_buf());
        let ours_before = previous
            .layer
            .as_ref()
            .is_some_and(|before| before.added && before.program == target.program);
        if !layer.lists(&target.program) {
            change.add_app = Some(target.program.clone());
        }
        layer_use = Some(LayerUse {
            program: target.program.clone(),
            added: ours_before || !layer.lists(&target.program),
        });
    }
    // A program Game Manager listed before, and no longer uses, comes off the list.
    if let Some(before) = previous.layer.as_ref().filter(|before| before.added)
        && layer_use
            .as_ref()
            .is_none_or(|now| now.program != before.program)
    {
        change.remove_app = Some(before.program.clone());
    }
    let changes_layer = change != LayerChange::default();
    Ok(Prepared {
        job: ReshadeJob {
            ops,
            layer: changes_layer.then_some(change),
            sources: SourceHashes::default(),
        },
        installed: Installed {
            version: build.version.clone(),
            page: build.page.clone(),
            folder: folder.clone(),
            files: recorded,
            backups: plan.backups,
        },
        layer: layer_use,
        effects,
    })
}

/// A record's effects (under `reshade-shaders\`), and the record without them.
fn split_effects(installed: Option<&Installed>) -> (Vec<InstalledFile>, Installed) {
    let Some(installed) = installed else {
        return (Vec::new(), Installed::default());
    };
    let prefix = format!(r"{SHADERS_FOLDER}\");
    let (effects, files) = installed.files.iter().cloned().partition(|file| {
        file.name
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(&prefix))
    });
    (
        effects,
        Installed {
            files,
            ..installed.clone()
        },
    )
}

/// The job that removes `state`'s install with everything ReShade keeps in the game's folder,
/// and takes the game off the layer if Game Manager put it there.
fn removal(state: &ReshadeState) -> Result<ReshadeJob> {
    let mut ops = Vec::new();
    if let Some(installed) = &state.installed {
        let folder = &installed.folder;
        // The DLL goes and the game's own files come back, through `plan`.
        let (_, files) = split_effects(Some(installed));
        ops = plan(Some(&files), folder, &[])?.ops;
        // So do ReShade's settings, preset and log, whoever wrote them (`plan` keeps changed
        // files), and the whole effects folder (a game's own one comes back).
        let saved: Vec<String> = SAVED_FILES
            .iter()
            .filter(|name| folder.join(name).is_file())
            .map(|name| (*name).to_owned())
            .collect();
        if !saved.is_empty() {
            ops.push(FileOp::DeleteFiles {
                folder: folder.clone(),
                names: saved,
            });
        }
        ops.extend(effects_removal(state, folder));
        if let Some(addons) = &state.addons {
            ops.extend(plan(Some(addons), &addons.folder, &[])?.ops);
        }
    }
    let layer = state
        .layer
        .as_ref()
        .filter(|layer| layer.added)
        .map(|layer| LayerChange {
            remove_app: Some(layer.program.clone()),
            ..LayerChange::default()
        });
    Ok(ReshadeJob {
        ops,
        layer,
        sources: SourceHashes::default(),
    })
}

fn state(game: &Game) -> Result<(ModsFile, ReshadeState)> {
    let mods = super::load()?;
    let state = mods.reshade(&game.id).cloned().unwrap_or_default();
    Ok((mods, state))
}

/// A fresh folder in Game Manager's staging area, for laying out what a job copies.
fn fresh_staging(name: &str) -> Result<PathBuf> {
    let staging = json_file::local_file("mods")?.join("staging").join(name);
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(Error::io(&staging))?;
    Ok(staging)
}

/// A choice of shaders and add-ons, laid out for a job.
struct Chosen {
    /// The add-ons switched on, with their folders.
    addons: Vec<(Addon, PathBuf)>,
    /// What could not be had: "Name: what failed".
    failed: Vec<String>,
}

/// Lays out `selection` under `tree`, with the effects of the add-ons `addon_ids` (found in
/// `list`), and gets those add-ons. Blocking: downloads what is not cached and what changed.
fn stage_choice(
    tree: &Path,
    catalog: &Catalog,
    selection: &Selection,
    addon_ids: &[String],
    list: &[Addon],
) -> Result<Chosen> {
    let staged = shader_packs::stage(catalog, selection, tree, true)?;
    let mut failed = staged.failed;
    let mut addons = Vec::new();
    for id in addon_ids {
        let Some(addon) = list.iter().find(|addon| addon.id == *id) else {
            continue;
        };
        match reshade_addons::ensure(addon, true) {
            Ok(dir) => {
                reshade_addons::stage_effects(addon, &dir, &tree.join(SHADERS_FOLDER))?;
                addons.push((addon.clone(), dir));
            }
            Err(error) => failed.push(format!("{}: {error}", addon.name)),
        }
    }
    Ok(Chosen { addons, failed })
}

/// The add-on list, read only when `needed` (an add-on is chosen). Blocking: a web request.
fn addon_list(needed: bool) -> Result<Vec<Addon>> {
    if needed {
        reshade_addons::list()
    } else {
        Ok(Vec::new())
    }
}

/// The operations that put the chosen add-ons' files for a 64-bit or 32-bit game into `folder`,
/// replacing `previous`'s, and the record of them (`None` when there are none). A build without
/// add-on support gets none. An add-on without a file for the game's bitness is named in
/// `missing`.
fn addon_ops(
    previous: &ReshadeState,
    folder: &Path,
    is_64_bit: bool,
    addons: &[(Addon, PathBuf)],
    missing: &mut Vec<String>,
) -> Result<(Vec<FileOp>, Option<Installed>)> {
    let mut files = Vec::new();
    if previous.loads_addons() {
        for (addon, dir) in addons {
            match reshade_addons::file_for(addon, dir, is_64_bit) {
                Some(file) => files.push(file),
                None => missing.push(format!(
                    "{}: no {}-bit build",
                    addon.name,
                    if is_64_bit { 64 } else { 32 }
                )),
            }
        }
    }
    let plan = plan(previous.addons.as_ref(), folder, &files)?;
    let record = (!plan.files.is_empty() || !plan.backups.is_empty()).then(|| Installed {
        version: String::new(),
        page: String::new(),
        folder: folder.to_path_buf(),
        files: plan.files,
        backups: plan.backups,
    });
    Ok((plan.ops, record))
}

/// The error for a job that did its work but left some of it out.
fn incomplete(done: &str, failed: &[String]) -> Error {
    Error::Incomplete(format!("{done}, but not all of it:\n{}", failed.join("\n")))
}

/// Installs the latest build of the game's ReShade variant for `api`, with the shaders and add-ons
/// chosen for it (its own, else the shared ones), replacing an earlier install, and returns
/// `mods.json` as saved. Blocking: downloads, 7-Zip and file copies (as administrator when the
/// folder or the Vulkan layer needs it). Shader packs that cannot be downloaded are left out, and
/// named in an `Error::Incomplete` after the install.
pub fn install(game: &Game, api: GraphicsApi) -> Result<ModsFile> {
    let (mods, state) = state(game)?;
    let target = target(game, Some(api), dxvk_present(game, &mods))
        .map_err(|blocked| Error::NotInstallable(blocked.to_string()))?;
    let build = latest(state.variant)?;
    let cache = cached_build(state.variant, &build)?;
    let staging = fresh_staging("reshade")?;
    let effects = staging.join("effects");
    let catalog = shader_packs::catalog()?;
    let settings = reshade_settings::load()?;
    let addon_ids = settings.addons_for(&game.id);
    let chosen = stage_choice(
        &effects,
        &catalog,
        &settings.shaders_for(&game.id, &catalog),
        addon_ids,
        &addon_list(!addon_ids.is_empty())?,
    )?;
    let mut failed = chosen.failed;
    let sid = if folder_writable(&target.folder)? {
        None
    } else {
        Some(security::current_user_sid()?)
    };
    let sources = Sources {
        build: &cache,
        effects: &effects,
        staging: &staging,
    };
    let mut prepared = prepare(
        &state,
        &target,
        &sources,
        &build,
        &reshade_layer::status(),
        sid.as_deref(),
    )?;
    // The chosen add-ons, for the build being installed.
    let built = ReshadeState {
        built: Some(state.variant),
        ..state.clone()
    };
    let (ops, addons) = addon_ops(
        &built,
        &target.folder,
        target.is_64_bit,
        &chosen.addons,
        &mut failed,
    )?;
    prepared.job.ops.extend(ops);
    run_job(&prepared.job)?;
    let _ = fs::remove_dir_all(&staging);
    let mut installed = prepared.installed;
    if state.variant == ReshadeVariant::NoAddons {
        installed.version = format!("{} (no add-ons)", installed.version);
    }
    let saved = update(&game.id, |mods| {
        let reshade = mods.reshade.get_or_insert_with(Default::default);
        reshade.installed = Some(installed);
        reshade.layer = prepared.layer;
        reshade.effects = Some(prepared.effects);
        reshade.built = Some(state.variant);
        reshade.addons = addons;
    })?;
    if failed.is_empty() {
        Ok(saved)
    } else {
        Err(incomplete("ReShade is installed", &failed))
    }
}

/// Removes the game's ReShade (and its place on the Vulkan layer) and puts back the files it
/// replaced; returns `mods.json` as saved.
pub fn uninstall(game: &Game) -> Result<ModsFile> {
    let (_, state) = state(game)?;
    run_job(&removal(&state)?)?;
    update(&game.id, |mods| {
        if let Some(reshade) = &mut mods.reshade {
            reshade.installed = None;
            reshade.layer = None;
            reshade.effects = None;
            reshade.built = None;
            reshade.addons = None;
        }
        drop_default(&mut mods.reshade);
    })
}

/// Which games a deploy reaches (among those where Game Manager installed ReShade).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeployTo {
    /// Every game that follows the shared choice of this part: the shared one changed.
    Sharing(Part),
    /// One game: its own choice changed, or it went back to the shared one.
    Game(String),
}

/// The ids of the games with ReShade that `to` reaches.
fn deploy_targets(mods: &ModsFile, settings: &ReshadeSettings, to: &DeployTo) -> Vec<String> {
    mods.games
        .iter()
        .filter(|(_, entry)| {
            entry
                .reshade
                .as_ref()
                .is_some_and(|state| state.installed.is_some())
        })
        .filter(|(id, _)| match to {
            DeployTo::Sharing(part) => !settings.has_own(id, *part),
            DeployTo::Game(game) => *id == game,
        })
        .map(|(id, _)| id.clone())
        .collect()
}

/// Brings the shaders and add-ons chosen for them (each game's own, else the shared ones) to the
/// games `to` reaches, replacing each one's `reshade-shaders` and add-on files, and returns
/// `mods.json` as saved. Each different choice is laid out once. Writable folders are changed
/// directly, the protected ones together as administrator (one UAC prompt). `games` names them in
/// messages and gives their bitness (a game not among them keeps its add-ons). Blocking:
/// downloads, 7-Zip and file copies. What could not be done is named in an `Error::Incomplete`,
/// after the rest was saved: a game whose choice can't be laid out (a disk error) fails alone, and
/// every game changed is recorded, so `mods.json` always says what each folder holds.
pub fn deploy(games: &[Game], to: &DeployTo) -> Result<ModsFile> {
    let mods = super::load()?;
    let settings = reshade_settings::load()?;
    let targets = deploy_targets(&mods, &settings, to);
    if targets.is_empty() {
        return Ok(mods);
    }
    let catalog = shader_packs::catalog()?;
    let list = addon_list(targets.iter().any(|id| !settings.addons_for(id).is_empty()))?;
    let staging = fresh_staging("reshade-deploy")?;
    let mut failed: Vec<String> = Vec::new();
    /// A different choice, laid out once in its own folder: its shaders and add-ons, the folder,
    /// and the add-ons' files.
    type LaidOut<'a> = (Selection, &'a [String], PathBuf, Vec<(Addon, PathBuf)>);
    let mut choices: Vec<LaidOut> = Vec::new();
    let mut laid_out = 0;
    let name = |id: &str| {
        games
            .iter()
            .find(|game| game.id == id)
            .map_or_else(|| id.to_owned(), |game| game.name.clone())
    };
    /// What a game gets: its effects' record, and its add-ons' (unchanged when `None`).
    type Records = (Effects, Option<Option<Installed>>);
    let mut done: Vec<(String, Records)> = Vec::new();
    let mut protected: Vec<(String, Vec<FileOp>, Records)> = Vec::new();
    for id in &targets {
        let Some(state) = mods.reshade(id) else {
            continue;
        };
        let Some(installed) = state.installed.as_ref().filter(|i| i.folder.is_dir()) else {
            continue;
        };
        let selection = settings.shaders_for(id, &catalog);
        let addon_ids = settings.addons_for(id);
        let index = match choices
            .iter()
            .position(|(shaders, addons, _, _)| *shaders == selection && *addons == addon_ids)
        {
            Some(index) => index,
            None => {
                // A folder of its own each time: a failed layout leaves its files behind.
                let tree = staging.join(format!("choice-{laid_out}"));
                laid_out += 1;
                // Games before this one may be changed already: a choice that can't be laid out
                // fails this game alone, so theirs are still recorded below.
                let chosen = match stage_choice(&tree, &catalog, &selection, addon_ids, &list) {
                    Ok(chosen) => chosen,
                    Err(error) => {
                        failed.push(format!("{}: {error}", name(id)));
                        continue;
                    }
                };
                for what in chosen.failed {
                    if !failed.contains(&what) {
                        failed.push(what);
                    }
                }
                choices.push((selection, addon_ids, tree, chosen.addons));
                choices.len() - 1
            }
        };
        let (_, _, effects, addons) = &choices[index];
        let planned =
            effects_ops(state, &installed.folder, effects).and_then(|(mut ops, effects)| {
                let bits = games
                    .iter()
                    .find(|game| game.id == *id)
                    .and_then(|game| game.arch)
                    .map(|arch| !matches!(arch, Arch::X86));
                let addons = match bits {
                    Some(is_64_bit) => {
                        let mut missing = Vec::new();
                        let (more, record) =
                            addon_ops(state, &installed.folder, is_64_bit, addons, &mut missing)?;
                        ops.extend(more);
                        failed.extend(
                            missing
                                .into_iter()
                                .map(|what| format!("{}: {what}", name(id))),
                        );
                        Some(record)
                    }
                    None => None,
                };
                Ok((ops, (effects, addons)))
            });
        let (ops, record) = match planned {
            Ok(planned) => planned,
            Err(error) => {
                failed.push(format!("{}: {error}", name(id)));
                continue;
            }
        };
        match super::all_writable(&ops) {
            Ok(true) => match super::run(&ops) {
                Ok(()) => done.push((id.clone(), record)),
                Err(error) => failed.push(format!("{}: {error}", name(id))),
            },
            Ok(false) => protected.push((id.clone(), ops, record)),
            Err(error) => failed.push(format!("{}: {error}", name(id))),
        }
    }
    if !protected.is_empty() {
        let ops: Vec<FileOp> = protected
            .iter()
            .flat_map(|(_, ops, _)| ops.iter().cloned())
            .collect();
        match super::apply(&ops) {
            Ok(()) => done.extend(protected.into_iter().map(|(id, _, record)| (id, record))),
            Err(error) => failed.extend(
                protected
                    .iter()
                    .map(|(id, _, _)| format!("{}: {error}", name(id))),
            ),
        }
    }
    let _ = fs::remove_dir_all(&staging);
    // Every game changed is recorded, even when another one's record can't be saved.
    let mut saved = mods;
    for (id, (effects, addons)) in done {
        let recorded = update(&id, |mods| {
            if let Some(reshade) = &mut mods.reshade {
                // An earlier version's list of effect files is replaced by the folder's record.
                if let Some(installed) = &mut reshade.installed {
                    *installed = split_effects(Some(installed)).1;
                }
                reshade.effects = Some(effects);
                if let Some(addons) = addons {
                    reshade.addons = addons;
                }
            }
        });
        match recorded {
            Ok(mods) => saved = mods,
            Err(error) => failed.push(format!("{}: {error}", name(&id))),
        }
    }
    if failed.is_empty() {
        Ok(saved)
    } else {
        Err(incomplete(
            "ReShade's shaders and add-ons were deployed",
            &failed,
        ))
    }
}

/// Saves the game's ReShade variant, used by the next Install or Reinstall.
pub fn save_settings(game: &Game, variant: ReshadeVariant) -> Result<ModsFile> {
    update(&game.id, |mods| {
        mods.reshade.get_or_insert_with(Default::default).variant = variant;
        drop_default(&mut mods.reshade);
    })
}

/// Nothing to keep: no install and default settings.
fn drop_default(reshade: &mut Option<ReshadeState>) {
    if reshade
        .as_ref()
        .is_some_and(|state| *state == ReshadeState::default())
    {
        *reshade = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mods::{GameMods, run};
    use crate::seven_zip::tests::{seven_zip, temp_dir};
    use crate::{Launch, Store};
    use GraphicsApi::*;

    fn game(arch: Option<Arch>, program: &str) -> Game {
        Game {
            id: "custom:1".into(),
            name: "Test".into(),
            store: Store::Custom,
            install_dir: None,
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: Some(PathBuf::from(program)),
            arch,
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    #[test]
    fn targets_follow_the_api_dxvk_and_bitness() {
        let x86 = game(Some(Arch::X86), r"C:\Games\Old\old.exe");
        let legend = target(&x86, Some(DirectX9), false).unwrap();
        assert_eq!(legend.mode, Mode::Dll("d3d9.dll"));
        assert_eq!(legend.summary(), "ReShade32.dll as d3d9.dll for DirectX 9");
        assert_eq!(legend.folder, PathBuf::from(r"C:\Games\Old"));

        let x64 = game(Some(Arch::X64), r"C:\Games\New\new.exe");
        for api in [DirectX10, DirectX11, DirectX12] {
            assert_eq!(
                target(&x64, Some(api), false).unwrap().mode,
                Mode::Dll("dxgi.dll")
            );
        }
        assert_eq!(
            target(&x64, Some(OpenGl), false).unwrap().mode,
            Mode::Dll("opengl32.dll")
        );
        assert_eq!(target(&x64, Some(Vulkan), false).unwrap().mode, Mode::Layer);
        // DXVK turns Direct3D into Vulkan: the layer, Direct3D 8 included.
        assert_eq!(
            target(&x86, Some(DirectX9), true).unwrap().mode,
            Mode::Layer
        );
        assert_eq!(
            target(&x86, Some(DirectX8), true).unwrap().mode,
            Mode::Layer
        );
        assert_eq!(target(&x86, Some(DirectX8), false), Err(Blocked::Direct3D8));
        assert!(Blocked::Direct3D8.to_string().contains("d3d8to9"));

        assert_eq!(target(&x64, None, false), Err(Blocked::NoApi));
        assert_eq!(
            target(&game(None, r"C:\a.exe"), Some(DirectX9), false),
            Err(Blocked::UnknownBitness)
        );
        assert_eq!(
            target(&game(Some(Arch::Arm64), r"C:\a.exe"), Some(DirectX9), false),
            Err(Blocked::Arm)
        );
        // The layer cannot list a path with a comma; a DLL install does not care.
        let comma = game(Some(Arch::X64), r"C:\GOG Games\Warhammer 40,000\game.exe");
        assert!(matches!(
            target(&comma, Some(Vulkan), false),
            Err(Blocked::Comma(_))
        ));
        assert!(target(&comma, Some(DirectX11), false).is_ok());
    }

    /// `cargo test -p gm-core live_builds -- --ignored`: downloads each variant's latest build
    /// into Game Manager's cache.
    #[test]
    #[ignore = "downloads from reshade.me, GitHub and nightly.link"]
    fn live_builds_download() {
        for variant in ReshadeVariant::ALL {
            let build = latest(variant).unwrap();
            let dir = cached_build(variant, &build).unwrap();
            for name in LAYER_FILES {
                assert!(dir.join(name).is_file(), "{variant:?} {build:?}: {name}");
            }
            let product = version_info::product_name(&dir.join("ReShade64.dll"));
            println!(
                "{variant:?}: {} from {} ({product:?})",
                build.version, build.url
            );
        }
    }

    #[test]
    fn deploy_reaches_the_games_that_follow_the_changed_choice() {
        let mut mods = ModsFile::default();
        let entry = |installed: Option<Installed>| GameMods {
            reshade: Some(ReshadeState {
                installed,
                ..ReshadeState::default()
            }),
            ..GameMods::default()
        };
        for id in ["a", "b", "c"] {
            mods.games
                .insert(id.to_owned(), entry(Some(Installed::default())));
        }
        // "d" has only a variant chosen: ReShade is not installed there.
        mods.games.insert("d".to_owned(), entry(None));

        let mut settings = ReshadeSettings::default();
        settings.set_own_shaders("a", Some(Selection::default()));
        settings.set_own_addons("b", Some(Vec::new()));
        let reached = |to: DeployTo| deploy_targets(&mods, &settings, &to);
        assert_eq!(reached(DeployTo::Sharing(Part::Shaders)), ["b", "c"]);
        assert_eq!(reached(DeployTo::Sharing(Part::Addons)), ["a", "c"]);
        assert_eq!(reached(DeployTo::Game("a".to_owned())), ["a"]);
        assert!(reached(DeployTo::Game("d".to_owned())).is_empty());
    }

    #[test]
    fn reads_the_download_links() {
        let page = r#"<a href="/downloads/ReShade_Setup_6.8.0.exe">Download</a>
            <a href="/downloads/ReShade_Setup_6.8.0_Addon.exe">with full add-on support</a>"#;
        assert_eq!(setup_version(page, true).as_deref(), Some("6.8.0"));
        assert_eq!(setup_version(page, false).as_deref(), Some("6.8.0"));
        let addon_only = r#"<a href="/downloads/ReShade_Setup_6.8.1_Addon.exe">"#;
        assert_eq!(setup_version(addon_only, false), None);
        assert_eq!(setup_version("ReShade_Setup_latest.exe", false), None);

        let artifacts: serde_json::Value = serde_json::from_str(
            r#"{ "artifacts": [ { "name": "ReShade (64-bit)", "expired": false },
                                { "name": "ReShade Setup", "expired": false } ] }"#,
        )
        .unwrap();
        let setup = |name: &str| name == SETUP_ARTIFACT;
        assert_eq!(
            run_artifact(&artifacts, setup),
            Some((SETUP_ARTIFACT.to_owned(), None))
        );
        let expired =
            serde_json::json!({ "artifacts": [ { "name": "ReShade Setup", "expired": true } ] });
        assert_eq!(run_artifact(&expired, setup), None);
    }

    #[test]
    fn cuts_the_zip_out_of_a_setup() {
        let dir = temp_dir("reshade-setup");
        fs::create_dir_all(dir.join("files")).unwrap();
        for name in LAYER_FILES {
            fs::write(dir.join("files").join(name), name).unwrap();
        }
        seven_zip(&dir.join("files"), &["a", "-tzip", r"..\payload.zip", "*"]);
        // A program with the ZIP appended, as ReShade's setup is built.
        let mut setup = b"MZ fake program".repeat(1000);
        setup.extend(fs::read(dir.join("payload.zip")).unwrap());
        let zip = appended_zip(&setup).unwrap();
        assert_eq!(zip, fs::read(dir.join("payload.zip")).unwrap());
        fs::write(dir.join("cut.zip"), zip).unwrap();
        seven_zip::extract(&dir.join("cut.zip"), &dir.join("unpacked")).unwrap();
        assert_eq!(
            fs::read_to_string(dir.join(r"unpacked\ReShade64.dll")).unwrap(),
            "ReShade64.dll"
        );
        assert_eq!(appended_zip(b"MZ no zip here at all, nothing"), None);
        fs::remove_dir_all(dir).unwrap();
    }

    /// A fake cached build, effects and staging under `dir`.
    fn sources(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let (build, effects, staging) =
            (dir.join("build"), dir.join("effects"), dir.join("staging"));
        for folder in [&build, &staging] {
            fs::create_dir_all(folder).unwrap();
        }
        for name in LAYER_FILES {
            fs::write(build.join(name), name).unwrap();
        }
        for (name, text) in [
            (r"reshade-shaders\Shaders\LumaSharpen.fx", "luma"),
            (r"reshade-shaders\Shaders\ReShade.fxh", "header"),
            (r"reshade-shaders\Textures\Layer.png", "png"),
        ] {
            let path = effects.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        (build, effects, staging)
    }

    fn listing(dir: &Path) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<(String, Vec<u8>)> = files_under(dir)
            .unwrap()
            .into_iter()
            .map(|relative| {
                let bytes = fs::read(dir.join(&relative)).unwrap();
                (relative.to_string_lossy().into_owned(), bytes)
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn installs_reinstalls_and_uninstalls_as_a_dll() {
        let dir = temp_dir("reshade-dll");
        let folder = dir.join("game");
        fs::create_dir_all(folder.join(r"reshade-shaders\Shaders")).unwrap();
        fs::write(folder.join("game.exe"), b"exe").unwrap();
        fs::write(folder.join("d3d9.dll"), b"the game's own d3d9").unwrap();
        fs::write(
            folder.join(r"reshade-shaders\Shaders\Mine.fx"),
            b"the user's",
        )
        .unwrap();
        fs::write(
            folder.join(r"reshade-shaders\Shaders\ReShade.fxh"),
            b"the user's header",
        )
        .unwrap();
        let before = listing(&folder);
        let program = folder.join("game.exe");
        let test_game = game(Some(Arch::X86), &program.to_string_lossy());
        let target = target(&test_game, Some(DirectX9), false).unwrap();
        let build = Build {
            version: "6.8.0".into(),
            page: RELEASES_PAGE.into(),
            url: String::new(),
            file_name: String::new(),
            sha256: None,
        };
        let install = |previous: &ReshadeState| {
            let (build_dir, effects, staging) = sources(&dir.join("sources"));
            let sources = Sources {
                build: &build_dir,
                effects: &effects,
                staging: &staging,
            };
            let prepared = prepare(
                previous,
                &target,
                &sources,
                &build,
                &LayerStatus::default(),
                None,
            )
            .unwrap();
            assert_eq!(prepared.job.layer, None);
            run(&prepared.job.ops).unwrap();
            fs::remove_dir_all(dir.join("sources")).unwrap();
            ReshadeState {
                installed: Some(prepared.installed),
                layer: prepared.layer,
                effects: Some(prepared.effects),
                ..ReshadeState::default()
            }
        };

        let first = install(&ReshadeState::default());
        assert_eq!(first.dll(), Some("d3d9.dll"));
        assert_eq!(fs::read(folder.join("d3d9.dll")).unwrap(), b"ReShade32.dll");
        assert_eq!(
            fs::read(folder.join("d3d9.dll.gm-backup")).unwrap(),
            b"the game's own d3d9"
        );
        assert_eq!(
            fs::read_to_string(folder.join(CONFIG_FILE)).unwrap(),
            config_text()
        );
        assert_eq!(
            fs::read(folder.join(r"reshade-shaders\Shaders\LumaSharpen.fx")).unwrap(),
            b"luma"
        );
        // The game's own reshade-shaders is set aside whole; Game Manager's is in its place.
        assert_eq!(
            fs::read(folder.join(r"reshade-shaders\Shaders\ReShade.fxh")).unwrap(),
            b"header"
        );
        assert_eq!(
            fs::read(folder.join(r"reshade-shaders.gm-backup\Shaders\Mine.fx")).unwrap(),
            b"the user's"
        );
        assert_eq!(
            first.effects,
            Some(Effects {
                files: 3,
                backup: true
            })
        );
        assert!(first.installed.as_ref().unwrap().files.len() <= 2);

        // Reinstalling replaces Game Manager's files and folder and keeps the game's.
        let second = install(&first);
        assert_eq!(second.installed, first.installed);
        assert_eq!(second.effects, first.effects);
        assert_eq!(
            fs::read(folder.join("d3d9.dll.gm-backup")).unwrap(),
            b"the game's own d3d9"
        );
        assert!(
            folder
                .join(r"reshade-shaders.gm-backup\Shaders\Mine.fx")
                .exists()
        );

        // ReShade ran and saved its settings, preset and log: Reinstall keeps them.
        let saved = b"[GENERAL]\r\nsaved by ReShade";
        fs::write(folder.join(CONFIG_FILE), saved).unwrap();
        fs::write(folder.join("ReShadePreset.ini"), b"Techniques=LumaSharpen").unwrap();
        fs::write(folder.join("ReShade.log"), b"log").unwrap();
        let third = install(&second);
        assert_eq!(fs::read(folder.join(CONFIG_FILE)).unwrap(), saved);
        assert!(folder.join("ReShadePreset.ini").exists());

        // Uninstalling removes all of ReShade, its settings included, and puts the game's own
        // reshade-shaders back: the folder is as before.
        run(&removal(&third).unwrap().ops).unwrap();
        assert_eq!(listing(&folder), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn earlier_installs_and_a_backup_in_the_way() {
        let dir = temp_dir("reshade-effects");
        let folder = dir.join("game");
        let effects = dir.join("effects");
        fs::create_dir_all(folder.join(r"reshade-shaders\Shaders")).unwrap();
        fs::write(folder.join(r"reshade-shaders\Shaders\Old.fx"), b"old").unwrap();
        fs::create_dir_all(effects.join(r"reshade-shaders\Shaders")).unwrap();
        fs::write(effects.join(r"reshade-shaders\Shaders\New.fx"), b"new").unwrap();
        // An earlier version listed its effect files: the folder is Game Manager's.
        let earlier = ReshadeState {
            installed: Some(Installed {
                folder: folder.clone(),
                files: vec![
                    InstalledFile::of(
                        r"reshade-shaders\Shaders\Old.fx",
                        &folder.join(r"reshade-shaders\Shaders\Old.fx"),
                    )
                    .unwrap(),
                ],
                ..Installed::default()
            }),
            ..ReshadeState::default()
        };
        assert!(earlier.owns_effects());
        let (ops, record) = effects_ops(&earlier, &folder, &effects).unwrap();
        assert!(!record.backup);
        run(&ops).unwrap();
        assert!(!folder.join(r"reshade-shaders\Shaders\Old.fx").exists());
        assert!(folder.join(r"reshade-shaders\Shaders\New.fx").exists());
        // Not Game Manager's, with a backup already there: refused.
        fs::create_dir_all(folder.join("reshade-shaders.gm-backup")).unwrap();
        let error = effects_ops(&ReshadeState::default(), &folder, &effects).unwrap_err();
        assert!(error.to_string().contains("already exists"), "{error}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_layer_lists_only_games_it_did_not_have() {
        let dir = temp_dir("reshade-layer");
        let folder = dir.join("game");
        fs::create_dir_all(&folder).unwrap();
        let program = folder.join("game.exe");
        fs::write(&program, b"exe").unwrap();
        let test_game = game(Some(Arch::X64), &program.to_string_lossy());
        let target = target(&test_game, Some(DirectX11), true).unwrap();
        let build = Build {
            version: "6.8.0".into(),
            page: String::new(),
            url: String::new(),
            file_name: String::new(),
            sha256: None,
        };
        let (build_dir, effects, staging) = sources(&dir.join("sources"));
        let sources = Sources {
            build: &build_dir,
            effects: &effects,
            staging: &staging,
        };
        let prepare_with = |layer: &LayerStatus| {
            prepare(
                &ReshadeState::default(),
                &target,
                &sources,
                &build,
                layer,
                None,
            )
            .unwrap()
        };

        // Not listed yet: added, and taken off again on uninstall.
        let fresh = prepare_with(&LayerStatus::default());
        let change = fresh.job.layer.clone().unwrap();
        assert_eq!(change.install_from.as_deref(), Some(build_dir.as_path()));
        assert_eq!(change.add_app.as_deref(), Some(program.as_path()));
        assert!(
            fresh
                .installed
                .files
                .iter()
                .all(|file| !file.name.ends_with(".dll"))
        );
        let state = ReshadeState {
            installed: Some(fresh.installed),
            layer: fresh.layer,
            ..ReshadeState::default()
        };
        assert_eq!(state.dll(), None);
        let removing = removal(&state).unwrap().layer.unwrap();
        assert_eq!(removing.remove_app.as_deref(), Some(program.as_path()));

        // Listed by ReShade's own setup: the layer is updated, the list left alone.
        let listed = LayerStatus {
            installed: true,
            version: None,
            apps: vec![program.to_string_lossy().to_uppercase()],
        };
        let kept = prepare_with(&listed);
        assert_eq!(kept.job.layer.as_ref().unwrap().add_app, None);
        assert!(!kept.layer.as_ref().unwrap().added);
        let state = ReshadeState {
            installed: Some(kept.installed),
            layer: kept.layer,
            ..ReshadeState::default()
        };
        assert_eq!(removal(&state).unwrap().layer, None);
        fs::remove_dir_all(dir).unwrap();
    }
}
