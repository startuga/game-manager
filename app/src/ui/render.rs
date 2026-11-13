//! Draws a laid-out frame with Direct2D (windows-canvas) and DirectWrite (windows-text).
//!
//! - **`Text`** keeps DirectWrite formats and layouts, keyed by what they show, so measuring and
//!   drawing the same text reuses one layout; entries unused for a few frames are dropped. It also
//!   answers the layout's `Measure`. Oxanium, the page titles' font, is embedded and loaded once.
//! - **`Renderer`** owns the GPU device and the window's swap chain, and keeps each picture's
//!   bitmap by key. A frame clears to the base colour and draws the tree in order: fills and
//!   outlines (hover and press states from `UiState`), text, pictures, spinners, drop-down faces,
//!   text boxes, custom painters, scroll views clipped to their viewport with their thin bars, and
//!   the keyboard focus ring; then the open drop-down's list, the text box menu and the tooltip on
//!   top.
//! - **Text boxes** (`edit`): the text (or the placeholder) clipped to the box and scrolled by its
//!   `EditState`, the selection behind it and the caret while the box has the keyboard, an IME
//!   composition underlined at the caret (`with_marked_layout`), and an accent line along the
//!   bottom of the focused box, as WinUI draws it. `edit_spec` is the text's format, which the
//!   shell's hit-testing uses too.
//! - **`Painter`** is what custom painters (the Play button) draw with.
//!
//! A frame with a spinner reports `animating`, so the loop keeps drawing while it turns; any other
//! frame is drawn once. `present` answering `false` means the device was lost: the caller makes a
//! new renderer.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::Arc;
use std::time::Instant;
use windows::Win32::{D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_RECT_F, ID2D1DeviceContext};
use windows_canvas::{
    Bitmap, Brush, ColorF, DrawingSession, Ellipse, GpuDevice, GradientStop, RoundedRect,
    SwapChain, Vector2,
};
use windows_text::{
    FontCollection, FontWeight, TextAlignment, TextFormat, TextLayout, Trimming, WordWrapping,
};

use super::el::{Edges, El, Font, Interaction, Kind, Rect, TextAlign, TextSpec};
use super::image::Picture;
use super::input::{self, Frame, ITEM_HEIGHT, UiState};
use super::layout::{Measure, Placed, item_spec};
use super::theme;

/// Oxanium (SIL Open Font License 1.1; `app/assets/Oxanium-OFL.txt`).
const OXANIUM: &[u8] = include_bytes!("../../assets/Oxanium.ttf");
/// "Unbounded" for DirectWrite, which needs a finite width.
const UNBOUNDED: f32 = 100_000.0;
/// Layouts unused for this many frames are dropped.
const KEEP_FRAMES: u64 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct FormatKey {
    font: Font,
    size: u32,
    weight: u16,
    wrap: bool,
    align: TextAlign,
}

impl FormatKey {
    fn of(spec: &TextSpec) -> Self {
        Self {
            font: spec.font,
            size: spec.size.to_bits(),
            weight: spec.weight,
            wrap: spec.wrap,
            align: spec.align,
        }
    }
}

/// Layouts by text, format and maximum width, with the frame that last used each.
type Layouts = HashMap<(String, FormatKey, u32), (TextLayout, u64)>;

/// DirectWrite formats and layouts, shared by layout and drawing.
pub struct Text {
    titles: Option<FontCollection>,
    formats: RefCell<HashMap<FormatKey, TextFormat>>,
    layouts: RefCell<Layouts>,
    frame: Cell<u64>,
}

impl Default for Text {
    fn default() -> Self {
        Self::new()
    }
}

impl Text {
    pub fn new() -> Self {
        Self {
            titles: FontCollection::from_data([OXANIUM]).ok(),
            formats: RefCell::new(HashMap::new()),
            layouts: RefCell::new(HashMap::new()),
            frame: Cell::new(0),
        }
    }

    fn format(&self, key: FormatKey) -> TextFormat {
        if let Some(format) = self.formats.borrow().get(&key) {
            return format.clone();
        }
        let family = match key.font {
            Font::Text => theme::FONT_TEXT,
            Font::Display => theme::FONT_DISPLAY,
            Font::Title if self.titles.is_some() => theme::FONT_TITLE,
            Font::Title => theme::FONT_DISPLAY,
            Font::Icons => theme::FONT_ICONS,
        };
        let mut format = TextFormat::new(family, f32::from_bits(key.size))
            .with_weight(FontWeight(key.weight))
            .with_alignment(match key.align {
                TextAlign::Start => TextAlignment::Leading,
                TextAlign::Center => TextAlignment::Center,
            });
        format = if key.wrap {
            format.with_word_wrapping(WordWrapping::Wrap)
        } else {
            format
                .with_word_wrapping(WordWrapping::NoWrap)
                .with_trimming(Trimming::Character)
        };
        if key.font == Font::Title
            && let Some(titles) = &self.titles
        {
            format = format.with_collection(titles);
        }
        self.formats.borrow_mut().insert(key, format.clone());
        format
    }

    /// Runs `f` with the layout of `spec` at most `max_width` wide.
    pub fn with_layout<R>(
        &self,
        spec: &TextSpec,
        max_width: f32,
        f: impl FnOnce(&TextLayout) -> R,
    ) -> R {
        let max_width = if max_width.is_finite() {
            max_width.max(0.0)
        } else {
            UNBOUNDED
        };
        let format_key = FormatKey::of(spec);
        let key = (spec.text.clone(), format_key, max_width.to_bits());
        let frame = self.frame.get();
        if let Some((layout, used)) = self.layouts.borrow_mut().get_mut(&key) {
            *used = frame;
            return f(layout);
        }
        let layout = TextLayout::new(&spec.text, &self.format(format_key), max_width, UNBOUNDED);
        let result = f(&layout);
        self.layouts.borrow_mut().insert(key, (layout, frame));
        result
    }

    /// Runs `f` with a layout of `spec` on one unbounded line, `marked` underlined: an IME
    /// composition in a text box. Made for the call, not kept.
    pub fn with_marked_layout<R>(
        &self,
        spec: &TextSpec,
        marked: std::ops::Range<usize>,
        f: impl FnOnce(&TextLayout) -> R,
    ) -> R {
        let format = self.format(FormatKey::of(spec));
        let mut layout = TextLayout::new(&spec.text, &format, UNBOUNDED, UNBOUNDED);
        layout.set_underline(marked, true);
        f(&layout)
    }

    /// Starts a new frame, dropping layouts no recent frame used.
    pub fn next_frame(&self) {
        let frame = self.frame.get() + 1;
        self.frame.set(frame);
        self.layouts
            .borrow_mut()
            .retain(|_, (_, used)| frame.saturating_sub(*used) <= KEEP_FRAMES);
    }
}

impl Measure for Text {
    fn text(&self, spec: &TextSpec, max_width: f32) -> (f32, f32) {
        self.with_layout(spec, max_width, |layout| {
            let metrics = layout.metrics();
            (metrics.width.ceil(), metrics.height.ceil())
        })
    }
}

/// What a drawn frame needs next.
pub struct Drawn {
    /// `false`: the device was lost; make a new renderer.
    pub presented: bool,
    /// Something moves (a spinner): draw again right away.
    pub animating: bool,
}

pub struct Renderer {
    _device: GpuDevice,
    chain: SwapChain,
    /// Pixels per DIP, for drawing pictures on whole pixels.
    scale: f32,
    bitmaps: HashMap<String, (Bitmap, Arc<Picture>)>,
    start: Instant,
}

impl Renderer {
    /// A swap chain for the window's client area, `width` × `height` pixels at `dpi`.
    ///
    /// # Safety
    ///
    /// `hwnd` must stay a live window for as long as the renderer exists (the shell drops the
    /// renderer before the window).
    pub unsafe fn new(
        hwnd: isize,
        width: u32,
        height: u32,
        dpi: f32,
    ) -> windows_core::Result<Self> {
        let device = GpuDevice::new_or_warp()?;
        // SAFETY: the caller keeps `hwnd` alive for the renderer's life.
        let mut chain = unsafe {
            device.create_swap_chain_for_hwnd(hwnd as *mut _, width.max(1), height.max(1))
        }?;
        chain.set_dpi(dpi, dpi);
        Ok(Self {
            _device: device,
            chain,
            scale: dpi / 96.0,
            bitmaps: HashMap::new(),
            start: Instant::now(),
        })
    }

    pub fn resize(&mut self, width: u32, height: u32, dpi: f32) -> windows_core::Result<()> {
        if self.chain.width() != width.max(1) || self.chain.height() != height.max(1) {
            self.chain.resize(width.max(1), height.max(1))?;
        }
        self.chain.set_dpi(dpi, dpi);
        self.scale = dpi / 96.0;
        Ok(())
    }

    pub fn draw<M: Clone>(
        &mut self,
        frame: &Frame<M>,
        ui: &UiState,
        text: &Text,
    ) -> windows_core::Result<Drawn> {
        let elapsed = self.start.elapsed().as_secs_f32();
        let session = self.chain.begin_draw()?;
        session.clear(theme::BASE);
        let brush = session.create_solid_brush(ColorF::WHITE)?;
        let mut painter = Painter {
            session: &session,
            brush: &brush,
            text,
            scale: self.scale,
            alpha: 1.0,
            bitmaps: &mut self.bitmaps,
            elapsed,
            now: Instant::now(),
            animating: false,
        };
        painter.element(&frame.root, &frame.placed, ui);
        painter.overlays(frame, ui);
        let animating = painter.animating;
        // The session ends (EndDraw) before presenting.
        drop(session);
        let presented = self.chain.present()?;
        Ok(Drawn {
            presented,
            animating,
        })
    }
}

impl Renderer {
    /// Draws `frame` offscreen, one pixel per DIP, and returns its premultiplied BGRA pixels: the
    /// frame capture checks use (`shell::WM_CAPTURE`), since an agent session can't see the screen.
    pub fn capture<M: Clone>(
        &mut self,
        frame: &Frame<M>,
        ui: &UiState,
        text: &Text,
    ) -> windows_core::Result<(u32, u32, Vec<u8>)> {
        let width = ui.window.w.ceil().max(1.0) as u32;
        let height = ui.window.h.ceil().max(1.0) as u32;
        let target = self._device.create_render_target(width, height)?;
        let elapsed = self.start.elapsed().as_secs_f32();
        let bitmaps = &mut self.bitmaps;
        target.draw(|session| {
            session.clear(theme::BASE);
            let brush = session.create_solid_brush(ColorF::WHITE)?;
            let mut painter = Painter {
                session,
                brush: &brush,
                text,
                // One pixel per DIP.
                scale: 1.0,
                alpha: 1.0,
                bitmaps: &mut *bitmaps,
                elapsed,
                now: Instant::now(),
                animating: false,
            };
            painter.element(&frame.root, &frame.placed, ui);
            painter.overlays(frame, ui);
            Ok(())
        })?;
        Ok((width, height, target.read_pixels()?))
    }
}

fn to_canvas(rect: Rect) -> windows_canvas::Rect {
    windows_canvas::Rect::from_xywh(rect.x, rect.y, rect.w, rect.h)
}

/// What elements and custom painters draw with.
pub struct Painter<'a> {
    session: &'a DrawingSession<'a>,
    brush: &'a Brush,
    text: &'a Text,
    /// Pixels per DIP.
    scale: f32,
    /// Multiplies every colour's alpha (an element's opacity, a disabled subtree).
    alpha: f32,
    bitmaps: &'a mut HashMap<String, (Bitmap, Arc<Picture>)>,
    elapsed: f32,
    /// The frame's time, for the hover fades.
    now: Instant,
    animating: bool,
}

/// `from` moved towards `to` by `amount` (0–1); a missing colour is the other one, transparent.
fn blend(from: Option<ColorF>, to: Option<ColorF>, amount: f32) -> Option<ColorF> {
    if amount <= 0.0 || from == to {
        return from;
    }
    if amount >= 1.0 {
        return to;
    }
    let clear = |c: ColorF| ColorF::new(c.r, c.g, c.b, 0.0);
    let (a, b) = match (from, to) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, clear(a)),
        (None, Some(b)) => (clear(b), b),
        (None, None) => return None,
    };
    let mix = |x: f32, y: f32| x + (y - x) * amount;
    Some(ColorF::new(
        mix(a.r, b.r),
        mix(a.g, b.g),
        mix(a.b, b.b),
        mix(a.a, b.a),
    ))
}

impl Painter<'_> {
    fn paint(&self, color: ColorF) -> &Brush {
        self.brush
            .set_color(ColorF::new(color.r, color.g, color.b, color.a * self.alpha));
        self.brush
    }

    pub fn fill_rect(&self, rect: Rect, color: ColorF) {
        self.session.fill_rect(&to_canvas(rect), self.paint(color));
    }

    pub fn fill_rounded(&self, rect: Rect, radius: f32, color: ColorF) {
        if radius <= 0.0 {
            return self.fill_rect(rect, color);
        }
        self.session.fill_rounded_rect(
            &RoundedRect::uniform(to_canvas(rect), radius),
            self.paint(color),
        );
    }

    pub fn stroke_rounded(&self, rect: Rect, radius: f32, color: ColorF, width: f32) {
        self.session.draw_rounded_rect(
            &RoundedRect::uniform(to_canvas(rect), radius),
            self.paint(color),
            width,
        );
    }

    pub fn line(&self, from: (f32, f32), to: (f32, f32), color: ColorF, width: f32) {
        self.session.draw_line(
            Vector2::new(from.0, from.1),
            Vector2::new(to.0, to.1),
            self.paint(color),
            width,
        );
    }

    /// A rounded rectangle filled with a vertical gradient from `top` to `bottom`.
    pub fn fill_gradient(&self, rect: Rect, radius: f32, top: ColorF, bottom: ColorF) {
        let fade = |c: ColorF| ColorF::new(c.r, c.g, c.b, c.a * self.alpha);
        let Ok(gradient) = self.session.create_linear_gradient(
            Vector2::new(rect.x, rect.y),
            Vector2::new(rect.x, rect.bottom()),
            &[
                GradientStop::new(0.0, fade(top)),
                GradientStop::new(1.0, fade(bottom)),
            ],
        ) else {
            return self.fill_rounded(rect, radius, top);
        };
        self.session
            .fill_rounded_rect(&RoundedRect::uniform(to_canvas(rect), radius), &gradient);
    }

    /// A soft glow: rounded rectangles growing outwards at falling opacity.
    pub fn glow(&self, rect: Rect, radius: f32, color: ColorF, spread: f32) {
        let steps = 6;
        for step in 1..=steps {
            let grow = spread * step as f32 / steps as f32;
            let alpha = color.a * 0.10 * (1.0 - step as f32 / (steps as f32 + 1.0));
            let ring = Rect::new(
                rect.x - grow,
                rect.y - grow + grow * 0.4,
                rect.w + 2.0 * grow,
                rect.h + 2.0 * grow,
            );
            self.fill_rounded(
                ring,
                radius + grow,
                ColorF::new(color.r, color.g, color.b, alpha),
            );
        }
    }

    /// Text in a rectangle: wrapped from its top, or one line centred vertically.
    pub fn text(&self, spec: &TextSpec, rect: Rect) {
        if rect.w <= 0.0 {
            return;
        }
        self.text.with_layout(spec, rect.w, |layout| {
            let y = if spec.wrap {
                rect.y
            } else {
                rect.y + ((rect.h - layout.metrics().height) / 2.0).max(0.0)
            };
            self.session.draw_text_layout(
                Vector2::new(rect.x, y.round()),
                layout,
                self.paint(spec.color),
            );
        });
    }

    /// The size `spec` takes at most `max_width` wide.
    pub fn measure(&self, spec: &TextSpec, max_width: f32) -> (f32, f32) {
        self.text.text(spec, max_width)
    }

    fn picture(&mut self, key: &str, picture: &Arc<Picture>, rect: Rect) {
        let fresh = !matches!(self.bitmaps.get(key), Some((_, kept)) if Arc::ptr_eq(kept, picture));
        if fresh {
            let Ok(bitmap) =
                self.session
                    .create_bitmap(&picture.pixels, picture.width, picture.height)
            else {
                return;
            };
            self.bitmaps
                .insert(key.to_owned(), (bitmap, Arc::clone(picture)));
        }
        let Some((bitmap, _)) = self.bitmaps.get(key) else {
            return;
        };
        let (w, h) = (picture.width.max(1) as f32, picture.height.max(1) as f32);
        let fit = (rect.w / w).min(rect.h / h);
        let (dw, dh) = (w * fit, h * fit);
        let mut dest = Rect::new(
            rect.x + (rect.w - dw) / 2.0,
            rect.y + (rect.h - dh) / 2.0,
            dw,
            dh,
        );
        // A picture made for the pixels it takes (icons, covers) lands on whole pixels, so each of
        // its pixels is one on screen and nothing blurs it.
        if (dw * self.scale - w).abs() < 0.5 && (dh * self.scale - h).abs() < 0.5 {
            let snap = |value: f32| (value * self.scale).round() / self.scale;
            dest = Rect::new(snap(dest.x), snap(dest.y), w / self.scale, h / self.scale);
        }
        self.session
            .draw_bitmap(bitmap, &to_canvas(dest), self.alpha);
    }

    fn spinner(&mut self, rect: Rect) {
        self.animating = true;
        let (cx, cy) = (rect.x + rect.w / 2.0, rect.y + rect.h / 2.0);
        let radius = rect.w.min(rect.h) / 2.0 - 2.0;
        let dot = (rect.w.min(rect.h) / 10.0).max(1.5);
        let dots = 8;
        for i in 0..dots {
            let angle = self.elapsed * 5.0 + i as f32 * TAU / dots as f32;
            let alpha = (i + 1) as f32 / dots as f32;
            let center = Vector2::new(cx + radius * angle.cos(), cy + radius * angle.sin());
            self.session.fill_ellipse(
                &Ellipse::circle(center, dot),
                self.paint(theme::with_alpha(theme::TEXT, (alpha * 255.0) as u8)),
            );
        }
    }

    fn push_clip(&self, rect: Rect) {
        let context = self.session.as_interface::<ID2D1DeviceContext>();
        let clip = D2D1_RECT_F {
            left: rect.x,
            top: rect.y,
            right: rect.right(),
            bottom: rect.bottom(),
        };
        // SAFETY: `clip` is a valid rectangle for the call; the session is between BeginDraw and
        // EndDraw, and every push is popped by `pop_clip` before the session ends.
        unsafe { context.PushAxisAlignedClip(&clip, D2D1_ANTIALIAS_MODE_PER_PRIMITIVE) };
    }

    fn pop_clip(&self) {
        let context = self.session.as_interface::<ID2D1DeviceContext>();
        // SAFETY: pairs with the `push_clip` just before it, in the same session.
        unsafe { context.PopAxisAlignedClip() };
    }

    fn element<M>(&mut self, el: &El<M>, placed: &Placed, ui: &UiState) {
        if placed.clip.is_empty() {
            return;
        }
        let saved = self.alpha;
        self.alpha *= el.opacity
            * if el.enabled {
                1.0
            } else {
                super::theme::DISABLED_OPACITY
            };
        let rect = placed.rect;
        let hover = ui.hover == Some(placed.id);
        let pressed = hover && ui.pressed == Some(placed.id);
        let fill = if pressed {
            input::fill_for(el, true, true)
        } else {
            // The hover fill fades in and out (`input::HOVER_FADE`).
            let amount = ui.hover_amount(placed.id, self.now);
            if amount > 0.0 && amount < 1.0 && el.enabled && el.hover_fill.is_some() {
                self.animating = true;
            }
            blend(
                input::fill_for(el, false, false),
                input::fill_for(el, true, false),
                amount,
            )
        };
        if let Some(fill) = fill {
            self.fill_rounded(rect, el.radius, fill);
        }
        if let Some((color, width)) = el.stroke {
            self.stroke_rounded(rect.inset(Edges::all(width / 2.0)), el.radius, color, width);
        }
        match &el.kind {
            Kind::Box | Kind::Scroll => {}
            Kind::Edit {
                text, placeholder, ..
            } => self.edit(placed, text, placeholder, ui),
            Kind::Text(spec) => self.text(spec, rect.inset(el.padding)),
            Kind::Image { key, picture } => self.picture(key, picture, rect.inset(el.padding)),
            Kind::Spinner => self.spinner(rect),
            Kind::DropDown {
                items, selected, ..
            } => {
                self.dropdown_face(rect, items, *selected, hover, pressed);
            }
            Kind::Paint(painter) => {
                let interaction = Interaction {
                    hover,
                    pressed,
                    focused: ui.focus == Some(placed.id),
                    enabled: el.enabled,
                };
                painter(self, rect, interaction);
            }
            Kind::Slider {
                value,
                min,
                max,
                unit,
                ..
            } => {
                // While dragged, the value under the pointer.
                let value = match ui.sliding {
                    Some((id, dragged)) if id == placed.id => dragged,
                    _ => *value,
                };
                self.slider(rect, (value, *min, *max), unit, hover || pressed);
            }
        }
        if let Some(info) = placed.scroll {
            self.push_clip(info.viewport.intersect(&placed.clip));
            for (child, child_placed) in el.children.iter().zip(&placed.children) {
                self.element(child, child_placed, ui);
            }
            self.pop_clip();
            if let Some(thumb) = input::thumb_rect(&info, ui.scrollbar_hover == Some(placed.id)) {
                let color = if ui.scrollbar_hover == Some(placed.id) {
                    0x8C
                } else {
                    0x5C
                };
                self.fill_rounded(thumb, thumb.w / 2.0, theme::with_alpha(theme::TEXT, color));
            }
        } else {
            for (child, child_placed) in el.children.iter().zip(&placed.children) {
                self.element(child, child_placed, ui);
            }
        }
        // Over the children: a cover tile's picture would hide a hover fill.
        if let Some(tint) = el.hover_overlay.filter(|_| el.enabled) {
            let amount = if pressed {
                1.6
            } else {
                ui.hover_amount(placed.id, self.now)
            };
            if amount > 0.0 && amount < 1.0 {
                self.animating = true;
            }
            if amount > 0.0 {
                let tint = ColorF::new(tint.r, tint.g, tint.b, (tint.a * amount).min(1.0));
                self.fill_rounded(rect, el.radius, tint);
            }
        }
        // A text box shows its focus with the caret and its accent line instead.
        if ui.focus_visible && ui.focus == Some(placed.id) && !matches!(el.kind, Kind::Edit { .. })
        {
            let ring = rect.inset(Edges::all(-2.0));
            self.stroke_rounded(ring, el.radius + 2.0, theme::TEXT, 2.0);
        }
        self.alpha = saved;
    }

    /// A text box's inside: its text or placeholder, the selection, the caret and an IME
    /// composition while it has the keyboard, and the focused box's accent line.
    fn edit(&mut self, placed: &Placed, text: &str, placeholder: &str, ui: &UiState) {
        let rect = placed.rect;
        let inner = input::edit_inner(rect);
        let focused = ui.focus == Some(placed.id);
        let state = ui.edits.get(&placed.id);
        let scroll = state.map_or(0.0, |state| state.scroll);
        let composing = ui
            .composition
            .as_ref()
            .filter(|composition| focused && composition.id == placed.id);
        let x = inner.x - scroll;
        self.push_clip(inner.intersect(&placed.clip));
        if text.is_empty() && composing.is_none() {
            if !placeholder.is_empty() {
                let spec = edit_spec(placeholder, theme::TEXT_TERTIARY);
                self.text.with_layout(&spec, f32::INFINITY, |layout| {
                    let y = line_top(rect, layout);
                    self.session.draw_text_layout(
                        Vector2::new(inner.x, y),
                        layout,
                        self.paint(spec.color),
                    );
                });
            }
            if focused && ui.caret_on {
                let spec = edit_spec("", theme::TEXT);
                self.text.with_layout(&spec, f32::INFINITY, |layout| {
                    let caret = layout.caret_bounds(0, false);
                    let y = line_top(rect, layout);
                    self.caret(inner.x + caret.left, y + caret.top, caret.height());
                });
            }
        } else if let Some(composition) = composing {
            // The composition stands in for the selection until the IME gives its result.
            let selection = state.map_or(text.len()..text.len(), |s| s.edit.selection());
            let shown = format!(
                "{}{}{}",
                &text[..selection.start],
                composition.text,
                &text[selection.end..]
            );
            let marked = selection.start..selection.start + composition.text.len();
            let caret = selection.start + composition.cursor.min(composition.text.len());
            let spec = edit_spec(&shown, theme::TEXT);
            self.text.with_marked_layout(&spec, marked, |layout| {
                let y = line_top(rect, layout);
                self.session
                    .draw_text_layout(Vector2::new(x, y), layout, self.paint(spec.color));
                if ui.caret_on {
                    let bounds = caret_at(layout, &shown, caret);
                    self.caret(x + bounds.left, y + bounds.top, bounds.height());
                }
            });
        } else {
            let spec = edit_spec(text, theme::TEXT);
            self.text.with_layout(&spec, f32::INFINITY, |layout| {
                let y = line_top(rect, layout);
                if let Some(state) = state.filter(|state| focused && state.edit.has_selection()) {
                    for part in layout.range_bounds(state.edit.selection()) {
                        let mark =
                            Rect::new(x + part.left, y + part.top, part.width(), part.height());
                        self.fill_rect(mark, theme::TEXT_SELECTION);
                    }
                }
                self.session
                    .draw_text_layout(Vector2::new(x, y), layout, self.paint(spec.color));
                if focused
                    && ui.caret_on
                    && let Some(state) = state
                {
                    let bounds = caret_at(layout, text, state.edit.caret());
                    self.caret(x + bounds.left, y + bounds.top, bounds.height());
                }
            });
        }
        self.pop_clip();
        if focused {
            let inset = theme::RADIUS_CONTROL / 2.0;
            let line = Rect::new(
                rect.x + inset,
                rect.bottom() - 2.0,
                rect.w - 2.0 * inset,
                2.0,
            );
            self.fill_rounded(line, 1.0, theme::ACCENT);
        }
    }

    /// The text caret: a line one DIP wide.
    fn caret(&self, x: f32, y: f32, height: f32) {
        let x = x.round() + 0.5;
        self.line((x, y), (x, y + height), theme::TEXT, 1.0);
    }

    /// The text box menu: Cut, Copy, Paste and Select all with their shortcuts, greyed when they
    /// can't act.
    fn edit_menu(&self, menu: &input::EditMenu) {
        let rect = menu.rect;
        self.glow(
            rect,
            theme::RADIUS_FLYOUT,
            theme::with_alpha(ColorF::BLACK, 0xFF),
            10.0,
        );
        self.fill_rounded(rect, theme::RADIUS_FLYOUT, theme::FLYOUT);
        self.stroke_rounded(
            rect.inset(Edges::all(0.5)),
            theme::RADIUS_FLYOUT,
            theme::FLYOUT_STROKE,
            1.0,
        );
        for (index, command) in input::EditCommand::ALL.iter().enumerate() {
            let row = menu.row(index);
            let enabled = menu.enabled[index];
            if enabled && menu.hover == Some(index) {
                self.fill_rounded(row, theme::RADIUS_CONTROL, theme::SUBTLE_HOVER);
            }
            let mut label = item_spec(command.label());
            let mut keys = item_spec(command.shortcut());
            keys.size = theme::TEXT_SMALL;
            keys.color = theme::TEXT_TERTIARY;
            if !enabled {
                label.color = theme::with_alpha(theme::TEXT, 0x5C);
                keys.color = theme::with_alpha(theme::TEXT, 0x40);
            }
            self.text(&label, Rect::new(row.x + 12.0, row.y, row.w - 24.0, row.h));
            let width = self.measure(&keys, row.w).0;
            self.text(
                &keys,
                Rect::new(row.right() - 12.0 - width, row.y, width + 1.0, row.h),
            );
        }
    }

    /// A slider: the track, its filled part, the thumb (a ring with an accent dot that grows
    /// under the pointer, as WinUI's does) and the value.
    fn slider(&self, rect: Rect, (value, min, max): (u32, u32, u32), unit: &str, hot: bool) {
        let track = input::slider_track(rect);
        let ratio = if max > min {
            (value - min) as f32 / (max - min) as f32
        } else {
            0.0
        };
        let middle = rect.y + rect.h / 2.0;
        let bar = Rect::new(track.x, middle - 2.0, track.w, 4.0);
        self.fill_rounded(bar, 2.0, theme::with_alpha(theme::TEXT, 0x5C));
        self.fill_rounded(
            Rect::new(bar.x, bar.y, bar.w * ratio, bar.h),
            2.0,
            theme::ACCENT,
        );
        let center = Vector2::new(track.x + track.w * ratio, middle);
        let thumb = input::SLIDER_THUMB / 2.0;
        self.session
            .fill_ellipse(&Ellipse::circle(center, thumb), self.paint(theme::FLYOUT));
        self.session.draw_ellipse(
            &Ellipse::circle(center, thumb - 0.5),
            self.paint(theme::FLYOUT_STROKE),
            1.0,
        );
        self.session.fill_ellipse(
            &Ellipse::circle(center, if hot { 6.0 } else { 5.0 }),
            self.paint(theme::ACCENT),
        );
        let label = TextSpec {
            text: format!("{value}{unit}"),
            size: theme::TEXT_SIZE,
            weight: 600,
            font: Font::Text,
            color: theme::TEXT,
            wrap: false,
            align: TextAlign::Start,
        };
        self.text(
            &label,
            Rect::new(
                rect.right() - input::SLIDER_VALUE_WIDTH + 8.0,
                rect.y,
                input::SLIDER_VALUE_WIDTH - 8.0,
                rect.h,
            ),
        );
    }

    fn dropdown_face(
        &self,
        rect: Rect,
        items: &[String],
        selected: Option<usize>,
        hover: bool,
        pressed: bool,
    ) {
        let fill = if pressed {
            theme::CONTROL_PRESSED
        } else if hover {
            theme::CONTROL_HOVER
        } else {
            theme::CONTROL
        };
        self.fill_rounded(rect, theme::RADIUS_CONTROL, fill);
        if let Some(label) = selected.and_then(|i| items.get(i)) {
            let spec = item_spec(label);
            self.text(
                &spec,
                Rect::new(rect.x + 10.0, rect.y, rect.w - 40.0, rect.h),
            );
        }
        let chevron = glyph_spec(theme::glyph::CHEVRON_DOWN, 10.0, theme::TEXT_SECONDARY);
        self.text(
            &chevron,
            Rect::new(rect.right() - 26.0, rect.y, 16.0, rect.h),
        );
    }

    fn overlays<M: Clone>(&mut self, frame: &Frame<M>, ui: &UiState) {
        if let Some((rect, items, selected, _)) = frame.dropdown_popup(ui) {
            self.glow(
                rect,
                theme::RADIUS_FLYOUT,
                theme::with_alpha(ColorF::BLACK, 0xFF),
                10.0,
            );
            self.fill_rounded(rect, theme::RADIUS_FLYOUT, theme::FLYOUT);
            self.stroke_rounded(
                rect.inset(Edges::all(0.5)),
                theme::RADIUS_FLYOUT,
                theme::FLYOUT_STROKE,
                1.0,
            );
            self.push_clip(rect);
            for (i, item) in items.iter().enumerate() {
                let row = Rect::new(
                    rect.x + 4.0,
                    rect.y + 4.0 + i as f32 * ITEM_HEIGHT,
                    rect.w - 8.0,
                    ITEM_HEIGHT,
                );
                if ui.dropdown_hover == Some(i) {
                    self.fill_rounded(row, theme::RADIUS_CONTROL, theme::SUBTLE_HOVER);
                }
                if selected == Some(i) {
                    let bar = Rect::new(row.x, row.y + 8.0, 3.0, row.h - 16.0);
                    self.fill_rounded(bar, 1.5, theme::ACCENT);
                }
                self.text(
                    &item_spec(item),
                    Rect::new(row.x + 12.0, row.y, row.w - 20.0, row.h),
                );
            }
            self.pop_clip();
        }
        if let Some(menu) = &ui.edit_menu {
            self.edit_menu(menu);
        }
        if let Some(tip) = ui.tooltip.as_ref().filter(|tip| tip.shown) {
            let mut spec = item_spec(&tip.text);
            spec.size = theme::TEXT_SMALL;
            spec.wrap = true;
            let size = self.measure(&spec, 320.0);
            let rect = input::tooltip_rect(tip.at, size, ui.window);
            self.fill_rounded(rect, theme::RADIUS_CONTROL, theme::TOOLTIP);
            self.stroke_rounded(
                rect.inset(Edges::all(0.5)),
                theme::RADIUS_CONTROL,
                theme::FLYOUT_STROKE,
                1.0,
            );
            self.text(
                &spec,
                Rect::new(rect.x + 8.0, rect.y + 5.0, size.0 + 1.0, size.1),
            );
        }
    }
}

/// A text box's text: the body font on one line that never wraps or trims (the box scrolls it).
pub fn edit_spec(text: &str, color: ColorF) -> TextSpec {
    TextSpec {
        text: text.to_owned(),
        size: theme::TEXT_SIZE,
        weight: 400,
        font: Font::Text,
        color,
        wrap: false,
        align: TextAlign::Start,
    }
}

/// The caret's rectangle before byte `position` of `text` (a zero-width line): at the end, the
/// trailing edge of the last character, which DirectWrite places reliably.
pub fn caret_at(layout: &TextLayout, text: &str, position: usize) -> windows_text::Rect {
    let position = position.min(text.len());
    match text[..position].char_indices().next_back() {
        Some((last, _)) if position == text.len() => layout.caret_bounds(last, true),
        _ => layout.caret_bounds(position, false),
    }
}

/// Where a text box's line starts, centred in its frame, on a whole DIP.
fn line_top(rect: Rect, layout: &TextLayout) -> f32 {
    (rect.y + ((rect.h - layout.metrics().height) / 2.0).max(0.0)).round()
}

/// A glyph's text spec.
pub fn glyph_spec(glyph: &str, size: f32, color: ColorF) -> TextSpec {
    TextSpec {
        text: glyph.to_owned(),
        size,
        weight: 400,
        font: Font::Icons,
        color,
        wrap: false,
        align: TextAlign::Center,
    }
}
