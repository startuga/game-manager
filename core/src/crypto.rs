//! SHA-256 and random bytes from Windows' own cryptography (CNG, `bcrypt.dll`), with no extra
//! crates:
//! - **SHA-256** (`sha256`, `sha256_file`, as lowercase hex with `hex`): what proves that a file is
//!   the one meant. An elevated copy checks its long work and every source file it copies with it
//!   (`elevate`, `mods`), and a development build is checked against the digest GitHub lists for its
//!   artifact before it is unpacked (`mods::check_download`).
//! - **Random bytes** (`random`): names no other program can guess (`elevate`'s work files).
//!
//! Through the algorithm's pseudo-handle (Windows 10 and later), so nothing is opened or freed.

use crate::{Error, Result};
use std::fs;
use std::path::Path;
use windows::Win32::{
    BCRYPT_SHA256_ALG_HANDLE, BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom, BCryptHash,
};
use windows::core::NTSTATUS;

/// SHA-256 of `bytes`.
pub(crate) fn sha256(bytes: &[u8]) -> Result<[u8; 32]> {
    let length = u32::try_from(bytes.len()).map_err(|_| cng_error("SHA-256", NTSTATUS(-1)))?;
    let mut digest = [0u8; 32];
    // SAFETY: the SHA-256 pseudo-handle needs no opening; `bytes` and `digest` live for the call
    // with the lengths given, and the input is only read.
    let status = unsafe {
        BCryptHash(
            BCRYPT_SHA256_ALG_HANDLE,
            None,
            0,
            bytes.as_ptr().cast_mut(),
            length,
            digest.as_mut_ptr(),
            digest.len() as u32,
        )
    };
    if status.is_ok() {
        Ok(digest)
    } else {
        Err(cng_error("SHA-256", status))
    }
}

/// The SHA-256 of the file at `path`, as lowercase hex. Blocking: reads the whole file.
pub(crate) fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(Error::io(path))?;
    Ok(hex(&sha256(&bytes)?))
}

/// Sixteen random bytes from the system's generator.
pub(crate) fn random() -> Result<[u8; 16]> {
    let mut bytes = [0u8; 16];
    // SAFETY: `bytes` is writable for the length given; the system's generator needs no handle.
    let status = unsafe {
        BCryptGenRandom(
            None,
            bytes.as_mut_ptr(),
            bytes.len() as u32,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG as u32,
        )
    };
    if status.is_ok() {
        Ok(bytes)
    } else {
        Err(cng_error("random numbers", status))
    }
}

/// `bytes` as lowercase hex.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether `text` is a SHA-256 as hex (64 digits, either case).
pub(crate) fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn cng_error(what: &str, status: NTSTATUS) -> Error {
    Error::Windows {
        context: what.to_owned(),
        source: status.to_hresult().into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_sha256() {
        assert_eq!(
            hex(&sha256(b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let file = std::env::temp_dir().join(format!("GM-Crypto-{}.txt", std::process::id()));
        fs::write(&file, b"abc").unwrap();
        assert_eq!(
            sha256_file(&file).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_file(&file).unwrap();
        assert!(sha256_file(&file).is_err());
        assert!(is_sha256(&"aB".repeat(32)) && !is_sha256("abc") && !is_sha256(&"g".repeat(64)));
        assert_ne!(random().unwrap(), random().unwrap());
    }
}
