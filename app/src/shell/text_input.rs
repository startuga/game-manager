//! The window's side of the drawn text boxes (`ui::input::EditState`, `ui::text_edit`): it turns
//! window messages into edits of the focused box and sends the new text to the app.
//!
//! - **Keys** (`WM_KEYDOWN`, `edit_action`): arrows, Home/End (Shift selects, Ctrl by word),
//!   Backspace/Delete (Ctrl: a word), Ctrl+A/C/X/V, Ctrl+Insert, Shift+Insert, Shift+Delete,
//!   Ctrl+Z, Ctrl+Y and Ctrl+Shift+Z. Enter sends the box's `on_enter`; Escape empties a box made
//!   with `clear_on_escape` that has text, else goes on to the window; Tab moves focus on.
//! - **Characters** (`WM_CHAR`) are typed, a UTF-16 pair joined first; control characters (the
//!   ones Ctrl+letters and Backspace also send) are dropped.
//! - **The pointer**: a press puts the caret under it (Shift extends the selection), a drag
//!   selects, a double-click selects a word and a triple-click everything. The point becomes an
//!   offset through the text's own layout. Over an enabled box the pointer is the I-beam.
//! - **The clipboard**: text, through `gm_core::clipboard` (no window of ours owns it).
//! - **The caret** blinks at Windows' rate (`GetCaretBlinkTime`) while a box has the keyboard and
//!   the window has the focus; any edit shows it again at once. Each box keeps its text scrolled
//!   so the caret shows (`fit_edit`).
//! - **The text box menu** (right-click, or the Menu key at the caret): Cut, Copy, Paste and
//!   Select all, drawn by the toolkit (`EditMenu`); ↑↓ move, Enter picks, Escape closes, and a
//!   press outside closes it without reaching what is under it.
//! - **IME**: the window's input context is on only while a box has the keyboard. Windows' own
//!   composition window is turned off (`WM_IME_SETCONTEXT`): the composition is drawn in the box,
//!   underlined, the result is typed in, and the candidate list opens under the caret. A
//!   composition left unfinished when the box loses the keyboard is cancelled.

use std::time::Instant;
use windows::Win32::{
    CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, DefWindowProcW,
    GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR, GetCaretBlinkTime, GetDoubleClickTime,
    GetSystemMetrics, HIMC, HWND, IACE_DEFAULT, ISC_SHOWUICOMPOSITIONWINDOW, ImmAssociateContextEx,
    ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionWindow, KillTimer, NI_COMPOSITIONSTR, POINT, RECT,
    SM_CXDOUBLECLK, SM_CYDOUBLECLK, SetTimer, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE,
    VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};

use super::window::hwnd;
use super::{App, Shell};
use crate::ui::el::{Kind, Rect};
use crate::ui::input::{Composition, EditCommand, EditMenu, edit_inner};
use crate::ui::render::{caret_at, edit_spec};
use crate::ui::text_edit::Motion;
use crate::ui::theme;
use windows_canvas::Vector2;

/// The caret's blink timer.
pub const CARET_TIMER: usize = 2;

/// What a key does to a text box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditAction {
    /// Moves the caret; `true` extends the selection (Shift).
    Move(Motion, bool),
    /// Deletes before the caret; `true` a word (Ctrl).
    Backspace(bool),
    /// Deletes after the caret; `true` up to the next word (Ctrl).
    Delete(bool),
    Command(EditCommand),
    Undo,
    Redo,
    /// The box takes the key and does nothing with it: Space is typed as a character, and must
    /// not press a focused button.
    Swallow,
}

/// What `key` (a virtual-key code) does to a text box with Ctrl and Shift as given; `None` for a
/// key that isn't the box's (Tab, Enter and Escape are handled apart; letters are typed through
/// `WM_CHAR`).
pub fn edit_action(key: i32, ctrl: bool, shift: bool) -> Option<EditAction> {
    let letter = |c: u8| key == i32::from(c);
    let by_word = |plain: Motion, word: Motion| if ctrl { word } else { plain };
    Some(match key {
        VK_LEFT => EditAction::Move(by_word(Motion::Left, Motion::WordLeft), shift),
        VK_RIGHT => EditAction::Move(by_word(Motion::Right, Motion::WordRight), shift),
        VK_HOME => EditAction::Move(Motion::Home, shift),
        VK_END => EditAction::Move(Motion::End, shift),
        VK_BACK => EditAction::Backspace(ctrl),
        VK_DELETE if shift && !ctrl => EditAction::Command(EditCommand::Cut),
        VK_DELETE => EditAction::Delete(ctrl),
        VK_INSERT if ctrl && !shift => EditAction::Command(EditCommand::Copy),
        VK_INSERT if shift && !ctrl => EditAction::Command(EditCommand::Paste),
        _ if ctrl && letter(b'A') => EditAction::Command(EditCommand::SelectAll),
        _ if ctrl && letter(b'C') => EditAction::Command(EditCommand::Copy),
        _ if ctrl && letter(b'X') => EditAction::Command(EditCommand::Cut),
        _ if ctrl && letter(b'V') => EditAction::Command(EditCommand::Paste),
        _ if ctrl && shift && letter(b'Z') => EditAction::Redo,
        _ if ctrl && letter(b'Z') => EditAction::Undo,
        _ if ctrl && letter(b'Y') => EditAction::Redo,
        VK_SPACE => EditAction::Swallow,
        _ => return None,
    })
}

/// A UTF-16 unit from `WM_CHAR`, joined with a high surrogate that came first: the character to
/// type, or `None` (a high surrogate waits in `pending` for its pair).
pub fn join_unit(pending: &mut Option<u16>, unit: u16) -> Option<char> {
    match unit {
        0xD800..=0xDBFF => {
            *pending = Some(unit);
            None
        }
        0xDC00..=0xDFFF => {
            let high = pending.take()?;
            char::decode_utf16([high, unit]).next()?.ok()
        }
        _ => {
            *pending = None;
            char::from_u32(u32::from(unit))
        }
    }
}

/// `text`'s first `units` UTF-16 units, as a byte offset: the IME's cursor in its composition.
pub fn byte_offset(text: &str, units: usize) -> usize {
    let mut counted = 0;
    for (at, c) in text.char_indices() {
        if counted >= units {
            return at;
        }
        counted += c.len_utf16();
    }
    text.len()
}

/// Presses close enough in time and place to count as one double- or triple-click.
#[derive(Clone, Copy, Debug)]
pub struct Clicks {
    at: Instant,
    point: (f32, f32),
    count: u32,
}

/// How many clicks a press at `point` makes with the one before (1, 2, 3, then 1 again), given
/// Windows' double-click time and area (`slop`, in DIPs).
pub fn count_click(
    last: Option<Clicks>,
    now: Instant,
    point: (f32, f32),
    time_ms: u32,
    slop: (f32, f32),
) -> Clicks {
    let count = match last {
        Some(last)
            if now.duration_since(last.at).as_millis() <= u128::from(time_ms)
                && (point.0 - last.point.0).abs() <= slop.0
                && (point.1 - last.point.1).abs() <= slop.1
                && last.count < 3 =>
        {
            last.count + 1
        }
        _ => 1,
    };
    Clicks {
        at: now,
        point,
        count,
    }
}

/// The window's input context while it is used; released when dropped.
struct ImeContext {
    window: HWND,
    context: HIMC,
}

impl ImeContext {
    fn of(window: HWND) -> Option<Self> {
        // SAFETY: a plain query of our own window's input context, released by `drop`.
        let context = unsafe { ImmGetContext(window) };
        (!context.is_null()).then_some(Self { window, context })
    }

    /// The composition's string of kind `which` (`GCS_COMPSTR`, `GCS_RESULTSTR`).
    fn string(&self, which: i32) -> String {
        // SAFETY: asks for the size in bytes only (no buffer).
        let bytes = unsafe { ImmGetCompositionStringW(self.context, which as u32, None, 0) };
        let Ok(bytes) = usize::try_from(bytes) else {
            return String::new();
        };
        let mut buffer = vec![0u16; bytes.div_ceil(2)];
        // SAFETY: `buffer` holds `bytes` bytes, as asked.
        let copied = unsafe {
            ImmGetCompositionStringW(
                self.context,
                which as u32,
                Some(buffer.as_mut_ptr().cast()),
                bytes as u32,
            )
        };
        let units = usize::try_from(copied).map_or(0, |copied| copied / 2);
        String::from_utf16_lossy(&buffer[..units.min(buffer.len())])
    }

    /// The IME's cursor in its composition, in UTF-16 units.
    fn cursor(&self) -> usize {
        // SAFETY: GCS_CURSORPOS answers with the position itself.
        let cursor =
            unsafe { ImmGetCompositionStringW(self.context, GCS_CURSORPOS as u32, None, 0) };
        usize::try_from(cursor).unwrap_or(0)
    }

    /// Opens the candidate list under `caret` (client pixels), keeping it off the caret, and
    /// puts a composition window an IME may still show there too.
    fn place(&self, caret: RECT) {
        let point = POINT {
            x: caret.left,
            y: caret.bottom,
        };
        let candidates = CANDIDATEFORM {
            dwIndex: 0,
            dwStyle: CFS_EXCLUDE as u32,
            ptCurrentPos: point,
            rcArea: caret,
        };
        // SAFETY: a valid form for our window's context, read during the call.
        let _ = unsafe { ImmSetCandidateWindow(self.context, &candidates as *const _ as *mut _) };
        let composition = COMPOSITIONFORM {
            dwStyle: CFS_POINT as u32,
            ptCurrentPos: POINT {
                x: caret.left,
                y: caret.top,
            },
            rcArea: caret,
        };
        // SAFETY: as above.
        let _ =
            unsafe { ImmSetCompositionWindow(self.context, &composition as *const _ as *mut _) };
    }

    /// Drops the composition being typed.
    fn cancel(&self) {
        // SAFETY: a plain notification to our window's context.
        let _ =
            unsafe { ImmNotifyIME(self.context, NI_COMPOSITIONSTR as u32, CPS_CANCEL as u32, 0) };
    }
}

impl Drop for ImeContext {
    fn drop(&mut self) {
        // SAFETY: the context `of` got for this window.
        let _ = unsafe { ImmReleaseContext(self.window, self.context) };
    }
}

impl<A: App> Shell<A> {
    /// The text box with the keyboard.
    pub(super) fn edit_target(&self) -> Option<u64> {
        self.frame
            .as_ref()
            .and_then(|frame| self.ui.focused_edit(frame))
    }

    /// The enabled text box under a point, if no dialog or menu covers it.
    pub(super) fn edit_at(&self, x: f32, y: f32) -> Option<u64> {
        let hit = self.frame.as_ref()?.hit(x, y)?;
        (hit.el.enabled && matches!(hit.el.kind, Kind::Edit { .. })).then_some(hit.placed.id)
    }

    /// The text a box shows and its caret in it: the composition stands at the caret while the
    /// IME types.
    fn shown_text(&self, id: u64) -> Option<(String, usize)> {
        let state = self.ui.edits.get(&id)?;
        let text = state.edit.text();
        Some(match self.ui.composition.as_ref().filter(|c| c.id == id) {
            Some(composition) => {
                let selection = state.edit.selection();
                (
                    format!(
                        "{}{}{}",
                        &text[..selection.start],
                        composition.text,
                        &text[selection.end..]
                    ),
                    selection.start + composition.cursor.min(composition.text.len()),
                )
            }
            None => (text.to_owned(), state.edit.caret()),
        })
    }

    /// Scrolls a box's text so its caret shows, and no further than its end.
    pub(super) fn fit_edit(&mut self, id: u64) {
        let Some(rect) = self
            .frame
            .as_ref()
            .and_then(|frame| frame.find(id))
            .map(|hit| hit.placed.rect)
        else {
            return;
        };
        let Some((shown, caret)) = self.shown_text(id) else {
            return;
        };
        let width = edit_inner(rect).w;
        let spec = edit_spec(&shown, theme::TEXT);
        let (caret_x, end_x) = self.text.with_layout(&spec, f32::INFINITY, |layout| {
            (
                caret_at(layout, &shown, caret).left,
                caret_at(layout, &shown, shown.len()).left,
            )
        });
        let Some(state) = self.ui.edits.get_mut(&id) else {
            return;
        };
        let mut scroll = state.scroll;
        if caret_x - scroll > width - 1.0 {
            scroll = caret_x - width + 1.0;
        }
        if caret_x < scroll {
            scroll = caret_x;
        }
        state.scroll = scroll.min((end_x - width + 1.0).max(0.0)).max(0.0);
    }

    /// Every box's scroll, after a layout (a box may have changed width).
    pub(super) fn fit_edits(&mut self) {
        let ids: Vec<u64> = self.ui.edits.keys().copied().collect();
        for id in ids {
            self.fit_edit(id);
        }
    }

    /// The caret's rectangle in a box, in client DIPs.
    fn caret_rect(&self, id: u64) -> Option<Rect> {
        let rect = self.frame.as_ref()?.find(id)?.placed.rect;
        let inner = edit_inner(rect);
        let scroll = self.ui.edits.get(&id)?.scroll;
        let (shown, caret) = self.shown_text(id)?;
        let spec = edit_spec(&shown, theme::TEXT);
        let (bounds, height) = self.text.with_layout(&spec, f32::INFINITY, |layout| {
            (caret_at(layout, &shown, caret), layout.metrics().height)
        });
        let top = (rect.y + ((rect.h - height) / 2.0).max(0.0)).round();
        Some(Rect::new(
            inner.x - scroll + bounds.left,
            top + bounds.top,
            1.0,
            bounds.height(),
        ))
    }

    /// The offset in a box's text under the x coordinate `x`.
    fn edit_offset(&self, id: u64, x: f32) -> Option<usize> {
        let rect = self.frame.as_ref()?.find(id)?.placed.rect;
        let state = self.ui.edits.get(&id)?;
        let inner = edit_inner(rect);
        let spec = edit_spec(state.edit.text(), theme::TEXT);
        let point = Vector2::new(x - inner.x + state.scroll, 1.0);
        Some(self.text.with_layout(&spec, f32::INFINITY, |layout| {
            layout.hit_test_point(point).caret_position()
        }))
    }

    /// Shows the caret now and starts its blink over.
    pub(super) fn restart_caret(&mut self) {
        self.ui.caret_on = true;
        self.redraw = true;
        // SAFETY: a plain query.
        let blink = unsafe { GetCaretBlinkTime() };
        // INFINITE: the user turned blinking off; the caret stays.
        if blink == 0 || blink == u32::MAX {
            return;
        }
        // SAFETY: a timer on our own window; the same id replaces a running one.
        unsafe { SetTimer(Some(hwnd(self.hwnd)), CARET_TIMER, blink, None) };
    }

    /// `WM_TIMER` for the caret: it shows or hides.
    pub(super) fn blink(&mut self) {
        if self.caret_box.is_some() {
            self.ui.caret_on = !self.ui.caret_on;
            self.redraw = true;
        } else {
            // SAFETY: stops our own timer.
            let _ = unsafe { KillTimer(Some(hwnd(self.hwnd)), CARET_TIMER) };
        }
    }

    /// After every message: when the box with the keyboard changed (or the window got or lost
    /// the focus), the caret, the IME and a composition follow.
    pub(super) fn follow_focus(&mut self) {
        let target = self.edit_target().filter(|_| self.window_focused);
        if target == self.caret_box {
            return;
        }
        let window = hwnd(self.hwnd);
        if self.caret_box.is_some()
            && self.ui.composition.take().is_some()
            && let Some(context) = ImeContext::of(window)
        {
            context.cancel();
        }
        self.caret_box = target;
        let flags = if target.is_some() { IACE_DEFAULT } else { 0 };
        // SAFETY: our own window; no context of ours (null) with IACE_DEFAULT gives it back its
        // default one, and with no flag leaves it none, so keys elsewhere don't compose.
        let _ = unsafe { ImmAssociateContextEx(window, std::ptr::null_mut(), flags as u32) };
        if target.is_some() {
            self.restart_caret();
        } else {
            self.ui.caret_on = false;
            self.redraw = true;
            // SAFETY: stops our own timer.
            let _ = unsafe { KillTimer(Some(window), CARET_TIMER) };
        }
    }

    /// A box's text changed in it: the app gets the new text, the caret shows at once.
    fn edit_changed(&mut self, id: u64, text: String) {
        self.fit_edit(id);
        self.restart_caret();
        let message = self
            .frame
            .as_ref()
            .and_then(|frame| frame.find(id))
            .and_then(|hit| match &hit.el.kind {
                Kind::Edit { on_change, .. } => Some(on_change(text)),
                _ => None,
            });
        self.dispatch_some(message);
    }

    /// The caret or selection moved, with no change to the text.
    fn edit_moved(&mut self, id: u64) {
        self.fit_edit(id);
        self.restart_caret();
    }

    /// A key for the box with the keyboard. Returns whether the box took it.
    pub(super) fn edit_key(&mut self, key: i32) -> bool {
        let Some(id) = self.edit_target() else {
            return false;
        };
        match key {
            VK_RETURN => {
                let message = self.on_enter(id);
                self.dispatch_some(message);
                return true;
            }
            VK_ESCAPE => {
                let has_text = self
                    .ui
                    .edits
                    .get(&id)
                    .is_some_and(|state| !state.edit.text().is_empty());
                if has_text && self.clears_on_escape(id) {
                    if let Some(state) = self.ui.edits.get_mut(&id) {
                        state.edit.sync("");
                    }
                    self.edit_changed(id, String::new());
                    return true;
                }
                return false;
            }
            VK_TAB | VK_UP | VK_DOWN | VK_PRIOR | VK_NEXT => return false,
            _ => {}
        }
        let Some(action) = edit_action(key, self.control(), self.shift()) else {
            return false;
        };
        self.edit_do(id, action);
        true
    }

    fn on_enter(&self, id: u64) -> Option<A::Message> {
        match &self.frame.as_ref()?.find(id)?.el.kind {
            Kind::Edit { on_enter, .. } => on_enter.clone(),
            _ => None,
        }
    }

    fn clears_on_escape(&self, id: u64) -> bool {
        self.frame
            .as_ref()
            .and_then(|frame| frame.find(id))
            .is_some_and(|hit| {
                matches!(
                    hit.el.kind,
                    Kind::Edit {
                        clear_on_escape: true,
                        ..
                    }
                )
            })
    }

    /// Does what a key asked of box `id`.
    fn edit_do(&mut self, id: u64, action: EditAction) {
        if let EditAction::Command(command) = action {
            self.edit_command(id, command);
            return;
        }
        let Some(state) = self.ui.edits.get_mut(&id) else {
            return;
        };
        let changed = match action {
            EditAction::Move(motion, extend) => {
                state.edit.move_caret(motion, extend);
                None
            }
            EditAction::Backspace(word) => state.edit.backspace(word),
            EditAction::Delete(word) => state.edit.delete(word),
            EditAction::Undo => state.edit.undo(),
            EditAction::Redo => state.edit.redo(),
            EditAction::Swallow | EditAction::Command(_) => return,
        };
        match changed {
            Some(text) => self.edit_changed(id, text),
            None => self.edit_moved(id),
        }
    }

    /// Replaces a box's whole text, as if typed over it (UI Automation's SetValue).
    pub(super) fn set_edit_text(&mut self, id: u64, text: &str) {
        let Some(state) = self.ui.edits.get_mut(&id) else {
            return;
        };
        state.edit.select_all();
        if let Some(text) = state.edit.paste(text) {
            self.edit_changed(id, text);
        }
    }

    /// Cut, Copy, Paste or Select all in box `id` (a key or the menu).
    pub(super) fn edit_command(&mut self, id: u64, command: EditCommand) {
        let Some(state) = self.ui.edits.get_mut(&id) else {
            return;
        };
        let changed = match command {
            EditCommand::Copy => {
                if state.edit.has_selection() {
                    let _ = gm_core::clipboard::put_text(state.edit.selected_text());
                }
                None
            }
            EditCommand::Cut => {
                // Taken from the box only once it is on the clipboard.
                let copied = state.edit.has_selection()
                    && gm_core::clipboard::put_text(state.edit.selected_text()).is_ok();
                copied
                    .then(|| state.edit.cut())
                    .flatten()
                    .map(|(_, text)| text)
            }
            EditCommand::Paste => match gm_core::clipboard::text() {
                Ok(Some(pasted)) => state.edit.paste(&pasted),
                _ => None,
            },
            EditCommand::SelectAll => {
                state.edit.select_all();
                None
            }
        };
        match changed {
            Some(text) => self.edit_changed(id, text),
            None => self.edit_moved(id),
        }
    }

    /// `WM_CHAR`: a character typed into the box with the keyboard. Returns whether a box took
    /// it.
    pub(super) fn edit_char(&mut self, unit: u16) -> bool {
        let Some(id) = self.edit_target() else {
            self.high_surrogate = None;
            return false;
        };
        let Some(c) = join_unit(&mut self.high_surrogate, unit) else {
            return true;
        };
        if c.is_control() {
            return true;
        }
        let typed = self
            .ui
            .edits
            .get_mut(&id)
            .and_then(|state| state.edit.insert(c.encode_utf8(&mut [0; 4])));
        match typed {
            Some(text) => self.edit_changed(id, text),
            None => self.edit_moved(id),
        }
        true
    }

    /// A left press: when it landed in a text box, the caret goes there (Shift extends the
    /// selection), a double-click selects the word and a triple-click everything.
    pub(super) fn edit_press(&mut self, x: f32, y: f32) {
        let Some(id) = self.edit_at(x, y) else {
            self.last_click = None;
            return;
        };
        // SAFETY: plain queries of the user's double-click settings.
        let time = unsafe { GetDoubleClickTime() };
        // SAFETY: as above.
        let slop_x = unsafe { GetSystemMetrics(SM_CXDOUBLECLK) };
        // SAFETY: as above.
        let slop_y = unsafe { GetSystemMetrics(SM_CYDOUBLECLK) };
        let slop = (slop_x, slop_y);
        let scale = self.scale();
        let clicks = count_click(
            self.last_click,
            Instant::now(),
            (x, y),
            time,
            (slop.0 as f32 / scale / 2.0, slop.1 as f32 / scale / 2.0),
        );
        self.last_click = Some(clicks);
        let Some(offset) = self.edit_offset(id, x) else {
            return;
        };
        let extend = self.shift();
        let Some(state) = self.ui.edits.get_mut(&id) else {
            return;
        };
        match clicks.count {
            2 => state.edit.select_word_at(offset),
            3 => state.edit.select_all(),
            _ => {
                state.edit.set_caret(offset, extend);
                self.ui.selecting = Some(id);
            }
        }
        self.edit_moved(id);
    }

    /// The pointer moved with the button held after a press in a box: the selection follows.
    pub(super) fn edit_drag(&mut self, x: f32) {
        let Some(id) = self.ui.selecting else {
            return;
        };
        let Some(offset) = self.edit_offset(id, x) else {
            return;
        };
        if let Some(state) = self.ui.edits.get_mut(&id)
            && state.edit.caret() != offset
        {
            state.edit.set_caret(offset, true);
            self.edit_moved(id);
        }
    }

    /// Opens the text box menu of box `id` at a point, with what can act now.
    pub(super) fn open_edit_menu(&mut self, id: u64, at: (f32, f32)) {
        let Some(state) = self.ui.edits.get(&id) else {
            return;
        };
        let selected = state.edit.has_selection();
        let enabled = [
            selected,
            selected,
            gm_core::clipboard::has_text(),
            !state.edit.text().is_empty(),
        ];
        self.ui.tooltip = None;
        self.ui.edit_menu = Some(EditMenu::new(id, at, enabled, self.ui.window));
        self.redraw = true;
    }

    /// The Menu key in a box: its menu opens under the caret.
    pub(super) fn edit_menu_key(&mut self) -> bool {
        let Some(id) = self.edit_target() else {
            return false;
        };
        let at = self
            .caret_rect(id)
            .map_or((self.ui.pointer.0, self.ui.pointer.1), |caret| {
                (caret.x, caret.bottom())
            });
        self.open_edit_menu(id, at);
        if let Some(menu) = &mut self.ui.edit_menu {
            menu.step(true);
        }
        true
    }

    /// The pointer over an open text box menu. Returns whether the menu took the event.
    pub(super) fn edit_menu_move(&mut self, x: f32, y: f32) -> bool {
        let Some(menu) = &mut self.ui.edit_menu else {
            return false;
        };
        let hover = menu.item_at(x, y).filter(|index| menu.enabled[*index]);
        if hover != menu.hover {
            menu.hover = hover;
            self.redraw = true;
        }
        true
    }

    /// A press while the menu is open: inside it waits for the release; outside, the menu closes
    /// and the press is spent. Returns whether the menu took it.
    pub(super) fn edit_menu_press(&mut self, x: f32, y: f32) -> bool {
        let Some(menu) = &self.ui.edit_menu else {
            return false;
        };
        if !menu.rect.contains(x, y) {
            self.ui.edit_menu = None;
            self.redraw = true;
        }
        true
    }

    /// A release while the menu is open: on a row that can act, its command. Returns whether
    /// the menu took it.
    pub(super) fn edit_menu_release(&mut self, x: f32, y: f32) -> bool {
        let Some(menu) = &self.ui.edit_menu else {
            return false;
        };
        let picked = menu
            .item_at(x, y)
            .and_then(|index| menu.command(index))
            .map(|command| (menu.id, command));
        if let Some((id, command)) = picked {
            self.ui.edit_menu = None;
            self.redraw = true;
            self.edit_command(id, command);
        }
        true
    }

    /// A key while the menu is open: ↑↓ Home End move, Enter and Space pick, Escape and Tab
    /// close; any other key closes it and goes on. Returns whether the menu took the key.
    pub(super) fn edit_menu_key_down(&mut self, key: i32) -> bool {
        let Some(menu) = &mut self.ui.edit_menu else {
            return false;
        };
        self.redraw = true;
        match key {
            VK_DOWN | VK_HOME => menu.step(true),
            VK_UP | VK_END => menu.step(false),
            VK_RETURN | VK_SPACE => {
                let picked = menu
                    .hover
                    .and_then(|index| menu.command(index))
                    .map(|command| (menu.id, command));
                self.ui.edit_menu = None;
                if let Some((id, command)) = picked {
                    self.edit_command(id, command);
                }
            }
            VK_ESCAPE | VK_TAB => self.ui.edit_menu = None,
            _ => {
                self.ui.edit_menu = None;
                return false;
            }
        }
        true
    }

    /// `WM_IME_SETCONTEXT`: Windows' composition window stays hidden; the box draws it.
    pub(super) fn ime_set_context(&self, wparam: usize, lparam: isize) -> isize {
        let lparam = lparam & !(ISC_SHOWUICOMPOSITIONWINDOW as isize);
        // SAFETY: the default handling of the message for our own window.
        unsafe {
            DefWindowProcW(
                hwnd(self.hwnd),
                windows::Win32::WM_IME_SETCONTEXT as u32,
                wparam,
                lparam,
            )
        }
    }

    /// Places the IME's candidate list at the caret of the box with the keyboard.
    pub(super) fn ime_place(&self) {
        let Some(caret) = self.edit_target().and_then(|id| self.caret_rect(id)) else {
            return;
        };
        let Some(context) = ImeContext::of(hwnd(self.hwnd)) else {
            return;
        };
        let scale = self.scale();
        let pixels = |value: f32| (value * scale).round() as i32;
        context.place(RECT {
            left: pixels(caret.x),
            top: pixels(caret.y),
            right: pixels(caret.right()),
            bottom: pixels(caret.bottom()),
        });
    }

    /// `WM_IME_COMPOSITION`: the result is typed in; the composition so far is drawn at the
    /// caret.
    pub(super) fn ime_composition(&mut self, flags: isize) {
        let Some(id) = self.edit_target() else {
            return;
        };
        let Some(context) = ImeContext::of(hwnd(self.hwnd)) else {
            return;
        };
        if flags & GCS_RESULTSTR as isize != 0 {
            let result = context.string(GCS_RESULTSTR);
            self.ui.composition = None;
            let typed = self
                .ui
                .edits
                .get_mut(&id)
                .and_then(|state| state.edit.insert(&result));
            if let Some(text) = typed {
                self.edit_changed(id, text);
            }
        }
        if flags & GCS_COMPSTR as isize != 0 {
            let text = context.string(GCS_COMPSTR);
            let cursor = byte_offset(&text, context.cursor());
            self.ui.composition = (!text.is_empty()).then_some(Composition { id, text, cursor });
            self.edit_moved(id);
        }
        drop(context);
        self.ime_place();
    }

    /// `WM_IME_ENDCOMPOSITION`.
    pub(super) fn ime_end(&mut self) {
        if self.ui.composition.take().is_some() {
            self.redraw = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn keys_map_to_what_a_text_box_does() {
        use EditAction::*;
        let key = |code: u8| i32::from(code);
        assert_eq!(
            edit_action(VK_LEFT, false, false),
            Some(Move(Motion::Left, false))
        );
        assert_eq!(
            edit_action(VK_LEFT, true, true),
            Some(Move(Motion::WordLeft, true))
        );
        assert_eq!(
            edit_action(VK_RIGHT, true, false),
            Some(Move(Motion::WordRight, false))
        );
        assert_eq!(
            edit_action(VK_END, false, true),
            Some(Move(Motion::End, true))
        );
        assert_eq!(edit_action(VK_BACK, true, false), Some(Backspace(true)));
        assert_eq!(edit_action(VK_DELETE, false, false), Some(Delete(false)));
        assert_eq!(
            edit_action(VK_DELETE, false, true),
            Some(Command(EditCommand::Cut))
        );
        assert_eq!(
            edit_action(VK_INSERT, true, false),
            Some(Command(EditCommand::Copy))
        );
        assert_eq!(
            edit_action(VK_INSERT, false, true),
            Some(Command(EditCommand::Paste))
        );
        assert_eq!(
            edit_action(key(b'A'), true, false),
            Some(Command(EditCommand::SelectAll))
        );
        assert_eq!(
            edit_action(key(b'V'), true, false),
            Some(Command(EditCommand::Paste))
        );
        assert_eq!(edit_action(key(b'Z'), true, false), Some(Undo));
        assert_eq!(edit_action(key(b'Z'), true, true), Some(Redo));
        assert_eq!(edit_action(key(b'Y'), true, false), Some(Redo));
        assert_eq!(edit_action(VK_SPACE, false, false), Some(Swallow));
        // Letters are typed as characters; Tab, Enter and Escape are the window's.
        assert_eq!(edit_action(key(b'A'), false, false), None);
        assert_eq!(edit_action(VK_TAB, false, false), None);
        assert_eq!(edit_action(VK_RETURN, false, false), None);
    }

    #[test]
    fn utf16_pairs_are_joined_into_one_character() {
        let mut pending = None;
        assert_eq!(join_unit(&mut pending, u16::from(b'a')), Some('a'));
        // 😀 is D83D DE00.
        assert_eq!(join_unit(&mut pending, 0xD83D), None);
        assert_eq!(join_unit(&mut pending, 0xDE00), Some('😀'));
        // A low surrogate alone, or a high one followed by a plain unit, types nothing broken.
        assert_eq!(join_unit(&mut pending, 0xDE00), None);
        assert_eq!(join_unit(&mut pending, 0xD83D), None);
        assert_eq!(join_unit(&mut pending, u16::from(b'b')), Some('b'));
        assert_eq!(pending, None);
        assert_eq!(join_unit(&mut pending, 0x00E9), Some('é'));
    }

    #[test]
    fn the_ime_cursor_becomes_a_byte_offset() {
        assert_eq!(byte_offset("かな", 0), 0);
        assert_eq!(byte_offset("かな", 1), 3);
        assert_eq!(byte_offset("かな", 2), 6);
        assert_eq!(byte_offset("😀a", 2), 4);
        assert_eq!(byte_offset("ab", 9), 2);
    }

    #[test]
    fn presses_close_together_count_as_double_and_triple_clicks() {
        let start = Instant::now();
        let later = |ms: u64| start + Duration::from_millis(ms);
        let press = |last, ms, x| count_click(last, later(ms), (x, 10.0), 500, (2.0, 2.0));
        let one = press(None, 0, 10.0);
        let two = press(Some(one), 200, 11.0);
        let three = press(Some(two), 400, 10.0);
        let four = press(Some(three), 450, 10.0);
        assert_eq!(
            (one.count, two.count, three.count, four.count),
            (1, 2, 3, 1)
        );
        // Too slow, or too far.
        assert_eq!(press(Some(one), 600, 10.0).count, 1);
        assert_eq!(press(Some(one), 100, 20.0).count, 1);
    }
}
