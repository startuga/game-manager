//! My PC: what this computer is (graphics cards, processor, memory, Windows, board, drives), its
//! displays, and the Windows switches that matter for games. Everything comes from the registry
//! and Win32, with no WMI; reading needs no rights.
//!
//! - `hardware`: processor, graphics cards, memory, Windows and the board, drives.
//! - `smbios`: memory modules from the firmware's SMBIOS table.
//! - `displays`: the active displays with their modes, HDR state, colour profiles and DDC/CI
//!   brightness, and the changes the My PC page makes to them.
//! - `switches`: GPU scheduling, Game Mode, optimizations for windowed games and Auto HDR.
//!
//! `read` takes a snapshot in a few milliseconds; call it off the UI thread all the same, as a
//! drive or display can be slow to answer. Each part is read on its own, so one that fails never
//! hides the others. Brightness is not part of it: DDC/CI is slower still, so the page asks for it
//! after the snapshot.

mod displays;
mod hardware;
mod smbios;
mod switches;

pub use displays::{
    Display, DisplayMode, brightness, set_brightness, set_color_profile, set_hdr, set_mode,
};
pub use hardware::{Cpu, Drive, Gpu, GpuVendor, Memory, System};
pub use smbios::MemoryModule;
pub use switches::{
    GPU_SCHEDULING_ARG, Switches, run_encoded, set_auto_hdr, set_game_mode, set_gpu_scheduling,
    set_windowed_optimizations,
};

use crate::Result;

/// Everything the My PC page shows, each part read on its own.
#[derive(Debug)]
pub struct Snapshot {
    /// The computer's name.
    pub machine: String,
    pub cpu: Result<Cpu>,
    /// Largest video memory first.
    pub gpus: Result<Vec<Gpu>>,
    pub memory: Result<Memory>,
    pub system: Result<System>,
    pub drives: Result<Vec<Drive>>,
    /// The main display first.
    pub displays: Result<Vec<Display>>,
    pub switches: Switches,
}

/// Reads the PC.
pub fn read() -> Snapshot {
    Snapshot {
        machine: std::env::var("COMPUTERNAME").unwrap_or_default(),
        cpu: hardware::cpu(),
        gpus: hardware::gpus(),
        memory: hardware::memory(),
        system: hardware::system(),
        drives: hardware::drives(),
        displays: displays::displays(),
        switches: switches::read(),
    }
}

/// A size as Windows writes it: binary units, one decimal from GB up ("24 GB", "1.8 TB").
pub fn format_bytes(bytes: u64) -> String {
    const UNIT: f64 = 1024.0;
    let size = bytes as f64;
    let one_decimal = |value: f64| {
        let text = format!("{value:.1}");
        text.strip_suffix(".0").map_or(text.clone(), str::to_owned)
    };
    if size >= UNIT.powi(4) {
        format!("{} TB", one_decimal(size / UNIT.powi(4)))
    } else if size >= UNIT.powi(3) {
        format!("{} GB", one_decimal(size / UNIT.powi(3)))
    } else if size >= UNIT.powi(2) {
        format!("{:.0} MB", size / UNIT.powi(2))
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_bytes(25_757_220_864), "24 GB");
        assert_eq!(format_bytes(34_271_457_280), "31.9 GB");
        assert_eq!(format_bytes(2_000_398_934_016), "1.8 TB");
        assert_eq!(format_bytes(512 << 20), "512 MB");
        assert_eq!(format_bytes(100), "100 B");
    }
}
