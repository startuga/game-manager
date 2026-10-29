//! Runs file operations as administrator, for games in protected folders (`C:\Program Files`):
//! Game Manager starts a copy of itself with `--file-ops <operations>` through the `runas` verb,
//! which shows the UAC prompt, and waits for it. The operations travel on the command line, so the
//! copy running as administrator reads nothing that another program could change in between.

use crate::{Error, Result};
use windows::Win32::{
    CloseHandle, ERROR_CANCELLED, GetExitCodeProcess, HANDLE, INFINITE, SEE_MASK_NOASYNC,
    SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SW_HIDE, ShellExecuteExW, WaitForSingleObject,
};
use windows::core::{HSTRING, PCWSTR, WIN32_ERROR, w};

/// The argument that makes `app.exe` run file operations instead of its window.
pub const FILE_OPS_ARG: &str = "--file-ops";

/// Runs this program as administrator with `--file-ops <encoded>` and waits for it to finish.
pub(crate) fn run(encoded: &str) -> Result<()> {
    let program = std::env::current_exe().map_err(Error::io("Game Manager"))?;
    let file = HSTRING::from(program.as_os_str());
    let parameters = HSTRING::from(format!("{FILE_OPS_ARG} {encoded}"));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: (SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC) as u32,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // SAFETY: `info` is initialized, its size is set, and its strings outlive the call.
    if let Err(error) = unsafe { ShellExecuteExW(&mut info) }.ok() {
        return Err(
            if WIN32_ERROR::from_error(&error) == Some(WIN32_ERROR(ERROR_CANCELLED as u32)) {
                Error::ElevationRefused
            } else {
                Error::Windows {
                    context: "Game Manager as administrator".to_owned(),
                    source: error,
                }
            },
        );
    }
    let process = OwnedHandle(info.hProcess);
    // SAFETY: a live process handle, owned by `process`.
    unsafe { WaitForSingleObject(process.0, INFINITE) };
    let mut code = 0u32;
    // SAFETY: the process has ended; `code` is a valid out-pointer.
    unsafe { GetExitCodeProcess(process.0, &mut code) }
        .ok()
        .map_err(Error::windows("Game Manager as administrator"))?;
    match code {
        0 => Ok(()),
        code => Err(Error::Elevated { code }),
    }
}

/// A process handle, closed when dropped.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from ShellExecuteExW, only this value owns it, and it is
            // closed once.
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}
