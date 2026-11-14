//! A program or DLL mapped as data, to read its resources without running any of its code: the
//! EA scanner's Game Definition File (`ea.rs`) and programs' icons (`icon.rs`).
//!
//! `LoadLibraryExW` with `LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE` only maps the
//! file; resources are read in place (`resource`) and listed in the file's own order (`names`).
//! The module is freed when the value is dropped, and the resources' bytes borrow it, so none
//! outlives it. A resource's size is checked against what Windows mapped (`VirtualQuery`), so a
//! damaged or crafted program that states more can't make the app read past its mapping.

use std::path::Path;
use windows::Win32::{
    FindResourceW, FreeLibrary, HMODULE, LOAD_LIBRARY_AS_DATAFILE, LOAD_LIBRARY_AS_IMAGE_RESOURCE,
    LONG_PTR, LoadLibraryExW, LoadResource, LockResource, MEM_COMMIT, MEMORY_BASIC_INFORMATION,
    PAGE_GUARD, PAGE_NOACCESS, SizeofResource, VirtualQuery,
};
use windows::core::{BOOL, HSTRING, PCWSTR};

/// A resource's name: a number, or a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ResourceName {
    Id(u16),
    /// UTF-16, null-terminated.
    Text(Vec<u16>),
}

impl ResourceName {
    /// As Windows' resource functions take it: a number in a pointer (`MAKEINTRESOURCE`), or the
    /// string. Valid while `self` is.
    pub fn as_pcwstr(&self) -> PCWSTR {
        match self {
            ResourceName::Id(id) => int_resource(*id),
            ResourceName::Text(text) => PCWSTR(text.as_ptr()),
        }
    }
}

/// A resource number or type as Windows' resource functions take it (`MAKEINTRESOURCE`).
pub(crate) fn int_resource(id: u16) -> PCWSTR {
    PCWSTR(std::ptr::without_provenance(usize::from(id)))
}

/// A file mapped as data only, so none of its code runs; freed when dropped.
pub(crate) struct DataModule(HMODULE);

impl DataModule {
    pub fn load(file: &Path) -> Option<Self> {
        let path = HSTRING::from(file.as_os_str());
        // SAFETY: `path` is a valid wide string. Loaded as a data file and image resource, the
        // module is only mapped: none of its code executes.
        let module = unsafe {
            LoadLibraryExW(
                &path,
                None,
                (LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE) as u32,
            )
        };
        (!module.is_null()).then_some(Self(module))
    }

    /// A resource's bytes. They stay mapped while the module is loaded, which the borrow of
    /// `self` guarantees.
    pub fn resource(&self, name: PCWSTR, kind: PCWSTR) -> Option<&[u8]> {
        // SAFETY: the module is loaded while `self` lives; `name` and `kind` are resource names.
        let resource = unsafe { FindResourceW(Some(self.0), name, kind) };
        if resource.is_null() {
            return None;
        }
        // SAFETY: `resource` was just found in this module.
        let size = unsafe { SizeofResource(Some(self.0), resource) } as usize;
        // SAFETY: as above.
        let loaded = unsafe { LoadResource(Some(self.0), resource) };
        if loaded.is_null() || size == 0 {
            return None;
        }
        // SAFETY: `loaded` is the resource just loaded from this module.
        let data = unsafe { LockResource(loaded) };
        if data.is_null() {
            return None;
        }
        // A damaged or crafted file can state a size past its mapping: read only what is there.
        if !readable(data.cast_const(), size) {
            return None;
        }
        // SAFETY: LockResource points to the resource's `size` bytes, mapped read-only while the
        // module is loaded (checked just above); the returned slice borrows `self`, so it cannot
        // outlive the module.
        Some(unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) })
    }

    /// The names of the resources of type `kind`, in the file's order (named ones first, then
    /// numbers, ascending), which is the order Windows counts icons by index in.
    pub fn names(&self, kind: PCWSTR) -> Vec<ResourceName> {
        let mut names: Vec<ResourceName> = Vec::new();
        // SAFETY: the module is loaded; the callback only runs during the call, and `lparam`
        // points to `names`, alive and not otherwise used meanwhile.
        let _ = unsafe {
            windows::Win32::EnumResourceNamesW(
                Some(self.0),
                kind,
                Some(collect_name),
                std::ptr::from_mut(&mut names) as LONG_PTR,
            )
        };
        names
    }
}

/// Whether the `size` bytes at `data` are all mapped and readable, in one mapping: what a resource
/// claims is checked against what Windows mapped, page range by page range (`VirtualQuery`).
fn readable(data: *const std::ffi::c_void, size: usize) -> bool {
    let Some(end) = (data as usize).checked_add(size) else {
        return false;
    };
    let mut at = data as usize;
    let mut mapping = None;
    while at < end {
        let mut region = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: a plain query of this process's memory at `at`; `region` is a valid buffer of
        // the size given.
        let written = unsafe {
            VirtualQuery(
                Some(std::ptr::without_provenance(at)),
                &mut region,
                size_of::<MEMORY_BASIC_INFORMATION>(),
            )
        };
        let protect = region.Protect;
        let ok = written != 0
            && region.State == MEM_COMMIT as u32
            && protect & (PAGE_NOACCESS | PAGE_GUARD) as u32 == 0
            && *mapping.get_or_insert(region.AllocationBase) == region.AllocationBase;
        if !ok || region.RegionSize == 0 {
            return false;
        }
        at = (region.BaseAddress as usize).saturating_add(region.RegionSize);
    }
    true
}

/// `EnumResourceNamesW`'s callback: copies each name into the `Vec` `lparam` points to.
unsafe extern "system" fn collect_name(
    _module: HMODULE,
    _kind: PCWSTR,
    name: PCWSTR,
    lparam: LONG_PTR,
) -> BOOL {
    // SAFETY: `lparam` is the `Vec` `names` passed, alive and borrowed by no one else during the
    // enumeration.
    let names = unsafe { &mut *(lparam as *mut Vec<ResourceName>) };
    let address = name.0 as usize;
    if address <= usize::from(u16::MAX) {
        names.push(ResourceName::Id(address as u16));
    } else {
        // SAFETY: a name that isn't a number is a null-terminated string, valid during the
        // callback; it is copied here.
        let text = unsafe { name.as_wide() };
        names.push(ResourceName::Text(
            text.iter().copied().chain([0]).collect(),
        ));
    }
    true.into()
}

impl Drop for DataModule {
    fn drop(&mut self) {
        // SAFETY: loaded by `load`, freed once, here; no slice from `resource` outlives `self`.
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mapped_bytes_are_read() {
        let bytes = vec![7u8; 4096];
        let data = bytes.as_ptr().cast::<std::ffi::c_void>();
        assert!(readable(data, bytes.len()));
        // Past what is mapped there, at no address, or past the end of memory.
        assert!(!readable(data, 1 << 40));
        assert!(!readable(std::ptr::null(), 16));
        assert!(!readable(data, usize::MAX));
        // A program's resource read through the check: Windows' own notepad has icons.
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        let module = DataModule::load(&system.join(r"System32\notepad.exe")).unwrap();
        let groups = module.names(int_resource(14));
        let first = groups.first().unwrap();
        assert!(
            module
                .resource(first.as_pcwstr(), int_resource(14))
                .is_some()
        );
    }
}
