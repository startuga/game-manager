//! The clipboard, for Export on the Shaders page: a file put on it as a dropped file (`CF_HDROP`),
//! which File Explorer and Discord paste. Setting clipboard data needs a window to own it, so a
//! message-only window is made for the call and destroyed afterwards; the data stays on the
//! clipboard, which owns its memory once `SetClipboardData` succeeds.

use crate::{Error, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::Win32::{
    CF_HDROP, CloseClipboard, CreateWindowExW, DROPFILES, DestroyWindow, EmptyClipboard,
    GMEM_MOVEABLE, GlobalAlloc, GlobalFree, GlobalLock, GlobalUnlock, HGLOBAL, HWND, HWND_MESSAGE,
    OpenClipboard, SetClipboardData,
};
use windows::core::{PCWSTR, w};

const WHAT: &str = "the clipboard";

/// Puts `file` on the clipboard, to paste as a file. Blocking, briefly.
pub fn put_file(file: &Path) -> Result<()> {
    // The DROPFILES header, then the path and two terminating nulls, as UTF-16.
    let mut wide: Vec<u16> = file.as_os_str().encode_wide().collect();
    wide.extend([0, 0]);
    let header = size_of::<DROPFILES>();
    let size = header + wide.len() * 2;

    let window = MessageWindow::new()?;
    // SAFETY: a window this thread made, alive for the call.
    let opened = unsafe { OpenClipboard(Some(window.0)) };
    opened.ok().map_err(Error::windows(WHAT))?;
    let _open = OpenClipboardGuard;
    // SAFETY: the clipboard is open on this thread.
    let emptied = unsafe { EmptyClipboard() };
    emptied.ok().map_err(Error::windows(WHAT))?;

    // SAFETY: a plain allocation of `size` bytes, owned by `block` until the clipboard takes it.
    let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE as u32, size) };
    if memory.is_null() {
        return Err(Error::windows(WHAT)(windows::core::Error::from_thread()));
    }
    let mut block = Global(memory);
    // SAFETY: a live moveable block, unlocked below.
    let pointer = unsafe { GlobalLock(memory) }.cast::<u8>();
    if pointer.is_null() {
        return Err(Error::windows(WHAT)(windows::core::Error::from_thread()));
    }
    let dropfiles = DROPFILES {
        pFiles: header as u32,
        fWide: true.into(),
        ..Default::default()
    };
    // SAFETY: the block holds `size` bytes: the header fits at its start (written unaligned, as
    // DROPFILES is packed).
    unsafe { std::ptr::write_unaligned(pointer.cast::<DROPFILES>(), dropfiles) };
    // SAFETY: the path's bytes fit after the header, in the same block; the two do not overlap.
    let path_at = unsafe { pointer.add(header) };
    // SAFETY: `wide` holds `wide.len() * 2` bytes and `path_at` as many free ones.
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr().cast::<u8>(), path_at, wide.len() * 2) };
    // SAFETY: the block was locked above, once.
    let _ = unsafe { GlobalUnlock(memory) };
    // SAFETY: the clipboard is open and emptied by this thread; it owns the block once this
    // succeeds.
    let set = unsafe { SetClipboardData(CF_HDROP as u32, Some(memory)) };
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

/// Closes the clipboard when dropped.
struct OpenClipboardGuard;

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
