//! Windows security for mod files: who Game Manager runs as, and access rights added to a file
//! or folder. ReShade needs both, as its own setup does:
//!
//! - In a protected game folder (`C:\Program Files`) the files ReShade saves (`ReShade.ini`, its
//!   preset and log) are created by the copy running as administrator, so they would be read-only
//!   for the user; they are given to the user who started Game Manager. That user's SID is read
//!   here before elevating and travels with the work, as the elevated copy may run as another
//!   account.
//! - ReShade's shared Vulkan layer folder is opened to ALL APPLICATION PACKAGES, so Store and Xbox
//!   games can load the layer.
//!
//! **Who made a file** (`owner_is_admin`): work done as administrator trusts a file, folder or
//! junction only when its owner is Administrators, SYSTEM or TrustedInstaller. A standard user
//! owns what they create, so this tells apart what they could have put in place (a folder in
//! `%ProgramData%`, a junction) from what an administrator or Windows made. The owner read is the
//! path's own: a junction or symbolic link is opened, not followed.

use crate::elevate::OwnedHandle;
use crate::{Error, Result};
use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use windows::Win32::{
    ConvertSidToStringSidW, ConvertStringSidToSidW, DACL_SECURITY_INFORMATION, DELETE,
    EXPLICIT_ACCESS_W, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, GRANT_ACCESS, GetCurrentProcess,
    GetNamedSecurityInfoW, GetSecurityInfo, GetTokenInformation, HANDLE, HLOCAL, LocalFree,
    OWNER_SECURITY_INFORMATION, OpenProcessToken, PACL, PSECURITY_DESCRIPTOR, PSID, READ_CONTROL,
    SE_FILE_OBJECT, SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetEntriesInAclW, SetNamedSecurityInfoW,
    TOKEN_ELEVATION, TOKEN_QUERY, TOKEN_USER, TRUSTEE_IS_SID, TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
    TokenElevation, TokenUser,
};
use windows::core::{HSTRING, PWSTR, WIN32_ERROR};

/// Read, write, execute and delete: the "Modify" permission.
pub(crate) const MODIFY: u32 =
    (FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE | DELETE) as u32;
/// "Read & execute".
pub(crate) const READ_EXECUTE: u32 = (FILE_GENERIC_READ | FILE_GENERIC_EXECUTE) as u32;
/// ALL APPLICATION PACKAGES, the group of every Store app.
pub(crate) const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";
/// The owners of what an administrator or Windows made: Administrators, SYSTEM and
/// TrustedInstaller.
const ADMIN_OWNERS: [&str; 3] = [
    "S-1-5-32-544",
    "S-1-5-18",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];

/// The SID of the user this process runs as ("S-1-5-21-…").
pub(crate) fn current_user_sid() -> Result<String> {
    const WHAT: &str = "the user's account";
    let token = process_token()?;
    let mut size = 0u32;
    // SAFETY: asks only for the size; `size` is a valid out-pointer. It fails by design.
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut size) };
    // u64 storage keeps the TOKEN_USER at the start of the buffer aligned.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    // SAFETY: `buffer` holds at least `size` bytes, and `size` is a valid out-pointer.
    let filled = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            size,
            &mut size,
        )
    };
    filled.ok().map_err(Error::windows(WHAT))?;
    // SAFETY: the call wrote a TOKEN_USER at the start of the aligned buffer, which outlives
    // this borrow.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    // SAFETY: the SID points into `buffer`, alive for the call.
    unsafe { sid_text(user.User.Sid, WHAT) }
}

/// `sid` as text ("S-1-5-…"); `what` names whose it is in an error.
///
/// # Safety
/// `sid` must point to a valid SID for the call.
unsafe fn sid_text(sid: PSID, what: &str) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: the caller keeps `sid` valid; `text` is a valid out-pointer.
    let converted = unsafe { ConvertSidToStringSidW(sid, &mut text) };
    converted.ok().map_err(Error::windows(what))?;
    let owner = Local(text.0.cast());
    // SAFETY: a null-terminated string from ConvertSidToStringSidW, freed by `owner` afterwards.
    let sid = unsafe { text.to_string() };
    drop(owner);
    sid.map_err(|_| Error::UnreadableText {
        what: format!("the SID of {what}"),
    })
}

/// Whether `path` itself (a junction or symbolic link is not followed) is owned by
/// Administrators, SYSTEM or TrustedInstaller (see the module's doc). An error when it can't be
/// opened.
pub(crate) fn owner_is_admin(path: &Path) -> Result<bool> {
    let file = fs::OpenOptions::new()
        .access_mode(READ_CONTROL as u32)
        .custom_flags((FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS) as u32)
        .open(path)
        .map_err(Error::io(path))?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `file` is open with READ_CONTROL and outlives the call; the out-pointers are
    // valid; `owner` points into `descriptor`, freed by `_descriptor` after its last use below.
    let code = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION as u32,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    let _descriptor = Local(descriptor);
    if code != 0 {
        return Err(Error::Windows {
            context: format!("the owner of {}", path.display()),
            source: WIN32_ERROR(code).to_hresult().into(),
        });
    }
    // SAFETY: `owner` points into the descriptor, alive until `_descriptor` drops.
    let owner = unsafe { sid_text(owner, "the owner") }?;
    Ok(ADMIN_OWNERS.contains(&owner.as_str()))
}

/// Whether this process runs as administrator (elevated).
pub(crate) fn is_elevated() -> bool {
    let Ok(token) = process_token() else {
        return false;
    };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    // SAFETY: `elevation` is a TOKEN_ELEVATION of the size passed; both pointers are valid.
    let read = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    };
    read.as_bool() && elevation.TokenIsElevated != 0
}

fn process_token() -> Result<OwnedHandle> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle of this process needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: `token` is a valid out-pointer; the handle is owned by the returned value.
    let opened = unsafe { OpenProcessToken(process, TOKEN_QUERY as u32, &mut token) };
    opened
        .ok()
        .map_err(Error::windows("this process's account"))?;
    Ok(OwnedHandle(token))
}

/// Adds an entry granting `rights` to `sid` on `path` (a file, or a folder whose files and
/// subfolders inherit it), keeping the entries already there.
pub(crate) fn grant(path: &Path, sid: &str, rights: u32) -> Result<()> {
    let context = || format!("the permissions of {}", path.display());
    let check = |code: u32| match code {
        0 => Ok(()),
        code => Err(Error::Windows {
            context: context(),
            source: WIN32_ERROR(code).to_hresult().into(),
        }),
    };
    let name = HSTRING::from(path.as_os_str());
    let mut sid_ptr: PSID = std::ptr::null_mut();
    // SAFETY: `HSTRING` is null-terminated; `sid_ptr` is a valid out-pointer, freed by `_sid`.
    let converted = unsafe { ConvertStringSidToSidW(&HSTRING::from(sid), &mut sid_ptr) };
    converted.ok().map_err(Error::windows(context()))?;
    let _sid = Local(sid_ptr);

    let mut dacl: PACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the out-pointers are valid; `dacl` points into `descriptor`, freed by `_descriptor`
    // after its last use below.
    let code = unsafe {
        GetNamedSecurityInfoW(
            &name,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION as u32,
            None,
            None,
            Some(&mut dacl),
            None,
            &mut descriptor,
        )
    };
    let _descriptor = Local(descriptor);
    check(code)?;

    let inheritance = if path.is_dir() {
        SUB_CONTAINERS_AND_OBJECTS_INHERIT as u32
    } else {
        0
    };
    let mut entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: rights,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: inheritance,
        Trustee: TRUSTEE_W {
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: sid_ptr.cast(),
            ..Default::default()
        },
    };
    let mut new_dacl: PACL = std::ptr::null_mut();
    // SAFETY: a list of one entry, `entry`, which names the live SID; `dacl` is the file's list,
    // alive with `_descriptor`; `new_dacl` is a valid out-pointer, freed by `_new_dacl`.
    let code = unsafe { SetEntriesInAclW(1, Some(&raw mut entry), Some(dacl), &mut new_dacl) };
    let _new_dacl = Local(new_dacl.cast());
    check(code)?;
    // SAFETY: `name` is null-terminated and `new_dacl` a valid list until `_new_dacl` drops.
    let code = unsafe {
        SetNamedSecurityInfoW(
            &name,
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION as u32,
            None,
            None,
            Some(new_dacl),
            None,
        )
    };
    check(code)
}

/// Memory Windows allocated with `LocalAlloc` (a SID, security descriptor, access list or
/// string), freed when dropped.
struct Local(HLOCAL);

impl Drop for Local {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a Windows call that allocates with LocalAlloc, only
            // this value owns it, and it is freed once.
            unsafe { LocalFree(self.0) };
        }
    }
}

/// Makes `link` a junction to `target`, as a standard user may, for tests.
#[cfg(test)]
pub(crate) fn junction(link: &Path, target: &Path) {
    let made = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn grants_a_user_rights_on_a_file() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
        let dir = std::env::temp_dir().join(format!("GM-Security-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ReShade.ini");
        fs::write(&file, b"").unwrap();

        grant(&file, ALL_APPLICATION_PACKAGES, READ_EXECUTE).unwrap();
        grant(&file, &sid, MODIFY).unwrap();
        // icacls lists the new entries: ALL APPLICATION PACKAGES' by name.
        let listing = Command::new("icacls").arg(&file).output().unwrap();
        let listing = String::from_utf8_lossy(&listing.stdout).to_ascii_uppercase();
        assert!(listing.contains("APPLICATION PACKAGE"), "{listing}");
        assert!(grant(&dir.join("missing"), &sid, MODIFY).is_err());
        assert!(grant(&file, "not a SID", MODIFY).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn owners_are_read_from_the_path_itself() {
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        // Windows' own folder and programs: TrustedInstaller.
        assert!(owner_is_admin(&system).unwrap());
        assert!(owner_is_admin(&system.join(r"System32\hostname.exe")).unwrap());

        let dir = std::env::temp_dir().join(format!("GM-Owner-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        // A junction to Windows' folder is the user's, whatever it leads to.
        let link = dir.join("link");
        junction(&link, &system);
        // Run as administrator, what the test makes is Administrators'.
        let by_user = !is_elevated();
        assert_eq!(owner_is_admin(&link).unwrap(), !by_user);
        assert_eq!(owner_is_admin(&dir).unwrap(), !by_user);
        assert!(owner_is_admin(&dir.join("missing")).is_err());
        fs::remove_dir(&link).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }
}
