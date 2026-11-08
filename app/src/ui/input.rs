//! What the pointer and keyboard do to a laid-out frame, and the state only the UI keeps.
//!
//! `UiState` holds what a view cannot know: the element under the pointer, the one pressed, the
//! one with keyboard focus, each scroll view's offset, the open drop-down and the tooltip. All are
//! keyed by element ids (`layout.rs`), so they survive the view being rebuilt.
//!
//! A `Frame` is the last element tree with its layout. Events are hit-tested against it, topmost
//! first: an open drop-down's list, then the tree from its last layer down. A `modal` element (a
//! dialog's backdrop) stops the search, so nothing under a dialog reacts. Disabled elements and
//! everything inside them don't react either.
//!
//! Events return the `Message` the app should get, if any; the caller redraws when they report a
//! change.

use std::collections::HashMap;
use std::time::Instant;

use super::el::{El, Kind, Rect, Role};
use super::layout::{Placed, ScrollInfo};
use super::theme;

/// A drop-down list's item height.
pub const ITEM_HEIGHT: f32 = 30.0;
/// The scrollbar's thickness while the pointer is near, and its thin form otherwise.
pub const SCROLLBAR_WIDTH: f32 = 8.0;
pub const SCROLLBAR_THIN: f32 = 3.0;
const MIN_THUMB: f32 = 24.0;
/// How far one wheel notch scrolls.
const WHEEL_STEP: f32 = 48.0;

/// The pointer's button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
}

/// A key the kit handles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Tab { shift: bool },
    Enter,
    Space,
    Escape,
    Up,
    Down,
    Home,
    End,
}

/// What a key did.
#[derive(Debug, PartialEq)]
pub enum KeyResult<M> {
    /// Nothing for the kit to do: Escape with nothing open goes to the app.
    Unhandled,
    /// The view changed (focus moved, a list opened or closed).
    Changed,
    Message(M),
}

/// A scrollbar thumb being dragged.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ThumbDrag {
    scroll: u64,
    start_pointer: f32,
    start_offset: f32,
    /// Content moved per DIP the thumb moves.
    ratio: f32,
}

/// A tooltip waiting for its delay, or shown.
#[derive(Clone, Debug, PartialEq)]
pub struct Tooltip {
    pub id: u64,
    pub text: String,
    pub at: (f32, f32),
    pub shown: bool,
}

#[derive(Default)]
pub struct UiState {
    pub hover: Option<u64>,
    pub pressed: Option<u64>,
    pub focus: Option<u64>,
    /// The focus ring shows only after the keyboard moved focus, as in WinUI.
    pub focus_visible: bool,
    /// Scroll offsets by scroll view id (clamped at layout).
    pub scroll: HashMap<u64, f32>,
    /// The scroll view whose bar is under the pointer (drawn wide).
    pub scrollbar_hover: Option<u64>,
    drag: Option<ThumbDrag>,
    /// The open drop-down, and the item under the pointer or picked with the arrows.
    pub open_dropdown: Option<u64>,
    pub dropdown_hover: Option<usize>,
    pub tooltip: Option<Tooltip>,
    pub pointer: (f32, f32),
    /// The window's size in DIPs, for placing drop-down lists and tooltips.
    pub window: Rect,
    /// The element being dragged (a resize grip) while its button is down.
    held: Option<u64>,
    /// When the pointer came onto `hover`, and the element it last left with when: hover fills
    /// fade in and out over `HOVER_FADE`.
    hover_since: Option<Instant>,
    unhover: Option<(u64, Instant)>,
}

/// How long a hover fill takes to fade in or out, in seconds.
pub const HOVER_FADE: f32 = 0.12;

/// An element and its place.
pub struct Hit<'a, M> {
    pub el: &'a El<M>,
    pub placed: &'a Placed,
}

enum Found<'a, M> {
    Hit(Hit<'a, M>),
    Blocked,
    Miss,
}

/// The last view and its layout.
pub struct Frame<M> {
    pub root: El<M>,
    pub placed: Placed,
}

impl<M: Clone> Frame<M> {
    /// The interactive element under a point, unless a modal layer is in the way.
    pub fn hit(&self, x: f32, y: f32) -> Option<Hit<'_, M>> {
        match hit(&self.root, &self.placed, x, y) {
            Found::Hit(hit) => Some(hit),
            Found::Blocked | Found::Miss => None,
        }
    }

    pub fn find(&self, id: u64) -> Option<Hit<'_, M>> {
        find(&self.root, &self.placed, id)
    }

    /// The innermost scroll view under a point.
    fn scroll_at(&self, x: f32, y: f32) -> Option<(u64, ScrollInfo)> {
        fn walk(placed: &Placed, x: f32, y: f32) -> Option<(u64, ScrollInfo)> {
            if !placed.clip.contains(x, y) {
                return None;
            }
            placed
                .children
                .iter()
                .rev()
                .find_map(|child| walk(child, x, y))
                .or_else(|| placed.scroll.map(|info| (placed.id, info)))
        }
        walk(&self.placed, x, y)
    }

    /// Every element taking keyboard focus, in reading order, those scrolled out of view too (the
    /// key that reaches one scrolls it into view).
    pub fn focusables(&self) -> Vec<u64> {
        fn walk<M>(el: &El<M>, placed: &Placed, out: &mut Vec<u64>) {
            if !el.enabled {
                return;
            }
            if (el.on_click.is_some() && !el.modal)
                || matches!(el.kind, Kind::DropDown { .. } | Kind::Edit { .. })
            {
                out.push(placed.id);
            }
            for (child, child_placed) in el.children.iter().zip(&placed.children) {
                walk(child, child_placed, out);
            }
        }
        // Only the top modal layer and what's above it can take focus.
        let mut out = Vec::new();
        let (root, placed) = self.top_modal();
        walk(root, placed, &mut out);
        out
    }

    /// The layer holding the topmost modal element (a dialog's backdrop and its card), if any.
    pub fn modal_layer(&self) -> Option<(&El<M>, &Placed)> {
        let (el, placed) = self.top_modal();
        (!std::ptr::eq(el, &self.root)).then_some((el, placed))
    }

    /// The layer holding the topmost modal element (a dialog's backdrop and its card), else the
    /// root.
    fn top_modal(&self) -> (&El<M>, &Placed) {
        fn walk<'a, M>(el: &'a El<M>, placed: &'a Placed) -> Option<(&'a El<M>, &'a Placed)> {
            for (child, child_placed) in el.children.iter().zip(&placed.children).rev() {
                if child.modal {
                    return Some((el, placed));
                }
                if let Some(found) = walk(child, child_placed) {
                    return Some(found);
                }
            }
            None
        }
        walk(&self.root, &self.placed).unwrap_or((&self.root, &self.placed))
    }

    /// The open drop-down's list: its rectangle and items.
    pub fn dropdown_popup(&self, ui: &UiState) -> Option<(Rect, &[String], Option<usize>, u64)> {
        let id = ui.open_dropdown?;
        let hit = self.find(id)?;
        let Kind::DropDown {
            items, selected, ..
        } = &hit.el.kind
        else {
            return None;
        };
        Some((
            popup_rect(hit.placed.rect, items.len(), ui.window),
            items,
            *selected,
            id,
        ))
    }
}

/// Where a drop-down's list opens: under it when there is room, else above, inside the window.
pub fn popup_rect(anchor: Rect, items: usize, window: Rect) -> Rect {
    let height = (items as f32 * ITEM_HEIGHT + 8.0)
        .min(window.h - 16.0)
        .max(ITEM_HEIGHT);
    let below = anchor.bottom() + 2.0;
    let y = if below + height <= window.bottom() - 8.0 {
        below
    } else {
        (anchor.y - 2.0 - height).max(window.y + 8.0)
    };
    let width = anchor.w.max(120.0);
    let x = anchor
        .x
        .min(window.right() - width - 8.0)
        .max(window.x + 8.0);
    Rect::new(x, y, width, height)
}

/// A scroll view's thumb, `wide` while the pointer is over its bar.
pub fn thumb_rect(info: &ScrollInfo, wide: bool) -> Option<Rect> {
    let max = info.max_offset();
    if max <= 0.0 {
        return None;
    }
    let view = info.viewport;
    let width = if wide {
        SCROLLBAR_WIDTH
    } else {
        SCROLLBAR_THIN
    };
    let height = (view.h * view.h / info.content).clamp(MIN_THUMB.min(view.h), view.h);
    let y = view.y + (view.h - height) * (info.offset / max);
    Some(Rect::new(view.right() - width - 2.0, y, width, height))
}

/// The strip where a scroll view's bar reacts to the pointer.
fn bar_strip(info: &ScrollInfo) -> Rect {
    let view = info.viewport;
    Rect::new(
        view.right() - SCROLLBAR_WIDTH - 4.0,
        view.y,
        SCROLLBAR_WIDTH + 4.0,
        view.h,
    )
}

fn hit<'a, M>(el: &'a El<M>, placed: &'a Placed, x: f32, y: f32) -> Found<'a, M> {
    if !placed.clip.contains(x, y) {
        return Found::Miss;
    }
    if el.enabled {
        for (child, child_placed) in el.children.iter().zip(&placed.children).rev() {
            match hit(child, child_placed, x, y) {
                Found::Miss => {}
                found => return found,
            }
        }
        if el.is_interactive() {
            return Found::Hit(Hit { el, placed });
        }
    }
    if el.modal {
        Found::Blocked
    } else {
        Found::Miss
    }
}

fn find<'a, M>(el: &'a El<M>, placed: &'a Placed, id: u64) -> Option<Hit<'a, M>> {
    if placed.id == id {
        return Some(Hit { el, placed });
    }
    el.children
        .iter()
        .zip(&placed.children)
        .find_map(|(child, child_placed)| find(child, child_placed, id))
}

/// The scroll views around an element, innermost first.
fn scrolls_around(placed: &Placed, id: u64) -> Option<Vec<(u64, ScrollInfo)>> {
    if placed.id == id {
        return Some(Vec::new());
    }
    for child in &placed.children {
        if let Some(mut around) = scrolls_around(child, id) {
            if let Some(info) = placed.scroll {
                around.push((placed.id, info));
            }
            return Some(around);
        }
    }
    None
}

impl UiState {
    /// How far `id`'s hover fill has faded in, from 0 to 1, at `now`.
    pub fn hover_amount(&self, id: u64, now: Instant) -> f32 {
        let fade = |since: Instant| (now.duration_since(since).as_secs_f32() / HOVER_FADE).min(1.0);
        if self.hover == Some(id) {
            self.hover_since.map_or(1.0, fade)
        } else {
            match self.unhover {
                Some((left, at)) if left == id => 1.0 - fade(at),
                _ => 0.0,
            }
        }
    }

    /// The pointer moved onto another element (or none): the fades start.
    fn set_hover(&mut self, hover: Option<u64>) {
        let now = Instant::now();
        if let Some(old) = self.hover {
            self.unhover = Some((old, now));
        }
        self.hover = hover;
        self.hover_since = Some(now);
    }

    /// The left-right arrows show: over a resize grip, or while one is dragged.
    pub fn resize_cursor<M: Clone>(&self, frame: &Frame<M>) -> bool {
        self.held.is_some()
            || self
                .hover
                .and_then(|id| frame.find(id))
                .is_some_and(|hit| hit.el.on_drag.is_some())
    }

    /// The pointer moved. Returns whether anything shown changed, and what a dragged element
    /// sends.
    pub fn pointer_move<M: Clone>(
        &mut self,
        frame: &Frame<M>,
        x: f32,
        y: f32,
    ) -> (bool, Option<M>) {
        self.pointer = (x, y);
        if let Some(id) = self.held {
            let message = frame
                .find(id)
                .and_then(|hit| hit.el.on_drag.as_ref().map(|drag| drag((x, y))));
            return (true, message);
        }
        if let Some(drag) = self.drag {
            let offset = drag.start_offset + (y - drag.start_pointer) * drag.ratio;
            self.scroll.insert(drag.scroll, offset.max(0.0));
            return (true, None);
        }
        let mut changed = false;
        if self.open_dropdown.is_some() {
            let item = frame.dropdown_popup(self).and_then(|(rect, items, _, _)| {
                rect.contains(x, y).then(|| {
                    (((y - rect.y - 4.0) / ITEM_HEIGHT).floor().max(0.0) as usize)
                        .min(items.len().saturating_sub(1))
                })
            });
            if item != self.dropdown_hover {
                self.dropdown_hover = item;
                changed = true;
            }
        }
        let bar = frame
            .scroll_at(x, y)
            .filter(|(_, info)| info.max_offset() > 0.0 && bar_strip(info).contains(x, y))
            .map(|(id, _)| id);
        if bar != self.scrollbar_hover {
            self.scrollbar_hover = bar;
            changed = true;
        }
        let hover = if bar.is_some() {
            None
        } else {
            frame.hit(x, y).map(|hit| hit.placed.id)
        };
        if hover != self.hover {
            self.set_hover(hover);
            changed = true;
            // A new element under the pointer: its tooltip waits for the delay again.
            let shown = self.tooltip.as_ref().is_some_and(|tip| tip.shown);
            self.tooltip = hover.and_then(|id| frame.find(id)).and_then(|hit| {
                hit.el.tooltip.clone().map(|text| Tooltip {
                    id: hit.placed.id,
                    text,
                    at: (x, y),
                    shown: false,
                })
            });
            changed |= shown;
        }
        (changed, None)
    }

    /// A scrollbar thumb is being dragged.
    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The pointer left the window.
    pub fn pointer_leave(&mut self) -> bool {
        let changed =
            self.hover.is_some() || self.scrollbar_hover.is_some() || self.tooltip.is_some();
        if self.hover.is_some() {
            self.set_hover(None);
        }
        self.scrollbar_hover = None;
        self.tooltip = None;
        changed
    }

    /// The tooltip's delay ran out. Returns whether one appeared.
    pub fn show_tooltip(&mut self) -> bool {
        match &mut self.tooltip {
            Some(tip) if !tip.shown && self.hover == Some(tip.id) && self.pressed.is_none() => {
                tip.shown = true;
                true
            }
            _ => false,
        }
    }

    /// A button went down. Returns whether anything shown changed.
    pub fn pointer_down<M: Clone>(
        &mut self,
        frame: &Frame<M>,
        x: f32,
        y: f32,
        button: Button,
    ) -> bool {
        self.pointer = (x, y);
        let had_tooltip = self.tooltip.take().is_some_and(|tip| tip.shown) || self.focus_visible;
        self.focus_visible = false;
        if self.open_dropdown.is_some() {
            // Inside the list, the release picks; outside, the list closes and the press is spent.
            if frame
                .dropdown_popup(self)
                .is_some_and(|(rect, ..)| rect.contains(x, y))
            {
                return had_tooltip;
            }
            self.open_dropdown = None;
            self.dropdown_hover = None;
            return true;
        }
        if button == Button::Left
            && let Some((id, info)) = frame.scroll_at(x, y)
            && info.max_offset() > 0.0
            && bar_strip(&info).contains(x, y)
        {
            if let Some(thumb) = thumb_rect(&info, true) {
                let track = info.viewport.h - thumb.h;
                let ratio = if track > 0.0 {
                    info.max_offset() / track
                } else {
                    0.0
                };
                let start_offset = if thumb.contains(thumb.x + 1.0, y) {
                    info.offset
                } else {
                    // A press on the track jumps there.
                    ((y - info.viewport.y - thumb.h / 2.0) * ratio).clamp(0.0, info.max_offset())
                };
                self.scroll.insert(id, start_offset);
                self.drag = Some(ThumbDrag {
                    scroll: id,
                    start_pointer: y,
                    start_offset,
                    ratio,
                });
            }
            return true;
        }
        let hit = frame.hit(x, y);
        let target = hit.as_ref().map(|hit| hit.placed.id);
        if button == Button::Left
            && let Some(hit) = &hit
            && hit.el.on_drag.is_some()
        {
            self.held = target;
        }
        let changed = self.pressed != target || had_tooltip;
        self.pressed = target;
        if button == Button::Left && target.is_some() {
            self.focus = target;
        }
        changed
    }

    /// A button came up: a click or a right-click when it is over the element it went down on.
    pub fn pointer_up<M: Clone>(
        &mut self,
        frame: &Frame<M>,
        x: f32,
        y: f32,
        button: Button,
    ) -> (bool, Option<M>) {
        if self.drag.take().is_some() {
            return (true, None);
        }
        if self.held.take().is_some() {
            self.pressed = None;
            return (true, None);
        }
        if let Some((rect, items, _, id)) = frame.dropdown_popup(self) {
            if rect.contains(x, y) && button == Button::Left {
                let index = (((y - rect.y - 4.0) / ITEM_HEIGHT).floor().max(0.0) as usize)
                    .min(items.len().saturating_sub(1));
                self.open_dropdown = None;
                self.dropdown_hover = None;
                let message = frame.find(id).and_then(|hit| match &hit.el.kind {
                    Kind::DropDown { on_select, .. } => Some(on_select(index)),
                    _ => None,
                });
                return (true, message);
            }
            return (false, None);
        }
        let pressed = self.pressed.take();
        let Some(hit) = frame.hit(x, y) else {
            return (pressed.is_some(), None);
        };
        if pressed != Some(hit.placed.id) {
            return (pressed.is_some(), None);
        }
        match button {
            Button::Right => (
                true,
                hit.el.on_context.as_ref().map(|handler| handler((x, y))),
            ),
            Button::Left => (true, self.activate(hit.el, hit.placed.id)),
        }
    }

    /// Clicking or pressing Enter on an element.
    fn activate<M: Clone>(&mut self, el: &El<M>, id: u64) -> Option<M> {
        if let Kind::DropDown { selected, .. } = &el.kind {
            self.open_dropdown = Some(id);
            self.dropdown_hover = *selected;
            return None;
        }
        el.on_click.clone()
    }

    /// The wheel turned (`notches` up is positive). Returns whether a view scrolled.
    pub fn wheel<M: Clone>(&mut self, frame: &Frame<M>, x: f32, y: f32, notches: f32) -> bool {
        if self.open_dropdown.is_some() {
            return false;
        }
        let Some((id, info)) = frame.scroll_at(x, y) else {
            return false;
        };
        let offset = (info.offset - notches * WHEEL_STEP).clamp(0.0, info.max_offset());
        if (offset - info.offset).abs() < f32::EPSILON {
            return false;
        }
        self.scroll.insert(id, offset);
        self.tooltip = None;
        true
    }

    /// A key went down.
    pub fn key<M: Clone>(&mut self, frame: &Frame<M>, key: Key) -> KeyResult<M> {
        if let Some((_, items, selected, id)) = frame.dropdown_popup(self) {
            let count = items.len();
            let current = self.dropdown_hover.or(selected);
            match key {
                Key::Escape | Key::Tab { .. } => {
                    self.open_dropdown = None;
                    self.dropdown_hover = None;
                    return KeyResult::Changed;
                }
                Key::Up | Key::Down | Key::Home | Key::End if count > 0 => {
                    self.dropdown_hover = Some(match key {
                        Key::Up => current.map_or(count - 1, |i| i.saturating_sub(1)),
                        Key::Down => current.map_or(0, |i| (i + 1).min(count - 1)),
                        Key::Home => 0,
                        _ => count - 1,
                    });
                    return KeyResult::Changed;
                }
                Key::Enter | Key::Space => {
                    self.open_dropdown = None;
                    let pick = self.dropdown_hover.take();
                    return match (pick, frame.find(id)) {
                        (Some(index), Some(hit)) => match &hit.el.kind {
                            Kind::DropDown { on_select, .. } => {
                                KeyResult::Message(on_select(index))
                            }
                            _ => KeyResult::Changed,
                        },
                        _ => KeyResult::Changed,
                    };
                }
                _ => return KeyResult::Unhandled,
            }
        }
        match key {
            Key::Tab { shift } => {
                let order = frame.focusables();
                if order.is_empty() {
                    return KeyResult::Unhandled;
                }
                let at = self
                    .focus
                    .and_then(|id| order.iter().position(|f| *f == id));
                let next = match (at, shift) {
                    (None, false) => 0,
                    (None, true) => order.len() - 1,
                    (Some(i), false) => (i + 1) % order.len(),
                    (Some(i), true) => (i + order.len() - 1) % order.len(),
                };
                self.focus = Some(order[next]);
                self.focus_visible = true;
                self.scroll_into_view(frame, order[next]);
                KeyResult::Changed
            }
            Key::Enter | Key::Space => {
                let Some(hit) = self.focus.and_then(|id| frame.find(id)) else {
                    return KeyResult::Unhandled;
                };
                if !hit.el.enabled {
                    return KeyResult::Unhandled;
                }
                match self.activate(hit.el, hit.placed.id) {
                    Some(message) => KeyResult::Message(message),
                    None if self.open_dropdown.is_some() => KeyResult::Changed,
                    None => KeyResult::Unhandled,
                }
            }
            // In a list, the arrows pick the next or previous row.
            Key::Up | Key::Down | Key::Home | Key::End => self.step_list(frame, key),
            Key::Escape => KeyResult::Unhandled,
        }
    }

    /// Moves the selection in the list holding the focused row.
    fn step_list<M: Clone>(&mut self, frame: &Frame<M>, key: Key) -> KeyResult<M> {
        let Some(focus) = self.focus else {
            return KeyResult::Unhandled;
        };
        let rows: Vec<u64> = frame
            .focusables()
            .into_iter()
            .filter(|id| {
                frame
                    .find(*id)
                    .is_some_and(|hit| hit.el.role == Role::ListItem)
            })
            .collect();
        let Some(at) = rows.iter().position(|id| *id == focus) else {
            return KeyResult::Unhandled;
        };
        let next = match key {
            Key::Up => at.saturating_sub(1),
            Key::Down => (at + 1).min(rows.len() - 1),
            Key::Home => 0,
            _ => rows.len() - 1,
        };
        if next == at {
            return KeyResult::Unhandled;
        }
        self.focus = Some(rows[next]);
        self.focus_visible = true;
        self.scroll_into_view(frame, rows[next]);
        match frame
            .find(rows[next])
            .and_then(|hit| hit.el.on_click.clone())
        {
            Some(message) => KeyResult::Message(message),
            None => KeyResult::Changed,
        }
    }

    /// Scrolls the views around an element so all of it shows.
    pub fn scroll_into_view<M: Clone>(&mut self, frame: &Frame<M>, id: u64) {
        let Some(hit) = frame.find(id) else {
            return;
        };
        let rect = hit.placed.rect;
        for (scroll, info) in scrolls_around(&frame.placed, id).unwrap_or_default() {
            let view = info.viewport;
            let offset = if rect.y < view.y {
                info.offset - (view.y - rect.y)
            } else if rect.bottom() > view.bottom() {
                info.offset + (rect.bottom() - view.bottom())
            } else {
                continue;
            };
            self.scroll
                .insert(scroll, offset.clamp(0.0, info.max_offset()));
        }
    }

    /// Drops what refers to elements that are gone (after the view changed).
    pub fn forget_missing<M: Clone>(&mut self, frame: &Frame<M>) {
        let exists = |id: &u64| frame.find(*id).is_some();
        if !self.focus.as_ref().is_some_and(exists) {
            self.focus = None;
        }
        if !self.open_dropdown.as_ref().is_some_and(exists) {
            self.open_dropdown = None;
            self.dropdown_hover = None;
        }
        if !self.hover.as_ref().is_some_and(exists) {
            self.hover = None;
            self.tooltip = None;
        }
    }
}

/// The tooltip's box near the pointer, inside the window.
pub fn tooltip_rect(at: (f32, f32), size: (f32, f32), window: Rect) -> Rect {
    let (w, h) = (size.0 + 16.0, size.1 + 10.0);
    let mut x = at.0 + 4.0;
    let mut y = at.1 + 22.0;
    if x + w > window.right() - 4.0 {
        x = (window.right() - w - 4.0).max(window.x);
    }
    if y + h > window.bottom() - 4.0 {
        y = (at.1 - h - 6.0).max(window.y);
    }
    Rect::new(x, y, w, h)
}

/// The colour of a box in its current state.
pub fn fill_for<M>(el: &El<M>, hover: bool, pressed: bool) -> Option<windows_canvas::ColorF> {
    if el.selected
        && el.role != Role::CheckBox
        && el.fill.is_none()
        && matches!(el.role, Role::ListItem | Role::Tab)
    {
        return Some(theme::SELECTED);
    }
    if el.enabled && pressed {
        el.pressed_fill.or(el.hover_fill).or(el.fill)
    } else if el.enabled && hover {
        el.hover_fill.or(el.fill)
    } else {
        el.fill
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::el::{El, TextSpec, col, dropdown, layers, scroll, text};
    use crate::ui::layout::{Layout, Measure};

    struct Fixed;
    impl Measure for Fixed {
        fn text(&self, spec: &TextSpec, _max_width: f32) -> (f32, f32) {
            (spec.text.chars().count() as f32 * 7.0, 16.0)
        }
    }

    fn frame(root: El<&'static str>, ui: &UiState) -> Frame<&'static str> {
        let placed = Layout::new(&Fixed, &ui.scroll).place(&root, ui.window);
        Frame { root, placed }
    }

    fn ui() -> UiState {
        UiState {
            window: Rect::new(0.0, 0.0, 200.0, 200.0),
            ..UiState::default()
        }
    }

    fn click(
        ui: &mut UiState,
        frame: &Frame<&'static str>,
        x: f32,
        y: f32,
    ) -> Option<&'static str> {
        ui.pointer_down(frame, x, y, Button::Left);
        ui.pointer_up(frame, x, y, Button::Left).1
    }

    #[test]
    fn a_click_reaches_the_innermost_clickable_element() {
        let root = col([
            text("a").height(20.0).on_click("a"),
            col([text("inner").height(20.0)]).on_click("outer"),
        ]);
        let mut ui = ui();
        let frame = frame(root, &ui);
        assert_eq!(click(&mut ui, &frame, 5.0, 5.0), Some("a"));
        // Text inside a clickable box: the box gets the click.
        assert_eq!(click(&mut ui, &frame, 5.0, 25.0), Some("outer"));
        assert_eq!(click(&mut ui, &frame, 5.0, 150.0), None);
    }

    #[test]
    fn a_release_elsewhere_is_no_click() {
        let root = col([
            text("a").height(20.0).on_click("a"),
            text("b").height(20.0).on_click("b"),
        ]);
        let mut ui = ui();
        let frame = frame(root, &ui);
        ui.pointer_down(&frame, 5.0, 5.0, Button::Left);
        assert_eq!(ui.pointer_up(&frame, 5.0, 25.0, Button::Left).1, None);
    }

    #[test]
    fn disabled_elements_and_whatever_is_under_a_modal_layer_take_no_click() {
        let mut ui = ui();
        let disabled = frame(
            col([col([text("x").height(20.0).on_click("x")]).enabled(false)]),
            &ui,
        );
        assert_eq!(click(&mut ui, &disabled, 5.0, 5.0), None);

        let dialog = frame(
            layers([
                col([text("under").height(200.0).on_click("under")]),
                layers([text("ok").size(40.0, 20.0).on_click("ok").center()]).modal(),
            ]),
            &ui,
        );
        assert_eq!(click(&mut ui, &dialog, 5.0, 5.0), None);
        assert_eq!(click(&mut ui, &dialog, 100.0, 100.0), Some("ok"));
    }

    #[test]
    fn content_scrolled_out_of_view_takes_no_click_and_the_wheel_stops_at_the_ends() {
        let rows: Vec<El<&'static str>> = ["0", "1", "2", "3", "4"]
            .into_iter()
            .map(|n| text(n).height(40.0).on_click(n))
            .collect();
        let root = col([
            scroll("list", col(rows)).height(80.0),
            text("below").height(20.0).on_click("below"),
        ]);
        let mut ui = ui();
        let mut f = frame(root, &ui);
        assert_eq!(click(&mut ui, &f, 5.0, 45.0), Some("1"));
        assert!(ui.wheel(&f, 5.0, 10.0, -1.0));
        f = frame(f.root, &ui);
        assert_eq!(click(&mut ui, &f, 5.0, 5.0), Some("1"));
        // Far down: clamped to the last rows; row 4 shows, nothing leaks below the view.
        assert!(ui.wheel(&f, 5.0, 10.0, -10.0));
        f = frame(f.root, &ui);
        assert_eq!(f.placed.children[0].scroll.unwrap().offset, 120.0);
        assert_eq!(click(&mut ui, &f, 5.0, 45.0), Some("4"));
        assert_eq!(click(&mut ui, &f, 5.0, 85.0), Some("below"));
        assert!(!ui.wheel(&f, 5.0, 10.0, -1.0));
    }

    #[test]
    fn a_drop_down_opens_on_click_and_sends_the_picked_item() {
        let items = vec!["One".to_owned(), "Two".to_owned()];
        let root = col([dropdown(
            "pick",
            items,
            Some(0),
            |i| if i == 0 { "one" } else { "two" },
        )
        .width(100.0)]);
        let mut ui = ui();
        let f = frame(root, &ui);
        assert_eq!(click(&mut ui, &f, 10.0, 10.0), None);
        let (rect, ..) = f.dropdown_popup(&ui).unwrap();
        assert_eq!(rect.y, 32.0);
        // The second item.
        assert_eq!(
            click(&mut ui, &f, 10.0, rect.y + 4.0 + ITEM_HEIGHT + 5.0),
            Some("two")
        );
        assert!(ui.open_dropdown.is_none());
        // Open again, then the keys.
        click(&mut ui, &f, 10.0, 10.0);
        assert_eq!(ui.key(&f, Key::Down), KeyResult::Changed);
        assert_eq!(ui.key(&f, Key::Enter), KeyResult::Message("two"));
    }

    #[test]
    fn tab_cycles_focus_and_enter_activates_it() {
        let root = col([
            text("a").height(20.0).on_click("a"),
            text("b").height(20.0).on_click("b").enabled(false),
            text("c").height(20.0).on_click("c"),
        ]);
        let mut ui = ui();
        let f = frame(root, &ui);
        assert_eq!(ui.key(&f, Key::Tab { shift: false }), KeyResult::Changed);
        assert_eq!(ui.key(&f, Key::Enter), KeyResult::Message("a"));
        ui.key(&f, Key::Tab { shift: false });
        // The disabled one is skipped.
        assert_eq!(ui.key(&f, Key::Enter), KeyResult::Message("c"));
        ui.key(&f, Key::Tab { shift: false });
        assert_eq!(ui.key(&f, Key::Enter), KeyResult::Message("a"));
    }

    #[test]
    fn arrows_select_the_next_row_and_bring_it_into_view() {
        let rows: Vec<El<&'static str>> = ["0", "1", "2", "3"]
            .into_iter()
            .map(|n| text(n).height(40.0).on_click(n).role(Role::ListItem))
            .collect();
        let root = col([scroll("list", col(rows)).height(80.0)]);
        let mut ui = ui();
        let mut f = frame(root, &ui);
        click(&mut ui, &f, 5.0, 45.0);
        assert_eq!(ui.key(&f, Key::Down), KeyResult::Message("2"));
        f = frame(f.root, &ui);
        assert_eq!(f.placed.children[0].scroll.unwrap().offset, 40.0);
        assert_eq!(ui.key(&f, Key::Home), KeyResult::Message("0"));
    }

    #[test]
    fn a_held_grip_sends_the_pointer_while_it_moves() {
        let root = crate::ui::el::row([
            text("side").width(100.0),
            col([])
                .width(6.0)
                .fill_height()
                .on_drag(|(x, _)| if x > 150.0 { "wide" } else { "narrow" }),
            col([]).fill_width(),
        ]);
        let mut ui = ui();
        let f = frame(root, &ui);
        assert!(!ui.resize_cursor(&f));
        ui.pointer_move(&f, 102.0, 5.0);
        assert!(ui.resize_cursor(&f));
        ui.pointer_down(&f, 102.0, 5.0, Button::Left);
        assert_eq!(ui.pointer_move(&f, 160.0, 5.0).1, Some("wide"));
        assert_eq!(ui.pointer_move(&f, 120.0, 5.0).1, Some("narrow"));
        assert_eq!(ui.pointer_up(&f, 120.0, 5.0, Button::Left).1, None);
        // Released: moving sends nothing.
        assert_eq!(ui.pointer_move(&f, 40.0, 5.0).1, None);
    }

    #[test]
    fn a_hover_fill_fades_in_and_out() {
        let root = col([
            text("a").height(20.0).on_click("a"),
            text("b").height(20.0).on_click("b"),
        ]);
        let mut ui = ui();
        let f = frame(root, &ui);
        ui.pointer_move(&f, 5.0, 5.0);
        let a = f.hit(5.0, 5.0).map(|hit| hit.placed.id).unwrap();
        let start = Instant::now();
        assert!(ui.hover_amount(a, start) < 0.5);
        let later = start + std::time::Duration::from_millis(500);
        assert_eq!(ui.hover_amount(a, later), 1.0);
        // Onto b: a fades out.
        ui.pointer_move(&f, 5.0, 25.0);
        assert!(ui.hover_amount(a, Instant::now()) > 0.5);
        assert_eq!(
            ui.hover_amount(a, Instant::now() + std::time::Duration::from_millis(500)),
            0.0
        );
    }

    #[test]
    fn a_drop_down_list_opens_above_when_there_is_no_room_below() {
        let window = Rect::new(0.0, 0.0, 300.0, 200.0);
        let below = popup_rect(Rect::new(10.0, 20.0, 100.0, 30.0), 3, window);
        assert_eq!(below.y, 52.0);
        let above = popup_rect(Rect::new(10.0, 150.0, 100.0, 30.0), 3, window);
        assert!(above.bottom() <= 150.0);
    }
}
