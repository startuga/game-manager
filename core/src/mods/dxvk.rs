//! DXVK (https://github.com/doitsujin/dxvk): Direct3D 8, 9, 10 and 11 on Vulkan, installed as
//! the game's Direct3D DLLs next to its program. Three variants:
//!
//! - **Stable**: the latest GitHub release, `dxvk-<version>.tar.gz`.
//! - **Development**: the latest successful build of master by the `artifacts.yml` workflow.
//!   GitHub gives workflow artifacts only to signed-in users, so it is downloaded through
//!   nightly.link (`/actions/runs/<run>/<artifact>.zip`); the artifact is named after the commit,
//!   so GitHub's API is asked for the run and its artifact first.
//! - **HDR-mod**: the latest release of EndlesslyFlowering's fork
//!   (https://github.com/EndlesslyFlowering/dxvk), which can upgrade render targets and the swap
//!   chain for HDR: `dxvk_v<dxvk>-HDR-mod-v<mod>.7z` (not the `-gplasync` builds some releases
//!   add).
//!
//! GitHub's API allows 60 anonymous requests an hour, plenty for a click each.
//!
//! All hold an `x32` and an `x64` folder of DLLs (HDR-mod's are in `normal\`, with its own HDR
//! configs, `dxvk_d3d9_<n>_<name>.conf` and `dxvk_d3d11_and_d3d10_<n>_<name>.conf`, at the
//! top), kept in `%LOCALAPPDATA%\Game Manager\mods\dxvk\<version>` with `configs` and a
//! `.format` file (`CACHE_FORMAT`; bump it when the layout changes).
//!
//! Which DLLs a game needs depends on its API (DXVK's README): Direct3D 8 `d3d8` + `d3d9`, 9
//! `d3d9`, 10 `d3d10core` + `d3d11` + `dxgi`, 11 `d3d11` + `dxgi`. DXVK does not do Direct3D 12
//! (VKD3D-Proton does), OpenGL or Vulkan.
//!
//! Settings go into `dxvk.conf` next to the program, which DXVK reads from the game's working
//! directory: the program's folder for nearly every launch.

use super::{Installed, ModsFile, apply, plan, update};
use crate::graphics::GraphicsApi;
use crate::nvidia::{self, NvidiaChoice, NvidiaOriginal};
use crate::{Arch, Error, Game, Result, http, json_file, seven_zip};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const API_URL: &str = "https://api.github.com/repos/doitsujin/dxvk";
const HDR_MOD_API_URL: &str = "https://api.github.com/repos/EndlesslyFlowering/dxvk";
const CONFIG_FILE: &str = "dxvk.conf";

/// What the Info dialog says about DXVK.
pub const INFO: &str = "DXVK translates Direct3D 8, 9, 10 and 11 into Vulkan.

It can:
• make ReShade's compute shaders work in older Direct3D games
• improve performance and reduce shader stutter in some games
• output HDR (the HDR setting in DXVK settings)

It needs a GPU driver with Vulkan 1.3. Borderless fullscreen works better than exclusive \
fullscreen.

The HDR-mod variant (a fork by EndlesslyFlowering) can also upgrade the game's render targets and \
output for HDR; its authors discourage using it online.

⚠ Games with anti-cheat may ban players who use DXVK.
⚠ Overlays (Steam, NVIDIA, RTSS) may conflict with it.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DxvkVariant {
    #[default]
    Stable,
    Development,
    HdrMod,
}

impl DxvkVariant {
    pub const ALL: [DxvkVariant; 3] = [
        DxvkVariant::Stable,
        DxvkVariant::Development,
        DxvkVariant::HdrMod,
    ];

    pub fn name(self) -> &'static str {
        match self {
            DxvkVariant::Stable => "Stable",
            DxvkVariant::Development => "Development",
            DxvkVariant::HdrMod => "HDR-mod",
        }
    }

    /// The name with where builds come from.
    pub fn label(self) -> &'static str {
        match self {
            DxvkVariant::Stable => "Stable (GitHub releases)",
            DxvkVariant::Development => "Development (latest master build, via nightly.link)",
            DxvkVariant::HdrMod => "HDR-mod (EndlesslyFlowering's fork for HDR, GitHub releases)",
        }
    }
}

/// The DXVK HUD, `dxvk.hud` in `dxvk.conf`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hud {
    #[default]
    Off,
    Fps,
    FrameTimes,
    Full,
}

impl Hud {
    pub const ALL: [Hud; 4] = [Hud::Off, Hud::Fps, Hud::FrameTimes, Hud::Full];

    pub fn label(self) -> &'static str {
        match self {
            Hud::Off => "Off",
            Hud::Fps => "Frame rate",
            Hud::FrameTimes => "Frame rate and frame times",
            Hud::Full => "Everything",
        }
    }

    fn value(self) -> Option<&'static str> {
        match self {
            Hud::Off => None,
            Hud::Fps => Some("fps"),
            Hud::FrameTimes => Some("fps,frametimes"),
            Hud::Full => Some("full"),
        }
    }
}

/// The DXVK settings written to `dxvk.conf`; all off means no file.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DxvkConfig {
    /// `dxgi.enableHDR`.
    pub hdr: bool,
    /// `dxgi.maxFrameRate` and `d3d9.maxFrameRate`; 0 is no limit.
    pub max_frame_rate: u32,
    pub hud: Hud,
    /// One of the HDR-mod build's own configs (`dxvk_d3d9_3_unsafer.conf`), which starts
    /// `dxvk.conf` when an HDR-mod build is installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_mod_config: Option<String>,
}

impl DxvkConfig {
    /// The `dxvk.conf` for these settings, starting with `hdr_mod` (an HDR-mod config's name and
    /// text) when given; `None` when there is nothing to write.
    pub fn text(&self, hdr_mod: Option<(&str, &str)>) -> Option<String> {
        let mut lines = vec![
            "# Written by Game Manager from this game's DXVK settings; saving them again replaces \
             this file."
                .to_owned(),
        ];
        if let Some((name, text)) = hdr_mod {
            lines.push(format!("# HDR-mod config: {name}"));
            lines.extend(text.lines().map(str::to_owned));
            lines.push(String::new());
        }
        if self.hdr {
            lines.push("dxgi.enableHDR = True".to_owned());
        }
        if self.max_frame_rate > 0 {
            lines.push(format!("dxgi.maxFrameRate = {}", self.max_frame_rate));
            lines.push(format!("d3d9.maxFrameRate = {}", self.max_frame_rate));
        }
        if let Some(hud) = self.hud.value() {
            lines.push(format!("dxvk.hud = {hud}"));
        }
        (lines.len() > 1).then(|| lines.join("\r\n") + "\r\n")
    }
}

/// HDR-mod's configs for Direct3D 8 and 9, and for 10 and 11, start with these.
const D3D9_CONFIGS: &str = "dxvk_d3d9_";
const D3D11_CONFIGS: &str = "dxvk_d3d11_and_d3d10_";

fn config_prefix(api: GraphicsApi) -> Option<&'static str> {
    match api {
        GraphicsApi::DirectX8 | GraphicsApi::DirectX9 => Some(D3D9_CONFIGS),
        GraphicsApi::DirectX10 | GraphicsApi::DirectX11 => Some(D3D11_CONFIGS),
        _ => None,
    }
}

/// The HDR-mod configs among `names` for a game on `api`.
pub fn configs_for(names: &[String], api: Option<GraphicsApi>) -> Vec<&str> {
    let Some(prefix) = api.and_then(config_prefix) else {
        return Vec::new();
    };
    names
        .iter()
        .map(String::as_str)
        .filter(|name| name.to_ascii_lowercase().starts_with(prefix))
        .collect()
}

/// "3 – unsafer" for `dxvk_d3d9_3_unsafer.conf`, "1 – 2nd safest" for
/// `dxvk_d3d11_and_d3d10_1_2nd_safest.conf`.
pub fn config_label(name: &str) -> String {
    let stem = name.strip_suffix(".conf").unwrap_or(name);
    let rest = [D3D11_CONFIGS, D3D9_CONFIGS]
        .iter()
        .find_map(|prefix| stem.strip_prefix(prefix))
        .unwrap_or(stem);
    match rest.split_once('_') {
        Some((level, words)) => format!("{level} – {}", words.replace('_', " ")),
        None => rest.replace('_', " "),
    }
}

/// A game's DXVK in `mods.json`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DxvkState {
    #[serde(default)]
    pub variant: DxvkVariant,
    #[serde(default)]
    pub config: DxvkConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<Installed>,
    /// The NVIDIA Present Method settings chosen for the game (for HDR with any variant); they
    /// stay when DXVK is uninstalled.
    #[serde(default, skip_serializing_if = "NvidiaChoice::is_original")]
    pub nvidia: NvidiaChoice,
    /// What the game's driver profile held before, while Game Manager has changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvidia_original: Option<NvidiaOriginal>,
}

/// A DXVK build to download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// `3.1.1`, `master-0cfe089` for a development build, or `3.1-HDR-mod-v0.3.4`.
    pub version: String,
    /// The release page, or the commit it was built from.
    pub page: String,
    pub url: String,
    pub file_name: String,
}

/// The latest build of the variant. Blocking: two or three web requests.
pub fn latest(variant: DxvkVariant) -> Result<Build> {
    match variant {
        DxvkVariant::Stable => {
            let url = format!("{API_URL}/releases/latest");
            release_build(&get_json(&url)?, is_stable_asset).ok_or(Error::NoRelease { url })
        }
        DxvkVariant::HdrMod => {
            let url = format!("{HDR_MOD_API_URL}/releases/latest");
            release_build(&get_json(&url)?, is_hdr_mod_asset).ok_or(Error::NoRelease { url })
        }
        DxvkVariant::Development => {
            let url = format!(
                "{API_URL}/actions/workflows/artifacts.yml/runs?branch=master&status=success&\
                 event=push&per_page=1"
            );
            let (run, commit) = latest_run(&get_json(&url)?).ok_or(Error::NoRelease { url })?;
            let url = format!("{API_URL}/actions/runs/{run}/artifacts");
            let artifact = artifact_name(&get_json(&url)?).ok_or(Error::NoRelease { url })?;
            Ok(Build {
                version: format!("master-{}", commit.get(..7).unwrap_or(&commit)),
                page: format!("https://github.com/doitsujin/dxvk/commit/{commit}"),
                url: format!(
                    "https://nightly.link/doitsujin/dxvk/actions/runs/{run}/{artifact}.zip"
                ),
                file_name: format!("{artifact}.zip"),
            })
        }
    }
}

fn get_json(url: &str) -> Result<Value> {
    serde_json::from_slice(&http::get(url)?).map_err(|source| Error::WebJson {
        url: url.to_owned(),
        source,
    })
}

/// The release's Windows build: `dxvk-<version>.tar.gz`, not the Linux `dxvk-native-…`.
fn is_stable_asset(name: &str, version: &str) -> bool {
    name == format!("dxvk-{version}.tar.gz")
}

/// The release's HDR-mod build: `dxvk_v<version>.7z`, not the `-gplasync` one.
fn is_hdr_mod_asset(name: &str, _version: &str) -> bool {
    name.starts_with("dxvk_") && name.ends_with(".7z") && !name.contains("gplasync")
}

/// `releases/latest`: the asset `is_asset(name, version)` picks, the version being the tag
/// without its `v`.
fn release_build(release: &Value, is_asset: fn(&str, &str) -> bool) -> Option<Build> {
    let version = release["tag_name"].as_str()?.trim_start_matches('v');
    let asset = release["assets"].as_array()?.iter().find(|asset| {
        asset["name"]
            .as_str()
            .is_some_and(|name| is_asset(name, version))
    })?;
    Some(Build {
        version: version.to_owned(),
        page: release["html_url"].as_str()?.to_owned(),
        url: asset["browser_download_url"].as_str()?.to_owned(),
        file_name: asset["name"].as_str()?.to_owned(),
    })
}

/// The workflow's latest run: its id and commit.
fn latest_run(runs: &Value) -> Option<(u64, String)> {
    let run = &runs["workflow_runs"][0];
    Some((run["id"].as_u64()?, run["head_sha"].as_str()?.to_owned()))
}

/// The run's DXVK artifact, `dxvk-master-<commit>`.
fn artifact_name(artifacts: &Value) -> Option<String> {
    artifacts["artifacts"]
        .as_array()?
        .iter()
        .filter(|artifact| !artifact["expired"].as_bool().unwrap_or(false))
        .filter_map(|artifact| artifact["name"].as_str())
        .find(|name| name.starts_with("dxvk-") && !name.starts_with("dxvk-native"))
        .map(str::to_owned)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bits {
    X32,
    X64,
}

impl Bits {
    /// The build's folder.
    pub fn folder(self) -> &'static str {
        match self {
            Bits::X32 => "x32",
            Bits::X64 => "x64",
        }
    }
}

/// Where and what DXVK installs for a game.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub bits: Bits,
    pub api: GraphicsApi,
    pub dlls: &'static [&'static str],
    /// The folder of the game's program.
    pub folder: PathBuf,
}

impl Target {
    /// "x64 d3d11.dll, dxgi.dll for DirectX 11".
    pub fn summary(&self) -> String {
        format!(
            "{} {} for {}",
            self.bits.folder(),
            self.dlls.join(", "),
            self.api.label()
        )
    }
}

/// Why DXVK cannot be installed in a game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    NoProgram,
    UnknownBitness,
    Arm,
    /// No graphics API was found, or several: the user chooses it.
    NoApi,
    Unsupported(GraphicsApi),
}

impl fmt::Display for Blocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Blocked::NoProgram => f.write_str("Choose the game program in Game settings first."),
            Blocked::UnknownBitness => f.write_str("Choose the game's bitness in Game settings."),
            Blocked::Arm => f.write_str("DXVK has no build for ARM programs."),
            Blocked::NoApi => f.write_str("Choose the game's graphics API in Game settings."),
            Blocked::Unsupported(GraphicsApi::DirectX12) => {
                f.write_str("DXVK translates Direct3D 8 to 11; Direct3D 12 needs VKD3D-Proton.")
            }
            Blocked::Unsupported(api) => write!(
                f,
                "DXVK translates Direct3D 8 to 11; this game uses {}.",
                api.label()
            ),
        }
    }
}

/// What DXVK installs for this game running on `api`, or why it can't.
pub fn target(game: &Game, api: Option<GraphicsApi>) -> Result<Target, Blocked> {
    let folder = game
        .program
        .as_deref()
        .and_then(Path::parent)
        .ok_or(Blocked::NoProgram)?;
    let bits = match game.arch.ok_or(Blocked::UnknownBitness)? {
        Arch::X86 => Bits::X32,
        Arch::X64 | Arch::AnyCpu => Bits::X64,
        Arch::Arm64 | Arch::Arm => return Err(Blocked::Arm),
    };
    let api = api.ok_or(Blocked::NoApi)?;
    let dlls: &'static [&'static str] = match api {
        GraphicsApi::DirectX8 => &["d3d8.dll", "d3d9.dll"],
        GraphicsApi::DirectX9 => &["d3d9.dll"],
        GraphicsApi::DirectX10 => &["d3d10core.dll", "d3d11.dll", "dxgi.dll"],
        GraphicsApi::DirectX11 => &["d3d11.dll", "dxgi.dll"],
        GraphicsApi::DirectX12 | GraphicsApi::OpenGl | GraphicsApi::Vulkan => {
            return Err(Blocked::Unsupported(api));
        }
    };
    Ok(Target {
        bits,
        api,
        dlls,
        folder: folder.to_path_buf(),
    })
}

fn state(game: &Game) -> Result<DxvkState> {
    Ok(super::load()?.dxvk(&game.id).cloned().unwrap_or_default())
}

/// Installs the latest build of the game's DXVK variant for `api`, replacing an earlier install,
/// and returns `mods.json` as saved. Blocking: downloads, 7-Zip and file copies (as administrator
/// when the folder needs it).
pub fn install(game: &Game, api: GraphicsApi) -> Result<ModsFile> {
    let target =
        target(game, Some(api)).map_err(|blocked| Error::NotInstallable(blocked.to_string()))?;
    let state = state(game)?;
    let build = latest(state.variant)?;
    let cache = cached_build(&build)?;
    let mut files = Vec::new();
    for dll in target.dlls {
        let source = cache.join(target.bits.folder()).join(dll);
        if !source.is_file() {
            return Err(Error::NoModBuild(source));
        }
        files.push(((*dll).to_owned(), source));
    }
    if let Some(text) = config_text(&state.config, &cache, Some(api))? {
        files.push((CONFIG_FILE.to_owned(), staged_config(&text)?));
    }
    let plan = plan(state.installed.as_ref(), &target.folder, &files)?;
    apply(&plan.ops)?;
    let installed = Installed {
        version: build.version,
        page: build.page,
        folder: target.folder,
        files: plan.files,
        backups: plan.backups,
    };
    update(&game.id, |mods| {
        mods.dxvk.get_or_insert_with(Default::default).installed = Some(installed);
    })
}

/// Removes the game's DXVK and puts back the files it replaced; returns `mods.json` as saved.
pub fn uninstall(game: &Game) -> Result<ModsFile> {
    if let Some(installed) = &state(game)?.installed {
        apply(&plan(Some(installed), &installed.folder, &[])?.ops)?;
    }
    update(&game.id, |mods| {
        if let Some(dxvk) = &mut mods.dxvk {
            dxvk.installed = None;
        }
        drop_default(&mut mods.dxvk);
    })
}

/// Saves the game's DXVK settings; when DXVK is installed, its `dxvk.conf` follows them (`api`
/// is the game's, to check an HDR-mod config against).
pub fn save_settings(
    game: &Game,
    api: Option<GraphicsApi>,
    variant: DxvkVariant,
    config: DxvkConfig,
    nvidia: NvidiaChoice,
) -> Result<ModsFile> {
    let state = state(game)?;
    let mut installed = state.installed.clone();
    if let Some(record) = installed.as_mut().filter(|_| config != state.config) {
        // Only dxvk.conf changes: plan it alone.
        let is_config = |name: &String| name.eq_ignore_ascii_case(CONFIG_FILE);
        let (config_files, mut files): (Vec<_>, Vec<_>) = record
            .files
            .drain(..)
            .partition(|file| is_config(&file.name));
        let (config_backups, mut backups): (Vec<_>, Vec<_>) =
            record.backups.drain(..).partition(is_config);
        let previous = Installed {
            files: config_files,
            backups: config_backups,
            ..record.clone()
        };
        let new = match config_text(&config, &build_dir(&record.version)?, api)? {
            Some(text) => vec![(CONFIG_FILE.to_owned(), staged_config(&text)?)],
            None => Vec::new(),
        };
        let plan = plan(Some(&previous), &record.folder, &new)?;
        apply(&plan.ops)?;
        files.extend(plan.files);
        backups.extend(plan.backups);
        record.files = files;
        record.backups = backups;
    }
    let saved = update(&game.id, |mods| {
        let dxvk = mods.dxvk.get_or_insert_with(Default::default);
        dxvk.variant = variant;
        dxvk.config = config;
        dxvk.installed = installed;
        drop_default(&mut mods.dxvk);
    })?;
    if nvidia == state.nvidia {
        return Ok(saved);
    }
    // Saved apart, as soon as the driver has them, so the originals are never lost.
    let original = apply_nvidia(game, nvidia, state.nvidia_original.as_ref())?;
    update(&game.id, |mods| {
        let dxvk = mods.dxvk.get_or_insert_with(Default::default);
        dxvk.nvidia = nvidia;
        dxvk.nvidia_original = original;
        drop_default(&mut mods.dxvk);
    })
}

/// Gives the game's driver profile the `wanted` Present Method settings; the originals to keep
/// afterwards come back.
fn apply_nvidia(
    game: &Game,
    wanted: NvidiaChoice,
    original: Option<&NvidiaOriginal>,
) -> Result<Option<NvidiaOriginal>> {
    let exe = game
        .program
        .as_deref()
        .and_then(Path::file_name)
        .ok_or_else(|| Error::NotInstallable(Blocked::NoProgram.to_string()))?
        .to_string_lossy();
    let now = nvidia::read(&exe)?.ok_or(Error::Nvapi {
        action: "find the NVIDIA driver",
        status: nvidia::NVIDIA_DEVICE_NOT_FOUND,
    })?;
    let new_profile = format!("{} (Game Manager)", game.name);
    let (plan, original) = nvidia::plan(&exe, &new_profile, &now, wanted, original);
    nvidia::run(&plan)?;
    Ok(original)
}

/// Nothing to keep: no install and default settings.
fn drop_default(dxvk: &mut Option<DxvkState>) {
    if dxvk
        .as_ref()
        .is_some_and(|state| *state == DxvkState::default())
    {
        *dxvk = None;
    }
}

/// The `dxvk.conf` for `config` with the build in `build`: an HDR-mod build (one with
/// `configs`) starts it with the chosen config, which must be in the build and for the game's
/// API. `None` when there is nothing to write.
fn config_text(
    config: &DxvkConfig,
    build: &Path,
    api: Option<GraphicsApi>,
) -> Result<Option<String>> {
    let hdr_mod = match &config.hdr_mod_config {
        Some(name) if build.join(CONFIGS).is_dir() => {
            check_config(name, api)?;
            let file = build.join(CONFIGS).join(name);
            let text = fs::read_to_string(&file).map_err(|_| {
                Error::NotInstallable(format!(
                    "This HDR-mod build has no {name}: choose another config in DXVK settings."
                ))
            })?;
            Some((name.as_str(), text))
        }
        Some(_) if !build.is_dir() => {
            return Err(Error::NotInstallable(
                "The installed DXVK build is no longer in Game Manager's cache: reinstall it to \
                 use the HDR-mod config."
                    .to_owned(),
            ));
        }
        _ => None,
    };
    Ok(config.text(hdr_mod.as_ref().map(|(name, text)| (*name, text.as_str()))))
}

/// An HDR-mod config is for Direct3D 8 and 9 or for 10 and 11; it must be the game's.
fn check_config(name: &str, api: Option<GraphicsApi>) -> Result<()> {
    let names = [name.to_owned()];
    if !configs_for(&names, api).is_empty() {
        return Ok(());
    }
    Err(Error::NotInstallable(match api {
        Some(api) => format!(
            "The HDR-mod config {name} is not for {}: choose another in DXVK settings.",
            api.label()
        ),
        None => Blocked::NoApi.to_string(),
    }))
}

/// The latest HDR-mod build's own configs (`dxvk_d3d9_0_safest.conf` …), downloading the build
/// when it is not cached yet. Blocking.
pub fn hdr_mod_configs() -> Result<Vec<String>> {
    let build = cached_build(&latest(DxvkVariant::HdrMod)?)?;
    let mut names: Vec<String> = fs::read_dir(build.join(CONFIGS))
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.to_ascii_lowercase().ends_with(".conf"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

/// `text` as `dxvk.conf`, written where the copy (possibly as administrator) reads it.
fn staged_config(text: &str) -> Result<PathBuf> {
    let dir = json_file::local_file("mods")?.join("staging");
    fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
    let file = dir.join(CONFIG_FILE);
    fs::write(&file, text).map_err(Error::io(&file))?;
    Ok(file)
}

/// The cache folder's layout: `x32`, `x64`, and `configs` (HDR-mod's own `.conf` files). A
/// folder without this number in its `.format` file is downloaded again.
const CACHE_FORMAT: &str = "2";
const FORMAT_FILE: &str = ".format";
const CONFIGS: &str = "configs";

/// Where a version's build is cached.
fn build_dir(version: &str) -> Result<PathBuf> {
    let version: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c))
        .collect();
    Ok(json_file::local_file("mods")?.join("dxvk").join(version))
}

/// One download into the cache at a time (an install, and the settings dialog listing HDR-mod's
/// configs, may both ask), as they share its work folder.
static BUILD_LOCK: Mutex<()> = Mutex::new(());

/// The build's folder in the cache, holding `x32`, `x64` and, for HDR-mod, `configs`:
/// downloaded and unpacked the first time.
fn cached_build(build: &Build) -> Result<PathBuf> {
    let _lock = BUILD_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = build_dir(&build.version)?;
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
    let archive = work.join(&build.file_name);
    http::download(&build.url, &archive)?;
    let unpacked = work.join("unpacked");
    seven_zip::extract(&archive, &unpacked)?;
    // Only the DLLs of each build folder are kept; moved into place last, so the cache never
    // holds half a build.
    let kept = work.join("build");
    let mut found = false;
    for bits in [Bits::X32, Bits::X64] {
        let Some(source) = find_build_folder(&unpacked, bits.folder()) else {
            continue;
        };
        let dest = kept.join(bits.folder());
        fs::create_dir_all(&dest).map_err(Error::io(&dest))?;
        for dll in dlls_in(&source) {
            let to = dest.join(dll.file_name().unwrap_or_default());
            fs::copy(&dll, &to).map_err(Error::io(&to))?;
        }
        found = true;
    }
    if !found {
        return Err(Error::NoModBuild(archive));
    }
    keep_configs(&unpacked, &kept.join(CONFIGS))?;
    let format = kept.join(FORMAT_FILE);
    fs::write(&format, CACHE_FORMAT).map_err(Error::io(&format))?;
    // A folder of an older layout is replaced.
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(Error::io(&dir))?;
    }
    fs::rename(&kept, &dir).map_err(Error::io(&dir))?;
    let _ = fs::remove_dir_all(&work);
    Ok(dir)
}

/// Copies the `.conf` files of an unpacked build into `configs`, except those inside its `x32` and
/// `x64` folders (HDR-mod ships its configs at the top). Upstream builds have none.
fn keep_configs(unpacked: &Path, configs: &Path) -> Result<()> {
    let mut folders = vec![(unpacked.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            if path.is_dir() {
                if depth < 3 && name != "x32" && name != "x64" {
                    folders.push((path, depth + 1));
                }
            } else if name.ends_with(".conf") {
                fs::create_dir_all(configs).map_err(Error::io(configs))?;
                let to = configs.join(entry.file_name());
                fs::copy(&path, &to).map_err(Error::io(&to))?;
            }
        }
    }
    Ok(())
}

/// A folder called `name` under `root` that holds DLLs (`dxvk-3.1.1\x64`, or `x64` at the top).
fn find_build_folder(root: &Path, name: &str) -> Option<PathBuf> {
    let mut folders = vec![(root.to_path_buf(), 0)];
    while let Some((folder, depth)) = folders.pop() {
        for entry in fs::read_dir(&folder).ok()?.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if entry.file_name().eq_ignore_ascii_case(name) && !dlls_in(&path).is_empty() {
                return Some(path);
            }
            if depth < 3 {
                folders.push((path, depth + 1));
            }
        }
    }
    None
}

fn dlls_in(folder: &Path) -> Vec<PathBuf> {
    fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("dll"))
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::seven_zip::tests::{seven_zip, temp_dir};
    use crate::{Launch, Store};
    use GraphicsApi::*;

    fn game(arch: Option<Arch>) -> Game {
        Game {
            id: "steam:7000".into(),
            name: "Tomb Raider: Legend".into(),
            store: Store::Steam,
            install_dir: Some(PathBuf::from(r"C:\Games\TRL")),
            launch: Launch::Uri("steam://rungameid/7000".into()),
            alt_launches: Vec::new(),
            icon: None,
            program: Some(PathBuf::from(r"C:\Games\TRL\trl.exe")),
            arch,
            edits: None,
            favorite: false,
        }
    }

    #[test]
    fn targets_follow_the_api_and_bitness() {
        let x86 = game(Some(Arch::X86));
        let legend = target(&x86, Some(DirectX9)).unwrap();
        assert_eq!(legend.bits, Bits::X32);
        assert_eq!(legend.dlls, ["d3d9.dll"]);
        assert_eq!(legend.folder, PathBuf::from(r"C:\Games\TRL"));
        assert_eq!(legend.summary(), "x32 d3d9.dll for DirectX 9");

        let x64 = game(Some(Arch::X64));
        assert_eq!(
            target(&x64, Some(DirectX8)).unwrap().dlls,
            ["d3d8.dll", "d3d9.dll"]
        );
        assert_eq!(
            target(&x64, Some(DirectX10)).unwrap().dlls,
            ["d3d10core.dll", "d3d11.dll", "dxgi.dll"]
        );
        let dx11 = target(&game(Some(Arch::AnyCpu)), Some(DirectX11)).unwrap();
        assert_eq!(
            (dx11.bits, dx11.dlls),
            (Bits::X64, &["d3d11.dll", "dxgi.dll"][..])
        );

        for api in [DirectX12, OpenGl, Vulkan] {
            assert_eq!(target(&x64, Some(api)), Err(Blocked::Unsupported(api)));
        }
        assert_eq!(target(&x64, None), Err(Blocked::NoApi));
        assert_eq!(
            target(&game(None), Some(DirectX9)),
            Err(Blocked::UnknownBitness)
        );
        assert_eq!(
            target(&game(Some(Arch::Arm64)), Some(DirectX9)),
            Err(Blocked::Arm)
        );
        let mut unknown = game(Some(Arch::X64));
        unknown.program = None;
        assert_eq!(target(&unknown, Some(DirectX9)), Err(Blocked::NoProgram));
        assert!(
            Blocked::Unsupported(DirectX12)
                .to_string()
                .contains("VKD3D-Proton")
        );
    }

    #[test]
    fn reads_releases_and_workflow_runs() {
        let release: Value = serde_json::from_str(
            r#"{ "tag_name": "v3.1.1", "html_url": "https://github.com/doitsujin/dxvk/releases/tag/v3.1.1",
                 "assets": [
                   { "name": "dxvk-native-3.1.1-steamrt-sniper.tar.gz", "browser_download_url": "https://x/native" },
                   { "name": "dxvk-3.1.1.tar.gz", "browser_download_url": "https://github.com/doitsujin/dxvk/releases/download/v3.1.1/dxvk-3.1.1.tar.gz" } ] }"#,
        )
        .unwrap();
        assert_eq!(
            release_build(&release, is_stable_asset),
            Some(Build {
                version: "3.1.1".into(),
                page: "https://github.com/doitsujin/dxvk/releases/tag/v3.1.1".into(),
                url: "https://github.com/doitsujin/dxvk/releases/download/v3.1.1/dxvk-3.1.1.tar.gz"
                    .into(),
                file_name: "dxvk-3.1.1.tar.gz".into(),
            })
        );
        assert_eq!(
            release_build(
                &serde_json::json!({ "message": "Not Found" }),
                is_stable_asset
            ),
            None
        );

        // HDR-mod: the plain build, not the gplasync one.
        let hdr_mod: Value = serde_json::from_str(
            r#"{ "tag_name": "v3.0.2-HDR-mod-v0.3.4", "html_url": "https://github.com/EndlesslyFlowering/dxvk/releases/tag/v3.0.2-HDR-mod-v0.3.4",
                 "assets": [
                   { "name": "dxvk_v3.0.2-HDR-mod-v0.3.4-gplasync-1.7z", "browser_download_url": "https://x/gplasync" },
                   { "name": "dxvk_v3.0.2-HDR-mod-v0.3.4.7z", "browser_download_url": "https://github.com/EndlesslyFlowering/dxvk/releases/download/v3.0.2-HDR-mod-v0.3.4/dxvk_v3.0.2-HDR-mod-v0.3.4.7z" } ] }"#,
        )
        .unwrap();
        let build = release_build(&hdr_mod, is_hdr_mod_asset).unwrap();
        assert_eq!(build.version, "3.0.2-HDR-mod-v0.3.4");
        assert_eq!(build.file_name, "dxvk_v3.0.2-HDR-mod-v0.3.4.7z");
        assert!(build.url.ends_with("/dxvk_v3.0.2-HDR-mod-v0.3.4.7z"));
        // Upstream's rule finds nothing in the fork's release, and the other way round.
        assert_eq!(release_build(&hdr_mod, is_stable_asset), None);
        assert_eq!(release_build(&release, is_hdr_mod_asset), None);

        let runs: Value = serde_json::from_str(
            r#"{ "total_count": 453, "workflow_runs": [ { "id": 40694781188,
                 "head_sha": "0cfe0898289fb11e3d8d0e1c62cd344b825b7d57", "conclusion": "success" } ] }"#,
        )
        .unwrap();
        assert_eq!(
            latest_run(&runs),
            Some((
                40694781188,
                "0cfe0898289fb11e3d8d0e1c62cd344b825b7d57".to_owned()
            ))
        );
        let artifacts: Value = serde_json::from_str(
            r#"{ "total_count": 2, "artifacts": [
                 { "name": "dxvk-native-master-0cfe089", "expired": false },
                 { "name": "dxvk-master-0cfe0898289fb11e3d8d0e1c62cd344b825b7d57", "expired": false } ] }"#,
        )
        .unwrap();
        assert_eq!(
            artifact_name(&artifacts).as_deref(),
            Some("dxvk-master-0cfe0898289fb11e3d8d0e1c62cd344b825b7d57")
        );
        assert_eq!(artifact_name(&serde_json::json!({ "artifacts": [] })), None);
    }

    #[test]
    fn writes_dxvk_conf_from_the_settings() {
        assert_eq!(DxvkConfig::default().text(None), None);
        let config = DxvkConfig {
            hdr: true,
            max_frame_rate: 60,
            hud: Hud::Fps,
            hdr_mod_config: None,
        };
        let text = config.text(None).unwrap();
        assert!(text.starts_with("# Written by Game Manager"));
        for line in [
            "dxgi.enableHDR = True",
            "dxgi.maxFrameRate = 60",
            "d3d9.maxFrameRate = 60",
            "dxvk.hud = fps",
        ] {
            assert!(text.contains(&format!("{line}\r\n")), "{text}");
        }
        let hud_only = DxvkConfig {
            hud: Hud::Full,
            ..DxvkConfig::default()
        }
        .text(None)
        .unwrap();
        assert!(hud_only.contains("dxvk.hud = full") && !hud_only.contains("HDR"));

        // An HDR-mod config comes first, as it is; then our settings.
        let hdr_mod = "#safest\nd3d9.enableSwapChainUpgrade = true\n";
        let text = config
            .text(Some(("dxvk_d3d9_0_safest.conf", hdr_mod)))
            .unwrap();
        let config_at = text.find("d3d9.enableSwapChainUpgrade = true\r\n").unwrap();
        assert!(text.contains("# HDR-mod config: dxvk_d3d9_0_safest.conf\r\n#safest\r\n"));
        assert!(config_at < text.find("dxvk.hud = fps").unwrap(), "{text}");
    }

    #[test]
    fn nvidia_choices_are_saved_with_the_game() {
        let state = DxvkState {
            nvidia: NvidiaChoice {
                present_method: Some(1),
                present_flags: Some(0x0008_0004),
            },
            nvidia_original: Some(NvidiaOriginal {
                profile: "Sifu".into(),
                created_profile: false,
                method: None,
                flags: Some(8),
            }),
            ..DxvkState::default()
        };
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["nvidia"]["presentMethod"], 1);
        assert_eq!(json["nvidia"]["presentFlags"], 0x0008_0004);
        assert_eq!(json["nvidiaOriginal"]["profile"], "Sifu");
        assert_eq!(json["nvidiaOriginal"]["flags"], 8);
        assert_eq!(serde_json::from_value::<DxvkState>(json).unwrap(), state);
        // Untouched settings write nothing.
        let json = serde_json::to_value(DxvkState::default()).unwrap();
        assert!(json.get("nvidia").is_none() && json.get("nvidiaOriginal").is_none());
    }

    #[test]
    fn hdr_mod_configs_follow_the_api() {
        let names: Vec<String> = [
            "dxvk_d3d11_and_d3d10_0_safest.conf",
            "dxvk_d3d11_and_d3d10_6_fully_experimental.conf",
            "dxvk_d3d9_1_2nd_safest.conf",
            "dxvk_d3d9_3_unsafer.conf",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            configs_for(&names, Some(DirectX9)),
            ["dxvk_d3d9_1_2nd_safest.conf", "dxvk_d3d9_3_unsafer.conf"]
        );
        assert_eq!(configs_for(&names, Some(DirectX8)).len(), 2);
        assert_eq!(
            configs_for(&names, Some(DirectX11)),
            [
                "dxvk_d3d11_and_d3d10_0_safest.conf",
                "dxvk_d3d11_and_d3d10_6_fully_experimental.conf"
            ]
        );
        assert_eq!(configs_for(&names, Some(DirectX10)).len(), 2);
        assert!(configs_for(&names, Some(DirectX12)).is_empty());
        assert!(configs_for(&names, None).is_empty());

        assert_eq!(config_label("dxvk_d3d9_3_unsafer.conf"), "3 – unsafer");
        assert_eq!(
            config_label("dxvk_d3d11_and_d3d10_1_2nd_safest.conf"),
            "1 – 2nd safest"
        );
        assert_eq!(
            config_label("dxvk_d3d11_and_d3d10_6_fully_experimental.conf"),
            "6 – fully experimental"
        );

        assert!(check_config("dxvk_d3d9_3_unsafer.conf", Some(DirectX9)).is_ok());
        let wrong = check_config("dxvk_d3d9_3_unsafer.conf", Some(DirectX11)).unwrap_err();
        assert!(wrong.to_string().contains("not for DirectX 11"), "{wrong}");
    }

    #[test]
    fn dxvk_conf_uses_the_config_of_hdr_mod_builds_only() {
        let dir = temp_dir("dxvk-conf");
        let (hdr_mod, upstream) = (dir.join("hdr-mod"), dir.join("upstream"));
        fs::create_dir_all(hdr_mod.join(CONFIGS)).unwrap();
        fs::create_dir_all(upstream.join("x64")).unwrap();
        fs::write(
            hdr_mod.join(CONFIGS).join("dxvk_d3d9_0_safest.conf"),
            "d3d9.enableSwapChainUpgrade = true\n",
        )
        .unwrap();
        let config = DxvkConfig {
            hdr_mod_config: Some("dxvk_d3d9_0_safest.conf".into()),
            ..DxvkConfig::default()
        };
        let text = config_text(&config, &hdr_mod, Some(DirectX9))
            .unwrap()
            .unwrap();
        assert!(text.contains("d3d9.enableSwapChainUpgrade = true"));
        // An upstream build has no configs: nothing else to write, so no file.
        assert_eq!(
            config_text(&config, &upstream, Some(DirectX9)).unwrap(),
            None
        );
        // A config the build lacks, or one for another API, stops it.
        let missing = DxvkConfig {
            hdr_mod_config: Some("dxvk_d3d9_9_gone.conf".into()),
            ..DxvkConfig::default()
        };
        assert!(config_text(&missing, &hdr_mod, Some(DirectX9)).is_err());
        assert!(config_text(&config, &hdr_mod, Some(DirectX11)).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn finds_the_build_folders_in_both_layouts() {
        let dir = temp_dir("dxvk-layouts");
        fs::create_dir_all(dir.join(r"release\dxvk-3.1.1\x64")).unwrap();
        fs::write(dir.join(r"release\dxvk-3.1.1\x64\d3d11.dll"), b"").unwrap();
        fs::create_dir_all(dir.join(r"artifact\x32")).unwrap();
        fs::write(dir.join(r"artifact\x32\d3d9.dll"), b"").unwrap();
        fs::create_dir_all(dir.join(r"artifact\empty\x64")).unwrap();
        assert_eq!(
            find_build_folder(&dir.join("release"), "x64"),
            Some(dir.join(r"release\dxvk-3.1.1\x64"))
        );
        assert_eq!(
            find_build_folder(&dir.join("artifact"), "x32"),
            Some(dir.join(r"artifact\x32"))
        );
        // A folder without DLLs is not a build.
        assert_eq!(find_build_folder(&dir.join("artifact"), "x64"), None);

        // A release-like archive unpacks to its build folders.
        seven_zip(
            &dir.join("release"),
            &["a", "-ttar", r"..\dxvk.tar", "dxvk-3.1.1"],
        );
        seven_zip(&dir, &["a", "-tgzip", "dxvk-3.1.1.tar.gz", "dxvk.tar"]);
        seven_zip::extract(&dir.join("dxvk-3.1.1.tar.gz"), &dir.join("unpacked")).unwrap();
        assert!(find_build_folder(&dir.join("unpacked"), "x64").is_some());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn keeps_the_configs_of_an_hdr_mod_archive() {
        let dir = temp_dir("dxvk-hdr-mod");
        let release = dir.join("release");
        fs::create_dir_all(release.join(r"normal\x64")).unwrap();
        fs::write(release.join(r"normal\x64\d3d11.dll"), b"").unwrap();
        fs::write(release.join(r"normal\x64\dxvk.conf"), b"not a config").unwrap();
        for name in [
            "dxvk_d3d9_0_safest.conf",
            "dxvk_d3d11_and_d3d10_0_safest.conf",
        ] {
            fs::write(release.join(name), b"#safest").unwrap();
        }
        seven_zip(&release, &["a", "-t7z", r"..\hdr-mod.7z", "*"]);
        seven_zip::extract(&dir.join("hdr-mod.7z"), &dir.join("unpacked")).unwrap();

        assert_eq!(
            find_build_folder(&dir.join("unpacked"), "x64"),
            Some(dir.join(r"unpacked\normal\x64"))
        );
        keep_configs(&dir.join("unpacked"), &dir.join(CONFIGS)).unwrap();
        let mut kept: Vec<String> = fs::read_dir(dir.join(CONFIGS))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        // Not the dxvk.conf inside the build folder.
        assert_eq!(
            kept,
            [
                "dxvk_d3d11_and_d3d10_0_safest.conf",
                "dxvk_d3d9_0_safest.conf"
            ]
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
