//! NVIDIA driver profile settings: the ones DXVK needs for HDR on NVIDIA GPUs, "Vulkan/OpenGL
//! Present Method" (`0x20D690F8`) and its flags (`0x20324987`, `OGL_DX_PRESENT_DEBUG`), in the
//! driver profile that holds the game's `.exe` (the database the NVIDIA Control Panel and
//! nvidiaProfileInspector edit; ids and values from the inspector's `CustomSettingNames.xml`).
//!
//! NVAPI's driver settings (DRS) functions come from `nvapi64.dll`, which the driver installs in
//! System32 and which exports only `nvapi_QueryInterface`: each function is asked for by its id
//! (NVIDIA's `nvapi_interface.h`), with the structures of `nvapi.h` (sizes checked below).
//!
//! Reading needs no rights, and neither does writing the Present Method; the flags setting needs
//! administrator rights (the driver answers `NVAPI_INVALID_USER_PRIVILEGE`, as tested with
//! driver 32.0.16.2112). Then the same plan runs in a copy of Game Manager started as
//! administrator with `--nvidia-ops <plan>` (see `elevate`). A plan's changes are saved together
//! (`DRS_SaveSettings`) or not at all: a refused session is dropped unsaved.

use crate::elevate;
use crate::text::{percent_decode, percent_encode};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use windows::Win32::{
    FreeLibrary, GetProcAddress, HMODULE, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows::core::{s, w};

/// The argument that makes `app.exe` run a `DrsPlan` instead of its window.
pub const NVIDIA_OPS_ARG: &str = "--nvidia-ops";

/// "Vulkan/OpenGL Present Method".
pub const PRESENT_METHOD: u32 = 0x20D6_90F8;
/// "Vulkan/OpenGL Present Method - Flags".
pub const PRESENT_FLAGS: u32 = 0x2032_4987;

/// The Present Method values.
pub const PRESENT_METHODS: [(u32, &str); 3] = [
    (0, "Prefer native"),
    (1, "Prefer layered on DXGI Swapchain"),
    (2, "Auto"),
];

/// The Present Method flags that matter for DXVK.
pub const PRESENT_FLAG_VALUES: [(u32, &str); 2] = [
    (0x0008_0004, "Allow promoting DXVK to DXGI/DirectFlip"),
    (0x0008_02A5, "Treat DXVK as Native"),
];

/// A Present Method value's name.
pub fn method_label(value: u32) -> String {
    PRESENT_METHODS
        .iter()
        .find(|(known, _)| *known == value)
        .map_or_else(
            || format!("0x{value:08X}"),
            |(_, label)| (*label).to_owned(),
        )
}

/// A Present Method flags value's name; other bit sets as hex.
pub fn flags_label(value: u32) -> String {
    if value == 0 {
        return "None".to_owned();
    }
    PRESENT_FLAG_VALUES
        .iter()
        .find(|(known, _)| *known == value)
        .map_or_else(
            || format!("0x{value:08X}"),
            |(_, label)| (*label).to_owned(),
        )
}

/// The game's choice in DXVK settings; `None` is "as it was before Game Manager changed it".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NvidiaChoice {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present_method: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub present_flags: Option<u32>,
}

impl NvidiaChoice {
    pub fn is_original(&self) -> bool {
        *self == Self::default()
    }
}

/// What the `.exe`'s driver profile held before Game Manager changed it, to put it back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NvidiaOriginal {
    pub profile: String,
    /// Game Manager created the profile (the `.exe` had none), so going back deletes it.
    #[serde(default)]
    pub created_profile: bool,
    /// The values the user had set in the profile; `None`: none (NVIDIA's or inherited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<u32>,
}

/// A setting in the `.exe`'s profile.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SettingState {
    /// The profile's value, the user's or NVIDIA's; `None`: not in the profile (the driver's
    /// global value applies).
    pub value: Option<u32>,
    /// The value when the user set it in this profile.
    pub user_value: Option<u32>,
}

/// The `.exe`'s driver profile, as far as these settings go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileState {
    /// The profile's name; `None`: no profile holds the `.exe`.
    pub profile: Option<String>,
    pub method: SettingState,
    pub flags: SettingState,
}

/// Changes to one `.exe`'s driver profile, saved together.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DrsPlan {
    /// The program's file name (`game.exe`), as profiles list it.
    pub exe: String,
    /// A profile to create for the `.exe` first, which has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_profile: Option<String>,
    #[serde(default)]
    pub set: Vec<(u32, u32)>,
    /// Settings to take out of the profile, so NVIDIA's or the inherited value applies again.
    #[serde(default)]
    pub delete: Vec<u32>,
    /// Delete the profile at the end when nothing else is in it (Game Manager created it).
    #[serde(default)]
    pub delete_profile: bool,
}

impl DrsPlan {
    pub fn is_empty(&self) -> bool {
        self.create_profile.is_none()
            && self.set.is_empty()
            && self.delete.is_empty()
            && !self.delete_profile
    }
}

/// The plan that gives the `.exe`'s profile (`state`) the `wanted` values, and the originals to
/// keep afterwards: `None` once everything is as it was. `new_profile` names a profile to create
/// when the `.exe` has none.
pub fn plan(
    exe: &str,
    new_profile: &str,
    state: &ProfileState,
    wanted: NvidiaChoice,
    original: Option<&NvidiaOriginal>,
) -> (DrsPlan, Option<NvidiaOriginal>) {
    let mut plan = DrsPlan {
        exe: exe.to_owned(),
        ..DrsPlan::default()
    };
    let changing = !wanted.is_original();
    let original = match original {
        Some(original) => original.clone(),
        None if changing => NvidiaOriginal {
            profile: state
                .profile
                .clone()
                .unwrap_or_else(|| new_profile.to_owned()),
            created_profile: state.profile.is_none(),
            method: state.method.user_value,
            flags: state.flags.user_value,
        },
        // Nothing changed before, nothing to change now.
        None => return (plan, None),
    };
    if state.profile.is_none() {
        if !changing {
            // The profile is gone already: nothing is left to put back.
            return (plan, None);
        }
        plan.create_profile = Some(new_profile.to_owned());
    }
    for (id, wanted, before, now) in [
        (
            PRESENT_METHOD,
            wanted.present_method,
            original.method,
            state.method,
        ),
        (
            PRESENT_FLAGS,
            wanted.present_flags,
            original.flags,
            state.flags,
        ),
    ] {
        match (wanted, before) {
            (Some(value), _) | (None, Some(value)) => plan.set.push((id, value)),
            (None, None) if now.user_value.is_some() => plan.delete.push(id),
            (None, None) => {}
        }
    }
    if changing {
        return (plan, Some(original));
    }
    plan.delete_profile = original.created_profile;
    (plan, None)
}

/// The `.exe`'s profile and both settings; `None` without an NVIDIA graphics driver.
pub fn read(exe: &str) -> Result<Option<ProfileState>> {
    let Some(api) = Nvapi::load()? else {
        return Ok(None);
    };
    let session = api.session()?;
    let Some(profile) = session.find(exe)? else {
        return Ok(Some(ProfileState::default()));
    };
    Ok(Some(ProfileState {
        profile: Some(session.profile_name(profile)?),
        method: session.setting(profile, PRESENT_METHOD)?,
        flags: session.setting(profile, PRESENT_FLAGS)?,
    }))
}

/// Applies the plan, as administrator when the driver asks for it.
pub fn run(plan: &DrsPlan) -> Result<()> {
    if plan.is_empty() {
        return Ok(());
    }
    match apply(plan) {
        Err(Error::Nvapi {
            status: INVALID_USER_PRIVILEGE,
            ..
        }) => {
            let json = serde_json::to_string(plan).map_err(Error::json("NVIDIA settings"))?;
            match elevate::run(NVIDIA_OPS_ARG, &percent_encode(&json)) {
                // The elevated copy's exit code carries NVAPI's status (see `run_encoded`).
                Err(Error::Elevated { code }) if code & 0xFFFF_0000 == ELEVATED_STATUS => {
                    Err(Error::Nvapi {
                        action: "save the settings",
                        status: -((code & 0xFFFF) as i32),
                    })
                }
                other => other,
            }
        }
        other => other,
    }
}

/// Exit codes of `run_encoded` that carry an NVAPI status: `0xE0000000 | -status`.
const ELEVATED_STATUS: u32 = 0xE000_0000;

/// Runs the plan passed with `--nvidia-ops`. The result is the program's exit code: 0, or
/// `0xE0000000` with NVAPI's status (negated) in the low bits.
pub fn run_encoded(encoded: &str) -> u32 {
    let plan = percent_decode(encoded).and_then(|json| serde_json::from_str::<DrsPlan>(&json).ok());
    let Some(plan) = plan else {
        return ELEVATED_STATUS | INVALID_ARGUMENT.unsigned_abs();
    };
    match apply(&plan) {
        Ok(()) => 0,
        Err(Error::Nvapi { status, .. }) => ELEVATED_STATUS | (status.unsigned_abs() & 0xFFFF),
        Err(_) => ELEVATED_STATUS | ERROR.unsigned_abs(),
    }
}

fn apply(plan: &DrsPlan) -> Result<()> {
    let api = Nvapi::load()?.ok_or(Error::Nvapi {
        action: "find the NVIDIA driver",
        status: NVIDIA_DEVICE_NOT_FOUND,
    })?;
    let session = api.session()?;
    let profile = match &plan.create_profile {
        Some(name) => session.create_profile(name, &plan.exe)?,
        None => session.find(&plan.exe)?.ok_or(Error::Nvapi {
            action: "find the game's profile",
            status: EXECUTABLE_NOT_FOUND,
        })?,
    };
    for &(id, value) in &plan.set {
        session.set_dword(profile, id, value)?;
    }
    for &id in &plan.delete {
        session.delete_setting(profile, id)?;
    }
    if plan.delete_profile && session.profile_info(profile)?.num_of_settings == 0 {
        session.delete_profile(profile)?;
    }
    session.save()
}

// NVAPI status codes (`nvapi_lite_common.h`).
const OK: Status = 0;
const ERROR: Status = -1;
const INVALID_ARGUMENT: Status = -5;
pub(crate) const NVIDIA_DEVICE_NOT_FOUND: Status = -6;
pub(crate) const INVALID_USER_PRIVILEGE: Status = -137;
const SETTING_NOT_FOUND: Status = -160;
const PROFILE_NAME_IN_USE: Status = -164;
pub(crate) const EXECUTABLE_NOT_FOUND: Status = -166;

/// What an NVAPI status means, for error messages.
pub(crate) fn status_text(status: Status) -> &'static str {
    match status {
        NVIDIA_DEVICE_NOT_FOUND => "no NVIDIA graphics driver was found",
        -9 => "this driver does not know the structure version Game Manager uses",
        INVALID_USER_PRIVILEGE => "it needs administrator rights",
        SETTING_NOT_FOUND => "the setting is not in the profile",
        -163 => "the profile was not found",
        PROFILE_NAME_IN_USE => "a profile with that name exists",
        EXECUTABLE_NOT_FOUND => "no profile holds the game's program",
        -167 => "another profile holds the game's program",
        _ => "NVAPI failed",
    }
}

type Status = i32;
type SessionHandle = *mut c_void;
type ProfileHandle = *mut c_void;

const UNICODE_STRING_MAX: usize = 2048;
type UnicodeString = [u16; UNICODE_STRING_MAX];

/// `NVDRS_SETTING_V1`, packed to 4 bytes in nvapi.h. Its two value unions (DWORD, binary of
/// 4 + 4096 bytes, string or QWORD) are 4100 bytes aligned to 4: plain `u32` arrays, whose first
/// element holds a DWORD.
#[repr(C)]
struct Setting {
    version: u32,
    setting_name: UnicodeString,
    setting_id: u32,
    setting_type: u32,
    setting_location: u32,
    is_current_predefined: u32,
    is_predefined_valid: u32,
    predefined_value: [u32; 1025],
    current_value: [u32; 1025],
}

/// `NVDRS_PROFILE_V1`.
#[repr(C)]
struct ProfileInfo {
    version: u32,
    profile_name: UnicodeString,
    gpu_support: u32,
    is_predefined: u32,
    num_of_apps: u32,
    num_of_settings: u32,
}

/// `NVDRS_APPLICATION_V4`.
#[repr(C)]
struct Application {
    version: u32,
    is_predefined: u32,
    app_name: UnicodeString,
    user_friendly_name: UnicodeString,
    launcher: UnicodeString,
    file_in_folder: UnicodeString,
    /// `isMetro`, `isCommandLine` and reserved bits.
    flags: u32,
    command_line: UnicodeString,
}

const fn version(size: usize, version: u32) -> u32 {
    size as u32 | (version << 16)
}

const SETTING_VER: u32 = version(size_of::<Setting>(), 1);
const PROFILE_VER: u32 = version(size_of::<ProfileInfo>(), 1);
const APPLICATION_VER: u32 = version(size_of::<Application>(), 4);
// The sizes nvapi.h gives these structures.
const _: () = assert!(size_of::<Setting>() == 12_320);
const _: () = assert!(size_of::<ProfileInfo>() == 4_116);
const _: () = assert!(size_of::<Application>() == 20_492);

/// `NVDRS_DWORD_TYPE`.
const DWORD_TYPE: u32 = 0;
/// `NVDRS_CURRENT_PROFILE_LOCATION`: the value comes from the profile itself.
const CURRENT_PROFILE_LOCATION: u32 = 0;

impl Setting {
    fn new(id: u32) -> Box<Self> {
        Box::new(Self {
            version: SETTING_VER,
            setting_name: [0; UNICODE_STRING_MAX],
            setting_id: id,
            setting_type: DWORD_TYPE,
            setting_location: 0,
            is_current_predefined: 0,
            is_predefined_valid: 0,
            predefined_value: [0; 1025],
            current_value: [0; 1025],
        })
    }
}

impl ProfileInfo {
    fn new(name: &str) -> Box<Self> {
        Box::new(Self {
            version: PROFILE_VER,
            profile_name: wide(name),
            gpu_support: 0,
            is_predefined: 0,
            num_of_apps: 0,
            num_of_settings: 0,
        })
    }
}

impl Application {
    fn new(exe: &str) -> Box<Self> {
        Box::new(Self {
            version: APPLICATION_VER,
            is_predefined: 0,
            app_name: wide(exe),
            user_friendly_name: [0; UNICODE_STRING_MAX],
            launcher: [0; UNICODE_STRING_MAX],
            file_in_folder: [0; UNICODE_STRING_MAX],
            flags: 0,
            command_line: [0; UNICODE_STRING_MAX],
        })
    }
}

/// `text` as an NVAPI string: UTF-16, null-terminated, at most 2047 units.
fn wide(text: &str) -> UnicodeString {
    let mut string = [0; UNICODE_STRING_MAX];
    for (unit, slot) in text
        .encode_utf16()
        .zip(string.iter_mut().take(UNICODE_STRING_MAX - 1))
    {
        *slot = unit;
    }
    string
}

fn from_wide(string: &UnicodeString) -> String {
    let len = string
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(string.len());
    String::from_utf16_lossy(&string[..len])
}

fn check(status: Status, action: &'static str) -> Result<()> {
    if status == OK {
        Ok(())
    } else {
        Err(Error::Nvapi { action, status })
    }
}

type QueryInterface = unsafe extern "C" fn(u32) -> *const c_void;
type NoArgs = unsafe extern "C" fn() -> Status;
type CreateSession = unsafe extern "C" fn(*mut SessionHandle) -> Status;
type SessionCall = unsafe extern "C" fn(SessionHandle) -> Status;
type FindApplicationByName =
    unsafe extern "C" fn(SessionHandle, *const u16, *mut ProfileHandle, *mut Application) -> Status;
type CreateProfile =
    unsafe extern "C" fn(SessionHandle, *mut ProfileInfo, *mut ProfileHandle) -> Status;
type CreateApplication =
    unsafe extern "C" fn(SessionHandle, ProfileHandle, *mut Application) -> Status;
type GetProfileInfo =
    unsafe extern "C" fn(SessionHandle, ProfileHandle, *mut ProfileInfo) -> Status;
type GetSetting = unsafe extern "C" fn(SessionHandle, ProfileHandle, u32, *mut Setting) -> Status;
type SetSetting = unsafe extern "C" fn(SessionHandle, ProfileHandle, *mut Setting) -> Status;
type ProfileSettingCall = unsafe extern "C" fn(SessionHandle, ProfileHandle, u32) -> Status;
type ProfileCall = unsafe extern "C" fn(SessionHandle, ProfileHandle) -> Status;

/// `nvapi64.dll`, freed when dropped.
struct Library(HMODULE);

impl Drop for Library {
    fn drop(&mut self) {
        // SAFETY: the module was loaded by `Nvapi::load`, only this value owns it, and it is
        // freed once, after `Nvapi` unloaded NVAPI.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

/// The NVAPI functions Game Manager uses. NVAPI is unloaded and the library freed when dropped.
struct Nvapi {
    unload: NoArgs,
    create_session: CreateSession,
    destroy_session: SessionCall,
    load_settings: SessionCall,
    save_settings: SessionCall,
    find_application: FindApplicationByName,
    create_profile: CreateProfile,
    create_application: CreateApplication,
    profile_info: GetProfileInfo,
    get_setting: GetSetting,
    set_setting: SetSetting,
    delete_setting: ProfileSettingCall,
    delete_profile: ProfileCall,
    // Last, so it is freed after `Drop::drop` unloaded NVAPI.
    _library: Library,
}

/// The function `id` from `nvapi_QueryInterface`.
///
/// # Safety
/// `F` must be the `extern "C" fn` type `nvapi.h` gives that function.
unsafe fn function<F: Copy>(query: QueryInterface, id: u32) -> Result<F> {
    // SAFETY: nvapi_QueryInterface takes any id and answers null for one it does not know.
    let pointer = unsafe { query(id) };
    if pointer.is_null() {
        return Err(Error::Nvapi {
            action: "find an NVAPI function",
            status: -3,
        });
    }
    const { assert!(size_of::<F>() == size_of::<*const c_void>()) };
    // SAFETY: the caller guarantees `F` is the function's pointer type, as large as a pointer.
    Ok(unsafe { std::mem::transmute_copy(&pointer) })
}

impl Nvapi {
    /// NVAPI, or `None` without an NVIDIA driver.
    fn load() -> Result<Option<Self>> {
        // SAFETY: a constant wide string; the search is limited to System32, where the driver
        // installs the library, so no other folder can supply it.
        let module =
            unsafe { LoadLibraryExW(w!("nvapi64.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32 as u32) };
        if module.is_null() {
            return Ok(None);
        }
        let library = Library(module);
        // SAFETY: a loaded module and a constant export name.
        let Some(query) = (unsafe { GetProcAddress(library.0, s!("nvapi_QueryInterface")) }) else {
            return Ok(None);
        };
        // SAFETY: nvapi_QueryInterface's signature: a function id in, the function's address out.
        let query: QueryInterface = unsafe { std::mem::transmute(query) };
        // SAFETY: NvAPI_Initialize takes nothing and returns a status (0x0150E828).
        let initialize: NoArgs = unsafe { function(query, 0x0150_E828) }?;
        // SAFETY: the function just found, with its signature; called once per load.
        match unsafe { initialize() } {
            NVIDIA_DEVICE_NOT_FOUND => return Ok(None),
            status => check(status, "start NVAPI")?,
        }
        // SAFETY (each): the type is the one nvapi.h gives the function with this id
        // (nvapi_interface.h).
        Ok(Some(Self {
            // SAFETY: NvAPI_Unload.
            unload: unsafe { function(query, 0xD22B_DD7E) }?,
            // SAFETY: NvAPI_DRS_CreateSession.
            create_session: unsafe { function(query, 0x0694_D52E) }?,
            // SAFETY: NvAPI_DRS_DestroySession.
            destroy_session: unsafe { function(query, 0xDAD9_CFF8) }?,
            // SAFETY: NvAPI_DRS_LoadSettings.
            load_settings: unsafe { function(query, 0x375D_BD6B) }?,
            // SAFETY: NvAPI_DRS_SaveSettings.
            save_settings: unsafe { function(query, 0xFCBC_7E14) }?,
            // SAFETY: NvAPI_DRS_FindApplicationByName.
            find_application: unsafe { function(query, 0xEEE5_66B2) }?,
            // SAFETY: NvAPI_DRS_CreateProfile.
            create_profile: unsafe { function(query, 0xCC17_6068) }?,
            // SAFETY: NvAPI_DRS_CreateApplication.
            create_application: unsafe { function(query, 0x4347_A9DE) }?,
            // SAFETY: NvAPI_DRS_GetProfileInfo.
            profile_info: unsafe { function(query, 0x61CD_6FD6) }?,
            // SAFETY: NvAPI_DRS_GetSetting.
            get_setting: unsafe { function(query, 0x73BF_8338) }?,
            // SAFETY: NvAPI_DRS_SetSetting.
            set_setting: unsafe { function(query, 0x577D_D202) }?,
            // SAFETY: NvAPI_DRS_DeleteProfileSetting.
            delete_setting: unsafe { function(query, 0xE4A2_6362) }?,
            // SAFETY: NvAPI_DRS_DeleteProfile.
            delete_profile: unsafe { function(query, 0x1709_3206) }?,
            _library: library,
        }))
    }

    /// A session with the driver's settings loaded.
    fn session(&self) -> Result<Session<'_>> {
        let mut handle: SessionHandle = std::ptr::null_mut();
        // SAFETY: NVAPI is initialized; `handle` is a valid out-pointer.
        let status = unsafe { (self.create_session)(&mut handle) };
        check(status, "open the driver settings")?;
        let session = Session { api: self, handle };
        // SAFETY: a session just created.
        let status = unsafe { (self.load_settings)(session.handle) };
        check(status, "load the driver settings")?;
        Ok(session)
    }
}

impl Drop for Nvapi {
    fn drop(&mut self) {
        // SAFETY: NVAPI was initialized by `load`, and every session is gone (they borrow `self`).
        let _ = unsafe { (self.unload)() };
    }
}

/// A DRS session, destroyed when dropped.
struct Session<'a> {
    api: &'a Nvapi,
    handle: SessionHandle,
}

impl Drop for Session<'_> {
    fn drop(&mut self) {
        // SAFETY: the session was created by `Nvapi::session` and is destroyed once.
        let _ = unsafe { (self.api.destroy_session)(self.handle) };
    }
}

impl Session<'_> {
    /// The profile that holds `exe`, if any.
    fn find(&self, exe: &str) -> Result<Option<ProfileHandle>> {
        let name = wide(exe);
        let mut profile: ProfileHandle = std::ptr::null_mut();
        let mut application = Application::new("");
        // SAFETY: a live session; `name` is a full NVAPI string; both out-pointers are valid.
        let status = unsafe {
            (self.api.find_application)(self.handle, name.as_ptr(), &mut profile, &mut *application)
        };
        match status {
            EXECUTABLE_NOT_FOUND => Ok(None),
            status => check(status, "find the game's profile").map(|()| Some(profile)),
        }
    }

    fn profile_info(&self, profile: ProfileHandle) -> Result<Box<ProfileInfo>> {
        let mut info = ProfileInfo::new("");
        // SAFETY: a live session, a profile of it, and a valid out-structure with its version.
        let status = unsafe { (self.api.profile_info)(self.handle, profile, &mut *info) };
        check(status, "read the game's profile")?;
        Ok(info)
    }

    fn profile_name(&self, profile: ProfileHandle) -> Result<String> {
        Ok(from_wide(&self.profile_info(profile)?.profile_name))
    }

    fn setting(&self, profile: ProfileHandle, id: u32) -> Result<SettingState> {
        let mut setting = Setting::new(id);
        // SAFETY: a live session, a profile of it, and a valid out-structure with its version.
        let status = unsafe { (self.api.get_setting)(self.handle, profile, id, &mut *setting) };
        if status == SETTING_NOT_FOUND {
            return Ok(SettingState::default());
        }
        check(status, "read a driver setting")?;
        // A DWORD is the union's first 4 bytes.
        let value = setting.current_value[0];
        let user = setting.setting_location == CURRENT_PROFILE_LOCATION
            && setting.is_current_predefined == 0;
        Ok(SettingState {
            value: Some(value),
            user_value: user.then_some(value),
        })
    }

    fn create_profile(&self, name: &str, exe: &str) -> Result<ProfileHandle> {
        let mut profile: ProfileHandle = std::ptr::null_mut();
        // A name in use gets a number: "DXVK Test (Game Manager) 2".
        let mut status = PROFILE_NAME_IN_USE;
        for attempt in 1..10 {
            let name = if attempt == 1 {
                name.to_owned()
            } else {
                format!("{name} {attempt}")
            };
            let mut info = ProfileInfo::new(&name);
            // SAFETY: a live session, a profile description with its version, a valid
            // out-pointer.
            status = unsafe { (self.api.create_profile)(self.handle, &mut *info, &mut profile) };
            if status != PROFILE_NAME_IN_USE {
                break;
            }
        }
        check(status, "create a profile for the game")?;
        let mut application = Application::new(&exe.to_lowercase());
        // SAFETY: a live session, the profile just created, an application with its version.
        let status =
            unsafe { (self.api.create_application)(self.handle, profile, &mut *application) };
        check(status, "add the game's program to its profile")?;
        Ok(profile)
    }

    fn set_dword(&self, profile: ProfileHandle, id: u32, value: u32) -> Result<()> {
        let mut setting = Setting::new(id);
        setting.current_value[0] = value;
        // SAFETY: a live session, a profile of it, and a DWORD setting with its version.
        let status = unsafe { (self.api.set_setting)(self.handle, profile, &mut *setting) };
        check(status, "change a driver setting")
    }

    /// Takes a setting out of the profile; one that is not there is fine.
    fn delete_setting(&self, profile: ProfileHandle, id: u32) -> Result<()> {
        // SAFETY: a live session and a profile of it.
        match unsafe { (self.api.delete_setting)(self.handle, profile, id) } {
            SETTING_NOT_FOUND => Ok(()),
            status => check(status, "restore a driver setting"),
        }
    }

    fn delete_profile(&self, profile: ProfileHandle) -> Result<()> {
        // SAFETY: a live session and a profile of it, not used afterwards.
        let status = unsafe { (self.api.delete_profile)(self.handle, profile) };
        check(status, "delete the game's profile")
    }

    fn save(&self) -> Result<()> {
        // SAFETY: a live session.
        let status = unsafe { (self.api.save_settings)(self.handle) };
        check(status, "save the driver settings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_have_nvapi_sizes() {
        assert_eq!(SETTING_VER, 0x0001_3020);
        assert_eq!(PROFILE_VER, 0x0001_1014);
        assert_eq!(APPLICATION_VER, 0x0004_500C);
        let name = wide("gm-dxvk-test.exe");
        assert_eq!(from_wide(&name), "gm-dxvk-test.exe");
        assert_eq!(from_wide(&wide(&"x".repeat(5000))).len(), 2047);
    }

    #[test]
    fn values_have_names() {
        assert_eq!(method_label(1), "Prefer layered on DXGI Swapchain");
        assert_eq!(method_label(9), "0x00000009");
        assert_eq!(
            flags_label(0x0008_0004),
            "Allow promoting DXVK to DXGI/DirectFlip"
        );
        assert_eq!(flags_label(0), "None");
        assert_eq!(flags_label(0x0000_0003), "0x00000003");
    }

    fn state(profile: Option<&str>, method: SettingState, flags: SettingState) -> ProfileState {
        ProfileState {
            profile: profile.map(str::to_owned),
            method,
            flags,
        }
    }

    const WANTED: NvidiaChoice = NvidiaChoice {
        present_method: Some(1),
        present_flags: Some(0x0008_0004),
    };

    #[test]
    fn a_game_without_a_profile_gets_one_and_loses_it_again() {
        let none = state(None, SettingState::default(), SettingState::default());
        let (plan, original) = plan_for(&none, WANTED, None);
        assert_eq!(
            plan.create_profile.as_deref(),
            Some("DXVK Test (Game Manager)")
        );
        assert_eq!(
            plan.set,
            [(PRESENT_METHOD, 1), (PRESENT_FLAGS, 0x0008_0004)]
        );
        let original = original.unwrap();
        assert!(original.created_profile);

        // Back to original: our values go, then the profile, when nothing else is in it.
        let ours = SettingState {
            value: Some(1),
            user_value: Some(1),
        };
        let now = state(Some("DXVK Test (Game Manager)"), ours, ours);
        let (plan, kept) = plan_for(&now, NvidiaChoice::default(), Some(&original));
        assert_eq!(plan.delete, [PRESENT_METHOD, PRESENT_FLAGS]);
        assert!(plan.delete_profile && plan.set.is_empty());
        assert_eq!(kept, None);
    }

    #[test]
    fn existing_profiles_get_their_values_back() {
        // NVIDIA's own profile with its value for the method; the user had set the flags.
        let nvidia = SettingState {
            value: Some(2),
            user_value: None,
        };
        let users = SettingState {
            value: Some(8),
            user_value: Some(8),
        };
        let before = state(Some("Tomb Raider: Legend"), nvidia, users);
        let (plan, original) = plan_for(&before, WANTED, None);
        assert_eq!(plan.create_profile, None);
        let original = original.unwrap();
        assert_eq!(
            (original.method, original.flags, original.created_profile),
            (None, Some(8), false)
        );

        // Only the method back: the flags stay chosen.
        let changed = state(
            Some("Tomb Raider: Legend"),
            SettingState {
                value: Some(1),
                user_value: Some(1),
            },
            SettingState {
                value: Some(0x0008_0004),
                user_value: Some(0x0008_0004),
            },
        );
        let flags_only = NvidiaChoice {
            present_method: None,
            present_flags: Some(0x0008_0004),
        };
        let (plan, kept) = plan_for(&changed, flags_only, Some(&original));
        assert_eq!(plan.delete, [PRESENT_METHOD]);
        assert_eq!(plan.set, [(PRESENT_FLAGS, 0x0008_0004)]);
        assert_eq!(kept.as_ref(), Some(&original));

        // All back: the user's flags value returns, the profile stays.
        let (plan, kept) = plan_for(&changed, NvidiaChoice::default(), Some(&original));
        assert_eq!(plan.delete, [PRESENT_METHOD]);
        assert_eq!(plan.set, [(PRESENT_FLAGS, 8)]);
        assert!(!plan.delete_profile);
        assert_eq!(kept, None);
    }

    #[test]
    fn nothing_changed_means_nothing_to_do() {
        let before = state(Some("X"), SettingState::default(), SettingState::default());
        let (plan, kept) = plan_for(&before, NvidiaChoice::default(), None);
        assert!(plan.is_empty());
        assert_eq!(kept, None);
    }

    #[test]
    fn plans_travel_as_text() {
        let plan = DrsPlan {
            exe: "gm-dxvk-test.exe".into(),
            create_profile: Some("DXVK Test (Game Manager)".into()),
            set: vec![(PRESENT_METHOD, 1)],
            delete: vec![PRESENT_FLAGS],
            delete_profile: false,
        };
        let encoded = percent_encode(&serde_json::to_string(&plan).unwrap());
        let decoded: DrsPlan = serde_json::from_str(&percent_decode(&encoded).unwrap()).unwrap();
        assert_eq!(decoded, plan);
        assert_eq!(run_encoded("%zz"), ELEVATED_STATUS | 5);
    }

    /// Creates a profile for a made-up program in the real driver settings, sets both values,
    /// reads them back and goes back to original (the profile is deleted). Without administrator
    /// rights the driver may refuse the writes (NVAPI −137): that is the case the UAC path covers.
    #[test]
    #[ignore = "changes the NVIDIA driver settings; run by hand on an NVIDIA PC"]
    fn live_profile_round_trip() {
        let exe = "gm-nvapi-test.exe";
        let name = "Game Manager NVAPI test (Game Manager)";
        let Some(before) = read(exe).unwrap() else {
            return;
        };
        assert_eq!(before.profile, None, "{exe} already has a profile");
        let (first, original) = plan(exe, name, &before, WANTED, None);
        match apply(&first) {
            Err(Error::Nvapi {
                status: INVALID_USER_PRIVILEGE,
                ..
            }) => {
                // Nothing was saved: the session is dropped without DRS_SaveSettings.
                assert_eq!(read(exe).unwrap().unwrap().profile, None);
                eprintln!("NVAPI writes need administrator rights here: run this test elevated");
                return;
            }
            other => other.unwrap(),
        }
        let during = read(exe).unwrap().unwrap();
        assert_eq!(during.profile.as_deref(), Some(name));
        assert_eq!(during.method.user_value, Some(1));
        assert_eq!(during.flags.user_value, Some(0x0008_0004));
        let (back, kept) = plan(
            exe,
            name,
            &during,
            NvidiaChoice::default(),
            original.as_ref(),
        );
        assert_eq!(kept, None);
        apply(&back).unwrap();
        assert_eq!(read(exe).unwrap().unwrap().profile, None);
    }

    fn plan_for(
        state: &ProfileState,
        wanted: NvidiaChoice,
        original: Option<&NvidiaOriginal>,
    ) -> (DrsPlan, Option<NvidiaOriginal>) {
        plan(
            "gm-dxvk-test.exe",
            "DXVK Test (Game Manager)",
            state,
            wanted,
            original,
        )
    }
}
