//! The system's dialogs: the Open dialog (`IFileOpenDialog`, replacing windows-pickers) and the
//! message box that reports a failure to start.
//!
//! The Open dialog is modal to the window, which Windows disables while it is open. It runs from
//! the message loop with no state borrowed (`ctx::Effect`), so the window keeps drawing under it.

use std::path::PathBuf;
use windows::Win32::{
    CLSCTX_INPROC_SERVER, COMDLG_FILTERSPEC, CoCreateInstance, CoTaskMemFree, FOS_FILEMUSTEXIST,
    FOS_FORCEFILESYSTEM, FileOpenDialog, HWND, IBindCtx, IFileOpenDialog, IShellItem, MB_ICONERROR,
    MB_OK, MessageBoxW, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, HSTRING, Interface, PCWSTR, PWSTR, w};

use super::ctx::FilePicker;

/// HRESULT_FROM_WIN32(ERROR_CANCELLED): the user closed the dialog.
const CANCELLED: HRESULT = HRESULT(0x8007_04C7_u32 as i32);

/// Shows the Open dialog for `owner`; `None` when cancelled.
pub fn pick_file(owner: isize, picker: &FilePicker) -> Result<Option<PathBuf>, String> {
    pick(owner, picker).map_err(|e| e.message())
}

fn pick(owner: isize, picker: &FilePicker) -> windows::core::Result<Option<PathBuf>> {
    // SAFETY: the UI thread initialized COM (single-threaded) before the window was created.
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER as u32) }?;
    if !picker.title.is_empty() {
        // SAFETY: the title is a valid wide string for the call.
        unsafe { dialog.SetTitle(&HSTRING::from(picker.title.as_str())) }.ok()?;
    }
    // The strings must outlive SetFileTypes, which copies them.
    let strings: Vec<(HSTRING, HSTRING)> = picker
        .filters
        .iter()
        .map(|(name, spec)| (HSTRING::from(name.as_str()), HSTRING::from(spec.as_str())))
        .collect();
    let specs: Vec<COMDLG_FILTERSPEC> = strings
        .iter()
        .map(|(name, spec)| COMDLG_FILTERSPEC {
            pszName: PCWSTR(name.as_ptr()),
            pszSpec: PCWSTR(spec.as_ptr()),
        })
        .collect();
    if !specs.is_empty() {
        // SAFETY: `specs` holds `specs.len()` entries pointing into `strings`, alive for the call.
        unsafe { dialog.SetFileTypes(specs.len() as u32, specs.as_ptr()) }.ok()?;
    }
    if let Some(folder) = &picker.folder {
        let mut item: Option<IShellItem> = None;
        // SAFETY: the path is a valid wide string; no bind context; `item` receives an
        // IShellItem, the interface asked for.
        let made = unsafe {
            SHCreateItemFromParsingName(
                &HSTRING::from(folder.as_os_str()),
                None::<&IBindCtx>,
                &IShellItem::IID,
                (&raw mut item).cast(),
            )
        };
        if let (Ok(()), Some(item)) = (made.ok(), item) {
            // SAFETY: `item` is a live shell item.
            unsafe { dialog.SetFolder(&item) }.ok()?;
        }
    }
    let mut options = 0;
    // SAFETY: `options` is a valid out-pointer.
    unsafe { dialog.GetOptions(&mut options) }.ok()?;
    let options = options | FOS_FILEMUSTEXIST as u32 | FOS_FORCEFILESYSTEM as u32;
    // SAFETY: adding flags to the dialog's own options.
    unsafe { dialog.SetOptions(options) }.ok()?;
    // SAFETY: `owner` is the app's window, alive while the dialog is modal to it.
    let shown = unsafe { dialog.Show(Some(owner as HWND)) };
    if shown == CANCELLED {
        return Ok(None);
    }
    shown.ok()?;
    // SAFETY: Show succeeded, so the dialog has a result.
    let item = unsafe { dialog.GetResult() }?;
    let mut name = PWSTR::null();
    // SAFETY: `name` is a valid out-pointer; a file-system item (FOS_FORCEFILESYSTEM) has a path.
    unsafe { item.GetDisplayName(SIGDN_FILESYSPATH, &mut name) }.ok()?;
    // SAFETY: GetDisplayName returned a valid, null-terminated string.
    let path = unsafe { name.to_string() };
    // SAFETY: the string was allocated by the Shell with CoTaskMemAlloc and is freed once.
    unsafe { CoTaskMemFree(name.0.cast()) };
    let path =
        path.map_err(|_| windows::core::Error::from_hresult(HRESULT(0x8007_0057_u32 as i32)))?;
    Ok(Some(PathBuf::from(path)))
}

/// The app has no console, so a failure to start would otherwise end it without a word.
pub fn show_start_error(message: &str) {
    let text = HSTRING::from(format!("Game Manager could not start.\n\n{message}"));
    // SAFETY: both strings are valid wide strings that outlive the call; no owner window.
    unsafe {
        MessageBoxW(
            None,
            &text,
            w!("Game Manager"),
            (MB_OK | MB_ICONERROR) as u32,
        )
    };
}
