//! Secrets kept in Windows Credential Manager (Control Panel ▸ Credential Manager ▸ Windows
//! Credentials), as generic credentials of the current user: Windows encrypts them for the user's
//! account (DPAPI), so they are never in Game Manager's own files, which a user may copy or
//! share. The SteamGridDB API key is kept this way (`steamgriddb.rs`).
//!
//! A credential's secret is its blob, here the secret's UTF-8 bytes; its target name says whose it
//! is ("Game Manager/SteamGridDB"). Reading and writing are calls into the Local Security Authority,
//! quick but blocking: run them off the UI thread.

use std::ptr::null_mut;

use windows::Win32::{
    CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW, ERROR_NOT_FOUND, PCREDENTIALW,
};
use windows::core::{HRESULT, HSTRING, PWSTR};

use crate::{Error, Result};

/// The largest secret kept: Credential Manager allows 2 560 bytes for a generic credential.
const LARGEST_SECRET: usize = 2560;

/// A credential Credential Manager handed out, freed when dropped.
struct Credential(PCREDENTIALW);

impl Drop for Credential {
    fn drop(&mut self) {
        // SAFETY: the pointer came from `CredReadW` and is freed only here, once.
        unsafe { CredFree(self.0.cast()) };
    }
}

/// Win32 errors as HRESULTs: severity 1, this facility, the error code.
const FACILITY_WIN32: u32 = 7;

/// Whether Credential Manager answered that there is no such credential.
fn not_found(error: &windows::core::Error) -> bool {
    error.code() == HRESULT::from_components(1, FACILITY_WIN32, ERROR_NOT_FOUND as u32)
}

/// What a failure is about, for its message.
fn context(target: &str) -> String {
    format!("Credential Manager ({target})")
}

/// The secret kept under `target`; `None` when there is none (or it isn't UTF-8).
pub(crate) fn read(target: &str) -> Result<Option<String>> {
    let name = HSTRING::from(target);
    let mut found: PCREDENTIALW = null_mut();
    // SAFETY: `name` is a null-terminated wide string for the call; `found` receives a pointer
    // Credential Manager allocates, owned by `Credential` below.
    let read = unsafe { CredReadW(&name, CRED_TYPE_GENERIC as u32, None, &mut found) }.ok();
    match read {
        Ok(()) => {}
        Err(error) if not_found(&error) => return Ok(None),
        Err(error) => return Err(Error::windows(context(target))(error)),
    }
    let credential = Credential(found);
    // SAFETY: `CredReadW` succeeded, so the pointer is to a live CREDENTIALW until it is freed.
    let entry = unsafe { &*credential.0 };
    if entry.CredentialBlob.is_null() || entry.CredentialBlobSize == 0 {
        return Ok(None);
    }
    // SAFETY: the blob is `CredentialBlobSize` bytes long and lives as long as `credential`.
    let blob = unsafe {
        std::slice::from_raw_parts(entry.CredentialBlob, entry.CredentialBlobSize as usize)
    };
    Ok(String::from_utf8(blob.to_vec()).ok())
}

/// Keeps `secret` under `target`, replacing what was there.
pub(crate) fn write(target: &str, secret: &str) -> Result<()> {
    if secret.len() > LARGEST_SECRET {
        return Err(Error::SecretTooLong {
            target: target.to_owned(),
            limit: LARGEST_SECRET,
        });
    }
    let mut name: Vec<u16> = target.encode_utf16().chain([0]).collect();
    let mut blob = secret.as_bytes().to_vec();
    let mut credential = CREDENTIALW {
        Type: CRED_TYPE_GENERIC as u32,
        TargetName: PWSTR(name.as_mut_ptr()),
        CredentialBlobSize: blob.len() as u32,
        CredentialBlob: blob.as_mut_ptr(),
        Persist: CRED_PERSIST_LOCAL_MACHINE as u32,
        ..CREDENTIALW::default()
    };
    // SAFETY: `credential` and the name and blob it points to live for the call; Credential
    // Manager copies them.
    unsafe { CredWriteW(&mut credential, 0) }
        .ok()
        .map_err(Error::windows(context(target)))
}

/// Deletes the secret under `target`; none is fine.
pub(crate) fn delete(target: &str) -> Result<()> {
    let name = HSTRING::from(target);
    // SAFETY: `name` is a null-terminated wide string for the call.
    match unsafe { CredDeleteW(&name, CRED_TYPE_GENERIC as u32, None) }.ok() {
        Ok(()) => Ok(()),
        Err(error) if not_found(&error) => Ok(()),
        Err(error) => Err(Error::windows(context(target))(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes to the user's own Credential Manager (under a test name, deleted after), so it runs
    /// only when asked: `cargo test -p gm-core credentials -- --ignored`.
    #[test]
    #[ignore = "writes to this user's Credential Manager"]
    fn a_secret_is_kept_read_and_deleted() {
        let target = format!("Game Manager/test-{}", std::process::id());
        assert_eq!(read(&target).unwrap(), None);
        write(&target, "first").unwrap();
        write(&target, "sécond").unwrap();
        assert_eq!(read(&target).unwrap().as_deref(), Some("sécond"));
        delete(&target).unwrap();
        delete(&target).unwrap();
        assert_eq!(read(&target).unwrap(), None);
    }

    #[test]
    fn a_secret_too_long_for_credential_manager_is_refused() {
        let error = write("Game Manager/never", &"k".repeat(LARGEST_SECRET + 1)).unwrap_err();
        assert!(matches!(error, Error::SecretTooLong { .. }), "{error}");
    }
}
