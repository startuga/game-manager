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

/// Trimmed, non-empty text of the first child element called `name` (namespaces ignored).
pub(crate) fn child_text<'a>(node: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)?
        .text()
        .map(str::trim)
        .filter(|t| !t.is_empty())
}
