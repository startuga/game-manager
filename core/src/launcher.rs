//! Starts games through the Windows shell.

use crate::Launch;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Launches a game. Store URIs (e.g. `steam://`) are handed to their registered client.
pub fn launch(launch: &Launch) -> Result<(), String> {
    let target = match launch {
        Launch::Uri(uri) => HSTRING::from(uri.as_str()),
        Launch::Exe(path) => HSTRING::from(path.as_os_str()),
    };

    // SAFETY: all string arguments are valid, null-terminated wide strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW reports success with a value greater than 32.
    if result.0 as usize > 32 {
        Ok(())
    } else {
        Err(format!(
            "failed to launch {target} (code {})",
            result.0 as usize
        ))
    }
}
