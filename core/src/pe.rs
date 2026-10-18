//! Reads a program's processor architecture from its PE (Portable Executable) header, the format
//! of Windows `.exe` and `.dll` files. Plain file reads of a few hundred bytes, no Windows APIs.

use crate::Arch;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MACHINE_I386: u16 = 0x014C;
const MACHINE_AMD64: u16 = 0x8664;
const MACHINE_ARM64: u16 = 0xAA64;
const MACHINE_ARMNT: u16 = 0x01C4;
const PE32_MAGIC: u16 = 0x10B;
const PE32_PLUS_MAGIC: u16 = 0x20B;
/// Index of the CLR (.NET) runtime header among the optional header's data directories.
const CLR_DIRECTORY: usize = 14;
const COMIMAGE_FLAGS_ILONLY: u32 = 0x1;
const COMIMAGE_FLAGS_32BITREQUIRED: u32 = 0x2;
const COMIMAGE_FLAGS_32BITPREFERRED: u32 = 0x2_0000;
const SECTION_HEADER_SIZE: usize = 40;

/// The architecture the program at `path` is built for, or `None` if it is not a readable PE file
/// for a known processor.
pub fn architecture(path: &Path) -> Option<Arch> {
    read_architecture(&mut File::open(path).ok()?)
}

fn read_architecture(file: &mut (impl Read + Seek)) -> Option<Arch> {
    let dos_header = read_at(file, 0, 64)?;
    if !dos_header.starts_with(b"MZ") {
        return None;
    }
    let pe_offset = u64::from(u32_at(&dos_header, 0x3C)?);
    // "PE\0\0" followed by the 20-byte COFF file header.
    let headers = read_at(file, pe_offset, 24)?;
    if !headers.starts_with(b"PE\0\0") {
        return None;
    }
    let arch = match u16_at(&headers, 4)? {
        MACHINE_I386 => Arch::X86,
        MACHINE_AMD64 => Arch::X64,
        MACHINE_ARM64 => Arch::Arm64,
        MACHINE_ARMNT => Arch::Arm,
        _ => return None,
    };
    // Only an x86 image can be a .NET program that runs as 64-bit ("Any CPU").
    if arch != Arch::X86 {
        return Some(arch);
    }
    let section_count = usize::from(u16_at(&headers, 6)?);
    let optional_size = usize::from(u16_at(&headers, 20)?);
    let optional_offset = pe_offset + 24;
    let optional = read_at(file, optional_offset, optional_size)?;
    let sections_offset = optional_offset + optional_size as u64;
    let any_cpu = clr_flags(file, &optional, sections_offset, section_count).is_some_and(|flags| {
        flags & COMIMAGE_FLAGS_ILONLY != 0
            && flags & (COMIMAGE_FLAGS_32BITREQUIRED | COMIMAGE_FLAGS_32BITPREFERRED) == 0
    });
    Some(if any_cpu { Arch::AnyCpu } else { arch })
}

/// The `Flags` of a .NET program's CLR header, or `None` for a native program.
fn clr_flags(
    file: &mut (impl Read + Seek),
    optional: &[u8],
    sections_offset: u64,
    section_count: usize,
) -> Option<u32> {
    let directories = match u16_at(optional, 0)? {
        PE32_MAGIC => 96,
        PE32_PLUS_MAGIC => 112,
        _ => return None,
    };
    let directory_count = u32_at(optional, directories - 4)? as usize;
    if directory_count <= CLR_DIRECTORY {
        return None;
    }
    let rva = u64::from(u32_at(optional, directories + CLR_DIRECTORY * 8)?);
    if rva == 0 || section_count > 96 {
        return None;
    }
    // The CLR header is addressed in memory (RVA): find the section holding it on disk.
    let sections = read_at(file, sections_offset, section_count * SECTION_HEADER_SIZE)?;
    let offset = sections
        .as_chunks::<SECTION_HEADER_SIZE>()
        .0
        .iter()
        .find_map(|section| {
            let virtual_size = u64::from(u32_at(section, 8)?);
            let address = u64::from(u32_at(section, 12)?);
            let raw_size = u64::from(u32_at(section, 16)?);
            let raw_offset = u64::from(u32_at(section, 20)?);
            let size = virtual_size.max(raw_size);
            (address..address + size)
                .contains(&rva)
                .then(|| raw_offset + (rva - address))
        })?;
    // Flags follow the header size (4), runtime version (4) and metadata directory (8).
    u32_at(&read_at(file, offset + 16, 4)?, 0)
}

fn read_at(file: &mut (impl Read + Seek), offset: u64, len: usize) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut buffer = vec![0; len];
    file.read_exact(&mut buffer).ok()?;
    Some(buffer)
}

fn u16_at(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    /// A minimal PE file: DOS header, PE headers, one section and, for .NET, a CLR header.
    fn pe_file(machine: u16, magic: u16, clr_flags: Option<u32>) -> Vec<u8> {
        let pe_offset = 64usize;
        let optional_size = if magic == PE32_PLUS_MAGIC { 240 } else { 224 };
        let directories = if magic == PE32_PLUS_MAGIC { 112 } else { 96 };
        let optional_offset = pe_offset + 24;
        let section_offset = optional_offset + optional_size;
        let clr_offset = 0x400usize;
        let mut file = vec![0u8; clr_offset + 72];

        let put16 = |file: &mut Vec<u8>, at: usize, value: u16| {
            file[at..at + 2].copy_from_slice(&value.to_le_bytes());
        };
        let put32 = |file: &mut Vec<u8>, at: usize, value: u32| {
            file[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        file[..2].copy_from_slice(b"MZ");
        put32(&mut file, 0x3C, pe_offset as u32);
        file[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");
        put16(&mut file, pe_offset + 4, machine);
        put16(&mut file, pe_offset + 6, 1);
        put16(&mut file, pe_offset + 20, optional_size as u16);
        put16(&mut file, optional_offset, magic);
        put32(&mut file, optional_offset + directories - 4, 16);
        if let Some(flags) = clr_flags {
            let clr = optional_offset + directories + CLR_DIRECTORY * 8;
            put32(&mut file, clr, 0x2000);
            put32(&mut file, clr + 4, 72);
            // One section: 0x1000 bytes at RVA 0x2000, stored at file offset 0x400.
            put32(&mut file, section_offset + 8, 0x1000);
            put32(&mut file, section_offset + 12, 0x2000);
            put32(&mut file, section_offset + 16, 0x1000);
            put32(&mut file, section_offset + 20, clr_offset as u32);
            put32(&mut file, clr_offset, 72);
            put32(&mut file, clr_offset + 16, flags);
        }
        file
    }

    fn arch_of(bytes: Vec<u8>) -> Option<Arch> {
        read_architecture(&mut Cursor::new(bytes))
    }

    #[test]
    fn reads_the_machine_type() {
        assert_eq!(
            arch_of(pe_file(MACHINE_I386, PE32_MAGIC, None)),
            Some(Arch::X86)
        );
        assert_eq!(
            arch_of(pe_file(MACHINE_AMD64, PE32_PLUS_MAGIC, None)),
            Some(Arch::X64)
        );
        assert_eq!(
            arch_of(pe_file(MACHINE_ARM64, PE32_PLUS_MAGIC, None)),
            Some(Arch::Arm64)
        );
        assert_eq!(
            arch_of(pe_file(MACHINE_ARMNT, PE32_MAGIC, None)),
            Some(Arch::Arm)
        );
        assert_eq!(arch_of(pe_file(0x0200, PE32_PLUS_MAGIC, None)), None);
    }

    #[test]
    fn dotnet_programs_follow_their_clr_flags() {
        let x86 = |flags| arch_of(pe_file(MACHINE_I386, PE32_MAGIC, Some(flags)));
        assert_eq!(x86(COMIMAGE_FLAGS_ILONLY), Some(Arch::AnyCpu));
        assert_eq!(
            x86(COMIMAGE_FLAGS_ILONLY | COMIMAGE_FLAGS_32BITREQUIRED),
            Some(Arch::X86)
        );
        assert_eq!(
            x86(COMIMAGE_FLAGS_ILONLY
                | COMIMAGE_FLAGS_32BITREQUIRED
                | COMIMAGE_FLAGS_32BITPREFERRED),
            Some(Arch::X86)
        );
        // Mixed-mode (C++/CLI) programs are not IL-only and keep their machine type.
        assert_eq!(x86(0), Some(Arch::X86));
        // A .NET program built for x64 is x64 whatever its flags say.
        assert_eq!(
            arch_of(pe_file(
                MACHINE_AMD64,
                PE32_PLUS_MAGIC,
                Some(COMIMAGE_FLAGS_ILONLY)
            )),
            Some(Arch::X64)
        );
    }

    #[test]
    fn rejects_files_that_are_not_pe() {
        assert_eq!(arch_of(Vec::new()), None);
        assert_eq!(
            arch_of(b"not a program at all, just some text".repeat(4)),
            None
        );
        let mut no_signature = pe_file(MACHINE_AMD64, PE32_PLUS_MAGIC, None);
        no_signature[64] = b'X';
        assert_eq!(arch_of(no_signature), None);
        let mut truncated = pe_file(MACHINE_AMD64, PE32_PLUS_MAGIC, None);
        truncated.truncate(70);
        assert_eq!(arch_of(truncated), None);
        assert_eq!(architecture(Path::new(r"C:\does\not\exist.exe")), None);
    }

    #[test]
    fn reads_windows_own_programs() {
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let native = architecture(&windows.join(r"System32\notepad.exe"));
        assert!(
            matches!(native, Some(Arch::X64 | Arch::Arm64)),
            "{native:?}"
        );
        let wow64 = windows.join(r"SysWOW64\notepad.exe");
        if wow64.is_file() {
            assert_eq!(architecture(&wow64), Some(Arch::X86));
        }
    }
}
