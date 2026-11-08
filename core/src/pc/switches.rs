//! The Windows switches that matter for games, read and written where Windows' Settings app keeps
//! them:
//!
//! - **Hardware-accelerated GPU scheduling**: `HwSchMode` under
//!   `HKLM\SYSTEM\CurrentControlSet\Control\GraphicsDrivers` (2 on, 1 off). With no value the card
//!   or driver doesn't offer it. Writing needs administrator rights: Game Manager starts a copy of
//!   itself with `--gpu-scheduling on|off` (see `elevate`). It applies after a restart.
//! - **Game Mode**: `AutoGameModeEnabled` under `HKCU\Software\Microsoft\GameBar` (missing = on).
//! - **Optimizations for windowed games** and **Auto HDR**: the `SwapEffectUpgradeEnable` and
//!   `AutoHDREnable` flags of one string, `DirectXUserGlobalSettings`, under
//!   `HKCU\Software\Microsoft\DirectX\UserGpuPreferences` ("VRROptimizeEnable=1;AutoHDREnable=0;").
//!   - Newer builds keep more bits in a flag's number (2097): only the lowest is on or off.
//!   - Writing changes that bit and keeps every other flag as it was.
//!   - DirectX reads them when a game starts.

use crate::{Error, Result, elevate, security};
use windows::core::WIN32_ERROR;
use windows_registry::{CURRENT_USER, LOCAL_MACHINE};

/// The argument that makes `app.exe` set GPU scheduling (`on` or `off`) instead of its window.
pub const GPU_SCHEDULING_ARG: &str = "--gpu-scheduling";

const GRAPHICS_KEY: &str = r"SYSTEM\CurrentControlSet\Control\GraphicsDrivers";
const GAME_BAR_KEY: &str = r"Software\Microsoft\GameBar";
const DIRECTX_KEY: &str = r"Software\Microsoft\DirectX\UserGpuPreferences";
const DIRECTX_VALUE: &str = "DirectXUserGlobalSettings";
const WINDOWED_FLAG: &str = "SwapEffectUpgradeEnable";
const AUTO_HDR_FLAG: &str = "AutoHDREnable";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Switches {
    /// As saved (it applies after a restart); `None`: not offered by the card or its driver.
    pub gpu_scheduling: Option<bool>,
    pub game_mode: bool,
    pub windowed_optimizations: bool,
    pub auto_hdr: bool,
}

pub(super) fn read() -> Switches {
    let settings = directx_settings();
    Switches {
        gpu_scheduling: LOCAL_MACHINE
            .open(GRAPHICS_KEY)
            .and_then(|key| key.get_u32("HwSchMode"))
            .ok()
            .map(|mode| mode == 2),
        game_mode: CURRENT_USER
            .open(GAME_BAR_KEY)
            .and_then(|key| key.get_u32("AutoGameModeEnabled"))
            .ok()
            .is_none_or(|enabled| enabled != 0),
        windowed_optimizations: flag(&settings, WINDOWED_FLAG).unwrap_or(false),
        auto_hdr: flag(&settings, AUTO_HDR_FLAG).unwrap_or(false),
    }
}

pub fn set_game_mode(on: bool) -> Result<()> {
    CURRENT_USER
        .create(GAME_BAR_KEY)
        .and_then(|key| key.set_u32("AutoGameModeEnabled", u32::from(on)))
        .map_err(Error::windows(GAME_BAR_KEY))
}

pub fn set_windowed_optimizations(on: bool) -> Result<()> {
    set_directx_flag(WINDOWED_FLAG, on)
}

pub fn set_auto_hdr(on: bool) -> Result<()> {
    set_directx_flag(AUTO_HDR_FLAG, on)
}

/// Saves GPU scheduling, as administrator: Windows asks first. It applies after a restart.
pub fn set_gpu_scheduling(on: bool) -> Result<()> {
    if security::is_elevated() {
        write_gpu_scheduling(on)
    } else {
        elevate::run(GPU_SCHEDULING_ARG, if on { "on" } else { "off" })
    }
}

/// Runs `--gpu-scheduling on|off`. The result is the program's exit code: 0, or the HRESULT of
/// what failed.
pub fn run_encoded(argument: &str) -> u32 {
    const INVALID_PARAMETER: WIN32_ERROR = WIN32_ERROR(87);
    const INVALID_FUNCTION: WIN32_ERROR = WIN32_ERROR(1);
    let on = match argument {
        "on" => true,
        "off" => false,
        _ => return INVALID_PARAMETER.to_hresult().0 as u32,
    };
    match write_gpu_scheduling(on) {
        Ok(()) => 0,
        Err(Error::Windows { source, .. }) => source.code().0 as u32,
        Err(_) => INVALID_FUNCTION.to_hresult().0 as u32,
    }
}

fn write_gpu_scheduling(on: bool) -> Result<()> {
    LOCAL_MACHINE
        .create(GRAPHICS_KEY)
        .and_then(|key| key.set_u32("HwSchMode", if on { 2 } else { 1 }))
        .map_err(Error::windows(GRAPHICS_KEY))
}

fn directx_settings() -> String {
    CURRENT_USER
        .open(DIRECTX_KEY)
        .and_then(|key| key.get_string(DIRECTX_VALUE))
        .unwrap_or_default()
}

fn set_directx_flag(name: &str, on: bool) -> Result<()> {
    let settings = with_flag(&directx_settings(), name, on);
    CURRENT_USER
        .create(DIRECTX_KEY)
        .and_then(|key| key.set_string(DIRECTX_VALUE, &settings))
        .map_err(Error::windows(DIRECTX_KEY))
}

/// A flag of `DirectXUserGlobalSettings`: its lowest bit; `None` when it isn't there.
pub(crate) fn flag(settings: &str, name: &str) -> Option<bool> {
    entries(settings)
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .and_then(|(_, value)| value.parse::<i64>().ok())
        .map(|value| value & 1 == 1)
}

/// `settings` with the flag's lowest bit set or cleared, every other flag kept in its place.
pub(crate) fn with_flag(settings: &str, name: &str, on: bool) -> String {
    let mut found = false;
    let mut parts: Vec<String> = entries(settings)
        .map(|(key, value)| {
            if key.eq_ignore_ascii_case(name) {
                found = true;
                let number = value.parse::<i64>().unwrap_or(0);
                let number = if on { number | 1 } else { number & !1 };
                format!("{key}={number}")
            } else {
                format!("{key}={value}")
            }
        })
        .collect();
    if !found {
        parts.push(format!("{name}={}", u8::from(on)));
    }
    parts.iter().map(|part| format!("{part};")).collect()
}

fn entries(settings: &str) -> impl Iterator<Item = (&str, &str)> {
    settings
        .split(';')
        .filter_map(|part| part.split_once('='))
        .map(|(key, value)| (key.trim(), value.trim()))
        .filter(|(key, _)| !key.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SETTINGS: &str = "VRROptimizeEnable=1;AutoHDREnable=0;SwapEffectUpgradeEnable=1;";

    #[test]
    fn reads_flags() {
        assert_eq!(flag(SETTINGS, "SwapEffectUpgradeEnable"), Some(true));
        assert_eq!(flag(SETTINGS, "AutoHDREnable"), Some(false));
        assert_eq!(flag(SETTINGS, "Missing"), None);
        // Newer builds keep more bits: only the lowest is on or off.
        assert_eq!(flag("AutoHDREnable=2097;", "AutoHDREnable"), Some(true));
        assert_eq!(flag("AutoHDREnable=2096;", "AutoHDREnable"), Some(false));
        assert_eq!(flag("", "AutoHDREnable"), None);
    }

    #[test]
    fn writes_one_flag_and_keeps_the_rest() {
        assert_eq!(
            with_flag(SETTINGS, "AutoHDREnable", true),
            "VRROptimizeEnable=1;AutoHDREnable=1;SwapEffectUpgradeEnable=1;"
        );
        assert_eq!(
            with_flag(SETTINGS, "SwapEffectUpgradeEnable", false),
            "VRROptimizeEnable=1;AutoHDREnable=0;SwapEffectUpgradeEnable=0;"
        );
        assert_eq!(
            with_flag("AutoHDREnable=2096;", "AutoHDREnable", true),
            "AutoHDREnable=2097;"
        );
        assert_eq!(
            with_flag("AutoHDREnable=2097;", "AutoHDREnable", false),
            "AutoHDREnable=2096;"
        );
        assert_eq!(
            with_flag("VRROptimizeEnable=1;", "AutoHDREnable", true),
            "VRROptimizeEnable=1;AutoHDREnable=1;"
        );
        assert_eq!(with_flag("", "AutoHDREnable", false), "AutoHDREnable=0;");
    }
}
