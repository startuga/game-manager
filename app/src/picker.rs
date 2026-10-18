//! The Windows "Open" dialog, used to choose a custom game's program.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;
use windows::Win32::Foundation::{ERROR_CANCELLED, HWND};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
};
use windows::core::w;

/// The window that owns the dialog, kept as a number so it can be sent to another thread.
#[derive(Clone, Copy)]
pub struct Owner(isize);

impl Owner {
    /// The active window of the calling thread. Called on the UI thread right after a click,
    /// that is the app's window.
    pub fn active() -> Self {
        // SAFETY: GetActiveWindow has no preconditions.
        Self(unsafe { GetActiveWindow() }.0 as isize)
    }
}

/// Shows the dialog and waits for it; `Ok(None)` when the user cancels. The owner window is
/// disabled while the dialog is open. Blocking: call it from a background task.
pub fn pick_program(owner: Owner) -> Result<Option<PathBuf>, String> {
    // The dialog needs a single-threaded COM apartment, which thread-pool threads are not.
    std::thread::spawn(move || {
        // SAFETY: a new thread, not yet in any COM apartment.
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) }
            .ok()
            .map_err(|e| e.message())?;
        let picked = show(owner).map_err(|e| e.message());
        // SAFETY: balances the CoInitializeEx above; the dialog was released inside `show`.
        unsafe { CoUninitialize() };
        picked
    })
    .join()
    .map_err(|_| "the file dialog stopped unexpectedly".to_owned())?
    .map_err(|e| format!("could not choose a program: {e}"))
}

fn show(owner: Owner) -> windows::core::Result<Option<PathBuf>> {
    let filters = [
        COMDLG_FILTERSPEC {
            pszName: w!("Programs"),
            pszSpec: w!("*.exe;*.bat;*.cmd"),
        },
        COMDLG_FILTERSPEC {
            pszName: w!("All files"),
            pszSpec: w!("*.*"),
        },
    ];
    // SAFETY: COM is initialized on this thread, the strings are static, and the returned name is
    // freed with CoTaskMemFree as the API requires.
    unsafe {
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetTitle(w!("Choose the game's program"))?;
        dialog.SetFileTypes(&filters)?;
        dialog.SetOptions(dialog.GetOptions()? | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM)?;
        match dialog.Show(Some(HWND(owner.0 as *mut _))) {
            Err(e) if e.code() == ERROR_CANCELLED.to_hresult() => return Ok(None),
            result => result?,
        }
        let name = dialog.GetResult()?.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = PathBuf::from(OsString::from_wide(name.as_wide()));
        CoTaskMemFree(Some(name.0 as *const _));
        Ok(Some(path))
    }
}
