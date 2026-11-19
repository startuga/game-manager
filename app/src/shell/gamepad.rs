//! A gamepad (XInput: Xbox controllers and the many that act like one) driving the window from the
//! couch, while Game Manager is the window in front.
//!
//! - **Polled** from a window timer (`shell`'s `GAMEPAD_TIMER`) only while the window is active,
//!   so a game in front keeps the pad to itself: every `FAST` with a pad found, else every `SLOW`
//!   to find one (asking empty slots is slow, so only then are all four asked).
//! - **What the pad does** (`Pad::step`, kept apart from the reading so it can be tested): the
//!   d-pad and the left stick (past about half its travel) are the arrows, at once and then,
//!   held, after `REPEAT_AFTER` every `REPEAT_EVERY`; A is Enter, B Escape and Y the Menu key; LB
//!   and RB, Start and View go to the app (`App::gamepad`: the previous and next tab, Play,
//!   Library). Its keys reach the toolkit as the keyboard's do (`UiState::key`), so focus moves
//!   and shows the same way (`input::spatial_next`).
//! - Leaving the window forgets what was held, so nothing repeats on coming back.

use crate::ui::input::Key;
use std::time::{Duration, Instant};
use windows::Win32::{
    XINPUT_GAMEPAD_A, XINPUT_GAMEPAD_B, XINPUT_GAMEPAD_BACK, XINPUT_GAMEPAD_DPAD_DOWN,
    XINPUT_GAMEPAD_DPAD_LEFT, XINPUT_GAMEPAD_DPAD_RIGHT, XINPUT_GAMEPAD_DPAD_UP,
    XINPUT_GAMEPAD_LEFT_SHOULDER, XINPUT_GAMEPAD_RIGHT_SHOULDER, XINPUT_GAMEPAD_START,
    XINPUT_GAMEPAD_Y, XINPUT_STATE, XInputGetState,
};

/// How often a found pad is read, and how often an empty slot is asked whether one is there.
pub const FAST: Duration = Duration::from_millis(16);
pub const SLOW: Duration = Duration::from_secs(2);
/// A direction held repeats after this, then every `REPEAT_EVERY`.
const REPEAT_AFTER: Duration = Duration::from_millis(400);
const REPEAT_EVERY: Duration = Duration::from_millis(100);
/// How far the left stick goes before it counts as a direction (of 32,767).
const STICK: i16 = 16_000;
/// XInput's four slots.
const SLOTS: u32 = 4;

/// What a button asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadButton {
    PreviousTab,
    NextTab,
    Play,
    Library,
}

/// What a step of the pad does: a key for the toolkit, or a button for the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Key(Key),
    Button(PadButton),
}

/// A pad's buttons and left stick, as XInput reads them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    pub buttons: u16,
    pub stick_x: i16,
    pub stick_y: i16,
}

/// The buttons, by what they do (directions apart).
const BUTTONS: [(i32, Press); 7] = [
    (XINPUT_GAMEPAD_A, Press::Key(Key::Enter)),
    (XINPUT_GAMEPAD_B, Press::Key(Key::Escape)),
    (XINPUT_GAMEPAD_Y, Press::Key(Key::Menu)),
    (
        XINPUT_GAMEPAD_LEFT_SHOULDER,
        Press::Button(PadButton::PreviousTab),
    ),
    (
        XINPUT_GAMEPAD_RIGHT_SHOULDER,
        Press::Button(PadButton::NextTab),
    ),
    (XINPUT_GAMEPAD_START, Press::Button(PadButton::Play)),
    (XINPUT_GAMEPAD_BACK, Press::Button(PadButton::Library)),
];

/// A pad's state between steps: the buttons held, and the direction held with when it started
/// and last repeated.
#[derive(Debug, Default)]
pub struct Pad {
    buttons: u16,
    direction: Option<(Key, Instant, Instant)>,
}

impl Pad {
    /// What changed since the last reading: each button newly pressed, and the direction held
    /// (the d-pad's first, else the stick's) at once or repeating.
    pub fn step(&mut self, reading: Reading, now: Instant) -> Vec<Press> {
        let mut presses: Vec<Press> = BUTTONS
            .iter()
            .filter(|(bit, _)| {
                let bit = *bit as u16;
                reading.buttons & bit != 0 && self.buttons & bit == 0
            })
            .map(|(_, press)| *press)
            .collect();
        self.buttons = reading.buttons;
        match (direction(reading), self.direction) {
            (Some(key), Some((held, since, last))) if key == held => {
                if now.duration_since(since) >= REPEAT_AFTER
                    && now.duration_since(last) >= REPEAT_EVERY
                {
                    presses.push(Press::Key(key));
                    self.direction = Some((key, since, now));
                }
            }
            (Some(key), _) => {
                presses.push(Press::Key(key));
                self.direction = Some((key, now, now));
            }
            (None, _) => self.direction = None,
        }
        presses
    }
}

/// The direction a reading points: the d-pad's, else the left stick's when pushed far enough.
fn direction(reading: Reading) -> Option<Key> {
    let pad = |bit: i32| reading.buttons & bit as u16 != 0;
    if pad(XINPUT_GAMEPAD_DPAD_UP) {
        return Some(Key::Up);
    }
    if pad(XINPUT_GAMEPAD_DPAD_DOWN) {
        return Some(Key::Down);
    }
    if pad(XINPUT_GAMEPAD_DPAD_LEFT) {
        return Some(Key::Left);
    }
    if pad(XINPUT_GAMEPAD_DPAD_RIGHT) {
        return Some(Key::Right);
    }
    let (x, y) = (i32::from(reading.stick_x), i32::from(reading.stick_y));
    let far = i32::from(STICK);
    // The stronger of the two axes; up is positive.
    if x.abs() >= y.abs() {
        (x >= far)
            .then_some(Key::Right)
            .or((x <= -far).then_some(Key::Left))
    } else {
        (y >= far)
            .then_some(Key::Up)
            .or((y <= -far).then_some(Key::Down))
    }
}

/// The pad found, and when the slots were last asked.
#[derive(Debug, Default)]
pub struct Gamepad {
    pad: Pad,
    slot: Option<u32>,
    probed: Option<Instant>,
}

impl Gamepad {
    /// Reads the pad found (or, every `SLOW`, looks for one) and says what it did.
    pub fn poll(&mut self, now: Instant) -> Vec<Press> {
        if let Some(slot) = self.slot {
            if let Some(reading) = read(slot) {
                return self.pad.step(reading, now);
            }
            // Unplugged or off.
            self.slot = None;
            self.pad = Pad::default();
        }
        if self.probed.is_none_or(|at| now.duration_since(at) >= SLOW) {
            self.probed = Some(now);
            self.slot = (0..SLOTS).find(|&slot| read(slot).is_some());
        }
        Vec::new()
    }

    /// How long until the next poll: `FAST` with a pad, else `SLOW`.
    pub fn interval(&self) -> Duration {
        if self.slot.is_some() { FAST } else { SLOW }
    }

    /// The window left the front: what was held is forgotten.
    pub fn forget(&mut self) {
        self.pad = Pad::default();
    }
}

/// Slot `slot`'s pad, or `None` when none is connected there.
fn read(slot: u32) -> Option<Reading> {
    let mut state = XINPUT_STATE::default();
    // SAFETY: a valid out-pointer to a state XInput fills.
    let result = unsafe { XInputGetState(slot, &mut state) };
    (result == 0).then_some(Reading {
        buttons: state.Gamepad.wButtons,
        stick_x: state.Gamepad.sThumbLX,
        stick_y: state.Gamepad.sThumbLY,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(buttons: i32) -> Reading {
        Reading {
            buttons: buttons as u16,
            ..Reading::default()
        }
    }

    #[test]
    fn buttons_press_once_and_directions_repeat_while_held() {
        let mut pad = Pad::default();
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        // A: Enter once, however long it is held.
        assert_eq!(
            pad.step(held(XINPUT_GAMEPAD_A), at(0)),
            [Press::Key(Key::Enter)]
        );
        assert!(pad.step(held(XINPUT_GAMEPAD_A), at(16)).is_empty());
        assert!(pad.step(held(0), at(32)).is_empty());
        // Down: at once, then after 400 ms every 100 ms.
        let down = held(XINPUT_GAMEPAD_DPAD_DOWN);
        assert_eq!(pad.step(down, at(100)), [Press::Key(Key::Down)]);
        assert!(pad.step(down, at(300)).is_empty());
        assert_eq!(pad.step(down, at(500)), [Press::Key(Key::Down)]);
        assert!(pad.step(down, at(550)).is_empty());
        assert_eq!(pad.step(down, at(600)), [Press::Key(Key::Down)]);
        // Let go, then press again: at once.
        assert!(pad.step(held(0), at(616)).is_empty());
        assert_eq!(pad.step(down, at(632)), [Press::Key(Key::Down)]);
        // The app's buttons, two at once.
        assert_eq!(
            pad.step(
                held(XINPUT_GAMEPAD_START | XINPUT_GAMEPAD_RIGHT_SHOULDER),
                at(700)
            ),
            [
                Press::Button(PadButton::NextTab),
                Press::Button(PadButton::Play)
            ]
        );
    }

    #[test]
    fn the_stick_counts_past_half_its_travel_by_its_stronger_axis() {
        let stick = |x: i16, y: i16| Reading {
            buttons: 0,
            stick_x: x,
            stick_y: y,
        };
        assert_eq!(direction(stick(0, 0)), None);
        assert_eq!(direction(stick(10_000, 0)), None);
        assert_eq!(direction(stick(30_000, 5_000)), Some(Key::Right));
        assert_eq!(direction(stick(-20_000, 0)), Some(Key::Left));
        assert_eq!(direction(stick(5_000, 25_000)), Some(Key::Up));
        assert_eq!(direction(stick(0, -32_768)), Some(Key::Down));
        // The d-pad wins over the stick.
        let both = Reading {
            buttons: XINPUT_GAMEPAD_DPAD_LEFT as u16,
            stick_x: 30_000,
            stick_y: 0,
        };
        assert_eq!(direction(both), Some(Key::Left));
    }
}
