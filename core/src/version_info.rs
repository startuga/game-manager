//! A program's or DLL's version information (the "Details" tab of its file properties): the name
//! for a newly added game, which mod a DLL in a game's folder belongs to (DXVK's say "DXVK" as
//! their product name, ReShade's "ReShade"), and an upscaler DLL's version (its fixed file
//! version, which every vendor sets, unlike the strings: Streamline's FileVersion string reads
//! "2,14,16,0").

use std::path::Path;
use windows::Win32::{GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW};
use windows::core::HSTRING;

/// The file's `ProductName`, if it has version information.
pub(crate) fn product_name(file: &Path) -> Option<String> {
    VersionInfo::read(file)?.string("ProductName")
}

/// The file's `ProductVersion`, if it has version information.
pub(crate) fn product_version(file: &Path) -> Option<String> {
    VersionInfo::read(file)?.string("ProductVersion")
}

/// The file's fixed file version (`VS_FIXEDFILEINFO`), as four numbers: `[310, 9, 16, 0]`.
pub(crate) fn file_version(file: &Path) -> Option<[u16; 4]> {
    let mut info = VersionInfo::read(file)?;
    // The root block is a VS_FIXEDFILEINFO: its signature, its structure's version, then the
    // file version's high and low 32 bits. Read as u16 pairs, as the block is only u16-aligned.
    let words = info.query(r"\")?;
    let dword = |index: usize| -> Option<u32> {
        let low = u32::from(*words.get(index * 2)?);
        let high = u32::from(*words.get(index * 2 + 1)?);
        Some(low | high << 16)
    };
    if dword(0)? != FIXED_FILE_INFO_SIGNATURE {
        return None;
    }
    let (high, low) = (dword(2)?, dword(3)?);
    Some([
        (high >> 16) as u16,
        high as u16,
        (low >> 16) as u16,
        low as u16,
    ])
}

/// `VS_FIXEDFILEINFO::dwSignature`.
const FIXED_FILE_INFO_SIGNATURE: u32 = 0xFEEF_04BD;

/// A program's version-information block.
pub(crate) struct VersionInfo {
    /// `u16` storage keeps the wide strings inside the block aligned.
    data: Vec<u16>,
}

impl VersionInfo {
    pub(crate) fn read(program: &Path) -> Option<Self> {
        let path = HSTRING::from(program.as_os_str());
        // SAFETY: `path` is a valid, null-terminated wide string.
        let size = unsafe { GetFileVersionInfoSizeW(&path, None) };
        if size == 0 {
            return None;
        }
        let mut data = vec![0u16; size.div_ceil(2) as usize];
        // SAFETY: `data` holds at least `size` bytes, the size the system asked for.
        unsafe { GetFileVersionInfoW(&path, None, size, data.as_mut_ptr().cast()) }
            .ok()
            .ok()?;
        Some(Self { data })
    }

    /// A string value such as `ProductName`, in the program's first listed language.
    pub(crate) fn string(&mut self, key: &str) -> Option<String> {
        // Language and code page pairs, e.g. 0x0409 0x04B0 for US English, Unicode.
        let mut languages: Vec<String> = self
            .query(r"\VarFileInfo\Translation")
            .map(|pairs| {
                pairs
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|[language, code_page]| format!("{language:04x}{code_page:04x}"))
                    .collect()
            })
            .unwrap_or_default();
        languages.push("040904b0".to_owned());
        languages.iter().find_map(|language| {
            let value = self.query(&format!(r"\StringFileInfo\{language}\{key}"))?;
            let text = String::from_utf16_lossy(value);
            let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            (!text.is_empty()).then(|| text.to_owned())
        })
    }

    /// The value at `sub_block` as `u16` units. Takes `&mut self` because Windows may convert
    /// strings in place, in the block's reserved space (16-bit programs), although the block is
    /// declared read-only.
    fn query(&mut self, sub_block: &str) -> Option<&[u16]> {
        let block = self.data.as_mut_ptr();
        let mut value = std::ptr::null_mut();
        let mut len = 0u32;
        // SAFETY: `block` points to the buffer GetFileVersionInfoW filled, through a mutable
        // borrow, so any write Windows makes into it is allowed; the sub-block string lives until
        // the call returns and the out-pointers are valid locals.
        let found = unsafe {
            VerQueryValueW(
                block.cast_const().cast(),
                &HSTRING::from(sub_block),
                &mut value,
                &mut len,
            )
        };
        if !found.as_bool() || value.is_null() || len == 0 {
            return None;
        }
        // Strings report their length in characters, the binary Translation value in bytes.
        let units = if sub_block.starts_with(r"\StringFileInfo") {
            len as usize
        } else {
            len as usize / 2
        };
        let start = block as usize;
        let end = start + self.data.len() * 2;
        let value_start = value as usize;
        if value_start < start || value_start + units * 2 > end || !value_start.is_multiple_of(2) {
            return None;
        }
        // SAFETY: the value lies inside `data` (checked above), is u16-aligned, and is borrowed
        // for no longer than `self`.
        Some(unsafe { std::slice::from_raw_parts(value.cast::<u16>(), units) })
    }
}
