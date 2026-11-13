//! The clipboard:
//! - a file put on it as a dropped file (`CF_HDROP`), which File Explorer and Discord paste
//!   (Export in ReShade ▸ Shaders);
//! - text, for the window's text boxes' Cut, Copy and Paste (`CF_UNICODETEXT`).
//!
//! Opening the clipboard takes a window, so a message-only window is made for the call and
//! destroyed afterwards; data put on the clipboard stays there, which owns its memory once
//! `SetClipboardData` succeeds. Text is read up to its first null, within the block's size.

use crate::{Error, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::{
    CF_HDROP, CF_UNICODETEXT, CloseClipboard, CreateWindowExW, DROPFILES, DestroyWindow,
    EmptyClipboard, GMEM_MOVEABLE, GetClipboardData, GlobalAlloc, GlobalFree, GlobalLock,
    GlobalSize, GlobalUnlock, HGLOBAL, HWND, HWND_MESSAGE, IsClipboardFormatAvailable,
    OpenClipboard, SetClipboardData,
};
use windows::core::{PCWSTR, w};

const WHAT: &str = "the clipboard";

/// Puts `file` on the clipboard, to paste as a file. Blocking, briefly.
pub fn put_file(file: &Path) -> Result<()> {
    // The DROPFILES header, then the path and two terminating nulls, as UTF-16.
    let mut wide: Vec<u16> = file.as_os_str().encode_wide().collect();
    wide.extend([0, 0]);
    let dropfiles = DROPFILES {
        pFiles: size_of::<DROPFILES>() as u32,
        fWide: true.into(),
        ..Default::default()
    };
    // SAFETY: DROPFILES is plain data; its bytes are what the format expects at the start.
    let header = unsafe {
        std::slice::from_raw_parts(
            std::ptr::from_ref(&dropfiles).cast::<u8>(),
            size_of::<DROPFILES>(),
        )
    };
    let mut bytes = header.to_vec();
    bytes.extend(wide.iter().flat_map(|unit| unit.to_le_bytes()));
    put(CF_HDROP as u32, &bytes)
}

/// Puts `text` on the clipboard, replacing what was there. Blocking, briefly.
pub fn put_text(text: &str) -> Result<()> {
    let bytes: Vec<u8> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    put(CF_UNICODETEXT as u32, &bytes)
}

/// Whether the clipboard holds text to paste.
pub fn has_text() -> bool {
    // SAFETY: a plain query.
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT as u32) }.as_bool()
}

/// The clipboard's text; `None` when it holds none. Blocking, briefly.
pub fn text() -> Result<Option<String>> {
    let window = MessageWindow::new()?;
    let _open = OpenClipboardGuard::open(&window)?;
    // SAFETY: the clipboard is open on this thread; the handle stays the clipboard's.
    let data = unsafe { GetClipboardData(CF_UNICODETEXT as u32) };
    if data.is_null() {
        return Ok(None);
    }
    let memory: HGLOBAL = data.cast();
    // SAFETY: a plain query of the clipboard's block.
    let size = unsafe { GlobalSize(memory) };
    // SAFETY: locks the clipboard's block for reading, unlocked below.
    let pointer = unsafe { GlobalLock(memory) }.cast::<u16>();
    if pointer.is_null() {
        return Ok(None);
    }
    // SAFETY: the locked block holds `size` bytes, so `size / 2` UTF-16 units.
    let units = unsafe { std::slice::from_raw_parts(pointer, size / 2) };
    let end = units
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(units.len());
    let text = String::from_utf16_lossy(&units[..end]);
    // SAFETY: the block was locked above, once.
    let _ = unsafe { GlobalUnlock(memory) };
    Ok(Some(text))
}

/// Puts `bytes` on the clipboard as `format`, replacing what was there.
fn put(format: u32, bytes: &[u8]) -> Result<()> {
    let window = MessageWindow::new()?;
    let _open = OpenClipboardGuard::open(&window)?;
    // SAFETY: the clipboard is open on this thread.
    let emptied = unsafe { EmptyClipboard() };
    emptied.ok().map_err(Error::windows(WHAT))?;

    // SAFETY: a plain allocation, owned by `block` until the clipboard takes it.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE as u32, bytes.len().max(1)) };
    if memory.is_null() {
        return Err(Error::windows(WHAT)(windows::core::Error::from_thread()));
    }
    let mut block = Global(memory);
    // SAFETY: a live moveable block, unlocked below.
    let pointer = unsafe { GlobalLock(memory) }.cast::<u8>();
    if pointer.is_null() {
        return Err(Error::windows(WHAT)(windows::core::Error::from_thread()));
    }
    // SAFETY: the block holds at least `bytes.len()` bytes; the two do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer, bytes.len()) };
    // SAFETY: the block was locked above, once.
    let _ = unsafe { GlobalUnlock(memory) };
    // SAFETY: the clipboard is open and emptied by this thread; it owns the block once this
    // succeeds.
    let set = unsafe { SetClipboardData(format, Some(memory)) };
    if set.is_null() {
        return Err(Error::windows(WHAT)(windows::core::Error::from_thread()));
    }
    block.0 = std::ptr::null_mut();
    Ok(())
}

/// A message-only window, owning the clipboard for the call; destroyed when dropped.
struct MessageWindow(HWND);

impl MessageWindow {
    fn new() -> Result<Self> {
        // SAFETY: the predefined STATIC class, no name, and a message-only parent: nothing is
        // shown. The window is destroyed by `Drop`.
        let window = unsafe {
            CreateWindowExW(
                0,
                w!("STATIC"),
                PCWSTR::null(),
                0,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        };
        if window.is_null() {
            return Err(Error::windows(WHAT)(windows::core::Error::from_thread()));
        }
        Ok(Self(window))
    }
}

impl Drop for MessageWindow {
    fn drop(&mut self) {
        // SAFETY: the window this value made, destroyed once, on its thread.
        let _ = unsafe { DestroyWindow(self.0) };
    }
}

/// The clipboard, open on this thread; closed when dropped.
struct OpenClipboardGuard;

impl OpenClipboardGuard {
    fn open(window: &MessageWindow) -> Result<Self> {
        // SAFETY: a window this thread made, alive for the call.
        let opened = unsafe { OpenClipboard(Some(window.0)) };
        opened.ok().map_err(Error::windows(WHAT))?;
        Ok(Self)
    }
}

impl Drop for OpenClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: the clipboard was opened on this thread.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Global memory freed when dropped, unless given away (null).
struct Global(HGLOBAL);

impl Drop for Global {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the block came from GlobalAlloc and only this value owns it.
            let _ = unsafe { GlobalFree(self.0) };
        }
    }
}
