//! Text boxes are real Win32 EDIT controls, so typing, IME, the clipboard, undo, the caret and
//! screen readers are Windows' own. The view draws their rounded frame (`widgets::text_box`), and
//! after each layout `sync` creates, moves, shows or hides one EDIT per frame, keyed by element id.
//!
//! - **Look**: borderless, the theme's font at the window's DPI, light text on the input colour
//!   (`WM_CTLCOLOREDIT`, answered by `ctl_color` without touching the app's state), a placeholder
//!   (`EM_SETCUEBANNER`). Single-line EDITs draw at their top, so each is as high as one line and
//!   centred in its frame.
//! - **Text**: what the view says is written into the EDIT only while it has no focus, so typing
//!   is never interrupted. Typed text reaches the app through `EN_CHANGE`.
//! - **Keys**: a subclass passes Tab, Escape, Enter and Ctrl+F to the window (`WM_EDIT_KEY`); an
//!   EDIT on its own would only beep. Tab moves focus; Enter sends the box's `on_enter`; Escape
//!   empties a box made with `clear_on_escape` (`set_text`), else closes what is open; Ctrl+F
//!   goes to the search box (`App::search_box`, `focus_select_all`).
//! - **Clipping**: a child window isn't clipped by a scroll view, so an EDIT shows only while its
//!   whole frame is visible.

use std::collections::HashMap;
use std::sync::OnceLock;
use windows::Win32::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, CreateWindowExW,
    DEFAULT_CHARSET, DEFAULT_PITCH, DWORD_PTR, DefSubclassProc, DeleteObject, DestroyWindow,
    EC_LEFTMARGIN, EC_RIGHTMARGIN, EM_SETCUEBANNER, EM_SETMARGINS, EM_SETSEL, ES_AUTOHSCROLL,
    EnableWindow, FW_NORMAL, GetFocus, GetKeyState, GetParent, GetWindowTextLengthW,
    GetWindowTextW, HDC, HFONT, HWND, LPARAM, LRESULT, OUT_DEFAULT_PRECIS, PostMessageW, SW_HIDE,
    SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetBkColor, SetFocus, SetTextColor,
    SetWindowPos, SetWindowSubclass, SetWindowTextW, ShowWindow, UINT, UINT_PTR, VK_CONTROL,
    VK_ESCAPE, VK_RETURN, VK_TAB, WM_APP, WM_CHAR, WM_KEYDOWN, WM_SETFONT, WPARAM, WS_CHILD,
    WS_CLIPSIBLINGS, WS_TABSTOP,
};
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::ui::el::{El, Kind, Rect};
use crate::ui::layout::Placed;
use crate::ui::theme;

/// Posted by an EDIT for Tab, Escape, Enter or Ctrl+F: `wparam` is the key, `lparam` the EDIT's
/// window.
pub const WM_EDIT_KEY: i32 = WM_APP + 3;

/// The F of Ctrl+F (its virtual-key code is its letter's).
pub const FIND_KEY: i32 = b'F' as i32;

/// One text box of the frame.
#[derive(Clone, Debug, PartialEq)]
pub struct EditSpec {
    pub id: u64,
    /// The frame drawn around it, in DIPs.
    pub rect: Rect,
    pub text: String,
    pub placeholder: String,
    pub visible: bool,
    pub enabled: bool,
}

/// The text boxes of a frame.
pub fn collect<M>(el: &El<M>, placed: &Placed, out: &mut Vec<EditSpec>) {
    if let Kind::Edit {
        text, placeholder, ..
    } = &el.kind
    {
        out.push(EditSpec {
            id: placed.id,
            rect: placed.rect,
            text: text.clone(),
            placeholder: placeholder.clone(),
            visible: placed.clip == placed.rect && !placed.rect.is_empty(),
            enabled: el.enabled,
        });
    }
    for (child, child_placed) in el.children.iter().zip(&placed.children) {
        collect(child, child_placed, out);
    }
}

/// A theme colour as GDI's `0x00BBGGRR`.
fn rgb(color: windows_canvas::ColorF) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    byte(color.r) | byte(color.g) << 8 | byte(color.b) << 16
}

/// The input colour's brush, made once for the app's life.
fn input_brush() -> isize {
    static BRUSH: OnceLock<isize> = OnceLock::new();
    *BRUSH.get_or_init(|| {
        // SAFETY: a plain GDI brush, kept for the process's life.
        let brush = unsafe { CreateSolidBrush(rgb(theme::INPUT)) };
        brush as isize
    })
}

/// `WM_CTLCOLOREDIT`: light text on the input colour.
pub fn ctl_color(hdc: usize) -> isize {
    let hdc = hdc as HDC;
    // SAFETY: the device context Windows passed with the message, for this call only.
    unsafe { SetTextColor(hdc, rgb(theme::TEXT)) };
    // SAFETY: as above.
    unsafe { SetBkColor(hdc, rgb(theme::INPUT)) };
    input_brush()
}

/// A GDI font, deleted when dropped.
struct Font(HFONT);

impl Drop for Font {
    fn drop(&mut self) {
        // SAFETY: the font was created by `font_for` and no EDIT uses it any more.
        let _ = unsafe { DeleteObject(self.0.cast()) };
    }
}

fn font_for(dpi: u32) -> Font {
    // A negative height is the character height, in pixels: the DIP size at this DPI.
    let height = -((theme::TEXT_SIZE * dpi as f32 / 96.0).round() as i32);
    // SAFETY: plain font creation with a valid face name.
    let font = unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            DEFAULT_PITCH as u32,
            &HSTRING::from(theme::FONT_TEXT),
        )
    };
    Font(font)
}

struct Edit {
    hwnd: isize,
    placeholder: String,
    shown: bool,
}

pub struct EditBoxes {
    parent: isize,
    edits: HashMap<u64, Edit>,
    font: Option<(u32, Font)>,
}

impl EditBoxes {
    pub fn new(parent: isize) -> Self {
        Self {
            parent,
            edits: HashMap::new(),
            font: None,
        }
    }

    /// Makes the EDITs match the frame's text boxes.
    pub fn sync(&mut self, specs: &[EditSpec], dpi: u32) {
        if self.font.as_ref().map(|(at, _)| *at) != Some(dpi) {
            self.font = Some((dpi, font_for(dpi)));
            for edit in self.edits.values() {
                self.apply_font(edit.hwnd);
            }
        }
        let scale = dpi as f32 / 96.0;
        let line = (theme::TEXT_SIZE * 1.4).ceil();
        for spec in specs {
            if !self.edits.contains_key(&spec.id) {
                let Some(hwnd) = self.create() else {
                    continue;
                };
                self.edits.insert(
                    spec.id,
                    Edit {
                        hwnd,
                        placeholder: String::new(),
                        shown: false,
                    },
                );
            }
            let Some(edit) = self.edits.get_mut(&spec.id) else {
                continue;
            };
            let window = edit.hwnd as HWND;
            if edit.placeholder != spec.placeholder {
                edit.placeholder.clone_from(&spec.placeholder);
                let cue = HSTRING::from(spec.placeholder.as_str());
                // SAFETY: the EDIT copies the cue text during the call.
                unsafe { SendMessageW(window, EM_SETCUEBANNER as u32, 0, cue.as_ptr() as isize) };
            }
            // SAFETY: a plain query of the thread's focus window.
            let focused = unsafe { GetFocus() } == window;
            if !focused && text_of(edit.hwnd) != spec.text {
                // SAFETY: the EDIT copies the text during the call.
                let _ = unsafe { SetWindowTextW(window, &HSTRING::from(spec.text.as_str())) };
            }
            let x = ((spec.rect.x + 8.0) * scale).round() as i32;
            let height = (line * scale).round() as i32;
            let y = ((spec.rect.y + spec.rect.h / 2.0) * scale).round() as i32 - height / 2;
            let width = (((spec.rect.w - 16.0) * scale).round() as i32).max(1);
            let flags = SWP_NOZORDER | SWP_NOACTIVATE;
            // SAFETY: moving a child window of ours.
            let _ = unsafe { SetWindowPos(window, None, x, y, width, height, flags as u32) };
            // SAFETY: enabling or disabling a child window of ours.
            let _ = unsafe { EnableWindow(window, spec.enabled) };
            if spec.visible != edit.shown {
                edit.shown = spec.visible;
                let show = if spec.visible { SW_SHOWNA } else { SW_HIDE };
                // SAFETY: showing or hiding a child window of ours.
                let _ = unsafe { ShowWindow(window, show) };
            }
        }
        // EDITs no longer in the frame.
        let gone: Vec<u64> = self
            .edits
            .keys()
            .filter(|id| !specs.iter().any(|spec| spec.id == **id))
            .copied()
            .collect();
        for id in gone {
            if let Some(edit) = self.edits.remove(&id) {
                // SAFETY: a child window this module created; nothing else refers to it.
                let _ = unsafe { DestroyWindow(edit.hwnd as HWND) };
            }
        }
    }

    fn create(&self) -> Option<isize> {
        let style = WS_CHILD | WS_CLIPSIBLINGS | WS_TABSTOP | ES_AUTOHSCROLL;
        // SAFETY: a child EDIT of our window; the class is the system's.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                w!("EDIT"),
                PCWSTR::null(),
                style as u32,
                0,
                0,
                0,
                0,
                Some(self.parent as HWND),
                None,
                None,
                None,
            )
        };
        if hwnd.is_null() {
            return None;
        }
        // SAFETY: subclassing a window this thread created; the procedure is a plain function.
        let _ = unsafe { SetWindowSubclass(hwnd, Some(keys), 1, 0) };
        let margins = 4 | (4 << 16);
        // SAFETY: a plain margins message to our EDIT.
        unsafe {
            SendMessageW(
                hwnd,
                EM_SETMARGINS as u32,
                (EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize,
                margins,
            )
        };
        self.apply_font(hwnd as isize);
        Some(hwnd as isize)
    }

    fn apply_font(&self, hwnd: isize) {
        if let Some((_, font)) = &self.font {
            // SAFETY: the font outlives the EDITs using it (replaced only after re-applying).
            unsafe { SendMessageW(hwnd as HWND, WM_SETFONT as u32, font.0 as usize, 1) };
        }
    }

    /// The element whose EDIT this window is.
    pub fn id_of(&self, hwnd: isize) -> Option<u64> {
        self.edits
            .iter()
            .find(|(_, edit)| edit.hwnd == hwnd)
            .map(|(id, _)| *id)
    }

    /// Every EDIT, for UI Automation.
    pub fn windows(&self) -> Vec<(u64, isize)> {
        self.edits
            .iter()
            .map(|(id, edit)| (*id, edit.hwnd))
            .collect()
    }

    /// Gives an EDIT the keyboard focus.
    pub fn focus(&self, id: u64) -> bool {
        let Some(edit) = self.edits.get(&id).filter(|edit| edit.shown) else {
            return false;
        };
        // SAFETY: focusing a child window of ours.
        let _ = unsafe { SetFocus(Some(edit.hwnd as HWND)) };
        true
    }

    /// Gives an EDIT the keyboard focus with its text selected, so typing replaces it (Ctrl+F).
    pub fn focus_select_all(&self, id: u64) -> bool {
        if !self.focus(id) {
            return false;
        }
        if let Some(edit) = self.edits.get(&id) {
            // SAFETY: selecting all of our EDIT's text (0 to -1).
            unsafe { SendMessageW(edit.hwnd as HWND, EM_SETSEL as u32, 0, -1) };
        }
        true
    }

    /// Writes an EDIT's text even while it has the focus (Escape emptying the search). The
    /// EDIT's `EN_CHANGE` then reaches the window while it is busy and is dropped, so the caller
    /// tells the app itself.
    pub fn set_text(&self, id: u64, text: &str) {
        if let Some(edit) = self.edits.get(&id) {
            // SAFETY: the EDIT copies the text during the call.
            let _ = unsafe { SetWindowTextW(edit.hwnd as HWND, &HSTRING::from(text)) };
        }
    }
}

impl Drop for EditBoxes {
    fn drop(&mut self) {
        for edit in self.edits.values() {
            // SAFETY: child windows this module created, destroyed before their font.
            let _ = unsafe { DestroyWindow(edit.hwnd as HWND) };
        }
    }
}

/// What an EDIT holds.
pub fn text_of(hwnd: isize) -> String {
    let window = hwnd as HWND;
    // SAFETY: a plain query.
    let length = unsafe { GetWindowTextLengthW(window) }.max(0) as usize;
    let mut buffer = vec![0u16; length + 1];
    // SAFETY: `buffer` holds `length + 1` characters, the terminator included.
    let copied = unsafe { GetWindowTextW(window, PWSTR(buffer.as_mut_ptr()), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..(copied.max(0) as usize).min(length)])
}

/// The EDITs' subclass: Tab, Escape, Enter and Ctrl+F go to the window.
unsafe extern "system" fn keys(
    hwnd: HWND,
    message: UINT,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: UINT_PTR,
    _data: DWORD_PTR,
) -> LRESULT {
    let key = wparam as i32;
    // SAFETY: a plain query of the keyboard state.
    let control = key == FIND_KEY && unsafe { GetKeyState(VK_CONTROL) } < 0;
    let ours = key == VK_TAB || key == VK_ESCAPE || key == VK_RETURN || control;
    if message == WM_KEYDOWN as u32 && ours {
        // SAFETY: a plain query of our EDIT's parent.
        let parent = unsafe { GetParent(hwnd) };
        if !parent.is_null() {
            // SAFETY: posting to our own window.
            let _ =
                unsafe { PostMessageW(Some(parent), WM_EDIT_KEY as u32, wparam, hwnd as isize) };
        }
        return 0;
    }
    // The characters of those keys (Ctrl+F's is 6) would only make the EDIT beep.
    if message == WM_CHAR as u32 && matches!(wparam, 6 | 9 | 13 | 27) {
        return 0;
    }
    // SAFETY: the default handling for this subclassed window's message.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
