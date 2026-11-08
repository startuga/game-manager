//! The Win32 window around the Direct2D toolkit: windows-window's HWND and message loop, and the
//! glue that turns window messages into toolkit events and app messages.
//!
//! - **The app** (`App`) is Elm-style, as it was on WinUI: `update` takes a message, `view` builds
//!   the element tree from the state. Background work and dialogs go through `Ctx`.
//! - **Each change** rebuilds the view and lays it out; pointer hover and the like only redraw.
//!   Drawing happens when something changed (no CPU at rest), or every frame while something
//!   animates (`render::Drawn::animating`), never while minimized.
//! - **Messages** reach `Shell` through windows-window's handler. The state is borrowed with
//!   `try_borrow_mut`: a message sent to the window while it is busy (an EDIT's `EN_CHANGE` for text
//!   we set ourselves) gets Windows' default handling. Messages that must always be answered
//!   (`WM_CTLCOLOREDIT`, `WM_NCCALCSIZE`, `WM_GETMINMAXINFO`) need no state.
//! - **Live resizing** happens inside Windows' modal size loop, so `WM_SIZE` lays out and draws
//!   at once.
//! - **The title bar** is part of the view. Elements keyed `CAPTION_DRAG`, `CAPTION_MIN`,
//!   `CAPTION_MAX` and `CAPTION_CLOSE` tell `WM_NCHITTEST` where the draggable bar and the caption
//!   buttons are (`window.rs`). Over the buttons the pointer arrives as non-client messages, which
//!   are fed to the toolkit like client ones.
//! - **Files dropped** on the window (`WM_DROPFILES`) go to `App::drop_files`.
//! - **Efficiency mode** follows `WM_ACTIVATEAPP` (`efficiency.rs`).
//! - **UI Automation**: `WM_GETOBJECT` and the actions it posts (`ui::uia`).
//!
//! Windows' message and style constants are `i32` in these bindings, so messages are matched as
//! `i32` and the app's own (`WM_WAKE`, `WM_EDIT_KEY`, `WM_UIA_ACTION`) are `i32` too.

pub mod ctx;
pub mod dialogs;
pub mod edit_box;
pub mod efficiency;
pub mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use windows::Win32::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DragAcceptFiles, DragFinish, DragQueryFileW,
    EN_CHANGE, GetDpiForSystem, GetKeyState, HDROP, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON,
    HTMINBUTTON, IDC_SIZEWE, KillTimer, LoadCursorW, ReleaseCapture, SIZE_MINIMIZED, SW_SHOWNORMAL,
    SetCapture, SetCursor, SetFocus, SetProcessDpiAwarenessContext, SetTimer, ShowWindow,
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_RETURN, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP, WM_ACTIVATEAPP, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_DPICHANGED, WM_DROPFILES, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_GETOBJECT,
    WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSELEAVE, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_NCCALCSIZE, WM_NCHITTEST, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE,
    WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SETCURSOR, WM_SIZE, WM_TIMER, WS_CLIPCHILDREN,
    WS_OVERLAPPEDWINDOW,
};
use windows::core::PWSTR;
use windows_window::{Window, run_with};

use crate::ui::el::{El, Kind, Rect};
use crate::ui::input::{Button, Frame, Key, KeyResult, UiState};
use crate::ui::layout::{Layout, key_id};
use crate::ui::render::{Renderer, Text};
use crate::ui::theme;
use crate::ui::uia::{self, Action, Uia, WM_UIA_ACTION};
use ctx::{Ctx, Effect, WM_WAKE};
use edit_box::{EditBoxes, WM_EDIT_KEY};
use window::hwnd;

/// Keys of the title bar's elements, which the window's hit-testing looks for.
pub const CAPTION_DRAG: &str = "caption:drag";
pub const CAPTION_MIN: &str = "caption:min";
pub const CAPTION_MAX: &str = "caption:max";
pub const CAPTION_CLOSE: &str = "caption:close";

/// The tooltip's timer.
const TOOLTIP_TIMER: usize = 1;

/// Posted by a check script: draws the current frame into the PNG named by the
/// `GAME_MANAGER_CAPTURE` environment variable (one pixel per DIP), so a session without screen
/// capture can see the window. Nothing happens without the variable.
pub const WM_CAPTURE: i32 = windows::Win32::WM_APP + 9;
const CAPTURE_VARIABLE: &str = "GAME_MANAGER_CAPTURE";

/// What a view knows of its window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WindowInfo {
    /// The client area in DIPs.
    pub size: (f32, f32),
    pub maximized: bool,
}

pub trait App: 'static {
    type Message: Clone + Send + 'static;
    fn update(&mut self, message: Self::Message, ctx: &Ctx<Self::Message>);
    fn view(&self, window: &WindowInfo) -> El<Self::Message>;
    /// Escape, when nothing of the toolkit's (a drop-down list) was open.
    fn escape(&self) -> Option<Self::Message>;
    /// Files dropped on the window; `None` when they aren't wanted now.
    fn drop_files(&self, paths: Vec<PathBuf>) -> Option<Self::Message>;
}

pub struct Options {
    pub title: String,
    /// The first client size, and the smallest, in DIPs.
    pub size: (f32, f32),
    pub min_size: (f32, f32),
}

type State<A> = Rc<RefCell<Option<Shell<A>>>>;

/// Opens the window and runs until it closes. `make` builds the app with the context it starts
/// its first background work with.
pub fn run<A: App>(
    make: impl FnOnce(&Ctx<A::Message>) -> A,
    options: Options,
) -> windows_core::Result<()> {
    // The UI thread's apartment: single-threaded, as the Shell and the Open dialog need. It is
    // never left: the process ends with the window.
    // SAFETY: plain COM initialization of this thread.
    let _ = unsafe {
        CoInitializeEx(
            None,
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    let (sender, receiver) = mpsc::channel();
    let ctx = Rc::new(Ctx::new(sender));
    let app = make(&ctx);
    let state: State<A> = Rc::new(RefCell::new(None));
    let handler = Rc::clone(&state);
    let min = options.min_size;
    // Per-monitor DPI awareness before anything measures (windows-window sets it too, but only
    // when creating the window, after the first size below is worked out).
    // SAFETY: a process-wide setting, made before any window exists.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    // SAFETY: a plain query; the first size uses the system's DPI until the window has its own.
    let scale = unsafe { GetDpiForSystem() } as f32 / 96.0;
    let window = Window::new(&options.title)
        .client_size(
            (options.size.0 * scale) as i32,
            (options.size.1 * scale) as i32,
        )
        .style((WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN) as u32)
        .visible(false)
        .on_message(move |hwnd, message, wparam, lparam| {
            route(&handler, hwnd as isize, message, wparam, lparam, min)
        })
        .create()?;
    let handle = window.hwnd() as isize;
    ctx.set_window(handle);
    window::setup(handle);
    // SAFETY: our window accepts files dropped from Explorer.
    unsafe { DragAcceptFiles(hwnd(handle), true) };
    *state.borrow_mut() = Some(Shell::new(app, Rc::clone(&ctx), receiver, handle));
    // SAFETY: shows our window.
    let _ = unsafe { ShowWindow(hwnd(handle), SW_SHOWNORMAL) };

    let result = run_with(|| {
        // Effects run with nothing borrowed: a dialog's own loop reaches the window normally.
        for effect in ctx.take_effects() {
            match effect {
                Effect::PickFile(picker, then) => {
                    let message = then(dialogs::pick_file(handle, &picker));
                    if let Some(shell) = state.borrow_mut().as_mut() {
                        shell.dispatch(message);
                    }
                }
                Effect::Window(command) => window::command(handle, command),
            }
        }
        let mut guard = state.borrow_mut();
        let Some(shell) = guard.as_mut() else {
            return Ok(false);
        };
        shell.drain();
        shell.render()
    });
    // The renderer's swap chain and the EDITs go before the window.
    state.borrow_mut().take();
    drop(window);
    result
}

/// Every window message: the few that need no state, then the shell's.
fn route<A: App>(
    state: &State<A>,
    handle: isize,
    message: u32,
    wparam: usize,
    lparam: isize,
    min: (f32, f32),
) -> Option<isize> {
    match message as i32 {
        WM_CTLCOLOREDIT => return Some(edit_box::ctl_color(wparam)),
        WM_NCCALCSIZE => return window::nc_calc_size(handle, wparam, lparam),
        WM_GETMINMAXINFO => {
            window::min_size(handle, lparam, min);
            return Some(0);
        }
        WM_ERASEBKGND => return Some(1),
        _ => {}
    }
    let Ok(mut guard) = state.try_borrow_mut() else {
        return None;
    };
    guard.as_mut()?.message(message as i32, wparam, lparam)
}

struct Shell<A: App> {
    app: A,
    ctx: Rc<Ctx<A::Message>>,
    receiver: Receiver<A::Message>,
    hwnd: isize,
    ui: UiState,
    text: Text,
    frame: Option<Frame<A::Message>>,
    renderer: Option<Renderer>,
    edits: EditBoxes,
    uia: Uia,
    dpi: u32,
    /// The client area in pixels.
    size: (u32, u32),
    /// The view must be built and laid out again.
    stale: bool,
    /// The frame must be drawn again.
    redraw: bool,
    minimized: bool,
    tracking: bool,
}

fn low(value: isize) -> i32 {
    i32::from(value as i16)
}

fn high(value: isize) -> i32 {
    i32::from((value >> 16) as i16)
}

impl<A: App> Shell<A> {
    fn new(
        app: A,
        ctx: Rc<Ctx<A::Message>>,
        receiver: Receiver<A::Message>,
        handle: isize,
    ) -> Self {
        Self {
            app,
            ctx,
            receiver,
            hwnd: handle,
            ui: UiState::default(),
            text: Text::new(),
            frame: None,
            renderer: None,
            edits: EditBoxes::new(handle),
            uia: Uia::new(handle),
            dpi: window::dpi(handle),
            size: (0, 0),
            stale: true,
            redraw: true,
            minimized: false,
            tracking: false,
        }
    }

    fn scale(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    /// Client pixels to DIPs.
    fn dips(&self, x: i32, y: i32) -> (f32, f32) {
        (x as f32 / self.scale(), y as f32 / self.scale())
    }

    fn dispatch(&mut self, message: A::Message) {
        self.app.update(message, &self.ctx);
        self.stale = true;
    }

    fn dispatch_some(&mut self, message: Option<A::Message>) {
        if let Some(message) = message {
            self.dispatch(message);
        }
    }

    /// Messages from background work.
    fn drain(&mut self) {
        while let Ok(message) = self.receiver.try_recv() {
            self.dispatch(message);
        }
    }

    /// Builds the view and lays it out; places the EDITs and refreshes UI Automation.
    fn layout(&mut self) {
        self.text.next_frame();
        let size = self.dips(self.size.0 as i32, self.size.1 as i32);
        let info = WindowInfo {
            size,
            maximized: window::is_maximized(self.hwnd),
        };
        let root = self.app.view(&info);
        let rect = Rect::new(0.0, 0.0, size.0, size.1);
        self.ui.window = rect;
        let placed = Layout::new(&self.text, &self.ui.scroll).place(&root, rect);
        let frame = Frame { root, placed };
        self.ui.forget_missing(&frame);
        let mut specs = Vec::new();
        edit_box::collect(&frame.root, &frame.placed, &mut specs);
        // Text boxes are child windows, drawn over everything: under a dialog or a menu, only
        // its own show.
        if let Some((layer, placed)) = frame.modal_layer() {
            let mut inside = Vec::new();
            edit_box::collect(layer, placed, &mut inside);
            for spec in &mut specs {
                spec.visible &= inside.iter().any(|kept| kept.id == spec.id);
            }
        }
        self.edits.sync(&specs, self.dpi);
        self.frame = Some(frame);
        self.stale = false;
        self.redraw = true;
        self.refresh_uia();
    }

    fn refresh_uia(&self) {
        let Some(frame) = &self.frame else {
            return;
        };
        let mut tree = uia::snapshot(frame, self.ui.focus, self.ui.open_dropdown);
        tree.scale = self.scale();
        let origin = window::client_origin(self.hwnd);
        tree.origin = (origin.0 as f32, origin.1 as f32);
        tree.edits = self.edits.windows();
        self.uia.update(tree);
    }

    /// Lays out and draws what changed. Returns whether to draw again at once (animation).
    fn render(&mut self) -> windows_core::Result<bool> {
        if self.minimized || self.size.0 == 0 || self.size.1 == 0 {
            return Ok(false);
        }
        if self.stale || self.frame.is_none() {
            self.layout();
        }
        if !self.redraw {
            return Ok(false);
        }
        if self.renderer.is_none() {
            // SAFETY: the shell, which owns the renderer, is dropped before the window (`run`).
            let renderer =
                unsafe { Renderer::new(self.hwnd, self.size.0, self.size.1, self.dpi as f32) }?;
            self.renderer = Some(renderer);
        }
        let (Some(renderer), Some(frame)) = (self.renderer.as_mut(), self.frame.as_ref()) else {
            return Ok(false);
        };
        renderer.resize(self.size.0, self.size.1, self.dpi as f32)?;
        let drawn = renderer.draw(frame, &self.ui, &self.text)?;
        self.redraw = drawn.animating;
        if !drawn.presented {
            // The device was lost: a new one draws the next frame.
            self.renderer = None;
            self.redraw = true;
            return Ok(true);
        }
        Ok(drawn.animating)
    }

    fn message(&mut self, message: i32, wparam: usize, lparam: isize) -> Option<isize> {
        match message {
            WM_WAKE => {
                self.drain();
                Some(0)
            }
            WM_SIZE => {
                self.minimized = wparam == SIZE_MINIMIZED as usize;
                if self.minimized {
                    efficiency::trim();
                    return Some(0);
                }
                self.size = ((lparam & 0xFFFF) as u32, ((lparam >> 16) & 0xFFFF) as u32);
                // Inside Windows' size loop the message loop doesn't run: draw now.
                self.stale = true;
                let _ = self.render();
                Some(0)
            }
            WM_DPICHANGED => {
                self.dpi = (wparam & 0xFFFF) as u32;
                window::dpi_changed(self.hwnd, lparam);
                self.stale = true;
                Some(0)
            }
            WM_ACTIVATEAPP => {
                efficiency::set(wparam == 0);
                None
            }
            WM_MOUSEMOVE => {
                self.track_leave(false);
                let (x, y) = self.dips(low(lparam), high(lparam));
                self.pointer_move(x, y);
                Some(0)
            }
            WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
                self.tracking = false;
                self.redraw |= self.ui.pointer_leave();
                None
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                // Drawn controls take the keyboard from a text box.
                // SAFETY: focusing our own window.
                let _ = unsafe { SetFocus(Some(hwnd(self.hwnd))) };
                // SAFETY: capturing the pointer for our own window until the button is released.
                let _ = unsafe { SetCapture(hwnd(self.hwnd)) };
                let (x, y) = self.dips(low(lparam), high(lparam));
                let button = if message == WM_LBUTTONDOWN {
                    Button::Left
                } else {
                    Button::Right
                };
                self.pointer_down(x, y, button);
                Some(0)
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                // SAFETY: releases the capture taken on the button's press.
                let _ = unsafe { ReleaseCapture() };
                let (x, y) = self.dips(low(lparam), high(lparam));
                let button = if message == WM_LBUTTONUP {
                    Button::Left
                } else {
                    Button::Right
                };
                self.pointer_up(x, y, button);
                Some(0)
            }
            WM_MOUSEWHEEL => {
                let (px, py) = window::client_point(self.hwnd, lparam);
                let (x, y) = self.dips(px, py);
                let notches = f32::from((wparam >> 16) as i16) / 120.0;
                let frame = self.frame.as_ref()?;
                if self.ui.wheel(frame, x, y, notches) {
                    self.stale = true;
                }
                Some(0)
            }
            WM_TIMER if wparam == TOOLTIP_TIMER => {
                // SAFETY: stops our own timer.
                let _ = unsafe { KillTimer(Some(hwnd(self.hwnd)), TOOLTIP_TIMER) };
                self.redraw |= self.ui.show_tooltip();
                Some(0)
            }
            WM_KEYDOWN => {
                let key = key_of(wparam as i32, self.shift())?;
                self.key(key);
                Some(0)
            }
            WM_EDIT_KEY => {
                let id = self.edits.id_of(lparam);
                let key = key_of(wparam as i32, self.shift())?;
                match key {
                    Key::Tab { .. } => {
                        // From the text box on: focus moves on through the drawn controls.
                        // SAFETY: focusing our own window.
                        let _ = unsafe { SetFocus(Some(hwnd(self.hwnd))) };
                        self.ui.focus = id;
                        self.key(key);
                    }
                    Key::Escape => {
                        let message = self.app.escape();
                        self.dispatch_some(message);
                    }
                    _ => {}
                }
                Some(0)
            }
            WM_COMMAND if (wparam >> 16) as i32 == EN_CHANGE => {
                let id = self.edits.id_of(lparam)?;
                let text = edit_box::text_of(lparam);
                let message = self
                    .frame
                    .as_ref()?
                    .find(id)
                    .and_then(|hit| match &hit.el.kind {
                        Kind::Edit {
                            on_change,
                            text: shown,
                            ..
                        } => (*shown != text).then(|| on_change(text)),
                        _ => None,
                    });
                self.dispatch_some(message);
                Some(0)
            }
            WM_DROPFILES => {
                let paths = dropped_paths(wparam);
                let message = self.app.drop_files(paths);
                self.dispatch_some(message);
                Some(0)
            }
            WM_NCHITTEST => Some(self.hit_test(wparam, lparam)),
            WM_SETCURSOR if (lparam & 0xFFFF) as i32 == HTCLIENT => self.set_cursor(),
            WM_NCMOUSEMOVE if is_caption_button(wparam) => {
                self.track_leave(true);
                let (px, py) = window::client_point(self.hwnd, lparam);
                let (x, y) = self.dips(px, py);
                self.pointer_move(x, y);
                Some(0)
            }
            WM_NCMOUSEMOVE => {
                self.redraw |= self.ui.pointer_leave();
                None
            }
            WM_NCLBUTTONDOWN if is_caption_button(wparam) => {
                let (px, py) = window::client_point(self.hwnd, lparam);
                let (x, y) = self.dips(px, py);
                self.pointer_down(x, y, Button::Left);
                Some(0)
            }
            WM_NCLBUTTONUP if is_caption_button(wparam) => {
                let (px, py) = window::client_point(self.hwnd, lparam);
                let (x, y) = self.dips(px, py);
                self.pointer_up(x, y, Button::Left);
                Some(0)
            }
            WM_CAPTURE => {
                self.capture();
                Some(0)
            }
            WM_GETOBJECT => self.uia.get_object(self.hwnd, wparam, lparam),
            WM_UIA_ACTION => {
                if let Some(action) = Action::from_wparam(wparam) {
                    self.uia_action(action, lparam as u64);
                }
                Some(0)
            }
            _ => None,
        }
    }

    /// `WM_CAPTURE`: the current frame as a PNG, for checks.
    fn capture(&mut self) {
        let Some(path) = std::env::var_os(CAPTURE_VARIABLE) else {
            return;
        };
        if self.stale || self.frame.is_none() {
            self.layout();
        }
        let (Some(renderer), Some(frame)) = (self.renderer.as_mut(), self.frame.as_ref()) else {
            return;
        };
        if let Ok((width, height, pixels)) = renderer.capture(frame, &self.ui, &self.text) {
            let _ = crate::ui::image::save_png(&pixels, width, height, std::path::Path::new(&path));
        }
    }

    fn shift(&self) -> bool {
        // SAFETY: a plain query of the keyboard state.
        let state = unsafe { GetKeyState(VK_SHIFT) };
        state < 0
    }

    /// Asks for `WM_MOUSELEAVE` (or `WM_NCMOUSELEAVE` over the caption buttons).
    fn track_leave(&mut self, non_client: bool) {
        if self.tracking && !non_client {
            return;
        }
        let flags = if non_client {
            TME_LEAVE | TME_NONCLIENT
        } else {
            TME_LEAVE
        };
        let mut track = TRACKMOUSEEVENT {
            cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
            dwFlags: flags as u32,
            hwndTrack: hwnd(self.hwnd),
            dwHoverTime: 0,
        };
        // SAFETY: `track` is a valid, sized structure for our own window.
        let _ = unsafe { TrackMouseEvent(&mut track) };
        self.tracking = !non_client;
    }

    fn pointer_move(&mut self, x: f32, y: f32) {
        let Some(frame) = &self.frame else {
            return;
        };
        let had_tip = self.ui.tooltip.as_ref().map(|tip| tip.id);
        let (changed, message) = self.ui.pointer_move(frame, x, y);
        if changed {
            self.redraw = true;
            // A dragged scrollbar thumb moves the content: a new layout.
            self.stale |= self.ui.dragging();
        }
        let tip = self.ui.tooltip.as_ref().map(|tip| tip.id);
        if tip.is_some() && tip != had_tip {
            // SAFETY: a timer on our own window; the same id replaces a running one.
            unsafe {
                SetTimer(
                    Some(hwnd(self.hwnd)),
                    TOOLTIP_TIMER,
                    theme::TOOLTIP_DELAY_MS,
                    None,
                )
            };
        }
        // A dragged grip's position.
        self.dispatch_some(message);
    }

    /// `WM_SETCURSOR` over the client area: the left-right arrows over a resize grip.
    fn set_cursor(&self) -> Option<isize> {
        let frame = self.frame.as_ref()?;
        if !self.ui.resize_cursor(frame) {
            return None;
        }
        // IDC_SIZEWE is a resource number, declared as a narrow string in these bindings.
        let name = windows::core::PCWSTR(IDC_SIZEWE.0.cast());
        // SAFETY: a system cursor; no module.
        let cursor = unsafe { LoadCursorW(None, name) };
        // SAFETY: shows a system cursor, which needs no freeing.
        unsafe { SetCursor(Some(cursor)) };
        Some(1)
    }

    fn pointer_down(&mut self, x: f32, y: f32, button: Button) {
        let Some(frame) = &self.frame else {
            return;
        };
        self.ui.pointer_down(frame, x, y, button);
        // A press on a scrollbar or a list opening changes the layout or what is drawn on top.
        self.stale = true;
    }

    fn pointer_up(&mut self, x: f32, y: f32, button: Button) {
        let Some(frame) = &self.frame else {
            return;
        };
        let (changed, message) = self.ui.pointer_up(frame, x, y, button);
        if changed {
            self.stale = true;
        }
        self.dispatch_some(message);
    }

    fn key(&mut self, key: Key) {
        let Some(frame) = &self.frame else {
            return;
        };
        match self.ui.key(frame, key) {
            KeyResult::Message(message) => self.dispatch(message),
            KeyResult::Changed => self.stale = true,
            KeyResult::Unhandled if key == Key::Escape => {
                let message = self.app.escape();
                self.dispatch_some(message);
            }
            KeyResult::Unhandled => {}
        }
        // Tabbing onto a text box gives it the keyboard.
        if let Some(focus) = self.ui.focus
            && self.edits.focus(focus)
        {
            self.ui.focus_visible = false;
        }
    }

    fn uia_action(&mut self, action: Action, id: u64) {
        let Some(frame) = &self.frame else {
            return;
        };
        let Some(hit) = frame.find(id) else {
            return;
        };
        match action {
            Action::Invoke => {
                let message = hit.el.enabled.then(|| hit.el.on_click.clone()).flatten();
                self.dispatch_some(message);
            }
            Action::Focus => {
                self.ui.focus = Some(id);
                if !self.edits.focus(id) {
                    self.stale = true;
                }
            }
            Action::Expand => {
                if let Kind::DropDown { selected, .. } = &hit.el.kind {
                    let selected = *selected;
                    self.ui.open_dropdown = Some(id);
                    self.ui.dropdown_hover = selected;
                    self.stale = true;
                }
            }
            Action::Collapse => {
                self.ui.open_dropdown = None;
                self.stale = true;
            }
        }
    }

    /// `WM_NCHITTEST`: borders, the caption buttons, the draggable bar, else the client area.
    fn hit_test(&self, wparam: usize, lparam: isize) -> isize {
        if let Some(border) = window::border_hit(self.hwnd, wparam, lparam) {
            return border;
        }
        let Some(frame) = &self.frame else {
            return HTCLIENT as isize;
        };
        let (px, py) = window::client_point(self.hwnd, lparam);
        let (x, y) = self.dips(px, py);
        let within = |key: &str| {
            frame
                .find(key_id(key))
                .is_some_and(|hit| hit.placed.clip.contains(x, y))
        };
        if within(CAPTION_MIN) {
            return HTMINBUTTON as isize;
        }
        if within(CAPTION_MAX) {
            return HTMAXBUTTON as isize;
        }
        if within(CAPTION_CLOSE) {
            return HTCLOSE as isize;
        }
        if within(CAPTION_DRAG) && frame.hit(x, y).is_none() {
            return HTCAPTION as isize;
        }
        HTCLIENT as isize
    }
}

fn is_caption_button(hit: usize) -> bool {
    [HTMINBUTTON, HTMAXBUTTON, HTCLOSE]
        .iter()
        .any(|code| *code as usize == hit)
}

fn key_of(key: i32, shift: bool) -> Option<Key> {
    Some(match key {
        VK_TAB => Key::Tab { shift },
        VK_RETURN => Key::Enter,
        VK_SPACE => Key::Space,
        VK_ESCAPE => Key::Escape,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        _ => return None,
    })
}

/// The paths of a `WM_DROPFILES`; the drop's handle is released.
fn dropped_paths(wparam: usize) -> Vec<PathBuf> {
    let drop = wparam as HDROP;
    // SAFETY: index 0xFFFFFFFF asks for the count; no buffer.
    let count = unsafe { DragQueryFileW(drop, u32::MAX, None, 0) };
    let mut paths = Vec::new();
    for index in 0..count {
        // SAFETY: a valid index; no buffer asks for the length.
        let length = unsafe { DragQueryFileW(drop, index, None, 0) } as usize;
        let mut buffer = vec![0u16; length + 1];
        // SAFETY: `buffer` holds the path and its terminator, as its length says.
        let copied = unsafe {
            DragQueryFileW(
                drop,
                index,
                Some(PWSTR(buffer.as_mut_ptr())),
                buffer.len() as u32,
            )
        } as usize;
        paths.push(PathBuf::from(String::from_utf16_lossy(
            &buffer[..copied.min(length)],
        )));
    }
    // SAFETY: releases the drop's memory once, after the last query.
    unsafe { DragFinish(drop) };
    paths
}
