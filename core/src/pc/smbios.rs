//! Memory modules from the firmware's SMBIOS table, read with `GetSystemFirmwareTable('RSMB')`
//! instead of WMI. Each Memory Device structure (type 17) gives a module's size, type, speed,
//! maker and part number.
//!
//! - **Layout**: Windows' copy of the table starts with an 8-byte header (`RawSMBIOSData`: the
//!   version, then the table's length). Then come the structures, each a formatted part of
//!   `length` bytes followed by its strings. Each string ends with a zero byte, and the set ends
//!   with one more (two zeros when there is none). Type 127 ends the table.
//! - **Size**: in MB, or in KB when bit 15 is set. 0x7FFF means "see the extended size" (offset
//!   0x1C, in MB), and 0 or 0xFFFF means an empty slot or an unknown size: no module.
//! - **Speed**: the configured speed (offset 0x20, SMBIOS 2.7+), else the rated one (0x15), in
//!   MT/s.
//! - **Placeholders**: boards fill unknown strings with "Unknown", "Not Specified",
//!   "Manufacturer00" and the like, which are dropped.
//!
//! The parser works on bytes, so tests build tables by hand.

use crate::{Error, Result};
use windows::Win32::GetSystemFirmwareTable;

/// One memory module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryModule {
    pub size: u64,
    /// MT/s, 0 when unknown.
    pub speed: u32,
    /// "DDR4", "DDR5"…, empty when unknown.
    pub kind: &'static str,
    pub manufacturer: String,
    pub part_number: String,
}

/// The modules the firmware lists.
pub(super) fn memory_modules() -> Result<Vec<MemoryModule>> {
    const RSMB: u32 = u32::from_be_bytes(*b"RSMB");
    // SAFETY: no buffer: asks for the table's size only.
    let size = unsafe { GetSystemFirmwareTable(RSMB, 0, None, 0) };
    if size == 0 {
        return Err(Error::windows("The SMBIOS table")(
            windows::core::Error::from_thread(),
        ));
    }
    let mut table = vec![0u8; size as usize];
    // SAFETY: `table` holds `size` bytes, the size the call asks to be given.
    let written = unsafe { GetSystemFirmwareTable(RSMB, 0, Some(table.as_mut_ptr().cast()), size) };
    table.truncate(written as usize);
    Ok(parse(&table))
}

/// The memory modules in Windows' copy of the SMBIOS table.
pub(crate) fn parse(raw: &[u8]) -> Vec<MemoryModule> {
    let Some(length) = raw.get(4..8).and_then(|bytes| bytes.try_into().ok()) else {
        return Vec::new();
    };
    let end = raw.len().min(8 + u32::from_le_bytes(length) as usize);
    let mut modules = Vec::new();
    let mut at = 8;
    while at + 4 <= end {
        let kind = raw[at];
        let length = raw[at + 1] as usize;
        if length < 4 || at + length > end {
            break;
        }
        let mut strings = Vec::new();
        let mut cursor = at + length;
        while cursor < end && raw[cursor] != 0 {
            let stop = raw[cursor..end]
                .iter()
                .position(|&byte| byte == 0)
                .map_or(end, |offset| cursor + offset);
            strings.push(
                String::from_utf8_lossy(&raw[cursor..stop])
                    .trim()
                    .to_owned(),
            );
            cursor = stop + 1;
        }
        if kind == 17
            && let Some(module) = memory_device(&raw[at..at + length], &strings)
        {
            modules.push(module);
        }
        if kind == 127 {
            break;
        }
        at = cursor + if strings.is_empty() { 2 } else { 1 };
    }
    modules
}

/// A Memory Device structure (type 17), or `None` for an empty slot.
fn memory_device(formatted: &[u8], strings: &[String]) -> Option<MemoryModule> {
    let word = |offset: usize| -> Option<u16> {
        Some(u16::from_le_bytes(
            formatted.get(offset..offset + 2)?.try_into().ok()?,
        ))
    };
    let dword = |offset: usize| -> Option<u32> {
        Some(u32::from_le_bytes(
            formatted.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    let byte = |offset: usize| formatted.get(offset).copied();
    let size = match word(0x0C)? {
        0 | 0xFFFF => return None,
        0x7FFF => u64::from(dword(0x1C)? & 0x7FFF_FFFF) << 20,
        kilobytes if kilobytes & 0x8000 != 0 => u64::from(kilobytes & 0x7FFF) << 10,
        megabytes => u64::from(megabytes) << 20,
    };
    if size == 0 {
        return None;
    }
    let speed = match word(0x20) {
        Some(configured) if configured > 0 && configured < 0xFFFF => configured,
        _ => word(0x15).filter(|&speed| speed < 0xFFFF).unwrap_or(0),
    };
    let text = |offset: usize| -> String {
        let Some(index) = byte(offset).filter(|&index| index > 0) else {
            return String::new();
        };
        let value = strings
            .get(usize::from(index) - 1)
            .map_or("", String::as_str);
        if is_placeholder(value) {
            String::new()
        } else {
            value.to_owned()
        }
    };
    Some(MemoryModule {
        size,
        speed: u32::from(speed),
        kind: match byte(0x12)? {
            0x18 => "DDR3",
            0x1A => "DDR4",
            0x1E => "LPDDR4",
            0x22 => "DDR5",
            0x23 => "LPDDR5",
            _ => "",
        },
        manufacturer: text(0x17),
        part_number: text(0x1A),
    })
}

/// Text a board puts where it knows nothing.
fn is_placeholder(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "" | "unknown" | "not specified" | "undefined" | "none" | "n/a"
    ) || lower.starts_with("manufacturer")
        || lower.starts_with("partnum")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A type 17 structure of `length` bytes with the given fields, and its strings.
    struct Device {
        size: u16,
        extended: u32,
        kind: u8,
        speed: u16,
        configured: u16,
        strings: Vec<&'static str>,
    }

    impl Device {
        fn bytes(&self, length: u8) -> Vec<u8> {
            let mut formatted = vec![0u8; usize::from(length)];
            formatted[0] = 17;
            formatted[1] = length;
            formatted[0x0C..0x0E].copy_from_slice(&self.size.to_le_bytes());
            formatted[0x12] = self.kind;
            formatted[0x15..0x17].copy_from_slice(&self.speed.to_le_bytes());
            // Strings: 1 locator, 2 bank, 3 manufacturer, 4 serial, 5 asset tag, 6 part number.
            formatted[0x10] = 1;
            formatted[0x11] = 2;
            formatted[0x17] = 3;
            formatted[0x18] = 4;
            formatted[0x19] = 5;
            formatted[0x1A] = 6;
            if length >= 0x22 {
                formatted[0x1C..0x20].copy_from_slice(&self.extended.to_le_bytes());
                formatted[0x20..0x22].copy_from_slice(&self.configured.to_le_bytes());
            }
            for string in &self.strings {
                formatted.extend_from_slice(string.as_bytes());
                formatted.push(0);
            }
            formatted.push(0);
            if self.strings.is_empty() {
                formatted.push(0);
            }
            formatted
        }
    }

    fn ddr4(size: u16) -> Device {
        Device {
            size,
            extended: 0,
            kind: 0x1A,
            speed: 3200,
            configured: 3600,
            strings: vec![
                "DIMM_A1",
                "BANK 0",
                "Kingston",
                "12345678",
                "Not Specified",
                "KF3600C18D4/16GX",
            ],
        }
    }

    /// Windows' header, the structures, then the end-of-table structure.
    fn table(structures: &[Vec<u8>]) -> Vec<u8> {
        let mut body: Vec<u8> = structures.concat();
        body.extend_from_slice(&[127, 4, 0xFF, 0xFF, 0, 0]);
        let mut raw = vec![0, 3, 3, 0];
        raw.extend_from_slice(&(body.len() as u32).to_le_bytes());
        raw.extend_from_slice(&body);
        raw
    }

    #[test]
    fn reads_modules_and_skips_empty_slots() {
        // A BIOS structure (type 0) with no strings first, as tables start.
        let bios = vec![0, 4, 0, 0, 0, 0];
        let empty = Device {
            size: 0,
            strings: Vec::new(),
            ..ddr4(0)
        };
        let raw = table(&[
            bios,
            ddr4(16384).bytes(0x28),
            empty.bytes(0x28),
            ddr4(16384).bytes(0x28),
        ]);
        let modules = parse(&raw);
        assert_eq!(modules.len(), 2);
        assert_eq!(
            modules[0],
            MemoryModule {
                size: 16 << 30,
                speed: 3600,
                kind: "DDR4",
                manufacturer: "Kingston".to_owned(),
                part_number: "KF3600C18D4/16GX".to_owned(),
            }
        );
    }

    #[test]
    fn extended_and_kilobyte_sizes() {
        let big = Device {
            size: 0x7FFF,
            extended: 65536,
            ..ddr4(0)
        };
        let small = Device {
            size: 0x8000 | 512,
            ..ddr4(0)
        };
        let modules = parse(&table(&[big.bytes(0x28), small.bytes(0x28)]));
        assert_eq!(modules[0].size, 64 << 30);
        assert_eq!(modules[1].size, 512 << 10);
    }

    #[test]
    fn rated_speed_without_a_configured_one_and_placeholders_dropped() {
        let ddr5 = Device {
            kind: 0x22,
            speed: 4800,
            configured: 0,
            // SMBIOS has no empty strings: an index of 0 means none.
            strings: vec!["A", "B", "Unknown", "Serial", "Asset", "Manufacturer00"],
            ..ddr4(16384)
        };
        // An SMBIOS 2.3 structure (0x1B bytes) has no configured speed at all.
        let old = Device {
            speed: 1600,
            kind: 0x18,
            ..ddr4(8192)
        };
        let modules = parse(&table(&[ddr5.bytes(0x28), old.bytes(0x1B)]));
        assert_eq!(modules[0].speed, 4800);
        assert_eq!(modules[0].kind, "DDR5");
        assert_eq!(modules[0].manufacturer, "");
        assert_eq!(modules[0].part_number, "");
        assert_eq!(modules[1].speed, 1600);
        assert_eq!(modules[1].kind, "DDR3");
    }

    #[test]
    fn a_short_or_broken_table_gives_nothing() {
        assert!(parse(&[]).is_empty());
        assert!(parse(&[0, 3, 3, 0, 200, 0, 0, 0, 17, 2]).is_empty());
    }
}
