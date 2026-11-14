//! Who signed a file, when it carries a valid Authenticode signature by a publisher Windows
//! trusts, as the "Digital Signatures" tab of its properties shows: `WinVerifyTrust` with the
//! generic verification policy, then the signing certificate's simple name ("NVIDIA Corporation",
//! its subject's common name). An upscaler DLL Game Manager downloads must be signed by its
//! vendor, as `config/upscalers.json` names it, before it goes into a game: a valid signature by
//! anyone else (a vendor's repository taken over, a certificate stolen from another company) is
//! not enough.
//!
//! No UI, and nothing is fetched from the network: revocation is not checked, so an offline check
//! gives the same answer (with it, an offline PC could keep no download at all).
//!
//! Only a signature embedded in the file counts; a file signed only through a Windows catalog reads
//! as unsigned here, which suits vendor DLLs, all signed in the file.

use std::path::Path;
use windows::Win32::{
    CERT_NAME_SIMPLE_DISPLAY_TYPE, CertGetNameStringW, HANDLE, WINTRUST_DATA, WINTRUST_DATA_0,
    WINTRUST_FILE_INFO, WTD_CACHE_ONLY_URL_RETRIEVAL, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_NONE,
    WTD_REVOKE_NONE, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    WTHelperGetProvSignerFromChain, WTHelperProvDataFromStateData, WinVerifyTrustEx,
};
use windows::core::{GUID, HSTRING, PCWSTR, PWSTR};

/// `WINTRUST_ACTION_GENERIC_VERIFY_V2` (softpub.h): Authenticode's own policy.
const GENERIC_VERIFY_V2: GUID = GUID::from_u128(0x00aac56b_cd44_11d0_8cc2_00c04fc295ee);

/// The name `file`'s signer goes by, when it is validly signed; `None` when it is not, or the
/// name can't be read. Blocking: reads the whole file to check its hash.
pub(crate) fn signer(file: &Path) -> Option<String> {
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
    // Read while the verification's state is kept, before it is closed.
    let name = if status == 0 {
        signer_name(data.hWVTStateData)
    } else {
        None
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE as u32;
    // SAFETY: the same data, now asking to free the state the first call left in it.
    let _ = unsafe { WinVerifyTrustEx(std::ptr::null_mut(), &mut action, &mut data) };
    name
}

/// The first signer's certificate's simple name, from the state a successful verification left.
fn signer_name(state: HANDLE) -> Option<String> {
    // SAFETY: the state the verification left, alive until it is closed after this returns.
    let provider = unsafe { WTHelperProvDataFromStateData(state) };
    if provider.is_null() {
        return None;
    }
    // SAFETY: the provider data above; its first signer, not a countersigner.
    let signer = unsafe { WTHelperGetProvSignerFromChain(provider, 0, false, 0) };
    if signer.is_null() {
        return None;
    }
    // SAFETY: a signer WinTrust filled in, alive with the state.
    let count = unsafe { (*signer).csCertChain };
    // SAFETY: as above.
    let chain = unsafe { (*signer).pasCertChain };
    if count == 0 || chain.is_null() {
        return None;
    }
    // SAFETY: the chain holds `count` (at least one) elements; the first is the signing
    // certificate's.
    let certificate = unsafe { (*chain).pCert };
    if certificate.is_null() {
        return None;
    }
    let mut buffer = [0u16; 256];
    // SAFETY: a certificate context alive with the state; `buffer` holds the characters given.
    let written = unsafe {
        CertGetNameStringW(
            certificate,
            CERT_NAME_SIMPLE_DISPLAY_TYPE as u32,
            0,
            None,
            Some(PWSTR(buffer.as_mut_ptr())),
            buffer.len() as u32,
        )
    } as usize;
    // The count includes the terminating null.
    let name = String::from_utf16_lossy(buffer.get(..written.saturating_sub(1))?);
    (!name.is_empty()).then_some(name)
}

/// Whether `signer` (as `signer` read it) is `expected`, the vendor's name: letter case aside, as
/// an older DLSS DLL is signed by "Nvidia Corporation" and newer ones by "NVIDIA Corporation".
pub(crate) fn is_signer(signer: &str, expected: &str) -> bool {
    signer.trim().eq_ignore_ascii_case(expected.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsigned_file_has_no_signer() {
        let file = std::env::temp_dir().join(format!("GM-unsigned-{}.dll", std::process::id()));
        std::fs::write(&file, b"MZ not really a DLL").unwrap();
        assert_eq!(signer(&file), None);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(signer(&file), None, "a missing file");
    }

    #[test]
    fn a_signed_file_names_its_signer() {
        // The Visual C++ runtime Game Manager needs carries its own signature by Microsoft.
        let runtime = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join(r"System32\vcruntime140.dll");
        if !runtime.is_file() {
            return;
        }
        let name = signer(&runtime).unwrap();
        assert!(name.starts_with("Microsoft"), "{name}");
    }

    #[test]
    fn signers_match_their_vendor_whatever_the_case() {
        assert!(is_signer("Nvidia Corporation", "NVIDIA Corporation"));
        assert!(is_signer("NVIDIA Corporation", "NVIDIA Corporation"));
        assert!(!is_signer("NVIDIA Corporation Ltd", "NVIDIA Corporation"));
        assert!(!is_signer("Intel Corporation", "NVIDIA Corporation"));
    }
}
