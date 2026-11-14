//! The active displays and the changes the My PC page makes to them: mode (resolution and refresh
//! rate), HDR, colour profile and brightness. Win32 only, no WMI.
//!
//! - **Reading**: `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)`, one path per active display.
//!   - The target gives the monitor's name, refresh rate and output technology. Internal, LVDS
//!     and embedded DisplayPort/UDI outputs are built-in panels.
//!   - The source gives the GDI name (`\\.\DISPLAY1`) and, through its mode, size and desktop
//!     position; the display at 0,0 is the main one.
//!   - HDR: `GET_ADVANCED_COLOR_INFO_2` (Windows 11 24H2+) tells HDR apart from wide colour.
//!     Older builds answer only `GET_ADVANCED_COLOR_INFO`, whose "advanced colour" is HDR unless
//!     wide colour is enforced.
//!   - Modes: `EnumDisplaySettingsW` (32-bit colour, 640 wide and up), largest and fastest first.
//! - **Mode**: `ChangeDisplaySettingsExW`, tested with `CDS_TEST` first, then tried
//!   (`try_mode`): applied but not saved, so the registry still holds the mode before it. The page
//!   asks to keep it; Keep saves it (`keep_mode`, `CDS_UPDATEREGISTRY | CDS_NORESET`, which
//!   writes the mode without switching again), and Go back or no answer tries the old one. A mode
//!   the display can't show is never saved: if the app ends or Windows restarts during the trial,
//!   the old mode comes back.
//! - **HDR**: `SET_HDR_STATE` where the newer packet exists, else `SET_ADVANCED_COLOR_STATE`
//!   (which, on 24H2, would switch wide colour too).
//! - **Colour profiles**: Windows 11's per-display associations in `mscms.dll`
//!   (`ColorProfileGetDisplayList`, `…GetDisplayDefault`, `…SetDisplayDefaultAssociation`), the
//!   user's first, then the system's. They are SDR or HDR ones by the display's mode.
//!   - They are loaded at run time: Windows 10 lacks them, and an import would stop `app.exe`
//!     from starting there.
//! - **Brightness**: DDC/CI through `dxva2.dll`'s monitor configuration API, like the monitor's
//!   own buttons.
//!   - These functions have no bindings in the `windows` crate, so they are declared here.
//!   - Many monitors keep DDC/CI off, and a built-in panel's brightness is reachable only through
//!     WMI, so both answer "not available".
//!   - DDC/CI is slow (tens of milliseconds a call): call it off the UI thread.

use super::hardware::wide;
use crate::{Error, Result};
use std::cmp::Reverse;
use std::ptr;
use std::sync::{Mutex, PoisonError};
use windows::Win32::{
    CDS_NORESET, CDS_TEST, CDS_UPDATEREGISTRY, COLORPROFILESUBTYPE, COLORPROFILETYPE,
    CPST_EXTENDED_DISPLAY_COLOR_MODE, CPST_STANDARD_DISPLAY_COLOR_MODE, CPT_ICC,
    ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_RESTART, DISP_CHANGE_SUCCESSFUL,
    DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR, DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE,
    DISPLAYCONFIG_DEVICE_INFO_TYPE, DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO,
    DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE, DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0,
    DISPLAYCONFIG_SET_HDR_STATE, DISPLAYCONFIG_SET_HDR_STATE_0, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DM_DISPLAYFREQUENCY, DM_PELSHEIGHT, DM_PELSWIDTH,
    DisplayConfigGetDeviceInfo, DisplayConfigSetDeviceInfo, ENUM_CURRENT_SETTINGS,
    EnumDisplaySettingsW, FreeLibrary, GetDisplayConfigBufferSizes, GetMonitorInfoW,
    GetProcAddress, HANDLE, HMODULE, HMONITOR, LOAD_LIBRARY_SEARCH_SYSTEM32, LUID, LoadLibraryExW,
    LocalFree, MONITOR_DEFAULTTONULL, MONITORINFO, MONITORINFOEXW, MonitorFromPoint, POINT,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig, WCS_PROFILE_MANAGEMENT_SCOPE,
    WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER, WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
};
use windows::core::{BOOL, HRESULT, HSTRING, PCWSTR, PWSTR, WIN32_ERROR, s, w};

/// An active display.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// The monitor's name ("LG ULTRAGEAR"), or "Display".
    pub name: String,
    /// `\\.\DISPLAY1`, empty when Windows did not say: then nothing can be changed.
    pub gdi_name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    /// The top-left corner on the desktop.
    pub position: (i32, i32),
    pub primary: bool,
    /// A laptop's own panel.
    pub built_in: bool,
    pub hdr_supported: bool,
    pub hdr_on: bool,
    /// Largest and fastest first.
    pub modes: Vec<DisplayMode>,
    pub current: Option<DisplayMode>,
    /// The ICC profiles linked to the display for its mode (SDR or HDR), file names.
    pub color_profiles: Vec<String>,
    /// The one Windows uses.
    pub color_profile: Option<String>,
    target: Target,
}

/// A resolution and refresh rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub hz: u32,
}

/// What the display configuration calls need to find the display again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Target {
    adapter_low: u32,
    adapter_high: i32,
    source_id: u32,
    target_id: u32,
    /// The display answered `GET_ADVANCED_COLOR_INFO_2`, so `SET_HDR_STATE` exists.
    hdr_state: bool,
}

impl Target {
    fn adapter(&self) -> LUID {
        LUID {
            LowPart: self.adapter_low,
            HighPart: self.adapter_high,
        }
    }
}

pub(super) fn displays() -> Result<Vec<Display>> {
    let (paths, modes) = query()?;
    let mscms = Mscms::load();
    let mut displays: Vec<Display> = paths
        .iter()
        .map(|path| display(path, &modes, mscms.as_ref()))
        .collect();
    displays.sort_by_key(|display| !display.primary);
    Ok(displays)
}

/// The active paths and their modes. The configuration can change between the two calls, which
/// then answers "insufficient buffer": asked again.
fn query() -> Result<(Vec<DISPLAYCONFIG_PATH_INFO>, Vec<DISPLAYCONFIG_MODE_INFO>)> {
    const INSUFFICIENT_BUFFER: i32 = 122;
    let flags = QDC_ONLY_ACTIVE_PATHS as u32;
    let mut result = 0;
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        // SAFETY: two valid out-pointers.
        let sized = unsafe { GetDisplayConfigBufferSizes(flags, &mut path_count, &mut mode_count) };
        check(sized, "The displays")?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: the arrays hold the counts given; no topology is asked for.
        result = unsafe {
            QueryDisplayConfig(
                flags,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                ptr::null_mut(),
            )
        };
        if result == INSUFFICIENT_BUFFER {
            continue;
        }
        check(result, "The displays")?;
        paths.truncate(path_count as usize);
        modes.truncate(mode_count as usize);
        return Ok((paths, modes));
    }
    check(result, "The displays").map(|()| (Vec::new(), Vec::new()))
}

fn display(
    path: &DISPLAYCONFIG_PATH_INFO,
    modes: &[DISPLAYCONFIG_MODE_INFO],
    mscms: Option<&Mscms>,
) -> Display {
    let source = &path.sourceInfo;
    let target_info = &path.targetInfo;
    let name = target_name(target_info)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Display".to_owned());
    let gdi_name = source_name(source).unwrap_or_default();
    // SAFETY: without QDC_VIRTUAL_MODE_AWARE the union holds the source mode's index.
    let index = unsafe { source.Anonymous.modeInfoIdx } as usize;
    let source_mode = modes
        .get(index)
        .filter(|mode| mode.infoType == DISPLAYCONFIG_MODE_INFO_TYPE_SOURCE)
        .map(|mode| {
            // SAFETY: a source mode's union holds its source mode.
            unsafe { mode.Anonymous.sourceMode }
        });
    let (width, height, position) = source_mode.map_or((0, 0, (0, 0)), |mode| {
        (mode.width, mode.height, (mode.position.x, mode.position.y))
    });
    let rate = target_info.refreshRate;
    let refresh_hz = if rate.Denominator == 0 {
        0.0
    } else {
        (f64::from(rate.Numerator) / f64::from(rate.Denominator) * 100.0).round() / 100.0
    };
    let (hdr_supported, hdr_on, hdr_state) = advanced_color(target_info.adapterId, target_info.id);
    let target = Target {
        adapter_low: target_info.adapterId.LowPart,
        adapter_high: target_info.adapterId.HighPart,
        source_id: source.id,
        target_id: target_info.id,
        hdr_state,
    };
    let (modes, current) = if gdi_name.is_empty() {
        (Vec::new(), None)
    } else {
        display_modes(&HSTRING::from(gdi_name.as_str()))
    };
    let (color_profiles, color_profile) =
        mscms.map_or((Vec::new(), None), |mscms| mscms.profiles(&target, hdr_on));
    Display {
        name,
        gdi_name,
        width,
        height,
        refresh_hz,
        position,
        primary: position == (0, 0),
        built_in: [
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
            DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED,
        ]
        .contains(&target_info.outputTechnology),
        hdr_supported,
        hdr_on,
        modes,
        current,
        color_profiles,
        color_profile,
        target,
    }
}

fn header(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    size: usize,
    adapter: LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size as u32,
        adapterId: adapter,
        id,
    }
}

fn target_name(target: &windows::Win32::DISPLAYCONFIG_PATH_TARGET_INFO) -> Option<String> {
    let mut packet = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(),
            target.adapterId,
            target.id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet starts with a header naming its type and size.
    let result = unsafe { DisplayConfigGetDeviceInfo(ptr::from_mut(&mut packet).cast()) };
    (result == 0).then(|| wide(&packet.monitorFriendlyDeviceName))
}

fn source_name(source: &windows::Win32::DISPLAYCONFIG_PATH_SOURCE_INFO) -> Option<String> {
    let mut packet = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
            size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(),
            source.adapterId,
            source.id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet starts with a header naming its type and size.
    let result = unsafe { DisplayConfigGetDeviceInfo(ptr::from_mut(&mut packet).cast()) };
    (result == 0).then(|| wide(&packet.viewGdiDeviceName))
}

/// HDR supported, HDR on, and whether the newer packets exist.
fn advanced_color(adapter: LUID, id: u32) -> (bool, bool, bool) {
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2 {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2,
            size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2>(),
            adapter,
            id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet starts with a header naming its type and size.
    if unsafe { DisplayConfigGetDeviceInfo(ptr::from_mut(&mut info).cast()) } == 0 {
        // SAFETY: the union is the packet's bit field, read whole.
        let bits = unsafe { info.Anonymous.value };
        // Bit 4: highDynamicRangeSupported; bit 5: highDynamicRangeUserEnabled.
        let on =
            bits & (1 << 5) != 0 || info.activeColorMode == DISPLAYCONFIG_ADVANCED_COLOR_MODE_HDR;
        return (bits & (1 << 4) != 0, on, true);
    }
    let mut info = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            size_of::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(),
            adapter,
            id,
        ),
        ..Default::default()
    };
    // SAFETY: the packet starts with a header naming its type and size.
    if unsafe { DisplayConfigGetDeviceInfo(ptr::from_mut(&mut info).cast()) } != 0 {
        return (false, false, false);
    }
    // SAFETY: the union is the packet's bit field, read whole.
    let bits = unsafe { info.Anonymous.value };
    // Bit 0: supported; bit 1: enabled; bit 2: wide colour enforced (not HDR).
    let supported = bits & 1 != 0;
    (supported, bits & 2 != 0 && bits & 4 == 0, false)
}

fn devmode() -> DEVMODEW {
    DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..Default::default()
    }
}

/// The display's modes and the current one.
fn display_modes(name: &HSTRING) -> (Vec<DisplayMode>, Option<DisplayMode>) {
    let mut modes = Vec::new();
    for index in 0u32.. {
        let mut mode = devmode();
        // SAFETY: a null-terminated device name and a DEVMODEW with its size set.
        if !unsafe { EnumDisplaySettingsW(name, index, &mut mode) }.as_bool() {
            break;
        }
        if mode.dmBitsPerPel == 32 && mode.dmPelsWidth >= 640 && mode.dmDisplayFrequency > 1 {
            modes.push(DisplayMode {
                width: mode.dmPelsWidth,
                height: mode.dmPelsHeight,
                hz: mode.dmDisplayFrequency,
            });
        }
    }
    let mut current = devmode();
    // SAFETY: as above.
    let read = unsafe { EnumDisplaySettingsW(name, ENUM_CURRENT_SETTINGS, &mut current) };
    let current = read.as_bool().then_some(DisplayMode {
        width: current.dmPelsWidth,
        height: current.dmPelsHeight,
        hz: current.dmDisplayFrequency,
    });
    (sorted(modes), current)
}

/// Largest first, then widest, then fastest; each mode once.
pub(crate) fn sorted(mut modes: Vec<DisplayMode>) -> Vec<DisplayMode> {
    modes.sort_by_key(|mode| {
        Reverse((
            u64::from(mode.width) * u64::from(mode.height),
            mode.width,
            mode.hz,
        ))
    });
    modes.dedup();
    modes
}

/// Switches the display to `mode` without saving it (see the module's doc): `keep_mode` saves it.
pub fn try_mode(display: &Display, mode: DisplayMode) -> Result<()> {
    change_mode(display, mode, 0)
}

/// Saves `mode`, the one the display shows after `try_mode`, as Settings saves it, without
/// switching again.
pub fn keep_mode(display: &Display, mode: DisplayMode) -> Result<()> {
    change_mode(display, mode, (CDS_UPDATEREGISTRY | CDS_NORESET) as u32)
}

/// `ChangeDisplaySettingsExW` with `mode` and `flags`, once `CDS_TEST` says the display takes it.
fn change_mode(display: &Display, mode: DisplayMode, flags: u32) -> Result<()> {
    let name = HSTRING::from(display.gdi_name.as_str());
    let mut devmode = devmode();
    // SAFETY: a null-terminated device name and a DEVMODEW with its size set.
    unsafe { EnumDisplaySettingsW(&name, ENUM_CURRENT_SETTINGS, &mut devmode) }
        .ok()
        .map_err(Error::windows(format!("{}'s current mode", display.name)))?;
    devmode.dmPelsWidth = mode.width;
    devmode.dmPelsHeight = mode.height;
    devmode.dmDisplayFrequency = mode.hz;
    devmode.dmFields = (DM_PELSWIDTH | DM_PELSHEIGHT | DM_DISPLAYFREQUENCY) as u32;
    let refused = |code: i32| Error::DisplayModeRefused {
        display: display.name.clone(),
        width: mode.width,
        height: mode.height,
        hz: mode.hz,
        code,
    };
    // SAFETY: a null-terminated device name and a filled DEVMODEW; no window, no extra data.
    let test =
        unsafe { ChangeDisplaySettingsExW(&name, Some(&devmode), None, CDS_TEST as u32, None) };
    if test != DISP_CHANGE_SUCCESSFUL {
        return Err(refused(test));
    }
    // SAFETY: as above.
    let result = unsafe { ChangeDisplaySettingsExW(&name, Some(&devmode), None, flags, None) };
    match result {
        DISP_CHANGE_SUCCESSFUL => Ok(()),
        DISP_CHANGE_RESTART => Err(Error::DisplayModeNeedsRestart(display.name.clone())),
        code => Err(refused(code)),
    }
}

/// Turns HDR ("Use HDR" in Settings ▸ Display) on or off.
pub fn set_hdr(display: &Display, on: bool) -> Result<()> {
    let target = &display.target;
    let result = if target.hdr_state {
        let packet = DISPLAYCONFIG_SET_HDR_STATE {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE,
                size_of::<DISPLAYCONFIG_SET_HDR_STATE>(),
                target.adapter(),
                target.target_id,
            ),
            Anonymous: DISPLAYCONFIG_SET_HDR_STATE_0 {
                value: u32::from(on),
            },
        };
        // SAFETY: the packet starts with a header naming its type and size.
        unsafe { DisplayConfigSetDeviceInfo(ptr::from_ref(&packet).cast()) }
    } else {
        let packet = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: header(
                DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                size_of::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(),
                target.adapter(),
                target.target_id,
            ),
            Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 {
                value: u32::from(on),
            },
        };
        // SAFETY: the packet starts with a header naming its type and size.
        unsafe { DisplayConfigSetDeviceInfo(ptr::from_ref(&packet).cast()) }
    };
    check(result, &format!("HDR on {}", display.name))
}

/// Makes `profile`, one of the display's linked profiles, its default for this user.
pub fn set_color_profile(display: &Display, profile: &str) -> Result<()> {
    let refused = || Error::DisplayRefused {
        display: display.name.clone(),
        what: "colour profile",
    };
    let mscms = Mscms::load().ok_or_else(refused)?;
    let name = HSTRING::from(profile);
    // SAFETY: a null-terminated profile name; the rest are values.
    unsafe {
        (mscms.set_default)(
            WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
            PCWSTR(name.as_ptr()),
            CPT_ICC,
            subtype(display.hdr_on),
            display.target.adapter(),
            display.target.source_id,
        )
    }
    .ok()
    .map_err(Error::windows(format!(
        "The colour profile of {}",
        display.name
    )))
}

fn subtype(hdr_on: bool) -> COLORPROFILESUBTYPE {
    if hdr_on {
        CPST_EXTENDED_DISPLAY_COLOR_MODE
    } else {
        CPST_STANDARD_DISPLAY_COLOR_MODE
    }
}

/// One DDC/CI conversation at a time: keys can send several brightness changes in a row, and a
/// monitor answers one request before the next.
static DDC: Mutex<()> = Mutex::new(());

/// The display's brightness in percent over DDC/CI; `None` when it doesn't take it (DDC/CI off,
/// or a built-in panel). Slow.
pub fn brightness(display: &Display) -> Option<u32> {
    if display.built_in {
        return None;
    }
    let _ddc = DDC.lock().unwrap_or_else(PoisonError::into_inner);
    let monitors = PhysicalMonitors::of(monitor(display)?)?;
    let (minimum, current, maximum) = monitors.brightness()?;
    (maximum > minimum).then(|| percent_of(current, minimum, maximum))
}

/// Sets the display's brightness in percent over DDC/CI. Slow.
pub fn set_brightness(display: &Display, percent: u32) -> Result<()> {
    let refused = || Error::DisplayRefused {
        display: display.name.clone(),
        what: "brightness",
    };
    let _ddc = DDC.lock().unwrap_or_else(PoisonError::into_inner);
    let monitors = monitor(display)
        .and_then(PhysicalMonitors::of)
        .ok_or_else(refused)?;
    let (minimum, _, maximum) = monitors.brightness().ok_or_else(refused)?;
    if maximum <= minimum {
        return Err(refused());
    }
    // SAFETY: a physical monitor handle owned by `monitors`.
    let set = unsafe { SetMonitorBrightness(monitors.first(), level(percent, minimum, maximum)) };
    set.as_bool().then_some(()).ok_or_else(refused)
}

/// `current` within the monitor's range, in percent.
pub(crate) fn percent_of(current: u32, minimum: u32, maximum: u32) -> u32 {
    let range = maximum - minimum;
    let current = current.clamp(minimum, maximum) - minimum;
    (current * 100 + range / 2) / range
}

/// A percentage as a level in the monitor's range.
pub(crate) fn level(percent: u32, minimum: u32, maximum: u32) -> u32 {
    minimum + ((maximum - minimum) * percent.min(100) + 50) / 100
}

/// The display's monitor handle: the one at its top-left corner, if it carries its GDI name.
fn monitor(display: &Display) -> Option<HMONITOR> {
    let point = POINT {
        x: display.position.0 + 1,
        y: display.position.1 + 1,
    };
    // SAFETY: no pointers.
    let monitor = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL as u32) };
    if monitor.is_null() {
        return None;
    }
    let mut info = MONITORINFOEXW {
        base: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: a monitor handle and an extended info struct whose size is set.
    let read = unsafe { GetMonitorInfoW(monitor, ptr::from_mut(&mut info).cast()) };
    (read.as_bool() && wide(&info.szDevice).eq_ignore_ascii_case(&display.gdi_name))
        .then_some(monitor)
}

fn check(result: i32, context: &str) -> Result<()> {
    if result == 0 {
        Ok(())
    } else {
        Err(Error::windows(context)(WIN32_ERROR(result as u32).into()))
    }
}

/// `PHYSICAL_MONITOR` (`PhysicalMonitorEnumerationAPI.h`).
#[repr(C)]
struct PhysicalMonitor {
    handle: HANDLE,
    description: [u16; 128],
}

#[link(name = "dxva2")]
unsafe extern "system" {
    fn GetNumberOfPhysicalMonitorsFromHMONITOR(monitor: HMONITOR, count: *mut u32) -> BOOL;
    fn GetPhysicalMonitorsFromHMONITOR(
        monitor: HMONITOR,
        count: u32,
        monitors: *mut PhysicalMonitor,
    ) -> BOOL;
    fn DestroyPhysicalMonitors(count: u32, monitors: *const PhysicalMonitor) -> BOOL;
    fn GetMonitorBrightness(
        monitor: HANDLE,
        minimum: *mut u32,
        current: *mut u32,
        maximum: *mut u32,
    ) -> BOOL;
    fn SetMonitorBrightness(monitor: HANDLE, brightness: u32) -> BOOL;
}

/// A monitor's physical monitors, destroyed when dropped.
struct PhysicalMonitors(Vec<PhysicalMonitor>);

impl PhysicalMonitors {
    fn of(monitor: HMONITOR) -> Option<Self> {
        let mut count = 0u32;
        // SAFETY: a monitor handle and a valid out-pointer.
        let counted = unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(monitor, &mut count) };
        if !counted.as_bool() || count == 0 {
            return None;
        }
        let mut monitors: Vec<PhysicalMonitor> = (0..count)
            .map(|_| PhysicalMonitor {
                handle: ptr::null_mut(),
                description: [0; 128],
            })
            .collect();
        // SAFETY: `monitors` holds `count` entries.
        let read =
            unsafe { GetPhysicalMonitorsFromHMONITOR(monitor, count, monitors.as_mut_ptr()) };
        read.as_bool().then_some(Self(monitors))
    }

    fn first(&self) -> HANDLE {
        self.0
            .first()
            .map_or(ptr::null_mut(), |monitor| monitor.handle)
    }

    /// Minimum, current and maximum.
    fn brightness(&self) -> Option<(u32, u32, u32)> {
        let (mut minimum, mut current, mut maximum) = (0, 0, 0);
        // SAFETY: a physical monitor handle owned by `self` and three out-pointers.
        let read =
            unsafe { GetMonitorBrightness(self.first(), &mut minimum, &mut current, &mut maximum) };
        read.as_bool().then_some((minimum, current, maximum))
    }
}

impl Drop for PhysicalMonitors {
    fn drop(&mut self) {
        // SAFETY: the handles came from GetPhysicalMonitorsFromHMONITOR and are destroyed once.
        let _ = unsafe { DestroyPhysicalMonitors(self.0.len() as u32, self.0.as_ptr()) };
    }
}

type GetDisplayList = unsafe extern "system" fn(
    WCS_PROFILE_MANAGEMENT_SCOPE,
    LUID,
    u32,
    *mut *mut PWSTR,
    *mut u32,
) -> HRESULT;
type GetDisplayDefault = unsafe extern "system" fn(
    WCS_PROFILE_MANAGEMENT_SCOPE,
    LUID,
    u32,
    COLORPROFILETYPE,
    COLORPROFILESUBTYPE,
    *mut PWSTR,
) -> HRESULT;
type SetDisplayDefault = unsafe extern "system" fn(
    WCS_PROFILE_MANAGEMENT_SCOPE,
    PCWSTR,
    COLORPROFILETYPE,
    COLORPROFILESUBTYPE,
    LUID,
    u32,
) -> HRESULT;

/// `mscms.dll`'s per-display profile functions, when this Windows has them.
struct Mscms {
    get_list: GetDisplayList,
    get_default: GetDisplayDefault,
    set_default: SetDisplayDefault,
    /// Kept loaded while the functions are used.
    _library: Library,
}

impl Mscms {
    fn load() -> Option<Self> {
        // SAFETY: a constant name; only System32 is searched.
        let module =
            unsafe { LoadLibraryExW(w!("mscms.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32 as u32) };
        if module.is_null() {
            return None;
        }
        let library = Library(module);
        // SAFETY: a loaded module and a constant export name.
        let get_list = unsafe { GetProcAddress(library.0, s!("ColorProfileGetDisplayList")) }?;
        // SAFETY: as above.
        let get_default =
            unsafe { GetProcAddress(library.0, s!("ColorProfileGetDisplayDefault")) }?;
        // SAFETY: as above.
        let set_default =
            unsafe { GetProcAddress(library.0, s!("ColorProfileSetDisplayDefaultAssociation")) }?;
        // SAFETY: the export's signature, from icm.h.
        let get_list: GetDisplayList = unsafe { std::mem::transmute(get_list) };
        // SAFETY: as above.
        let get_default: GetDisplayDefault = unsafe { std::mem::transmute(get_default) };
        // SAFETY: as above.
        let set_default: SetDisplayDefault = unsafe { std::mem::transmute(set_default) };
        Some(Self {
            get_list,
            get_default,
            set_default,
            _library: library,
        })
    }

    /// The display's linked profiles (the user's first) and its default.
    fn profiles(&self, target: &Target, hdr_on: bool) -> (Vec<String>, Option<String>) {
        let mut list: Vec<String> = Vec::new();
        let mut current: Option<String> = None;
        let add = |list: &mut Vec<String>, name: String| {
            if !name.is_empty() && !list.iter().any(|known| known.eq_ignore_ascii_case(&name)) {
                list.push(name);
            }
        };
        for scope in [
            WCS_PROFILE_MANAGEMENT_SCOPE_CURRENT_USER,
            WCS_PROFILE_MANAGEMENT_SCOPE_SYSTEM_WIDE,
        ] {
            let mut names: *mut PWSTR = ptr::null_mut();
            let mut count = 0u32;
            // SAFETY: two valid out-pointers; the rest are values.
            let listed = unsafe {
                (self.get_list)(
                    scope,
                    target.adapter(),
                    target.source_id,
                    &mut names,
                    &mut count,
                )
            };
            if listed.is_ok() && !names.is_null() {
                // SAFETY: Windows returned `count` string pointers in one block.
                let entries = unsafe { std::slice::from_raw_parts(names, count as usize) };
                for entry in entries {
                    // SAFETY: each entry is a null-terminated string inside the block.
                    add(&mut list, unsafe { entry.to_string() }.unwrap_or_default());
                }
                // SAFETY: the block came from the call and is freed once, after its last use.
                let _ = unsafe { LocalFree(names.cast()) };
            }
            if current.is_none() {
                let mut name = PWSTR::null();
                // SAFETY: a valid out-pointer; the rest are values.
                let read = unsafe {
                    (self.get_default)(
                        scope,
                        target.adapter(),
                        target.source_id,
                        CPT_ICC,
                        subtype(hdr_on),
                        &mut name,
                    )
                };
                if read.is_ok() && !name.is_null() {
                    // SAFETY: a null-terminated string from the call.
                    current = unsafe { name.to_string() }
                        .ok()
                        .filter(|name| !name.is_empty());
                    // SAFETY: allocated by the call and freed once, after its last use.
                    let _ = unsafe { LocalFree(name.0.cast()) };
                }
            }
        }
        if let Some(current) = &current
            && !list.iter().any(|known| known.eq_ignore_ascii_case(current))
        {
            list.insert(0, current.clone());
        }
        (list, current)
    }
}

/// A system library, freed when dropped.
struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: loaded by `Mscms::load`, owned only by this value and freed once, after the
        // functions taken from it are gone.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, hz: u32) -> DisplayMode {
        DisplayMode { width, height, hz }
    }

    #[test]
    fn modes_largest_then_fastest_once_each() {
        let modes = sorted(vec![
            mode(1920, 1080, 60),
            mode(2560, 1440, 144),
            mode(1920, 1080, 144),
            mode(2560, 1440, 165),
            mode(1920, 1080, 60),
            mode(1600, 1200, 60),
        ]);
        assert_eq!(
            modes,
            vec![
                mode(2560, 1440, 165),
                mode(2560, 1440, 144),
                mode(1920, 1080, 144),
                mode(1920, 1080, 60),
                mode(1600, 1200, 60),
            ]
        );
    }

    #[test]
    fn brightness_in_the_monitors_range() {
        assert_eq!(percent_of(50, 0, 100), 50);
        assert_eq!(percent_of(128, 0, 255), 50);
        assert_eq!(percent_of(300, 0, 100), 100);
        assert_eq!(level(50, 0, 100), 50);
        assert_eq!(level(100, 10, 60), 60);
        assert_eq!(level(150, 0, 100), 100);
        assert_eq!(level(0, 10, 60), 10);
    }
}
