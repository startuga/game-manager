//! The games on disk: each one's size (`gm_core::sizes`) and new installs noticed
//! (`gm_core::watch`).
//!
//! - **Sizes**: the last ones known are read at start; after every scan the games measured over a
//!   day ago (or never) are measured again in the background, one at a time
//!   (`measure_sizes`, one run at a time). Details shows Size on disk, and Sort and filters has
//!   Largest first (`library::Facts::sizes`). A size arriving rebuilds the view only when it
//!   shows (that sort, or the game's page).
//! - **New installs**: a thread of its own (`Ctx::spawn_thread`) waits on the places the stores
//!   list their games (`watch::places`, gathered again every `RECHECK`). A change waits for
//!   `QUIET` without another (a store writes many times while installing), then the app scans,
//!   never while it scans already and at most once every `MIN_GAP` (a Steam download rewrites its
//!   manifest all along). Such a scan keeps the error bar as it was unless it fails itself, and
//!   when its games are the same as before, nothing is loaded again.

use crate::shell::ctx::Ctx;
use crate::{GameManager, GameTab, Message, Page};
use gm_core::config::Sort;
use gm_core::{sizes, watch};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How long the stores must stay quiet after a change before the app scans.
const QUIET: Duration = Duration::from_secs(10);
/// The least time between a scan and one the stores' changes start.
const MIN_GAP: Duration = Duration::from_secs(60);
/// How often the watcher gathers the places again (a library added).
const RECHECK: Duration = Duration::from_secs(10 * 60);
/// How long the watcher rests after a change before it watches again.
const SETTLE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct InstallsState {
    measuring: bool,
    measure_again: bool,
    /// Counts the stores' changes, so the wait of an earlier one does nothing.
    changes: u64,
    /// When the last scan started.
    pub last_scan: Option<Instant>,
    /// The scan running was started by the stores' changes.
    pub auto_scan: bool,
}

#[derive(Clone)]
pub(crate) enum InstallsMessage {
    /// The sizes known from before, read at start.
    SizesLoaded(HashMap<String, u64>),
    SizeMeasured(String, u64),
    SizesDone,
    /// A store's list of games changed.
    StoresChanged,
    /// The wait after the change with this count is over.
    StoresQuiet(u64),
}

impl From<InstallsMessage> for Message {
    fn from(message: InstallsMessage) -> Self {
        Message::Installs(message)
    }
}

/// How long to wait before the scan the stores' changes ask for: `None` to scan now.
fn rescan_wait(scanning: bool, since_last_scan: Option<Duration>) -> Option<Duration> {
    if scanning {
        return Some(QUIET);
    }
    match since_last_scan {
        Some(since) if since < MIN_GAP => Some(MIN_GAP - since),
        _ => None,
    }
}

impl GameManager {
    /// At start: the sizes known from before, and the stores' watcher.
    pub(crate) fn start_installs(context: &Ctx<Message>) {
        context.spawn_background(|| InstallsMessage::SizesLoaded(sizes::cached()).into());
        context.spawn_thread("store watcher", |send| {
            loop {
                let places = watch::places();
                if watch::wait(&places, RECHECK) == watch::Woke::Changed {
                    if !send(InstallsMessage::StoresChanged.into()) {
                        return;
                    }
                    std::thread::sleep(SETTLE);
                }
            }
        });
    }

    /// Measures the games whose size is old or unknown, in the background, one run at a time.
    pub(crate) fn measure_sizes(&mut self, context: &Ctx<Message>) {
        let state = &mut self.installs;
        if state.measuring {
            state.measure_again = true;
            return;
        }
        state.measuring = true;
        let games = self.games.clone();
        context.spawn_stream(move |send| {
            for game in sizes::stale(&games) {
                if let Some(bytes) = sizes::measure(&game) {
                    send(InstallsMessage::SizeMeasured(game.id, bytes).into());
                }
            }
            send(InstallsMessage::SizesDone.into());
        });
    }

    pub(crate) fn handle_installs(&mut self, message: InstallsMessage, context: &Ctx<Message>) {
        match message {
            InstallsMessage::SizesLoaded(known) => {
                // Sizes measured meanwhile are newer.
                for (id, bytes) in known {
                    self.sizes.entry(id).or_insert(bytes);
                }
            }
            InstallsMessage::SizeMeasured(id, bytes) => {
                let shown = self.view.sort == Sort::Size
                    || (self.page == Page::Game
                        && self.tab == GameTab::Overview
                        && self.selected.as_deref() == Some(id.as_str()));
                self.sizes.insert(id, bytes);
                if !shown {
                    context.quiet();
                }
            }
            InstallsMessage::SizesDone => {
                context.quiet();
                self.installs.measuring = false;
                if std::mem::take(&mut self.installs.measure_again) {
                    self.measure_sizes(context);
                }
            }
            InstallsMessage::StoresChanged => {
                context.quiet();
                self.installs.changes += 1;
                context.after(
                    QUIET,
                    InstallsMessage::StoresQuiet(self.installs.changes).into(),
                );
            }
            InstallsMessage::StoresQuiet(count) => {
                context.quiet();
                if count != self.installs.changes {
                    return;
                }
                let since = self.installs.last_scan.map(|at| at.elapsed());
                match rescan_wait(self.scanning, since) {
                    Some(wait) => context.after(wait, InstallsMessage::StoresQuiet(count).into()),
                    None => {
                        self.installs.auto_scan = true;
                        self.start_scan(context);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stores_changes_wait_for_a_scan_running_and_a_minute_since_the_last() {
        assert_eq!(rescan_wait(false, None), None);
        assert_eq!(rescan_wait(true, None), Some(QUIET));
        assert_eq!(
            rescan_wait(false, Some(Duration::from_secs(20))),
            Some(Duration::from_secs(40))
        );
        assert_eq!(rescan_wait(false, Some(MIN_GAP)), None);
        assert_eq!(rescan_wait(true, Some(Duration::from_secs(5))), Some(QUIET));
    }
}
