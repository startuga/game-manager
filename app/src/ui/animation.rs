//! Animated pictures in the window (an animated WebP cover or page art), each frame decoded as it
//! comes due, never all at once (`gm_core::picture::Frames`).
//!
//! - **One thread an animation**, started the first time it plays: it decodes the next frame
//!   while the current one shows, made no larger than it is drawn (`image::decode_frame`), then
//!   waits until that one is taken. Dropping the `Animation` (its tile far from the screen,
//!   another game's page) ends the thread.
//! - **The renderer** asks for the frame to draw (`Animation::frame`) and hears when the next one
//!   is due. Frames never go through the app's `update`, so playing rebuilds no view; the shell
//!   draws again when the next frame is due (`render::Drawn::next_frame`). A frame the thread
//!   hasn't finished is looked for again a moment later (`LATE`), the current one still showing.
//! - **Stopping** (the pointer leaves a tile that plays on hover, the window goes behind another)
//!   shows the still picture again, and the next play starts from the first frame.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use gm_core::picture::Frames;

use super::image::{self, Picture};

/// How soon to look again for a frame the thread hasn't finished.
const LATE: Duration = Duration::from_millis(8);

/// An animated picture, playing or not.
pub struct Animation {
    shared: Arc<Shared>,
    clock: Mutex<Clock>,
    /// The frames, until the first play hands them to the thread.
    frames: Mutex<Option<Frames>>,
    /// The size its frames are made to cover, in pixels.
    size: (u32, u32),
}

/// What the renderer and the thread share.
#[derive(Default)]
struct Shared {
    slot: Mutex<Slot>,
    wake: Condvar,
}

#[derive(Default)]
struct Slot {
    /// The next frame, decoded ahead, and how long it shows.
    ready: Option<(Arc<Picture>, Duration)>,
    /// Frames are wanted.
    playing: bool,
    /// Start again from the first frame.
    rewind: bool,
    /// No more frames: all were played, or one couldn't be read.
    done: bool,
    /// The `Animation` is gone.
    stop: bool,
}

/// The renderer's side: what shows, until when.
#[derive(Default)]
struct Clock {
    shown: Option<Arc<Picture>>,
    due: Option<Instant>,
    playing: bool,
}

impl Animation {
    /// `bytes` as an animation whose frames are made to cover `size` pixels; `None` when they
    /// are no animated picture.
    pub fn new(bytes: Vec<u8>, size: (u32, u32)) -> Option<Arc<Self>> {
        let frames = Frames::new(bytes)?;
        Some(Arc::new(Self {
            shared: Arc::default(),
            clock: Mutex::default(),
            frames: Mutex::new(Some(frames)),
            size,
        }))
    }

    /// What to draw at `now`: the frame showing (`None`: the still picture), and how long until
    /// the next one is due (`None`: nothing more changes). `play`: whether it plays now.
    pub fn frame(&self, now: Instant, play: bool) -> (Option<Arc<Picture>>, Option<Duration>) {
        let (Ok(mut clock), Ok(mut slot)) = (self.clock.lock(), self.shared.slot.lock()) else {
            return (None, None);
        };
        if !play {
            if clock.playing {
                *clock = Clock::default();
                slot.ready = None;
                slot.playing = false;
                slot.rewind = true;
                slot.done = false;
                self.shared.wake.notify_one();
            }
            return (None, None);
        }
        if !clock.playing {
            clock.playing = true;
            slot.playing = true;
            self.start(&mut slot);
            self.shared.wake.notify_one();
        }
        let due = clock.due.unwrap_or(now);
        if now >= due {
            match slot.ready.take() {
                Some((picture, shows)) => {
                    clock.shown = Some(picture);
                    // Late (a slow decode, the window was busy): the next counts from now.
                    let next = due + shows;
                    clock.due = Some(if next < now { now + shows } else { next });
                    self.shared.wake.notify_one();
                }
                None if slot.done => return (clock.shown.clone(), None),
                None => return (clock.shown.clone(), Some(LATE)),
            }
        }
        let next = clock.due.map(|due| due.saturating_duration_since(now));
        (clock.shown.clone(), next)
    }

    /// Hands the frames to a thread of their own, the first time it plays.
    fn start(&self, slot: &mut Slot) {
        let Some(mut frames) = self.frames.lock().ok().and_then(|mut frames| frames.take()) else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        let size = self.size;
        let started = std::thread::Builder::new()
            .name("animation".into())
            .spawn(move || decode_ahead(&mut frames, size, &shared));
        if started.is_err() {
            slot.done = true;
        }
    }
}

impl Drop for Animation {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.shared.slot.lock() {
            slot.stop = true;
        }
        self.shared.wake.notify_one();
    }
}

/// The thread: decodes the next frame whenever the last one was taken, until the `Animation` is
/// gone.
fn decode_ahead(frames: &mut Frames, size: (u32, u32), shared: &Shared) {
    loop {
        {
            let Ok(mut slot) = shared.slot.lock() else {
                return;
            };
            loop {
                if slot.stop {
                    return;
                }
                if slot.rewind {
                    slot.rewind = false;
                    frames.rewind();
                }
                if slot.playing && slot.ready.is_none() && !slot.done {
                    break;
                }
                slot = match shared.wake.wait(slot) {
                    Ok(slot) => slot,
                    Err(_) => return,
                };
            }
        }
        let frame = frames
            .next_frame()
            .ok()
            .flatten()
            .and_then(|(frame, shows)| {
                let picture = image::decode_frame(frame, size)?;
                Some((Arc::new(picture), shows))
            });
        let Ok(mut slot) = shared.slot.lock() else {
            return;
        };
        // Stopped meanwhile: the frame belongs to the play that ended.
        if slot.rewind || !slot.playing {
            continue;
        }
        match frame {
            Some(frame) => slot.ready = Some(frame),
            None => slot.done = true,
        }
    }
}
