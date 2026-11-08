//! Windows' efficiency mode for Game Manager's own process while nobody works with it: EcoQoS
//! (its threads run at the most power-efficient speed) and a below-normal priority, the green leaf
//! in Task Manager. Set when another app comes to the front, cleared when the window is in front
//! again, so a game started from it gets the CPU. A minimized window also hands back the memory
//! pages nobody touches.

use windows::Win32::{
    BELOW_NORMAL_PRIORITY_CLASS, GetCurrentProcess, NORMAL_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, SetPriorityClass,
    SetProcessInformation, SetProcessWorkingSetSizeEx,
};

/// Turns efficiency mode on or off. Failures are ignored: it is only a hint to Windows.
pub fn set(on: bool) {
    let speed = PROCESS_POWER_THROTTLING_EXECUTION_SPEED as u32;
    let mut state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION as u32,
        // The process manages its execution speed itself; throttled (EcoQoS) when on.
        ControlMask: speed,
        StateMask: if on { speed } else { 0 },
    };
    // SAFETY: the current-process pseudo handle needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: `state` is a valid PROCESS_POWER_THROTTLING_STATE of the size given.
    let _ = unsafe {
        SetProcessInformation(
            process,
            ProcessPowerThrottling,
            (&raw mut state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    let priority = if on {
        BELOW_NORMAL_PRIORITY_CLASS
    } else {
        NORMAL_PRIORITY_CLASS
    };
    // SAFETY: a priority class for our own process.
    let _ = unsafe { SetPriorityClass(process, priority as u32) };
}

/// The window was minimized: pages nobody touches go back to Windows, and come back as needed.
pub fn trim() {
    // SAFETY: the current-process pseudo handle needs no closing.
    let process = unsafe { GetCurrentProcess() };
    // SAFETY: (-1, -1) asks Windows to trim the working set; no flags.
    let _ = unsafe { SetProcessWorkingSetSizeEx(process, usize::MAX, usize::MAX, 0) };
}
