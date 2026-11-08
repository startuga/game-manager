//! The window's frame: its own title bar, dark mode, minimum size and DPI changes.
//!
//! - **Title bar**: `WM_NCCALCSIZE` keeps Windows' frame on the left, right and bottom (resize
//!   borders and the shadow) but gives the caption's space to the client area, as Windows Terminal
//!   does. A maximized window hangs its frame off the screen, so its top is inset by that much.
//!   The view draws the title bar; `WM_NCHITTEST` answers `HTTOP` along the top edge,
//!   `HTMINBUTTON`/`HTMAXBUTTON`/`HTCLOSE` over the drawn buttons (so Windows 11 shows Snap Layouts
//!   over maximize) and `HTCAPTION` over the rest of the bar (dragging, double-click, the system
//!   menu).
//! - **Dark mode** for the frame and the system menu (`DWMWA_USE_IMMERSIVE_DARK_MODE`).
//! - **Minimum size** and **DPI**: sizes given in DIPs follow the window's monitor.
//!
//! Window handles travel as `isize` (`hwnd` turns one back into an `HWND`).

use windows::Win32::{
    ClientToScreen, DWMWA_USE_IMMERSIVE_DARK_MODE, DefWindowProcW, DwmSetWindowAttribute,
    GetClientRect, GetDpiForWindow, GetSystemMetricsForDpi, GetWindowPlacement, HTCLIENT, HTTOP,
    HTTOPLEFT, HTTOPRIGHT, HWND, MINMAXINFO, NCCALCSIZE_PARAMS, POINT, PostMessageW, RECT,
    SM_CXPADDEDBORDER, SM_CYFRAME, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOWMAXIMIZED,
    SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, ScreenToClient,
    SetWindowPos, ShowWindow, WINDOWPLACEMENT, WM_CLOSE, WM_NCCALCSIZE, WM_NCHITTEST,
};

use super::ctx::WindowCommand;

pub fn hwnd(value: isize) -> HWND {
    value as HWND
}

/// Dark frame, and the frame recalculated through our `WM_NCCALCSIZE` (the window was created
/// before the handler could see it).
pub fn setup(window: isize) {
    let dark: i32 = 1;
    // SAFETY: a valid window and a 4-byte BOOL attribute value that outlives the call.
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd(window),
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            std::ptr::from_ref(&dark).cast(),
            size_of::<i32>() as u32,
        )
    };
    let flags = SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE;
    // SAFETY: only asks Windows to send WM_NCCALCSIZE again.
    let _ = unsafe { SetWindowPos(hwnd(window), None, 0, 0, 0, 0, flags as u32) };
}

pub fn dpi(window: isize) -> u32 {
    // SAFETY: a plain query on a window handle.
    let dpi = unsafe { GetDpiForWindow(hwnd(window)) };
    if dpi == 0 { 96 } else { dpi }
}

pub fn is_maximized(window: isize) -> bool {
    let mut placement = WINDOWPLACEMENT {
        length: size_of::<WINDOWPLACEMENT>() as u32,
        ..Default::default()
    };
    // SAFETY: `placement` is a valid out-pointer with its length set.
    let ok = unsafe { GetWindowPlacement(hwnd(window), &mut placement) };
    ok.as_bool() && placement.showCmd == SW_SHOWMAXIMIZED as u32
}

/// The frame's thickness at the window's DPI: how far a maximized window hangs off the screen.
fn frame_thickness(dpi: u32) -> i32 {
    // SAFETY: plain metric queries.
    let frame = unsafe { GetSystemMetricsForDpi(SM_CYFRAME, dpi) };
    // SAFETY: as above.
    let padding = unsafe { GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi) };
    frame + padding
}

/// `WM_NCCALCSIZE` with `wparam` set: the standard frame, minus the caption.
pub fn nc_calc_size(window: isize, wparam: usize, lparam: isize) -> Option<isize> {
    if wparam == 0 {
        return None;
    }
    let params = lparam as *mut NCCALCSIZE_PARAMS;
    // SAFETY: with wparam set, lparam points to the NCCALCSIZE_PARAMS of this message.
    let top = unsafe { (*params).rgrc[0].top };
    // SAFETY: the same message, passed on unchanged to get Windows' frame.
    let result = unsafe { DefWindowProcW(hwnd(window), WM_NCCALCSIZE as u32, wparam, lparam) };
    if result != 0 {
        return Some(result);
    }
    let inset = if is_maximized(window) {
        frame_thickness(dpi(window))
    } else {
        0
    };
    // SAFETY: as above; only the top edge is moved back up.
    unsafe { (*params).rgrc[0].top = top + inset };
    Some(0)
}

/// `WM_NCHITTEST` before the view is asked: Windows' own answer for the side and bottom borders,
/// `HTTOP` (and the top corners) along the top edge. `None`: the client area, for the view.
pub fn border_hit(window: isize, wparam: usize, lparam: isize) -> Option<isize> {
    // SAFETY: the same message, passed on unchanged.
    let default = unsafe { DefWindowProcW(hwnd(window), WM_NCHITTEST as u32, wparam, lparam) };
    if default != HTCLIENT as isize {
        return Some(default);
    }
    if is_maximized(window) {
        return None;
    }
    let (x, y) = client_point(window, lparam);
    let edge = frame_thickness(dpi(window));
    if y >= edge {
        return None;
    }
    let width = client_width(window);
    Some(if x < edge * 2 {
        HTTOPLEFT as isize
    } else if x >= width - edge * 2 {
        HTTOPRIGHT as isize
    } else {
        HTTOP as isize
    })
}

fn client_width(window: isize) -> i32 {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out-pointer.
    let _ = unsafe { GetClientRect(hwnd(window), &mut rect) };
    rect.right - rect.left
}

/// A mouse message's screen point (`lparam` of `WM_NCHITTEST`, `WM_MOUSEWHEEL` and the `WM_NC*`
/// messages), in client pixels.
pub fn client_point(window: isize, lparam: isize) -> (i32, i32) {
    let mut point = POINT {
        x: i32::from(lparam as i16),
        y: i32::from((lparam >> 16) as i16),
    };
    // SAFETY: `point` is a valid in/out pointer.
    let _ = unsafe { ScreenToClient(hwnd(window), &mut point) };
    (point.x, point.y)
}

/// The client area's top-left corner on the screen, in pixels.
pub fn client_origin(window: isize) -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid in/out pointer.
    let _ = unsafe { ClientToScreen(hwnd(window), &mut point) };
    (point.x, point.y)
}

/// `WM_GETMINMAXINFO`: the smallest size, given in DIPs.
pub fn min_size(window: isize, lparam: isize, min: (f32, f32)) {
    let scale = dpi(window) as f32 / 96.0;
    let info = lparam as *mut MINMAXINFO;
    // SAFETY: lparam points to this message's MINMAXINFO.
    unsafe {
        (*info).ptMinTrackSize = POINT {
            x: (min.0 * scale) as i32,
            y: (min.1 * scale) as i32,
        };
    }
}

/// `WM_DPICHANGED`: moves the window to the rectangle Windows suggests for the new DPI.
pub fn dpi_changed(window: isize, lparam: isize) {
    // SAFETY: lparam points to this message's suggested RECT.
    let rect = unsafe { *(lparam as *const RECT) };
    let flags = SWP_NOZORDER | SWP_NOACTIVATE;
    // SAFETY: a plain move and resize of our own window.
    let _ = unsafe {
        SetWindowPos(
            hwnd(window),
            None,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            flags as u32,
        )
    };
}

pub fn command(window: isize, command: WindowCommand) {
    let show = match command {
        WindowCommand::Minimize => SW_MINIMIZE,
        WindowCommand::Maximize if is_maximized(window) => SW_RESTORE,
        WindowCommand::Maximize => SW_MAXIMIZE,
        WindowCommand::Close => {
            // SAFETY: asks our own window to close, as its close button would.
            let _ = unsafe { PostMessageW(Some(hwnd(window)), WM_CLOSE as u32, 0, 0) };
            return;
        }
    };
    // SAFETY: shows our own window in the new state.
    let _ = unsafe { ShowWindow(hwnd(window), show) };
}
