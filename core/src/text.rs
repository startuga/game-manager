//! Text and XML helpers shared by the scanners that read store files.

use std::fs;
use std::path::Path;

/// Reads a text file that may be UTF-8 or UTF-16 (older EA installerdata.xml files are UTF-16).
pub(crate) fn read_text(path: &Path) -> Option<String> {
    fs::read(path).ok().map(|bytes| decode_text(&bytes))
}

/// Decodes UTF-16 (LE/BE, with or without BOM) or UTF-8 text.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    let utf16 = |bytes: &[u8], from: fn([u8; 2]) -> u16| {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|&c| from(c)).collect();
        String::from_utf16_lossy(&units)
    };
    let text = match bytes {
        [0xFF, 0xFE, rest @ ..] => utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => utf16(rest, u16::from_be_bytes),
        [_, 0, ..] => utf16(bytes, u16::from_le_bytes),
        _ => String::from_utf8_lossy(bytes).into_owned(),
    };
    strip_bom(text.trim_end_matches('\0')).to_owned()
}

/// The document without BOM or XML declaration. Text reaching roxmltree is already decoded to
/// UTF-8, but the declaration may still claim `encoding="utf-16"`.
pub(crate) fn xml_body(text: &str) -> &str {
    let text = strip_bom(text).trim_start();
    match text.strip_prefix("<?xml") {
        Some(rest) => rest.split_once("?>").map_or(text, |(_, body)| body),
        None => text,
    }
}

pub(crate) fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Everything but unreserved URI characters as `%XX` (UTF-8 bytes), so spaces, quotes, slashes
/// and `&` in a value cannot break the link it goes into.
pub(crate) fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// The text `percent_encode` encoded; `None` for a broken `%XX` or bytes that are not UTF-8.
pub(crate) fn percent_decode(text: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'%' {
            let hex = std::str::from_utf8(after.get(..2)?).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            rest = after.get(2..)?;
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    String::from_utf8(bytes).ok()
}

/// FNV-1a, 64-bit: a small hash that stays the same across Rust versions and computers.
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Trimmed, non-empty text of the first child element called `name` (namespaces ignored).
pub(crate) fn child_text<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)?
        .text()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_encoding_round_trips() {
        let text =
            r#"[{"Copy":{"from":"C:\\Game Manager\\d3d9.dll","to":"C:\\Jogos\\Ação\\d3d9.dll"}}]"#;
        let encoded = percent_encode(text);
        assert!(!encoded.contains([' ', '"', '\\']));
        assert_eq!(percent_decode(&encoded).as_deref(), Some(text));
        assert_eq!(percent_decode("a%2"), None);
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%FF"), None);
    }
}
