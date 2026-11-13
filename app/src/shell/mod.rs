//! The Win32 window around the Direct2D toolkit: windows-window's HWND and message loop, and the
//! glue that turns window messages into toolkit events and app messages.
//!
//! - **The app** (`App`) is Elm-style, as it was on WinUI: `update` takes a message, `view` builds
//!   the element tree from the state. Background work and dialogs go through `Ctx`.
//! - **Each change** rebuilds the view and lays it out; pointer hover and the like only redraw.
//!   Drawing happens when something changed (no CPU at rest), or every frame while something
//!   animates (`render::Drawn::animating`), never while minimized.
//! - **Messages** reach `Shell` through windows-window's handler. The state is borrowed with
//!   `try_borrow_mut`: a message sent to the window while it is busy gets Windows' default
//!   handling. Messages that must always be answered (`WM_NCCALCSIZE`, `WM_GETMINMAXINFO`) need
//!   no state. After each message the caret and the IME follow the text box with the keyboard
//!   (`text_input::follow_focus`).
//! - **Live resizing** happens inside Windows' modal size loop, so `WM_SIZE` lays out and draws
//!   at once.
//! - **The title bar** is part of the view. Elements keyed `CAPTION_DRAG`, `CAPTION_MIN`,
//!   `CAPTION_MAX` and `CAPTION_CLOSE` tell `WM_NCHITTEST` where the draggable bar and the caption
//!   buttons are (`window.rs`). Over the buttons the pointer arrives as non-client messages, which
//!   are fed to the toolkit like client ones.
//! - **Keys**: `WM_KEYDOWN` goes to the text box with the keyboard first (`text_input.rs`), then
//!   becomes a toolkit key; `WM_CHAR` and the IME's messages type into the box. The Menu key and
//!   Shift+F10 arrive as a keyboard `WM_CONTEXTMENU` (`Key::Menu`, or a text box's menu).
//! - **No child windows**: text boxes are drawn like everything else, so nothing but Direct2D
//!   paints the client area.
//! - **Files dropped** on the window (`WM_DROPFILES`) go to `App::drop_files`.
//! - **Display scaling**: `Ctx::scale` holds the window's (set before the first message, and at
//!   `WM_DPICHANGED`, which also calls `App::scale_changed`), for pictures the app makes at the
//!   window's pixel size (covers).
//! - **Efficiency mode** follows `WM_ACTIVATEAPP` (`efficiency.rs`).
//! - **UI Automation**: `WM_GETOBJECT` and the actions it posts (`ui::uia`).
//!
//! Windows' message and style constants are `i32` in these bindings, so messages are matched as
//! `i32` and the app's own (`WM_WAKE`, `WM_UIA_ACTION`) are `i32` too.

pub mod ctx;
pub mod dialogs;
pub mod efficiency;
mod text_input;
pub mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver};
use windows::Win32::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, DragAcceptFiles, DragFinish, DragQueryFileW,
    GetDpiForSystem, GetKeyState, HDROP, HTCAPTION, HTCLIENT, HTCLOSE, HTMAXBUTTON, HTMINBUTTON,
    IDC_IBEAM, IDC_SIZEWE, KillTimer, LoadCursorW, ReleaseCapture, SIZE_MINIMIZED, SW_SHOWNORMAL,
    SetCapture, SetCursor, SetFocus, SetProcessDpiAwarenessContext, SetTimer, ShowWindow,
    TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT, TrackMouseEvent, VK_CONTROL, VK_DOWN, VK_END,
    VK_ESCAPE, VK_HOME, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE,
    VK_TAB, VK_UP, WM_ACTIVATEAPP, WM_CHAR, WM_CONTEXTMENU, WM_DPICHANGED, WM_DROPFILES,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_GETOBJECT, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSELEAVE, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCALCSIZE, WM_NCHITTEST,
    WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_RBUTTONDOWN,
    WM_RBUTTONUP, WM_SETCURSOR, WM_SETFOCUS, WM_SIZE, WM_TIMER, WS_OVERLAPPEDWINDOW,
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
use text_input::{CARET_TIMER, Clicks};
use window::hwnd;

/// Keys of the title bar's elements, which the window's hit-testing looks for.
pub const CAPTION_DRAG: &str = "caption:drag";
pub const CAPTION_MIN: &str = "caption:min";
pub const CAPTION_MAX: &str = "caption:max";
pub const CAPTION_CLOSE: &str = "caption:close";

/// The tooltip's timer.
const TOOLTIP_TIMER: usize = 1;

/// The F of Ctrl+F (its virtual-key code is its letter's).
const FIND_KEY: i32 = b'F' as i32;

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
    /// The key of the text box Ctrl+F goes to, if any.
    fn search_box(&self) -> Option<&'static str> {
        None
    }
    /// The window's display scaling changed (another monitor, a new setting); `Ctx::scale` has
    /// the new one already.
    fn scale_changed(&self, _scale: f32) -> Option<Self::Message> {
        None
    }
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
        .style(WS_OVERLAPPEDWINDOW as u32)
        .visible(false)
        .on_message(move |hwnd, message, wparam, lparam| {
            route(&handler, hwnd as isize, message, wparam, lparam, min)
        })
        .create()?;
    let handle = window.hwnd() as isize;
    ctx.set_window(handle);
    // Before any message reaches `update`: the app sizes its pictures with it.
    ctx.set_scale(window::dpi(handle) as f32 / 96.0);
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
    // The renderer's swap chain goes before the window.
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
    let shell = guard.as_mut()?;
    let result = shell.message(message as i32, wparam, lparam);
    shell.follow_focus();
    result
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
    /// The text box a dialog opened with (its first control), given the keyboard once.
    dialog_edit: Option<u64>,
    /// The window has the keyboard focus (the caret blinks only then).
    window_focused: bool,
    /// The text box whose caret blinks and for which the IME is on.
    caret_box: Option<u64>,
    /// A high surrogate from `WM_CHAR`, waiting for its pair.
    high_surrogate: Option<u16>,
    /// The last press, for double- and triple-clicks in text boxes.
    last_click: Option<Clicks>,
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
            uia: Uia::new(handle),
            dpi: window::dpi(handle),
            size: (0, 0),
            stale: true,
            redraw: true,
            minimized: false,
            tracking: false,
            dialog_edit: None,
            window_focused: false,
            caret_box: None,
            high_surrogate: None,
            last_click: None,
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

    /// Builds the view and lays it out; keeps the text boxes in step and refreshes UI Automation.
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
        self.ui.follow_menu(&frame);
        self.ui.sync_edits(&frame);
        // A dialog whose first control is a text box (Rename, Add a game) gives it the keyboard
        // as it opens, its text selected.
        let dialog_edit = frame
            .modal_layer()
            .and_then(|_| frame.focusables().first().copied())
            .filter(|id| {
                frame
                    .find(*id)
                    .is_some_and(|hit| matches!(hit.el.kind, Kind::Edit { .. }))
            });
        if dialog_edit != self.dialog_edit {
            self.dialog_edit = dialog_edit;
            if let Some(id) = dialog_edit {
                self.select_all_in(id);
            }
        }
        self.frame = Some(frame);
        self.fit_edits();
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
                // Pictures made for the old pixel size are made again.
                self.ctx.set_scale(self.scale());
                let message = self.app.scale_changed(self.scale());
                self.dispatch_some(message);
                Some(0)
            }
            WM_ACTIVATEAPP => {
                efficiency::set(wparam == 0);
                None
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                self.window_focused = message == WM_SETFOCUS;
                if !self.window_focused && self.ui.edit_menu.take().is_some() {
                    self.redraw = true;
                }
                None
            }
            WM_MOUSEMOVE => {
                self.track_leave(false);
                let (x, y) = self.dips(low(lparam), high(lparam));
                if !self.edit_menu_move(x, y) {
                    self.pointer_move(x, y);
                    self.edit_drag(x);
                }
                Some(0)
            }
            WM_MOUSELEAVE | WM_NCMOUSELEAVE => {
                self.tracking = false;
                self.redraw |= self.ui.pointer_leave();
                None
            }
            WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
                // SAFETY: the keyboard comes to our own window.
                let _ = unsafe { SetFocus(Some(hwnd(self.hwnd))) };
                // SAFETY: capturing the pointer for our own window until the button is released.
                let _ = unsafe { SetCapture(hwnd(self.hwnd)) };
                let (x, y) = self.dips(low(lparam), high(lparam));
                if self.edit_menu_press(x, y) {
                    return Some(0);
                }
                let button = if message == WM_LBUTTONDOWN {
                    Button::Left
                } else {
                    Button::Right
                };
                self.pointer_down(x, y, button);
                if button == Button::Left {
                    self.edit_press(x, y);
                }
                Some(0)
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                // SAFETY: releases the capture taken on the button's press.
                let _ = unsafe { ReleaseCapture() };
                let (x, y) = self.dips(low(lparam), high(lparam));
                self.ui.selecting = None;
                if self.edit_menu_release(x, y) {
                    return Some(0);
                }
                let button = if message == WM_LBUTTONUP {
                    Button::Left
                } else {
                    Button::Right
                };
                // A right-click on a text box opens its menu, and gives it the keyboard.
                if button == Button::Right
                    && let Some(id) = self.edit_at(x, y)
                {
                    self.ui.pressed = None;
                    self.ui.focus = Some(id);
                    self.ui.focus_visible = false;
                    self.open_edit_menu(id, (x, y));
                    return Some(0);
                }
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
            WM_TIMER if wparam == CARET_TIMER => {
                self.blink();
                Some(0)
            }
            WM_TIMER if wparam == TOOLTIP_TIMER => {
                // SAFETY: stops our own timer.
                let _ = unsafe { KillTimer(Some(hwnd(self.hwnd)), TOOLTIP_TIMER) };
                self.redraw |= self.ui.show_tooltip();
                Some(0)
            }
            WM_KEYDOWN if self.edit_menu_key_down(wparam as i32) => Some(0),
            WM_KEYDOWN if wparam as i32 == FIND_KEY && self.control() => {
                self.focus_search();
                Some(0)
            }
            WM_KEYDOWN if self.edit_key(wparam as i32) => Some(0),
            WM_KEYDOWN => {
                let key = key_of(wparam as i32, self.shift())?;
                self.key(key);
                Some(0)
            }
            WM_CHAR if self.edit_char(wparam as u16) => Some(0),
            // The Menu key and Shift+F10 reach us from Windows' default handling as a
            // `WM_CONTEXTMENU` at (-1, -1); a right-click is handled at `WM_RBUTTONUP`.
            WM_CONTEXTMENU if lparam as i32 == -1 => {
                if !self.edit_menu_key() {
                    self.key(Key::Menu);
                }
                Some(0)
            }
            WM_IME_SETCONTEXT => Some(self.ime_set_context(wparam, lparam)),
            WM_IME_STARTCOMPOSITION => {
                self.ime_place();
                Some(0)
            }
            WM_IME_COMPOSITION => {
                self.ime_composition(lparam);
                Some(0)
            }
            WM_IME_ENDCOMPOSITION => {
                self.ime_end();
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
                    self.uia_action(action, lparam as u64, (wparam >> 8) as u32);
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

    fn control(&self) -> bool {
        // SAFETY: a plain query of the keyboard state.
        let state = unsafe { GetKeyState(VK_CONTROL) };
        state < 0
    }

    /// Ctrl+F: the app's search box takes the keyboard, its text selected.
    fn focus_search(&mut self) {
        let Some(key) = self.app.search_box() else {
            return;
        };
        self.ui.edit_menu = None;
        self.select_all_in(key_id(key));
    }

    /// Text box `id` takes the keyboard with its text selected, so typing replaces it.
    fn select_all_in(&mut self, id: u64) {
        let Some(state) = self.ui.edits.get_mut(&id) else {
            return;
        };
        state.edit.select_all();
        self.ui.focus = Some(id);
        self.ui.focus_visible = false;
        self.fit_edit(id);
        self.restart_caret();
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

    /// `WM_SETCURSOR` over the client area: the left-right arrows over a resize grip, the I-beam
    /// over a text box.
    fn set_cursor(&self) -> Option<isize> {
        let frame = self.frame.as_ref()?;
        // IDC_SIZEWE and IDC_IBEAM are resource numbers, declared as narrow strings in these
        // bindings.
        let resource = if self.ui.resize_cursor(frame) {
            IDC_SIZEWE
        } else if self.ui.edit_menu.is_none()
            && self.edit_at(self.ui.pointer.0, self.ui.pointer.1).is_some()
        {
            IDC_IBEAM
        } else {
            return None;
        };
        let name = windows::core::PCWSTR(resource.0.cast());
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
        // Tabbing onto a text box selects its text, as Windows' dialogs do.
        if let Key::Tab { .. } = key
            && let Some(id) = self.edit_target()
        {
            self.select_all_in(id);
        }
    }

    /// What UI Automation asked of an element; `value` is the one RangeValue's SetValue gave.
    fn uia_action(&mut self, action: Action, id: u64, value: u32) {
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
            // Like the keyboard, these scroll the element into view first.
            Action::Focus => {
                self.ui.focus = Some(id);
                self.ui.scroll_into_view(frame, id);
                self.stale = true;
            }
            Action::Expand => {
                if let Kind::DropDown { selected, .. } = &hit.el.kind {
                    let selected = *selected;
                    self.ui.scroll_into_view(frame, id);
                    self.ui.open_dropdown = Some(id);
                    self.ui.dropdown_hover = selected;
                    self.stale = true;
                }
            }
            Action::Collapse => {
                self.ui.open_dropdown = None;
                self.stale = true;
            }
            Action::SetValue => {
                let message = match &hit.el.kind {
                    Kind::Slider {
                        min,
                        max,
                        on_change,
                        ..
                    } if hit.el.enabled => Some(on_change(value.clamp(*min, *max))),
                    _ => None,
                };
                self.dispatch_some(message);
            }
            // A text box's whole text, as if typed over it.
            Action::SetText => {
                let editable = hit.el.enabled && matches!(hit.el.kind, Kind::Edit { .. });
                if let Some(text) = self.uia.take_text(id)
                    && editable
                {
                    self.set_edit_text(id, &text);
                }
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
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
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
