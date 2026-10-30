//! Reads a program's PE (Portable Executable) headers, the format of Windows `.exe` and `.dll`
//! files: its processor architecture, the DLLs it imports, its section names and whether it is
//! Large Address Aware. Plain file reads of a few kilobytes, no Windows APIs. It can also mark a
//! 32-bit program Large Address Aware, with a valid checksum.

use crate::Arch;
use std::fs::File;
use std::io::{Cursor, Read, Seek, SeekFrom};
use std::path::Path;

const MACHINE_I386: u16 = 0x014C;
const MACHINE_AMD64: u16 = 0x8664;
const MACHINE_ARM64: u16 = 0xAA64;
const MACHINE_ARMNT: u16 = 0x01C4;
const PE32_MAGIC: u16 = 0x10B;
const PE32_PLUS_MAGIC: u16 = 0x20B;
/// Indexes among the optional header's data directories.
const IMPORT_DIRECTORY: usize = 1;
const DELAY_IMPORT_DIRECTORY: usize = 13;
const CLR_DIRECTORY: usize = 14;
const COMIMAGE_FLAGS_ILONLY: u32 = 0x1;
const COMIMAGE_FLAGS_32BITREQUIRED: u32 = 0x2;
const COMIMAGE_FLAGS_32BITPREFERRED: u32 = 0x2_0000;
const SECTION_HEADER_SIZE: usize = 40;
/// More sections than any real program has; a larger count means a damaged file.
const MAX_SECTIONS: usize = 96;
/// More imported DLLs than any real program has.
const MAX_DESCRIPTORS: usize = 1024;
/// `IMAGE_FILE_LARGE_ADDRESS_AWARE` in the COFF header's Characteristics: a 32-bit program may
/// use 4 GB on 64-bit Windows instead of 2.
const LARGE_ADDRESS_AWARE: u16 = 0x0020;
/// Where the checksum is in the optional header (the same for PE32 and PE32+).
const CHECKSUM_AT: usize = 64;

/// Whether the program at `path` is Large Address Aware, or `None` if it is not a readable PE
/// file.
pub(crate) fn large_address_aware(path: &Path) -> Option<bool> {
    let headers = Headers::read(&mut File::open(path).ok()?)?;
    Some(headers.characteristics & LARGE_ADDRESS_AWARE != 0)
}

/// The names of the program's sections (`.text`, `.rsrc`, Steam's DRM wrapper adds `.bind`).
pub(crate) fn section_names(path: &Path) -> Option<Vec<String>> {
    let mut file = File::open(path).ok()?;
    let image = Headers::read(&mut file)?.image(&mut file)?;
    let names = image.sections.as_chunks::<SECTION_HEADER_SIZE>().0.iter();
    Some(
        names
            .map(|section| {
                let name = &section[..8];
                let len = name
                    .iter()
                    .position(|&byte| byte == 0)
                    .unwrap_or(name.len());
                String::from_utf8_lossy(&name[..len]).into_owned()
            })
            .collect(),
    )
}

/// Marks the 32-bit x86 program in `bytes` Large Address Aware and, when it has a checksum,
/// computes it again (a checksum of 0 stays 0: Windows checks it only for drivers and system
/// files). `None` when `bytes` are not an x86 PE32 program.
pub(crate) fn set_large_address_aware(bytes: &mut [u8]) -> Option<()> {
    let headers = Headers::read(&mut Cursor::new(&*bytes))?;
    let optional = usize::try_from(headers.optional_offset).ok()?;
    if headers.machine != MACHINE_I386 || u16_at(bytes, optional)? != PE32_MAGIC {
        return None;
    }
    let characteristics = usize::try_from(headers.pe_offset).ok()? + 22;
    let flags = headers.characteristics | LARGE_ADDRESS_AWARE;
    bytes
        .get_mut(characteristics..characteristics + 2)?
        .copy_from_slice(&flags.to_le_bytes());
    let checksum_at = optional + CHECKSUM_AT;
    if u32_at(bytes, checksum_at)? != 0 {
        let sum = checksum(bytes, checksum_at);
        bytes
            .get_mut(checksum_at..checksum_at + 4)?
            .copy_from_slice(&sum.to_le_bytes());
    }
    Some(())
}

/// The PE checksum, as Windows' `CheckSumMappedFile` computes it: the file's 16-bit words, with
/// the checksum field at `at` counted as zero, added with each carry folded back in, plus the
/// file's length.
pub(crate) fn checksum(bytes: &[u8], at: usize) -> u32 {
    let field = at..at + 4;
    let byte = |index: usize| -> u64 {
        if field.contains(&index) {
            0
        } else {
            bytes.get(index).copied().map_or(0, u64::from)
        }
    };
    let mut sum: u64 = 0;
    for index in (0..bytes.len()).step_by(2) {
        sum += byte(index) | (byte(index + 1) << 8);
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    sum = (sum & 0xFFFF) + (sum >> 16);
    (sum as u32).wrapping_add(bytes.len() as u32)
}

/// The architecture the program at `path` is built for, or `None` if it is not a readable PE file
/// for a known processor.
pub fn architecture(path: &Path) -> Option<Arch> {
    read_architecture(&mut File::open(path).ok()?)
}

/// The DLLs the program at `path` imports, directly or delay-loaded, in lowercase
/// (`d3d11.dll`), or `None` if it is not a readable PE file.
pub(crate) fn imported_dlls(path: &Path) -> Option<Vec<String>> {
    read_imported_dlls(&mut File::open(path).ok()?)
}

fn read_architecture(file: &mut (impl Read + Seek)) -> Option<Arch> {
    let headers = Headers::read(file)?;
    let arch = match headers.machine {
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
    let any_cpu = headers
        .image(file)
        .and_then(|image| clr_flags(file, &image))
        .is_some_and(|flags| {
            flags & COMIMAGE_FLAGS_ILONLY != 0
                && flags & (COMIMAGE_FLAGS_32BITREQUIRED | COMIMAGE_FLAGS_32BITPREFERRED) == 0
        });
    Some(if any_cpu { Arch::AnyCpu } else { arch })
}

/// The `Flags` of a .NET program's CLR header, or `None` for a native program.
fn clr_flags(file: &mut (impl Read + Seek), image: &Image) -> Option<u32> {
    let offset = image.offset(image.directory(CLR_DIRECTORY)?)?;
    // Flags follow the header size (4), runtime version (4) and metadata directory (8).
    u32_at(&read_at(file, offset + 16, 4)?, 0)
}

fn read_imported_dlls(file: &mut (impl Read + Seek)) -> Option<Vec<String>> {
    let image = Headers::read(file)?.image(file)?;
    let mut dlls = Vec::new();
    // Each table is a list of fixed-size descriptors ending with an empty one; the DLL name's RVA
    // is at `name_at` in a descriptor.
    for (directory, size, name_at) in [(IMPORT_DIRECTORY, 20, 12), (DELAY_IMPORT_DIRECTORY, 32, 4)]
    {
        let Some(start) = image.directory(directory).and_then(|rva| image.offset(rva)) else {
            continue;
        };
        for index in 0..MAX_DESCRIPTORS {
            let Some(descriptor) = read_at(file, start + (index * size) as u64, size) else {
                break;
            };
            if descriptor.iter().all(|&byte| byte == 0) {
                break;
            }
            // Delay-load tables from old linkers hold addresses, not RVAs (attribute bit 0 clear).
            if directory == DELAY_IMPORT_DIRECTORY && u32_at(&descriptor, 0)? & 1 == 0 {
                break;
            }
            let name = u32_at(&descriptor, name_at)
                .and_then(|rva| image.offset(rva))
                .and_then(|offset| read_name(file, offset));
            dlls.extend(name);
        }
    }
    dlls.sort();
    dlls.dedup();
    Some(dlls)
}

/// A null-terminated ASCII name at `offset`, in lowercase.
fn read_name(file: &mut (impl Read + Seek), offset: u64) -> Option<String> {
    file.seek(SeekFrom::Start(offset)).ok()?;
    // DLL names are short; the name may end near the end of the file, so read what is there.
    let mut bytes = Vec::new();
    file.by_ref().take(256).read_to_end(&mut bytes).ok()?;
    let len = bytes.iter().position(|&byte| byte == 0)?;
    let name = std::str::from_utf8(bytes.get(..len)?).ok()?;
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// The COFF file header: the machine type, the characteristics and where the optional header and
/// sections are.
struct Headers {
    pe_offset: u64,
    machine: u16,
    characteristics: u16,
    optional_offset: u64,
    optional_size: usize,
    section_count: usize,
}

impl Headers {
    fn read(file: &mut (impl Read + Seek)) -> Option<Self> {
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
        Some(Self {
            pe_offset,
            machine: u16_at(&headers, 4)?,
            characteristics: u16_at(&headers, 22)?,
            optional_offset: pe_offset + 24,
            optional_size: usize::from(u16_at(&headers, 20)?),
            section_count: usize::from(u16_at(&headers, 6)?),
        })
    }

    /// The optional header and the section table.
    fn image(&self, file: &mut (impl Read + Seek)) -> Option<Image> {
        if self.section_count > MAX_SECTIONS {
            return None;
        }
        let optional = read_at(file, self.optional_offset, self.optional_size)?;
        let sections_offset = self.optional_offset + self.optional_size as u64;
        let sections = read_at(
            file,
            sections_offset,
            self.section_count * SECTION_HEADER_SIZE,
        )?;
        Some(Image { optional, sections })
    }
}

struct Image {
    optional: Vec<u8>,
    sections: Vec<u8>,
}

impl Image {
    /// The RVA of data directory `index`; `None` when the program has none.
    fn directory(&self, index: usize) -> Option<u32> {
        let directories = match u16_at(&self.optional, 0)? {
            PE32_MAGIC => 96,
            PE32_PLUS_MAGIC => 112,
            _ => return None,
        };
        let count = u32_at(&self.optional, directories - 4)? as usize;
        if count <= index {
            return None;
        }
        u32_at(&self.optional, directories + index * 8).filter(|&rva| rva != 0)
    }

    /// Where the data at an RVA (an address in memory) is in the file: in the section holding it.
    fn offset(&self, rva: u32) -> Option<u64> {
        let rva = u64::from(rva);
        self.sections
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
            })
    }
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
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;
    use std::path::PathBuf;

    fn put16(file: &mut [u8], at: usize, value: u16) {
        file[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put32(file: &mut [u8], at: usize, value: u32) {
        file[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Where `pe_file` puts the optional header, the data directories and the section table.
    fn layout(magic: u16) -> (usize, usize, usize) {
        let optional_offset = 64 + 24;
        let optional_size = if magic == PE32_PLUS_MAGIC { 240 } else { 224 };
        let directories = if magic == PE32_PLUS_MAGIC { 112 } else { 96 };
        (
            optional_offset,
            optional_offset + directories,
            optional_offset + optional_size,
        )
    }

    /// A minimal PE file: DOS header, PE headers, one section and, for .NET, a CLR header.
    fn pe_file(machine: u16, magic: u16, clr_flags: Option<u32>) -> Vec<u8> {
        let pe_offset = 64usize;
        let optional_size = if magic == PE32_PLUS_MAGIC { 240 } else { 224 };
        let directories = if magic == PE32_PLUS_MAGIC { 112 } else { 96 };
        let optional_offset = pe_offset + 24;
        let section_offset = optional_offset + optional_size;
        let clr_offset = 0x400usize;
        let mut file = vec![0u8; clr_offset + 72];

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

    /// A PE file importing `imports` and delay-loading `delayed`. Its one section maps RVA 0x2000
    /// to file offset 0x400: import descriptors at RVA 0x2000, delay-load ones at 0x2200, names
    /// from 0x2800.
    fn pe_with_imports(magic: u16, imports: &[&str], delayed: &[&str]) -> Vec<u8> {
        let mut file = pe_file(MACHINE_AMD64, magic, None);
        file.resize(0x1400, 0);
        let (_, directories, sections) = layout(magic);
        put32(&mut file, sections + 8, 0x1000);
        put32(&mut file, sections + 12, 0x2000);
        put32(&mut file, sections + 16, 0x1000);
        put32(&mut file, sections + 20, 0x400);
        let file_offset = |rva: usize| rva - 0x2000 + 0x400;
        let mut name_rva = 0x2800;
        let mut put_name = |file: &mut Vec<u8>, name: &str| {
            let at = file_offset(name_rva);
            file[at..at + name.len()].copy_from_slice(name.as_bytes());
            let rva = name_rva as u32;
            name_rva += name.len() + 1;
            rva
        };
        put32(&mut file, directories + IMPORT_DIRECTORY * 8, 0x2000);
        for (i, name) in imports.iter().enumerate() {
            let rva = put_name(&mut file, name);
            put32(&mut file, file_offset(0x2000 + i * 20) + 12, rva);
        }
        put32(&mut file, directories + DELAY_IMPORT_DIRECTORY * 8, 0x2200);
        for (i, name) in delayed.iter().enumerate() {
            let rva = put_name(&mut file, name);
            let at = file_offset(0x2200 + i * 32);
            put32(&mut file, at, 1); // RVA-based
            put32(&mut file, at + 4, rva);
        }
        file
    }

    /// A 64-bit program importing `imports`, for other modules' tests.
    pub(crate) fn program_importing(imports: &[&str]) -> Vec<u8> {
        pe_with_imports(PE32_PLUS_MAGIC, imports, &[])
    }

    #[test]
    fn reads_imported_and_delay_loaded_dlls() {
        for magic in [PE32_MAGIC, PE32_PLUS_MAGIC] {
            let file = pe_with_imports(magic, &["KERNEL32.dll", "d3d11.dll"], &["D3D12.dll"]);
            assert_eq!(
                read_imported_dlls(&mut Cursor::new(file)).unwrap(),
                ["d3d11.dll", "d3d12.dll", "kernel32.dll"]
            );
        }
        // No import tables at all: no DLLs, but still a PE file.
        let plain = pe_file(MACHINE_AMD64, PE32_PLUS_MAGIC, None);
        assert_eq!(
            read_imported_dlls(&mut Cursor::new(plain)),
            Some(Vec::new())
        );
        assert_eq!(read_imported_dlls(&mut Cursor::new(b"MZ".to_vec())), None);

        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let notepad = imported_dlls(&windows.join(r"System32\notepad.exe")).unwrap();
        // user32 is imported directly, advapi32 delay-loaded (and some imports are `.drv`).
        assert!(notepad.contains(&"user32.dll".to_owned()), "{notepad:?}");
        assert!(notepad.contains(&"advapi32.dll".to_owned()), "{notepad:?}");
        assert!(notepad.iter().all(|dll| *dll == dll.to_lowercase()));
    }

    #[test]
    fn marks_x86_programs_large_address_aware() {
        // A program without a checksum keeps none.
        let mut file = pe_file(MACHINE_I386, PE32_MAGIC, None);
        set_large_address_aware(&mut file).unwrap();
        let headers = Headers::read(&mut Cursor::new(&file)).unwrap();
        assert_ne!(headers.characteristics & LARGE_ADDRESS_AWARE, 0);
        let at = headers.optional_offset as usize + CHECKSUM_AT;
        assert_eq!(u32_at(&file, at), Some(0));
        // Only 32-bit x86 programs.
        let mut x64 = pe_file(MACHINE_AMD64, PE32_PLUS_MAGIC, None);
        assert_eq!(set_large_address_aware(&mut x64), None);
        assert_eq!(set_large_address_aware(&mut b"MZ".to_vec()), None);

        // Windows' own 32-bit Notepad: its stored checksum is the one computed here, it stays
        // valid once patched, and only the flag and the checksum change.
        let windows = PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let notepad = windows.join(r"SysWOW64\notepad.exe");
        if !notepad.is_file() {
            return;
        }
        let original = std::fs::read(&notepad).unwrap();
        let headers = Headers::read(&mut Cursor::new(&original)).unwrap();
        let at = headers.optional_offset as usize + CHECKSUM_AT;
        assert_eq!(Some(checksum(&original, at)), u32_at(&original, at));
        let mut patched = original.clone();
        set_large_address_aware(&mut patched).unwrap();
        let after = Headers::read(&mut Cursor::new(&patched)).unwrap();
        assert_ne!(after.characteristics & LARGE_ADDRESS_AWARE, 0);
        assert_eq!(Some(checksum(&patched, at)), u32_at(&patched, at));
        let flag = headers.pe_offset as usize + 22;
        let changed = (0..original.len()).filter(|&i| original[i] != patched[i]);
        assert!(
            changed
                .into_iter()
                .all(|i| (flag..flag + 2).contains(&i) || (at..at + 4).contains(&i))
        );

        assert!(large_address_aware(&notepad).is_some());
        assert!(
            section_names(&notepad)
                .unwrap()
                .contains(&".text".to_owned())
        );
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
