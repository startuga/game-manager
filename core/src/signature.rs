//! Whether a file carries a valid Authenticode signature by a publisher Windows trusts, as the
//! "Digital Signatures" tab of its properties shows: `WinVerifyTrust` with the generic
//! verification policy. An upscaler DLL Game Manager downloads must pass before it goes into a
//! game. No UI, and nothing is fetched from the network: revocation is not checked, so an offline
//! check gives the same answer.
//!
//! Only a signature embedded in the file counts; a file signed through a Windows catalog (most of
//! Windows' own) reads as unsigned here, which suits vendor DLLs, all signed in the file.

use std::path::Path;
use windows::Win32::{
    WINTRUST_DATA, WINTRUST_DATA_0, WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL,
    WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE, WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE,
    WTD_STATEACTION_VERIFY, WTD_UI_NONE, WinVerifyTrustEx,
};
use windows::core::{GUID, HSTRING, PCWSTR};

/// `WINTRUST_ACTION_GENERIC_VERIFY_V2` (softpub.h): Authenticode's own policy.
const GENERIC_VERIFY_V2: GUID = GUID::from_u128(0x00aac56b_cd44_11d0_8cc2_00c04fc295ee);

/// Whether `file` is validly signed. Blocking: reads the whole file to check its hash.
pub(crate) fn is_signed(file: &Path) -> bool {
    let path = HSTRING::from(file.as_os_str());
    let mut info = WINTRUST_FILE_INFO {
        cbStruct: size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: PCWSTR(path.as_ptr()),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE as u32,
        fdwRevocationChecks: WTD_REVOKE_NONE as u32,
        dwUnionChoice: WTD_CHOICE_FILE as u32,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut info },
        dwStateAction: WTD_STATEACTION_VERIFY as u32,
        dwProvFlags: (WTD_CACHE_ONLY_URL_RETRIEVAL | WTD_REVOCATION_CHECK_NONE) as u32,
        ..Default::default()
    };
    let mut action = GENERIC_VERIFY_V2;
    // SAFETY: `data` and the `info` it points to are valid for the call, and `path` (which `info`
    // points to) outlives both calls; no window is passed, as no UI is asked for.
    let status = unsafe { WinVerifyTrustEx(std::ptr::null_mut(), &mut action, &mut data) };
    data.dwStateAction = WTD_STATEACTION_CLOSE as u32;
    // SAFETY: the same data, now asking to free the state the first call left in it.
    let _ = unsafe { WinVerifyTrustEx(std::ptr::null_mut(), &mut action, &mut data) };
    status == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsigned_file_is_not_signed() {
        let file = std::env::temp_dir().join(format!("GM-unsigned-{}.dll", std::process::id()));
        std::fs::write(&file, b"MZ not really a DLL").unwrap();
        assert!(!is_signed(&file));
        std::fs::remove_file(&file).unwrap();
        assert!(!is_signed(&file), "a missing file");
    }
}
