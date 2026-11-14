//! What the app's logic uses to reach beyond its state: background work, the Open dialog and the
//! window's own commands.
//!
//! - **Background work** (`spawn_background`) runs on the Windows thread pool (windows-threading).
//!   Its `Message` goes back through a channel, and `WM_WAKE` wakes the window to read it, so
//!   blocking work (disk, registry, network) never runs on the UI thread. `spawn_stream` is the
//!   same for work that sends several messages.
//! - **A message later** (`after`): a window timer sends it once the time has passed ("Starting…"
//!   for a few seconds, My PC's countdown), so no pool thread waits for it.
//! - **The window's scaling** (`scale`), for pictures made at the window's own pixel size.
//! - **Effects** (the Open dialog, minimize, maximize, close) are queued and run by the message
//!   loop between messages, with no state borrowed: a modal dialog's own message loop then reaches
//!   the window normally.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;
use windows::Win32::{HWND, PostMessageW, WM_APP};

/// Posted when background work sent a message.
pub const WM_WAKE: i32 = WM_APP + 1;

/// The window commands the title bar's buttons give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCommand {
    Minimize,
    /// Maximize, or restore when maximized.
    Maximize,
    Close,
}

/// The Open dialog's options.
#[derive(Default)]
pub struct FilePicker {
    pub title: String,
    /// Display name and `*.ext` patterns, in order.
    pub filters: Vec<(String, String)>,
    pub folder: Option<PathBuf>,
}

impl FilePicker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// A filter for files with these extensions.
    pub fn filter_extensions<'a>(
        mut self,
        name: &str,
        extensions: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let patterns: Vec<String> = extensions
            .into_iter()
            .map(|ext| format!("*.{ext}"))
            .collect();
        self.filters.push((name.to_owned(), patterns.join(";")));
        self
    }

    /// A filter for every file.
    pub fn filter_all(mut self) -> Self {
        self.filters
            .push(("All files".to_owned(), "*.*".to_owned()));
        self
    }

    /// The folder it opens in.
    pub fn folder(mut self, folder: PathBuf) -> Self {
        self.folder = Some(folder);
        self
    }

    /// Opens the dialog once the current message is handled; `then` makes the message for what
    /// was picked (`None`: cancelled).
    pub fn request<M>(
        self,
        ctx: &Ctx<M>,
        then: impl FnOnce(Result<Option<PathBuf>, String>) -> M + 'static,
    ) {
        ctx.effects
            .borrow_mut()
            .push(Effect::PickFile(self, Box::new(then)));
    }
}

/// Work for the message loop.
pub enum Effect<M> {
    PickFile(
        FilePicker,
        Box<dyn FnOnce(Result<Option<PathBuf>, String>) -> M>,
    ),
    Window(WindowCommand),
    /// A message to send once the time has passed.
    After(Duration, M),
}

pub struct Ctx<M> {
    hwnd: Cell<isize>,
    /// The window's display scaling (1.0 at 96 DPI), kept current by the shell.
    scale: Cell<f32>,
    sender: Sender<M>,
    effects: RefCell<Vec<Effect<M>>>,
}

impl<M: Send + 'static> Ctx<M> {
    pub fn new(sender: Sender<M>) -> Self {
        Self {
            hwnd: Cell::new(0),
            scale: Cell::new(1.0),
            sender,
            effects: RefCell::new(Vec::new()),
        }
    }

    /// The window's display scaling: 1.0 at 96 DPI, 1.5 at 150%. Pictures made for the window
    /// are sized with it; `App::scale_changed` says when it changes.
    pub fn scale(&self) -> f32 {
        self.scale.get()
    }

    pub fn set_scale(&self, scale: f32) {
        self.scale.set(scale);
    }

    /// The window that background work wakes.
    pub fn set_window(&self, hwnd: isize) {
        self.hwnd.set(hwnd);
    }

    /// Runs `work` on the thread pool; its message comes back to `update` on the UI thread.
    pub fn spawn_background<F>(&self, work: F)
    where
        F: FnOnce() -> M + Send + 'static,
    {
        let sender = self.sender.clone();
        let hwnd = self.hwnd.get();
        windows_threading::submit(move || {
            if sender.send(work()).is_ok() {
                wake(hwnd);
            }
        });
    }

    /// Runs `work` on the thread pool like `spawn_background`, for work with several things to
    /// say: every message it sends comes back to `update`, in order, as it is sent (covers
    /// arriving one by one).
    pub fn spawn_stream<F>(&self, work: F)
    where
        F: FnOnce(&dyn Fn(M)) + Send + 'static,
    {
        let sender = self.sender.clone();
        let hwnd = self.hwnd.get();
        windows_threading::submit(move || {
            work(&|message| {
                if sender.send(message).is_ok() {
                    wake(hwnd);
                }
            });
        });
    }

    /// Sends `message` to `update` once `delay` has passed (a window timer, so no thread waits).
    pub fn after(&self, delay: Duration, message: M) {
        self.effects
            .borrow_mut()
            .push(Effect::After(delay, message));
    }

    /// Minimizes, maximizes or closes the window once the current message is handled.
    pub fn window(&self, command: WindowCommand) {
        self.effects.borrow_mut().push(Effect::Window(command));
    }

    /// The queued effects, to run with nothing borrowed.
    pub fn take_effects(&self) -> Vec<Effect<M>> {
        std::mem::take(&mut self.effects.borrow_mut())
    }
}

/// Wakes the window's message loop to read the channel.
pub fn wake(hwnd: isize) {
    if hwnd == 0 {
        return;
    }
    // SAFETY: posting to a window handle; if the window is gone the call fails harmlessly.
    let _ = unsafe { PostMessageW(Some(hwnd as HWND), WM_WAKE as u32, 0, 0) };
}
