//! What the PC is made of, from the registry and Win32 (no WMI).
//!
//! - **Processor**: the name from `HARDWARE\DESCRIPTION\System\CentralProcessor\0` (padded with
//!   spaces, so collapsed). Physical cores come from `GetLogicalProcessorInformationEx`
//!   (`RelationProcessorCore`, one record per core), threads from `GetActiveProcessorCount` over
//!   every processor group, and the base clock from `~MHz`.
//! - **Graphics cards**: the display adapter class key, one numbered subkey per installed driver.
//!   - Kept: subkeys whose `MatchingDeviceId` has a PCI vendor (`VEN_`). Microsoft's Basic Render
//!     Driver and virtual adapters (Parsec, remote displays) have none.
//!   - Dropped: drivers of cards no longer plugged in, which keep their key; a card is kept only
//!     when `EnumDisplayDevicesW` lists its name.
//!   - Video memory: `HardwareInformation.qwMemorySize` (a QWORD or 8 bytes), else the 32-bit
//!     `MemorySize`.
//!   - Driver: NVIDIA's own number is the last five digits of Windows' driver version
//!     (32.0.16.2112 is 621.12); AMD's is `RadeonSoftwareVersion` (Adrenalin). The date is
//!     `DriverDate`, written M-D-YYYY.
//! - **Memory**: `GlobalMemoryStatusEx`, and the modules from the SMBIOS table (`smbios`); a table
//!   that can't be read leaves the modules out.
//! - **Windows**: `ProductName` still says "Windows 10" on Windows 11, so build 22000 and later
//!   are named 11. Then come `DisplayVersion` (24H2…), the build and `UBR`.
//! - **Board and BIOS**: `HARDWARE\DESCRIPTION\System\BIOS`.
//! - **Drives**: fixed drives only, with label, file system, and free and total space.

use super::smbios::{self, MemoryModule};
use crate::{Error, Result};
use windows::Win32::{
    ALL_PROCESSOR_GROUPS, DISPLAY_DEVICEW, DRIVE_FIXED, EnumDisplayDevicesW,
    GetActiveProcessorCount, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDriveStringsW,
    GetLogicalProcessorInformationEx, GetVolumeInformationW, GlobalMemoryStatusEx, MEMORYSTATUSEX,
    RelationProcessorCore,
};
use windows::core::{HSTRING, PCWSTR, PWSTR};
use windows_registry::{Key, LOCAL_MACHINE};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cpu {
    pub name: String,
    /// Physical cores, 0 when unknown.
    pub cores: u32,
    pub threads: u32,
    /// The base clock in MHz, 0 when unknown.
    pub base_mhz: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl GpuVendor {
    pub fn name(self) -> &'static str {
        match self {
            GpuVendor::Nvidia => "NVIDIA",
            GpuVendor::Amd => "AMD",
            GpuVendor::Intel => "Intel",
            GpuVendor::Other => "Other",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gpu {
    pub name: String,
    pub vendor: GpuVendor,
    /// Dedicated video memory in bytes, 0 when unknown.
    pub vram: u64,
    /// The vendor's driver version ("621.12", "Adrenalin 26.10.1"), else Windows'.
    pub driver: String,
    /// YYYY-MM-DD, empty when unknown.
    pub driver_date: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    pub available: u64,
    /// Empty when the firmware's table could not be read.
    pub modules: Vec<MemoryModule>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct System {
    /// "Windows 11 Pro 24H2".
    pub windows: String,
    /// "26100.2314".
    pub build: String,
    /// The motherboard's maker and model, empty when unknown.
    pub board: String,
    /// The BIOS's vendor, version and date, empty when unknown.
    pub bios: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drive {
    /// "C:".
    pub root: String,
    pub label: String,
    /// "NTFS", empty when unknown.
    pub file_system: String,
    pub total: u64,
    pub free: u64,
}

const CPU_KEY: &str = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0";
const DISPLAY_CLASS: &str =
    r"SYSTEM\CurrentControlSet\Control\Class\{4d36e968-e325-11ce-bfc1-08002be10318}";
const WINDOWS_KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
const BIOS_KEY: &str = r"HARDWARE\DESCRIPTION\System\BIOS";

pub(super) fn cpu() -> Result<Cpu> {
    let key = LOCAL_MACHINE
        .open(CPU_KEY)
        .map_err(Error::windows(CPU_KEY))?;
    let name = key
        .get_string("ProcessorNameString")
        .map(|name| collapse(&name))
        .unwrap_or_else(|_| "Unknown processor".to_owned());
    // SAFETY: no pointers; counts the processors of every group.
    let threads = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS as u16) };
    Ok(Cpu {
        name,
        cores: physical_cores(),
        threads,
        base_mhz: key.get_u32("~MHz").unwrap_or(0),
    })
}

/// The number of physical cores, 0 when Windows does not say.
fn physical_cores() -> u32 {
    let mut length = 0u32;
    // SAFETY: no buffer: asks for the size only; `length` is a valid out-pointer.
    let _ = unsafe { GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut length) };
    if length == 0 {
        return 0;
    }
    // u64s, so the records are aligned as Windows writes them.
    let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
    // SAFETY: `buffer` holds at least `length` bytes, the size the call asked for.
    let read = unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buffer.as_mut_ptr().cast()),
            &mut length,
        )
    };
    if !read.as_bool() {
        return 0;
    }
    let bytes: Vec<u8> = buffer.iter().flat_map(|word| word.to_le_bytes()).collect();
    count_records(&bytes[..(length as usize).min(bytes.len())])
}

/// Counts `SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX` records: each starts with its relationship
/// (4 bytes) and its own size (4 bytes).
pub(crate) fn count_records(bytes: &[u8]) -> u32 {
    let mut count = 0;
    let mut at = 0usize;
    while let Some(size) = bytes.get(at + 4..at + 8) {
        let size = u32::from_le_bytes([size[0], size[1], size[2], size[3]]) as usize;
        if size == 0 {
            break;
        }
        count += 1;
        at += size;
    }
    count
}

pub(super) fn gpus() -> Result<Vec<Gpu>> {
    let class = LOCAL_MACHINE
        .open(DISPLAY_CLASS)
        .map_err(Error::windows(DISPLAY_CLASS))?;
    let shown = shown_adapters();
    let mut gpus: Vec<Gpu> = Vec::new();
    for subkey in class.keys().map_err(Error::windows(DISPLAY_CLASS))? {
        if subkey.len() != 4 || !subkey.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(key) = class.open(&subkey) else {
            continue;
        };
        let Ok(description) = key.get_string("DriverDesc") else {
            continue;
        };
        let device = key.get_string("MatchingDeviceId").unwrap_or_default();
        let Some(vendor) = vendor(&device) else {
            continue;
        };
        if !shown.is_empty()
            && !shown
                .iter()
                .any(|name| name.eq_ignore_ascii_case(description.trim()))
        {
            continue;
        }
        let vram = number(&key, "HardwareInformation.qwMemorySize")
            .or_else(|| number(&key, "HardwareInformation.MemorySize"))
            .unwrap_or(0);
        let gpu = Gpu {
            name: collapse(&description),
            vendor,
            vram,
            driver: driver_label(
                vendor,
                &key.get_string("DriverVersion").unwrap_or_default(),
                key.get_string("RadeonSoftwareVersion").ok().as_deref(),
            ),
            driver_date: iso_date(&key.get_string("DriverDate").unwrap_or_default()),
        };
        if !gpus
            .iter()
            .any(|known| known.name == gpu.name && known.driver == gpu.driver)
        {
            gpus.push(gpu);
        }
    }
    // Discrete cards first.
    gpus.sort_by_key(|gpu| std::cmp::Reverse(gpu.vram));
    Ok(gpus)
}

/// The vendor of a PCI device id (`PCI\VEN_10DE&DEV_2684…`); `None` for devices that aren't on
/// PCI (Basic Render Driver, virtual adapters).
pub(crate) fn vendor(device: &str) -> Option<GpuVendor> {
    let device = device.to_ascii_uppercase();
    let at = device.find("VEN_")?;
    Some(match device.get(at + 4..at + 8)? {
        "10DE" => GpuVendor::Nvidia,
        "1002" | "1022" => GpuVendor::Amd,
        "8086" => GpuVendor::Intel,
        _ => GpuVendor::Other,
    })
}

/// The driver version as its vendor numbers it.
pub(crate) fn driver_label(vendor: GpuVendor, windows: &str, radeon: Option<&str>) -> String {
    match vendor {
        GpuVendor::Nvidia => {
            let digits: String = windows.chars().filter(char::is_ascii_digit).collect();
            match digits.get(digits.len().saturating_sub(5)..) {
                Some(last) if digits.len() >= 5 => {
                    let major = last[..3].trim_start_matches('0');
                    format!(
                        "{}.{}",
                        if major.is_empty() { "0" } else { major },
                        &last[3..]
                    )
                }
                _ => windows.to_owned(),
            }
        }
        GpuVendor::Amd => match radeon.map(str::trim) {
            Some(version) if !version.is_empty() => format!("Adrenalin {version}"),
            _ => windows.to_owned(),
        },
        GpuVendor::Intel | GpuVendor::Other => windows.to_owned(),
    }
}

/// "10-27-2026" (as drivers write it) as "2026-10-27"; anything else as it is.
pub(crate) fn iso_date(date: &str) -> String {
    let parts: Vec<&str> = date.trim().split('-').collect();
    match parts.as_slice() {
        [month, day, year]
            if year.len() == 4
                && [month, day, year]
                    .iter()
                    .all(|part| part.parse::<u32>().is_ok()) =>
        {
            format!("{year}-{month:0>2}-{day:0>2}")
        }
        _ => date.trim().to_owned(),
    }
}

/// The names of the adapters Windows shows (`EnumDisplayDevicesW`).
fn shown_adapters() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for index in 0.. {
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `device` is initialized with its size set; no device name: lists adapters.
        let found = unsafe { EnumDisplayDevicesW(PCWSTR::null(), index, &mut device, 0) };
        if !found.as_bool() {
            break;
        }
        let name = wide(&device.DeviceString);
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// A registry number stored as a DWORD, a QWORD or 4 or 8 raw bytes.
fn number(key: &Key, name: &str) -> Option<u64> {
    let value = key.get_value(name).ok()?;
    let bytes: &[u8] = &value;
    match bytes.len() {
        8 => Some(u64::from_le_bytes(bytes.try_into().ok()?)),
        4 => Some(u64::from(u32::from_le_bytes(bytes.try_into().ok()?))),
        _ => None,
    }
}

pub(super) fn memory() -> Result<Memory> {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` is initialized with its size set.
    unsafe { GlobalMemoryStatusEx(&mut status) }
        .ok()
        .map_err(Error::windows("The memory's size"))?;
    Ok(Memory {
        total: status.ullTotalPhys,
        available: status.ullAvailPhys,
        modules: smbios::memory_modules().unwrap_or_default(),
    })
}

pub(super) fn system() -> Result<System> {
    let key = LOCAL_MACHINE
        .open(WINDOWS_KEY)
        .map_err(Error::windows(WINDOWS_KEY))?;
    let product = key
        .get_string("ProductName")
        .unwrap_or_else(|_| "Windows".to_owned());
    let build = key.get_string("CurrentBuild").unwrap_or_default();
    let windows = windows_name(
        &product,
        &build,
        &key.get_string("DisplayVersion").unwrap_or_default(),
    );
    let build = match key.get_u32("UBR") {
        Ok(update) if update > 0 && !build.is_empty() => format!("{build}.{update}"),
        _ => build,
    };
    let bios = LOCAL_MACHINE.open(BIOS_KEY).ok();
    let value = |name: &str| {
        bios.as_ref()
            .and_then(|key| key.get_string(name).ok())
            .unwrap_or_default()
    };
    Ok(System {
        windows,
        build,
        board: collapse(&format!(
            "{} {}",
            value("BaseBoardManufacturer"),
            value("BaseBoardProduct")
        )),
        bios: collapse(&format!(
            "{} {} {}",
            value("BIOSVendor"),
            value("BIOSVersion"),
            value("BIOSReleaseDate")
        )),
    })
}

/// "Windows 11 Pro 24H2": `ProductName` keeps "Windows 10" on Windows 11 (build 22000+).
pub(crate) fn windows_name(product: &str, build: &str, display_version: &str) -> String {
    let product = match build.parse::<u32>() {
        Ok(build) if build >= 22000 => product.replacen("Windows 10", "Windows 11", 1),
        _ => product.to_owned(),
    };
    collapse(&format!("{product} {display_version}"))
}

pub(super) fn drives() -> Result<Vec<Drive>> {
    // SAFETY: no buffer: asks for the length the list needs.
    let needed = unsafe { GetLogicalDriveStringsW(0, None) };
    if needed == 0 {
        return Err(Error::windows("The drives")(
            windows::core::Error::from_thread(),
        ));
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    // SAFETY: `buffer` holds `buffer.len()` characters, the size given.
    let written =
        unsafe { GetLogicalDriveStringsW(buffer.len() as u32, Some(PWSTR(buffer.as_mut_ptr()))) };
    let list = &buffer[..(written as usize).min(buffer.len())];
    Ok(list
        .split(|&c| c == 0)
        .filter(|root| !root.is_empty())
        .filter_map(|root| drive(&String::from_utf16_lossy(root)))
        .collect())
}

/// A fixed drive's label, file system and space; `None` for other drives and unready ones.
fn drive(root: &str) -> Option<Drive> {
    let path = HSTRING::from(root);
    // SAFETY: a null-terminated path.
    if unsafe { GetDriveTypeW(&path) } != DRIVE_FIXED as u32 {
        return None;
    }
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: a null-terminated path and two u64s for the ULARGE_INTEGERs.
    unsafe {
        GetDiskFreeSpaceExW(
            &path,
            Some((&raw mut free).cast()),
            Some((&raw mut total).cast()),
            None,
        )
    }
    .ok()
    .ok()?;
    let mut label = [0u16; 261];
    let mut file_system = [0u16; 261];
    // SAFETY: a null-terminated path and two buffers of the sizes given; the rest is not asked.
    let named = unsafe {
        GetVolumeInformationW(
            &path,
            Some(PWSTR(label.as_mut_ptr())),
            label.len() as u32,
            None,
            None,
            None,
            Some(PWSTR(file_system.as_mut_ptr())),
            file_system.len() as u32,
        )
    };
    let (label, file_system) = if named.as_bool() {
        (wide(&label), wide(&file_system))
    } else {
        (String::new(), String::new())
    };
    Some(Drive {
        root: root.trim_end_matches('\\').to_owned(),
        label,
        file_system,
        total,
        free,
    })
}

/// A fixed-size UTF-16 buffer up to its first zero.
pub(crate) fn wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).trim().to_owned()
}

/// Runs of spaces as one, trimmed.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vendors_from_pci_ids() {
        assert_eq!(
            vendor(r"pci\ven_10de&dev_2684&subsys_51031462"),
            Some(GpuVendor::Nvidia)
        );
        assert_eq!(vendor(r"PCI\VEN_1002&DEV_744C"), Some(GpuVendor::Amd));
        assert_eq!(vendor(r"PCI\VEN_8086&DEV_56A0"), Some(GpuVendor::Intel));
        assert_eq!(vendor(r"PCI\VEN_1AF4&DEV_1050"), Some(GpuVendor::Other));
        // Parsec's virtual adapter and Microsoft's Basic Render Driver.
        assert_eq!(vendor(r"Root\Parsec\VDA"), None);
        assert_eq!(vendor("ms_basicrender"), None);
    }

    #[test]
    fn driver_labels() {
        assert_eq!(
            driver_label(GpuVendor::Nvidia, "32.0.16.2112", None),
            "621.12"
        );
        assert_eq!(
            driver_label(GpuVendor::Nvidia, "31.0.15.6094", None),
            "560.94"
        );
        assert_eq!(driver_label(GpuVendor::Nvidia, "1.2", None), "1.2");
        assert_eq!(
            driver_label(GpuVendor::Amd, "32.0.21025.1024", Some("26.10.1")),
            "Adrenalin 26.10.1"
        );
        assert_eq!(
            driver_label(GpuVendor::Amd, "32.0.21025.1024", Some("")),
            "32.0.21025.1024"
        );
        assert_eq!(
            driver_label(GpuVendor::Intel, "32.0.101.6299", None),
            "32.0.101.6299"
        );
    }

    #[test]
    fn dates_and_names() {
        assert_eq!(iso_date("10-27-2026"), "2026-10-27");
        assert_eq!(iso_date("1-5-2025"), "2025-01-05");
        assert_eq!(iso_date("2025-01-05"), "2025-01-05");
        assert_eq!(
            windows_name("Windows 10 Pro", "29683", "Dev"),
            "Windows 11 Pro Dev"
        );
        assert_eq!(
            windows_name("Windows 10 Pro", "19045", "22H2"),
            "Windows 10 Pro 22H2"
        );
        assert_eq!(
            windows_name("Windows 10 Home", "26100", ""),
            "Windows 11 Home"
        );
        assert_eq!(
            collapse("AMD Ryzen 9 5900X 12-Core Processor             "),
            "AMD Ryzen 9 5900X 12-Core Processor"
        );
    }

    #[test]
    fn counts_processor_records() {
        // Two cores' records of 48 bytes each, then a zero size where the buffer ends.
        let mut bytes = vec![0u8; 96];
        bytes[4] = 48;
        bytes[52] = 48;
        assert_eq!(count_records(&bytes), 2);
        assert_eq!(count_records(&[]), 0);
    }
}
