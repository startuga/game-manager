//! Starts games through the Windows shell.

use crate::Launch;
use windows::Win32::{SW_SHOWNORMAL, ShellExecuteW};
use windows::core::{HSTRING, PCWSTR, w};

/// Launches a game. Store URIs (e.g. `steam://`) are handed to their registered client.
pub fn launch(launch: &Launch) -> Result<(), String> {
    let (target, args, working_dir) = match launch {
        Launch::Uri(uri) => (HSTRING::from(uri.as_str()), HSTRING::new(), HSTRING::new()),
        Launch::Exe {
            path,
            args,
            working_dir,
        } => {
            // Many games load their data relative to the current folder, so never inherit ours.
            let dir = working_dir.as_deref().or_else(|| path.parent());
            (
                HSTRING::from(path.as_os_str()),
                HSTRING::from(args.as_str()),
                dir.map(|d| HSTRING::from(d.as_os_str()))
                    .unwrap_or_default(),
            )
        }
    };
    let optional = |s: &HSTRING| {
        if s.is_empty() {
            PCWSTR::null()
        } else {
            PCWSTR(s.as_ptr())
        }
    };

    // SAFETY: all string arguments are valid, null-terminated wide strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            &target,
            optional(&args),
            optional(&working_dir),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW reports success with a value greater than 32.
    let code = result as usize;
    if code > 32 {
        Ok(())
    } else {
        Err(format!(
            "failed to launch {} (code {code})",
            target.display()
        ))
    }
}
