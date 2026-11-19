//! A game's display settings while it plays: the display, HDR and mode chosen in its settings
//! (`config::DisplayChoice`), applied when Play starts the game and put back when it ends.
//!
//! - **Finding the display** (`find_display`): by its id (`pc::Display::id`, the monitor on its
//!   connector), else by its name when one connected display has it (the monitor moved to another
//!   port); else it isn't connected (`Error::DisplayNotConnected`), and the game starts as it is.
//! - **Applying** (`apply`): only what differs from what the display shows (`plan_apply`). HDR
//!   first, then the mode, tried unsaved (`pc::try_mode`), so the mode Windows keeps stays the
//!   user's own and a restart brings it back whatever happens. A mode that fails puts HDR back.
//!   What changed comes back as a `DisplayRestore`, which the app keeps in the play history
//!   (`History::displays`) until it is put back: a Game Manager that ended meanwhile puts it back
//!   at its next start. HDR is saved by Windows as soon as it is set, so only that start puts it
//!   back.
//! - **Putting back** (`restore`): the mode, then HDR, each only while the display still shows
//!   what was set (`plan_restore`), so a change the user made meanwhile stays.

use crate::config::DisplayChoice;
use crate::pc::{self, Display, DisplayMode};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long a display takes to switch HDR before its mode is changed.
const HDR_SETTLE: Duration = Duration::from_millis(1500);

/// What was changed on a display for a game, to put back when it ends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayRestore {
    /// The display's id and name, as `DisplayChoice` keeps them.
    pub display: String,
    pub name: String,
    /// HDR as it was, when it was switched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hdr_before: Option<bool>,
    /// The mode as it was and the one set, when it was changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ModeChange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeChange {
    pub before: DisplayMode,
    pub set: DisplayMode,
}

/// What a display shows now, as the plans need it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    pub hdr_supported: bool,
    pub hdr_on: bool,
    /// `None` when Windows wouldn't say: then the mode is never changed, as it couldn't be put
    /// back.
    pub mode: Option<DisplayMode>,
}

impl Shown {
    pub fn of(display: &Display) -> Self {
        Self {
            hdr_supported: display.hdr_supported,
            hdr_on: display.hdr_on,
            mode: display.current,
        }
    }
}

/// Changes to make to a display: HDR on or off, and a mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    pub hdr: Option<bool>,
    pub mode: Option<DisplayMode>,
}

/// What applying `choice` changes: only what differs from what the display shows. HDR on is left
/// out where the display has none.
pub fn plan_apply(choice: &DisplayChoice, shown: &Shown) -> Changes {
    Changes {
        hdr: choice
            .hdr
            .filter(|&on| on != shown.hdr_on && (!on || shown.hdr_supported)),
        mode: choice
            .mode
            .filter(|&mode| shown.mode.is_some_and(|now| now != mode)),
    }
}

/// What putting `restore` back changes: each part only while the display still shows what was
/// set.
pub fn plan_restore(restore: &DisplayRestore, shown: &Shown) -> Changes {
    Changes {
        hdr: restore.hdr_before.filter(|&before| shown.hdr_on != before),
        mode: restore
            .mode
            .filter(|change| shown.mode == Some(change.set))
            .map(|change| change.before),
    }
}

/// The display a choice names: by id, else the one connected display with its name.
pub fn find_display<'a>(displays: &'a [Display], id: &str, name: &str) -> Option<&'a Display> {
    let index = pick(
        displays
            .iter()
            .map(|display| (display.id.as_str(), display.name.as_str())),
        id,
        name,
    )?;
    displays.get(index)
}

/// `find_display` over (id, name) pairs: the index found.
pub fn pick<'a>(
    displays: impl Iterator<Item = (&'a str, &'a str)>,
    id: &str,
    name: &str,
) -> Option<usize> {
    let displays: Vec<(&str, &str)> = displays.collect();
    if let Some(index) = displays
        .iter()
        .position(|&(other, _)| !id.is_empty() && other == id)
    {
        return Some(index);
    }
    let mut named = displays
        .iter()
        .enumerate()
        .filter(|(_, (_, other))| *other == name);
    let (index, _) = named.next()?;
    named.next().is_none().then_some(index)
}

/// Applies a game's display choice: `None` when the display already shows it. Blocking (seconds,
/// with HDR).
pub fn apply(choice: &DisplayChoice) -> Result<Option<DisplayRestore>> {
    let displays = pc::displays()?;
    let display = find_display(&displays, &choice.display, &choice.name)
        .ok_or_else(|| Error::DisplayNotConnected(choice.name.clone()))?;
    let shown = Shown::of(display);
    let changes = plan_apply(choice, &shown);
    if changes == Changes::default() {
        return Ok(None);
    }
    let mut restore = DisplayRestore {
        display: choice.display.clone(),
        name: choice.name.clone(),
        hdr_before: None,
        mode: None,
    };
    if let Some(on) = changes.hdr {
        pc::set_hdr(display, on)?;
        restore.hdr_before = Some(shown.hdr_on);
    }
    if let (Some(mode), Some(before)) = (changes.mode, shown.mode) {
        if changes.hdr.is_some() {
            std::thread::sleep(HDR_SETTLE);
        }
        if let Err(error) = pc::try_mode(display, mode) {
            if changes.hdr.is_some() {
                let _ = pc::set_hdr(display, shown.hdr_on);
            }
            return Err(error);
        }
        restore.mode = Some(ModeChange { before, set: mode });
    }
    Ok(Some(restore))
}

/// Puts back what `apply` changed, where the display still shows it. Both parts are tried; the
/// first failure is returned. Blocking.
pub fn restore(restore: &DisplayRestore) -> Result<()> {
    let displays = pc::displays()?;
    let display = find_display(&displays, &restore.display, &restore.name)
        .ok_or_else(|| Error::DisplayNotConnected(restore.name.clone()))?;
    let changes = plan_restore(restore, &Shown::of(display));
    let mode = changes.mode.map(|mode| pc::try_mode(display, mode));
    let hdr = changes.hdr.map(|on| {
        if mode.is_some() {
            std::thread::sleep(HDR_SETTLE);
        }
        pc::set_hdr(display, on)
    });
    mode.unwrap_or(Ok(())).and(hdr.unwrap_or(Ok(())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(width: u32, height: u32, hz: u32) -> DisplayMode {
        DisplayMode { width, height, hz }
    }

    fn shown(hdr_on: bool, now: DisplayMode) -> Shown {
        Shown {
            hdr_supported: true,
            hdr_on,
            mode: Some(now),
        }
    }

    fn choice(hdr: Option<bool>, mode: Option<DisplayMode>) -> DisplayChoice {
        DisplayChoice {
            display: "id".into(),
            name: "TV".into(),
            hdr,
            mode,
        }
    }

    #[test]
    fn applying_changes_only_what_differs() {
        let uhd = mode(3840, 2160, 120);
        let hd = mode(1920, 1080, 60);
        assert_eq!(
            plan_apply(&choice(Some(true), Some(uhd)), &shown(false, hd)),
            Changes {
                hdr: Some(true),
                mode: Some(uhd),
            }
        );
        assert_eq!(
            plan_apply(&choice(Some(true), Some(uhd)), &shown(true, uhd)),
            Changes::default()
        );
        // Left as it is.
        assert_eq!(
            plan_apply(&choice(None, None), &shown(false, hd)),
            Changes::default()
        );
        // HDR off always works; on only where the display has it.
        assert_eq!(
            plan_apply(&choice(Some(false), None), &shown(true, hd)).hdr,
            Some(false)
        );
        let no_hdr = Shown {
            hdr_supported: false,
            ..shown(false, hd)
        };
        assert_eq!(
            plan_apply(&choice(Some(true), None), &no_hdr),
            Changes::default()
        );
        // A mode Windows didn't report isn't changed: it couldn't be put back.
        let unknown = Shown {
            mode: None,
            ..shown(false, hd)
        };
        assert_eq!(plan_apply(&choice(None, Some(uhd)), &unknown).mode, None);
    }

    #[test]
    fn putting_back_only_what_still_shows() {
        let uhd = mode(3840, 2160, 120);
        let hd = mode(1920, 1080, 60);
        let restore = DisplayRestore {
            display: "id".into(),
            name: "TV".into(),
            hdr_before: Some(false),
            mode: Some(ModeChange {
                before: hd,
                set: uhd,
            }),
        };
        assert_eq!(
            plan_restore(&restore, &shown(true, uhd)),
            Changes {
                hdr: Some(false),
                mode: Some(hd),
            }
        );
        // The user changed both meanwhile: nothing goes back.
        assert_eq!(
            plan_restore(&restore, &shown(false, mode(2560, 1440, 144))),
            Changes::default()
        );
        // Only HDR was switched.
        let hdr_only = DisplayRestore {
            mode: None,
            ..restore.clone()
        };
        assert_eq!(
            plan_restore(&hdr_only, &shown(true, uhd)),
            Changes {
                hdr: Some(false),
                mode: None,
            }
        );
    }

    #[test]
    fn displays_are_found_by_id_then_by_a_name_only_one_has() {
        let connected = [("a", "LG TV"), ("b", "Dell"), ("c", "Dell")];
        let find = |id: &str, name: &str| pick(connected.iter().copied(), id, name);
        assert_eq!(find("b", "Dell"), Some(1));
        // Moved to another port: its id is gone, its name is still one display's.
        assert_eq!(find("gone", "LG TV"), Some(0));
        // Two of that name: neither.
        assert_eq!(find("gone", "Dell"), None);
        assert_eq!(find("gone", "Samsung"), None);
        // No id: by name.
        assert_eq!(find("", "LG TV"), Some(0));
    }

    #[test]
    fn restores_round_trip_without_the_parts_left_alone() {
        let restore = DisplayRestore {
            display: "id".into(),
            name: "TV".into(),
            hdr_before: Some(false),
            mode: None,
        };
        let json = serde_json::to_value(&restore).unwrap();
        assert_eq!(json["hdrBefore"], false);
        assert!(json.get("mode").is_none());
        assert_eq!(
            serde_json::from_value::<DisplayRestore>(json).unwrap(),
            restore
        );
    }
}
