//! The My PC page's state and messages; the view is `my_pc_page.rs`, the reading and changing
//! `gm_core::pc`.
//!
//! - **Reading**: the page reads a snapshot each time it opens, and on Refresh, in the background
//!   (a few dozen milliseconds). Each display's DDC/CI brightness is read after it, one display at
//!   a time, as monitors answer slowly.
//! - **Changes** (a switch, HDR, a colour profile, brightness) run in the background, one at a
//!   time (`busy`). Then the snapshot is read again, so the page shows what Windows now holds.
//!   HDR waits 1.5 s first: the display takes a moment to switch.
//! - **A new mode** (resolution or refresh rate) is a trial: Windows applies it without saving it
//!   (`pc::try_mode`). Once it has, a dialog asks to keep it and counts down from
//!   `TRIAL_SECONDS`; Keep saves it (`pc::keep_mode`), and no answer, Go back or Escape put the old
//!   mode back. Unsaved, it can't outlive the app or a restart. The countdown is a one-second timer (`Ctx::after`) carrying the trial's token, so a
//!   tick of an earlier trial does nothing.
//! - **GPU scheduling** asks for administrator rights (`gm_core::pc::set_gpu_scheduling`) and
//!   applies after a restart, which the page says.

use crate::shell::ctx::Ctx;
use crate::{GameManager, Message};
use gm_core::Launch;
use gm_core::pc::{self, Display, DisplayMode, Snapshot};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// How long a new display mode stays unless it is kept.
pub(crate) const TRIAL_SECONDS: u32 = 15;

#[derive(Default)]
pub(crate) struct MyPcState {
    /// The last snapshot, `None` until the page first opens.
    pub snapshot: Option<Arc<Snapshot>>,
    pub pending: bool,
    /// Each display's brightness in percent by GDI name, once read (`None`: not available).
    pub brightness: HashMap<String, Option<u32>>,
    /// A change running: its controls wait.
    pub busy: bool,
    /// A new display mode waiting to be kept.
    pub trial: Option<ModeTrial>,
    /// Counts trials, so a countdown tick of an earlier one is ignored.
    pub trials: u64,
}

/// A display mode applied for a trial.
pub(crate) struct ModeTrial {
    pub display: Display,
    pub before: DisplayMode,
    pub after: DisplayMode,
    pub seconds: u32,
    pub token: u64,
}

/// A Windows switch on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Switch {
    GpuScheduling,
    GameMode,
    WindowedOptimizations,
    AutoHdr,
}

#[derive(Clone)]
pub(crate) enum MyPcMessage {
    /// Reads the PC again.
    Refresh,
    Loaded(Arc<Snapshot>),
    /// The displays' brightness, by GDI name (`None`: not available).
    BrightnessRead(Vec<(String, Option<u32>)>),
    /// A resolution or refresh rate picked: the display (GDI name) and the mode.
    ModeChosen(String, DisplayMode),
    /// The trial's mode was applied, or what failed.
    ModeApplied(Result<(), String>),
    /// The trial dialog's Keep and Go back.
    KeepMode,
    RevertMode,
    /// A second of the trial with this token passed.
    Tick(u64),
    /// HDR switched on a display (GDI name).
    Hdr(String, bool),
    /// A colour profile picked for a display (GDI name), by its index in the display's list;
    /// `None` is ignored.
    ColorProfile(String, Option<usize>),
    /// A brightness set on a display (GDI name), in percent.
    Brightness(String, u32),
    BrightnessSet(String, u32, Result<(), String>),
    Switched(Switch, bool),
    /// A change finished: what failed, if anything, and the PC as it now is.
    Changed(Result<(), String>, Arc<Snapshot>),
    /// Opens a page of Windows' Settings (`ms-settings:display`).
    OpenSettings(&'static str),
}

impl From<MyPcMessage> for Message {
    fn from(message: MyPcMessage) -> Self {
        Message::MyPc(message)
    }
}

impl MyPcState {
    /// A display of the last snapshot, by GDI name.
    pub fn display(&self, gdi_name: &str) -> Option<&Display> {
        self.snapshot
            .as_ref()?
            .displays
            .as_ref()
            .ok()?
            .iter()
            .find(|display| display.gdi_name == gdi_name)
    }
}

impl GameManager {
    pub(crate) fn handle_my_pc(&mut self, message: MyPcMessage, context: &Ctx<Message>) {
        let state = &mut self.my_pc;
        match message {
            MyPcMessage::Refresh => {
                if state.pending {
                    return;
                }
                state.pending = true;
                context.spawn_background(|| MyPcMessage::Loaded(Arc::new(pc::read())).into());
            }
            MyPcMessage::Loaded(snapshot) => {
                state.pending = false;
                // Brightness is read again with each full read, one display after the other.
                let displays: Vec<Display> = snapshot
                    .displays
                    .as_ref()
                    .map(|displays| displays.clone())
                    .unwrap_or_default();
                state.brightness.clear();
                state.snapshot = Some(snapshot);
                context.spawn_background(move || {
                    MyPcMessage::BrightnessRead(
                        displays
                            .iter()
                            .filter(|display| !display.gdi_name.is_empty())
                            .map(|display| (display.gdi_name.clone(), pc::brightness(display)))
                            .collect(),
                    )
                    .into()
                });
            }
            MyPcMessage::BrightnessRead(read) => state.brightness.extend(read),
            MyPcMessage::ModeChosen(name, after) => {
                let Some(display) = state.display(&name).cloned() else {
                    return;
                };
                let Some(before) = display.current else {
                    return;
                };
                if before == after || state.busy || state.trial.is_some() {
                    return;
                }
                state.busy = true;
                state.trials += 1;
                state.trial = Some(ModeTrial {
                    display: display.clone(),
                    before,
                    after,
                    seconds: TRIAL_SECONDS,
                    token: state.trials,
                });
                context.spawn_background(move || {
                    MyPcMessage::ModeApplied(
                        pc::try_mode(&display, after).map_err(|e| e.to_string()),
                    )
                    .into()
                });
            }
            MyPcMessage::ModeApplied(Ok(())) => {
                state.busy = false;
                match &state.trial {
                    Some(trial) => tick(trial.token, context),
                    // Kept before Windows had finished: show the new mode.
                    None => self.handle_my_pc(MyPcMessage::Refresh, context),
                }
            }
            MyPcMessage::ModeApplied(Err(error)) => {
                state.busy = false;
                state.trial = None;
                self.error = Some(error);
                self.handle_my_pc(MyPcMessage::Refresh, context);
            }
            MyPcMessage::Tick(token) => {
                let Some(trial) = state.trial.as_mut().filter(|trial| trial.token == token) else {
                    return;
                };
                trial.seconds = trial.seconds.saturating_sub(1);
                if trial.seconds == 0 {
                    self.handle_my_pc(MyPcMessage::RevertMode, context);
                } else {
                    tick(token, context);
                }
            }
            MyPcMessage::KeepMode => {
                // Not while the new mode is still being applied (Keep is disabled then).
                if state.busy {
                    return;
                }
                let Some(trial) = state.trial.take() else {
                    return;
                };
                change(state, context, Duration::ZERO, move || {
                    pc::keep_mode(&trial.display, trial.after)
                });
            }
            MyPcMessage::RevertMode => {
                // Not while the new mode is still being applied: the countdown starts once it is,
                // and Go back works from then on.
                if state.busy {
                    return;
                }
                let Some(trial) = state.trial.take() else {
                    return;
                };
                change(state, context, Duration::ZERO, move || {
                    pc::try_mode(&trial.display, trial.before)
                });
            }
            MyPcMessage::Hdr(name, on) => {
                let Some(display) = state.display(&name).cloned() else {
                    return;
                };
                change(state, context, Duration::from_millis(1500), move || {
                    pc::set_hdr(&display, on)
                });
            }
            MyPcMessage::ColorProfile(name, Some(index)) => {
                let Some(display) = state.display(&name).cloned() else {
                    return;
                };
                let Some(profile) = display.color_profiles.get(index).cloned() else {
                    return;
                };
                if display.color_profile.as_deref() == Some(profile.as_str()) {
                    return;
                }
                change(state, context, Duration::ZERO, move || {
                    pc::set_color_profile(&display, &profile)
                });
            }
            MyPcMessage::ColorProfile(_, None) => {}
            MyPcMessage::Brightness(name, percent) => {
                let Some(display) = state.display(&name).cloned() else {
                    return;
                };
                // Shown at once; set back if the monitor refuses.
                let before = state
                    .brightness
                    .insert(name.clone(), Some(percent))
                    .flatten();
                context.spawn_background(move || {
                    let set = pc::set_brightness(&display, percent).map_err(|e| e.to_string());
                    MyPcMessage::BrightnessSet(name, before.unwrap_or(percent), set).into()
                });
            }
            MyPcMessage::BrightnessSet(name, before, Err(error)) => {
                state.brightness.insert(name, Some(before));
                self.error = Some(error);
            }
            MyPcMessage::BrightnessSet(_, _, Ok(())) => {}
            MyPcMessage::Switched(switch, on) => {
                change(state, context, Duration::ZERO, move || match switch {
                    Switch::GpuScheduling => pc::set_gpu_scheduling(on),
                    Switch::GameMode => pc::set_game_mode(on),
                    Switch::WindowedOptimizations => pc::set_windowed_optimizations(on),
                    Switch::AutoHdr => pc::set_auto_hdr(on),
                });
            }
            MyPcMessage::Changed(result, snapshot) => {
                state.busy = false;
                state.snapshot = Some(snapshot);
                if let Err(error) = result {
                    self.error = Some(error);
                }
            }
            MyPcMessage::OpenSettings(uri) => {
                self.error = gm_core::launch(&Launch::Uri(uri.to_owned()))
                    .err()
                    .map(|e| e.to_string());
            }
        }
    }
}

/// Runs a change in the background, then reads the PC again after `settle`.
fn change(
    state: &mut MyPcState,
    context: &Ctx<Message>,
    settle: Duration,
    work: impl FnOnce() -> gm_core::Result<()> + Send + 'static,
) {
    if state.busy {
        return;
    }
    state.busy = true;
    context.spawn_background(move || {
        let result = work().map_err(|e| e.to_string());
        std::thread::sleep(settle);
        MyPcMessage::Changed(result, Arc::new(pc::read())).into()
    });
}

/// One second of a trial.
fn tick(token: u64, context: &Ctx<Message>) {
    context.after(Duration::from_secs(1), MyPcMessage::Tick(token).into());
}

/// A display's resolutions, largest first, each once.
pub(crate) fn sizes(modes: &[DisplayMode]) -> Vec<(u32, u32)> {
    let mut sizes: Vec<(u32, u32)> = Vec::new();
    for mode in modes {
        if !sizes.contains(&(mode.width, mode.height)) {
            sizes.push((mode.width, mode.height));
        }
    }
    sizes
}

/// The refresh rates of one resolution, fastest first.
pub(crate) fn rates(modes: &[DisplayMode], size: (u32, u32)) -> Vec<u32> {
    let mut rates: Vec<u32> = modes
        .iter()
        .filter(|mode| (mode.width, mode.height) == size)
        .map(|mode| mode.hz)
        .collect();
    rates.sort_unstable_by(|a, b| b.cmp(a));
    rates.dedup();
    rates
}

/// The mode a newly picked resolution gets: the current refresh rate when the display has it at
/// that size, else the fastest.
pub(crate) fn mode_for_size(
    modes: &[DisplayMode],
    size: (u32, u32),
    current_hz: u32,
) -> Option<DisplayMode> {
    let rates = rates(modes, size);
    let hz = if rates.contains(&current_hz) {
        current_hz
    } else {
        *rates.first()?
    };
    Some(DisplayMode {
        width: size.0,
        height: size.1,
        hz,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, hz: u32) -> DisplayMode {
        DisplayMode { width, height, hz }
    }

    fn modes() -> Vec<DisplayMode> {
        vec![
            mode(3840, 2160, 120),
            mode(3840, 2160, 60),
            mode(2560, 1440, 120),
            mode(1920, 1080, 120),
            mode(1920, 1080, 60),
        ]
    }

    #[test]
    fn sizes_and_rates() {
        assert_eq!(
            sizes(&modes()),
            vec![(3840, 2160), (2560, 1440), (1920, 1080)]
        );
        assert_eq!(rates(&modes(), (1920, 1080)), vec![120, 60]);
        assert!(rates(&modes(), (800, 600)).is_empty());
    }

    #[test]
    fn a_new_resolution_keeps_the_rate_it_can() {
        assert_eq!(
            mode_for_size(&modes(), (1920, 1080), 60),
            Some(mode(1920, 1080, 60))
        );
        // 2560 × 1440 has no 60 Hz here: its fastest.
        assert_eq!(
            mode_for_size(&modes(), (2560, 1440), 60),
            Some(mode(2560, 1440, 120))
        );
        assert_eq!(mode_for_size(&modes(), (800, 600), 60), None);
    }
}
