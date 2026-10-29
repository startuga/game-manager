//! DXVK (https://github.com/doitsujin/dxvk): Direct3D 8, 9, 10 and 11 on Vulkan, installed as
//! the game's Direct3D DLLs next to its program. Two variants:
//!
//! - **Stable**: the latest GitHub release, `dxvk-<version>.tar.gz`.
//! - **Development**: the latest successful build of master by the `artifacts.yml` workflow.
//!   GitHub gives workflow artifacts only to signed-in users, so it is downloaded through
//!   nightly.link (`/actions/runs/<run>/<artifact>.zip`); the artifact is named after the commit,
//!   so GitHub's API is asked for the run and its artifact first.
//!
//! Both hold an `x32` and an `x64` folder of DLLs, kept in
//! `%LOCALAPPDATA%\Game Manager\mods\dxvk\<version>`. Which DLLs a game needs depends on its API
//! (DXVK's README): Direct3D 8 `d3d8` + `d3d9`, 9 `d3d9`, 10 `d3d10core` + `d3d11` + `dxgi`, 11
//! `d3d11` + `dxgi`. DXVK does not do Direct3D 12 (VKD3D-Proton does), OpenGL or Vulkan.
//!
//! Settings go into `dxvk.conf` next to the program, which DXVK reads from the game's working
//! directory: the program's folder for nearly every launch.

use super::{Installed, ModsFile, apply, plan, update};
use crate::graphics::GraphicsApi;
use crate::{Arch, Error, Game, Result, http, json_file, seven_zip};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const API_URL: &str = "https://api.github.com/repos/doitsujin/dxvk";
const CONFIG_FILE: &str = "dxvk.conf";

/// What the Info dialog says about DXVK.
pub const INFO: &str = "DXVK translates Direct3D 8, 9, 10 and 11 into Vulkan.

It can:
• make ReShade's compute shaders work in older Direct3D games
• improve performance and reduce shader stutter in some games
• output HDR (the HDR setting in DXVK settings)

It needs a GPU driver with Vulkan 1.3. Borderless fullscreen works better than exclusive \
fullscreen.

⚠ Games with anti-cheat may ban players who use DXVK.
⚠ Overlays (Steam, NVIDIA, RTSS) may conflict with it.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum DxvkVariant {
    #[default]
    Stable,
    Development,
}

impl DxvkVariant {
    pub const ALL: [DxvkVariant; 2] = [DxvkVariant::Stable, DxvkVariant::Development];

    pub fn name(self) -> &'static str {
        match self {
            DxvkVariant::Stable => "Stable",
            DxvkVariant::Development => "Development",
        }
    }

    /// The name with where builds come from.
    pub fn label(self) -> &'static str {
        match self {
            DxvkVariant::Stable => "Stable (GitHub releases)",
            DxvkVariant::Development => "Development (latest master build, via nightly.link)",
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
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DxvkConfig {
    /// `dxgi.enableHDR`.
    pub hdr: bool,
    /// `dxgi.maxFrameRate` and `d3d9.maxFrameRate`; 0 is no limit.
    pub max_frame_rate: u32,
    pub hud: Hud,
}

impl DxvkConfig {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The `dxvk.conf` for these settings.
    pub fn text(&self) -> String {
        let mut lines = vec![
            "# Written by Game Manager from this game's DXVK settings; saving them again replaces \
             this file."
                .to_owned(),
        ];
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
        lines.join("\r\n") + "\r\n"
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
}

/// A DXVK build to download.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Build {
    /// `3.1.1`, or `master-0cfe089` for a development build.
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
            stable_build(&get_json(&url)?).ok_or(Error::NoRelease { url })
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

/// `releases/latest`: the release's `dxvk-<version>.tar.gz` (not the Linux `dxvk-native-…`).
fn stable_build(release: &Value) -> Option<Build> {
    let version = release["tag_name"].as_str()?.trim_start_matches('v');
    let asset = release["assets"].as_array()?.iter().find(|asset| {
        asset["name"]
            .as_str()
            .is_some_and(|name| name == format!("dxvk-{version}.tar.gz"))
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
    if !state.config.is_empty() {
        files.push((CONFIG_FILE.to_owned(), staged_config(&state.config)?));
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

/// Saves the game's DXVK settings; when DXVK is installed, its `dxvk.conf` follows them.
pub fn save_settings(game: &Game, variant: DxvkVariant, config: DxvkConfig) -> Result<ModsFile> {
    let state = state(game)?;
    let mut installed = state.installed;
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
        let new = if config.is_empty() {
            Vec::new()
        } else {
            vec![(CONFIG_FILE.to_owned(), staged_config(&config)?)]
        };
        let plan = plan(Some(&previous), &record.folder, &new)?;
        apply(&plan.ops)?;
        files.extend(plan.files);
        backups.extend(plan.backups);
        record.files = files;
        record.backups = backups;
    }
    update(&game.id, |mods| {
        mods.dxvk = Some(DxvkState {
            variant,
            config,
            installed,
        });
        drop_default(&mut mods.dxvk);
    })
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

/// `dxvk.conf` for `config`, written where the copy (possibly as administrator) reads it.
fn staged_config(config: &DxvkConfig) -> Result<PathBuf> {
    let dir = json_file::local_file("mods")?.join("staging");
    fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
    let file = dir.join(CONFIG_FILE);
    fs::write(&file, config.text()).map_err(Error::io(&file))?;
    Ok(file)
}

/// The build's folder in the cache, holding `x32` and `x64`: downloaded and unpacked the first
/// time.
fn cached_build(build: &Build) -> Result<PathBuf> {
    let root = json_file::local_file("mods")?.join("dxvk");
    let version: String = build
        .version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || ".-_".contains(*c))
        .collect();
    let dir = root.join(version);
    if [Bits::X32, Bits::X64]
        .iter()
        .any(|bits| dir.join(bits.folder()).is_dir())
    {
        return Ok(dir);
    }
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
    fs::rename(&kept, &dir).map_err(Error::io(&dir))?;
    let _ = fs::remove_dir_all(&work);
    Ok(dir)
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
            stable_build(&release),
            Some(Build {
                version: "3.1.1".into(),
                page: "https://github.com/doitsujin/dxvk/releases/tag/v3.1.1".into(),
                url: "https://github.com/doitsujin/dxvk/releases/download/v3.1.1/dxvk-3.1.1.tar.gz"
                    .into(),
                file_name: "dxvk-3.1.1.tar.gz".into(),
            })
        );
        assert_eq!(
            stable_build(&serde_json::json!({ "message": "Not Found" })),
            None
        );

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
        assert!(DxvkConfig::default().is_empty());
        let config = DxvkConfig {
            hdr: true,
            max_frame_rate: 60,
            hud: Hud::Fps,
        };
        let text = config.text();
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
        .text();
        assert!(hud_only.contains("dxvk.hud = full") && !hud_only.contains("HDR"));
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
}
