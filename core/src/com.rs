//! COM setup for the threads that use the Shell's or WIC's COM objects (icons, shortcuts).

use std::marker::PhantomData;
use windows::Win32::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

/// Initializes COM for the current thread and balances it on drop. Not `Send`, so it is always
/// dropped on the thread that created it.
pub(crate) struct ComGuard {
    initialized: bool,
    _not_send: PhantomData<*const ()>,
}

impl ComGuard {
    pub(crate) fn new() -> Self {
        // SAFETY: plain COM initialization. If the thread already uses another apartment model
        // (RPC_E_CHANGED_MODE) COM is still usable, but must not be uninitialized by us.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED as u32) };
        Self {
            initialized: hr.is_ok(),
            _not_send: PhantomData,
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: balances the successful CoInitializeEx in `new`; the guard is not `Send`,
            // so this runs on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}
