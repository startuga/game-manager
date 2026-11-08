//! The element tree a view returns: boxes laid out as columns, rows or layers, text, glyphs,
//! pictures, and interactive widgets, each with the `Message` it sends when used.
//!
//! A view is rebuilt from the app's state whenever something changes (the Elm style the WinUI
//! window had), then laid out (`layout.rs`), drawn (`render.rs`) and hit-tested (`input.rs`).
//! What a view cannot know (the pointer over a button, a list's scroll offset, an open drop-down)
//! lives in `input::UiState`, keyed by each element's id; elements that keep such state across
//! frames get a stable `key`, others are identified by their place in the tree.
//!
//! Builders read like the reactor's did: `col([...]).gap(8.0).pad(12.0)`, `text("Play").bold()`,
//! `.on_click(Message::Play)`.

use std::rc::Rc;
use std::sync::Arc;
use windows_canvas::ColorF;

use super::theme;

/// What an element sends with a value: a picked item, typed text, a pointer position.
pub type OnValue<M, T> = Rc<dyn Fn(T) -> M>;

/// Custom drawing for an element (the Play button): the painter, the element's rectangle, and how
/// the pointer is interacting with it.
pub type Painter = Rc<dyn Fn(&mut super::render::Painter<'_>, Rect, Interaction)>;

/// A rectangle in DIPs.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.right() && y >= self.y && y < self.bottom()
    }
    /// The overlap of two rectangles (empty when they don't meet).
    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let w = (self.right().min(other.right()) - x).max(0.0);
        let h = (self.bottom().min(other.bottom()) - y).max(0.0);
        Rect::new(x, y, w, h)
    }
    pub fn is_empty(&self) -> bool {
        self.w <= 0.0 || self.h <= 0.0
    }
    /// Shrunk by `edges` on each side.
    pub fn inset(&self, edges: Edges) -> Rect {
        Rect::new(
            self.x + edges.left,
            self.y + edges.top,
            (self.w - edges.left - edges.right).max(0.0),
            (self.h - edges.top - edges.bottom).max(0.0),
        )
    }
}

/// How the pointer is interacting with an element, for drawing its states.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Interaction {
    pub hover: bool,
    pub pressed: bool,
    pub focused: bool,
    pub enabled: bool,
}

/// An element's size along one axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Size {
    /// As large as its content.
    #[default]
    Auto,
    /// A fixed size in DIPs.
    Px(f32),
    /// A share of the space left in its row or column (the weight), or all of it across.
    Fill(f32),
}

/// Where an element sits in the space its parent gives it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    #[default]
    Stretch,
    Start,
    Center,
    End,
}

/// How a box lays out its children.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    /// Top to bottom.
    #[default]
    Column,
    /// Left to right.
    Row,
    /// On top of each other, the last child on top (pages and their dialogs, menus).
    Layers,
}

/// Padding on each side.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Edges {
    pub const fn all(v: f32) -> Self {
        Self {
            left: v,
            top: v,
            right: v,
            bottom: v,
        }
    }
    pub const fn xy(x: f32, y: f32) -> Self {
        Self {
            left: x,
            top: y,
            right: x,
            bottom: y,
        }
    }
    pub const fn new(left: f32, top: f32, right: f32, bottom: f32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

/// The font a text uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Font {
    #[default]
    Text,
    /// Large headings (Segoe UI Variable Display).
    Display,
    /// Page titles (Oxanium).
    Title,
    /// Segoe Fluent Icons glyphs.
    Icons,
}

/// Horizontal alignment of text within its rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextAlign {
    #[default]
    Start,
    Center,
}

/// A text and how it looks.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpec {
    pub text: String,
    pub size: f32,
    pub weight: u16,
    pub font: Font,
    pub color: ColorF,
    /// Wraps onto more lines; otherwise one line, ending in "…" when it doesn't fit.
    pub wrap: bool,
    pub align: TextAlign,
}

/// What an element is (its children are laid out by `El::axis` whatever it is).
pub enum Kind<M> {
    /// Only a box: its fill, outline and children.
    Box,
    Text(TextSpec),
    /// A picture, uploaded to the GPU once and kept by `key`.
    Image {
        key: String,
        picture: Arc<super::image::Picture>,
    },
    /// A progress ring, turning while it is shown.
    Spinner,
    /// A Win32 EDIT control placed over the element (`shell::edit_box`). Enter sends `on_enter`;
    /// with `clear_on_escape`, Escape empties a box that has text (the search).
    Edit {
        text: String,
        placeholder: String,
        on_change: OnValue<M, String>,
        on_enter: Option<M>,
        clear_on_escape: bool,
    },
    /// A vertical scroll view of its one child.
    Scroll,
    /// A drop-down list: the picked item shows, a click opens the list over the page.
    DropDown {
        items: Vec<String>,
        selected: Option<usize>,
        on_select: OnValue<M, usize>,
    },
    /// Drawn by a painter of its own.
    Paint(Painter),
    /// A slider from `min` to `max`, showing its value with `unit` ("60%"). Dragging moves it
    /// (`input::UiState::sliding`) and `on_change` gets the value on release; the arrows,
    /// Page Up/Down, Home and End send it at once.
    Slider {
        value: u32,
        min: u32,
        max: u32,
        unit: &'static str,
        on_change: OnValue<M, u32>,
    },
}

/// What assistive technology and the UI Automation tree call an element.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    #[default]
    None,
    Button,
    /// A row of a list, a nav item or a tab: selectable.
    ListItem,
    Tab,
    CheckBox,
    ComboBox,
    Text,
    Edit,
    Group,
    Slider,
}

pub struct El<M> {
    pub kind: Kind<M>,
    pub axis: Axis,
    pub gap: f32,
    pub padding: Edges,
    pub width: Size,
    pub height: Size,
    pub min_width: f32,
    pub max_width: f32,
    pub min_height: f32,
    /// The most a content-sized (`Auto`) height grows to: a scroll view inside a dialog.
    pub max_height: f32,
    /// Placement across the parent's direction (and both ways among layers).
    pub halign: Align,
    pub valign: Align,
    /// Placement of the children along this box's direction when they don't fill it.
    pub justify: Align,
    pub fill: Option<ColorF>,
    pub hover_fill: Option<ColorF>,
    pub pressed_fill: Option<ColorF>,
    pub stroke: Option<(ColorF, f32)>,
    pub radius: f32,
    pub opacity: f32,
    pub children: Vec<El<M>>,
    /// A stable identity across frames (scroll offsets, focus, typed text).
    pub key: Option<String>,
    pub on_click: Option<M>,
    /// A right-click, with the pointer's position in window DIPs.
    pub on_context: Option<OnValue<M, (f32, f32)>>,
    /// Dragging it: the pointer's position in window DIPs while the button is held down (a
    /// resize grip). It also shows the left-right arrows under the pointer.
    pub on_drag: Option<OnValue<M, (f32, f32)>>,
    pub tooltip: Option<String>,
    pub enabled: bool,
    pub role: Role,
    /// The name in the UI Automation tree; else its text.
    pub name: Option<String>,
    /// A selected row, tab or nav item; a checked check box.
    pub selected: bool,
    /// Takes every click within it, even where nothing below is clickable (a dialog's backdrop).
    pub modal: bool,
}

impl<M> El<M> {
    pub fn new(kind: Kind<M>) -> Self {
        Self {
            kind,
            axis: Axis::Column,
            gap: 0.0,
            padding: Edges::default(),
            width: Size::Auto,
            height: Size::Auto,
            min_width: 0.0,
            max_width: f32::INFINITY,
            min_height: 0.0,
            max_height: f32::INFINITY,
            halign: Align::Stretch,
            valign: Align::Stretch,
            justify: Align::Start,
            fill: None,
            hover_fill: None,
            pressed_fill: None,
            stroke: None,
            radius: 0.0,
            opacity: 1.0,
            children: Vec::new(),
            key: None,
            on_click: None,
            on_context: None,
            on_drag: None,
            tooltip: None,
            enabled: true,
            role: Role::None,
            name: None,
            selected: false,
            modal: false,
        }
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }
    pub fn pad(mut self, all: f32) -> Self {
        self.padding = Edges::all(all);
        self
    }
    pub fn pad_xy(mut self, x: f32, y: f32) -> Self {
        self.padding = Edges::xy(x, y);
        self
    }
    pub fn padding(mut self, edges: Edges) -> Self {
        self.padding = edges;
        self
    }
    pub fn width(mut self, width: f32) -> Self {
        self.width = Size::Px(width);
        self
    }
    pub fn height(mut self, height: f32) -> Self {
        self.height = Size::Px(height);
        self
    }
    pub fn size(mut self, width: f32, height: f32) -> Self {
        self.width = Size::Px(width);
        self.height = Size::Px(height);
        self
    }
    /// Takes the space left in a row (with this weight).
    pub fn fill_width(mut self) -> Self {
        self.width = Size::Fill(1.0);
        self
    }
    /// Takes the space left in a column.
    pub fn fill_height(mut self) -> Self {
        self.height = Size::Fill(1.0);
        self
    }
    pub fn grow(mut self) -> Self {
        self.width = Size::Fill(1.0);
        self.height = Size::Fill(1.0);
        self
    }
    pub fn min_width(mut self, width: f32) -> Self {
        self.min_width = width;
        self
    }
    pub fn max_width(mut self, width: f32) -> Self {
        self.max_width = width;
        self
    }
    pub fn max_height(mut self, height: f32) -> Self {
        self.max_height = height;
        self
    }
    pub fn halign(mut self, align: Align) -> Self {
        self.halign = align;
        self
    }
    pub fn valign(mut self, align: Align) -> Self {
        self.valign = align;
        self
    }
    /// Centered both ways within its slot.
    pub fn center(mut self) -> Self {
        self.halign = Align::Center;
        self.valign = Align::Center;
        self
    }
    pub fn justify(mut self, align: Align) -> Self {
        self.justify = align;
        self
    }
    pub fn bg(mut self, color: ColorF) -> Self {
        self.fill = Some(color);
        self
    }
    pub fn hover_bg(mut self, color: ColorF) -> Self {
        self.hover_fill = Some(color);
        self
    }
    pub fn pressed_bg(mut self, color: ColorF) -> Self {
        self.pressed_fill = Some(color);
        self
    }
    pub fn stroke(mut self, color: ColorF, width: f32) -> Self {
        self.stroke = Some((color, width));
        self
    }
    pub fn radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }
    pub fn key(mut self, key: impl Into<String>) -> Self {
        self.key = Some(key.into());
        self
    }
    pub fn on_click(mut self, message: M) -> Self {
        self.on_click = Some(message);
        self
    }
    pub fn on_drag(mut self, handler: impl Fn((f32, f32)) -> M + 'static) -> Self {
        self.on_drag = Some(Rc::new(handler));
        self
    }
    pub fn on_context(mut self, handler: impl Fn((f32, f32)) -> M + 'static) -> Self {
        self.on_context = Some(Rc::new(handler));
        self
    }
    pub fn tooltip(mut self, tooltip: impl Into<String>) -> Self {
        let tooltip = tooltip.into();
        self.tooltip = (!tooltip.is_empty()).then_some(tooltip);
        self
    }
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
    pub fn role(mut self, role: Role) -> Self {
        self.role = role;
        self
    }
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
    pub fn modal(mut self) -> Self {
        self.modal = true;
        self
    }
    pub fn child(mut self, child: El<M>) -> Self {
        self.children.push(child);
        self
    }
    pub fn children(mut self, children: impl IntoIterator<Item = El<M>>) -> Self {
        self.children.extend(children);
        self
    }

    /// Text styling; no effect on other elements.
    pub fn text_size(mut self, size: f32) -> Self {
        if let Kind::Text(spec) = &mut self.kind {
            spec.size = size;
        }
        self
    }
    pub fn weight(mut self, weight: u16) -> Self {
        if let Kind::Text(spec) = &mut self.kind {
            spec.weight = weight;
        }
        self
    }
    pub fn bold(self) -> Self {
        self.weight(600)
    }
    pub fn color(mut self, color: ColorF) -> Self {
        if let Kind::Text(spec) = &mut self.kind {
            spec.color = color;
        }
        self
    }
    pub fn font(mut self, font: Font) -> Self {
        if let Kind::Text(spec) = &mut self.kind {
            spec.font = font;
        }
        self
    }
    pub fn wrap(mut self) -> Self {
        if let Kind::Text(spec) = &mut self.kind {
            spec.wrap = true;
        }
        self
    }
    pub fn text_align(mut self, align: TextAlign) -> Self {
        if let Kind::Text(spec) = &mut self.kind {
            spec.align = align;
        }
        self
    }

    /// Whether a click or a key can act on it.
    pub fn is_interactive(&self) -> bool {
        self.on_click.is_some()
            || self.on_context.is_some()
            || self.on_drag.is_some()
            || matches!(
                self.kind,
                Kind::DropDown { .. } | Kind::Edit { .. } | Kind::Slider { .. }
            )
    }

    /// Its name for UI Automation: the explicit one, else the text of its first text descendant.
    pub fn accessible_name(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        fn first_text<M>(el: &El<M>) -> Option<&str> {
            if let Kind::Text(spec) = &el.kind
                && spec.font != Font::Icons
            {
                return Some(&spec.text);
            }
            el.children.iter().find_map(first_text)
        }
        first_text(self).unwrap_or_default().to_owned()
    }
}

/// A box laying out its children top to bottom.
pub fn col<M>(children: impl IntoIterator<Item = El<M>>) -> El<M> {
    El::new(Kind::Box).children(children)
}

/// A box laying out its children left to right; children are centered vertically unless they say
/// otherwise.
pub fn row<M>(children: impl IntoIterator<Item = El<M>>) -> El<M> {
    let mut el = El::new(Kind::Box).children(children);
    el.axis = Axis::Row;
    for child in &mut el.children {
        if child.valign == Align::Stretch && !matches!(child.height, Size::Fill(_)) {
            child.valign = Align::Center;
        }
    }
    el
}

/// Children on top of each other, the last one on top.
pub fn layers<M>(children: impl IntoIterator<Item = El<M>>) -> El<M> {
    let mut el = El::new(Kind::Box).children(children);
    el.axis = Axis::Layers;
    el
}

/// An empty box taking the space left.
pub fn spacer<M>() -> El<M> {
    El::new(Kind::Box).fill_width()
}

pub fn text<M>(text: impl Into<String>) -> El<M> {
    El::new(Kind::Text(TextSpec {
        text: text.into(),
        size: theme::TEXT_SIZE,
        weight: 400,
        font: Font::Text,
        color: theme::TEXT,
        wrap: false,
        align: TextAlign::Start,
    }))
}

/// A Segoe Fluent Icons glyph.
pub fn glyph<M>(glyph: &str, size: f32) -> El<M> {
    text(glyph).font(Font::Icons).text_size(size)
}

pub fn image<M>(key: impl Into<String>, picture: Arc<super::image::Picture>) -> El<M> {
    El::new(Kind::Image {
        key: key.into(),
        picture,
    })
}

pub fn spinner<M>(size: f32) -> El<M> {
    El::new(Kind::Spinner).size(size, size)
}

/// A text box. `key` keeps the same EDIT control (and its caret) across frames.
pub fn edit<M>(
    key: impl Into<String>,
    text: impl Into<String>,
    placeholder: impl Into<String>,
    on_change: impl Fn(String) -> M + 'static,
) -> El<M> {
    El::new(Kind::Edit {
        text: text.into(),
        placeholder: placeholder.into(),
        on_change: Rc::new(on_change),
        on_enter: None,
        clear_on_escape: false,
    })
    .key(key)
    .height(theme::CONTROL_HEIGHT)
    .role(Role::Edit)
}

/// A vertical scroll view of `content`. `key` keeps its offset across frames.
pub fn scroll<M>(key: impl Into<String>, content: El<M>) -> El<M> {
    El::new(Kind::Scroll).key(key).child(content)
}

pub fn dropdown<M>(
    key: impl Into<String>,
    items: Vec<String>,
    selected: Option<usize>,
    on_select: impl Fn(usize) -> M + 'static,
) -> El<M> {
    El::new(Kind::DropDown {
        items,
        selected,
        on_select: Rc::new(on_select),
    })
    .key(key)
    .height(theme::CONTROL_HEIGHT)
    .role(Role::ComboBox)
}

pub fn paint<M>(
    painter: impl Fn(&mut super::render::Painter<'_>, Rect, Interaction) + 'static,
) -> El<M> {
    El::new(Kind::Paint(Rc::new(painter)))
}

/// A slider from `min` to `max` with its value written after it in `unit`. `key` keeps a drag
/// going across frames; `on_change` gets the value picked.
pub fn slider<M>(
    key: impl Into<String>,
    value: u32,
    (min, max): (u32, u32),
    unit: &'static str,
    on_change: impl Fn(u32) -> M + 'static,
) -> El<M> {
    El::new(Kind::Slider {
        value: value.clamp(min, max.max(min)),
        min,
        max: max.max(min),
        unit,
        on_change: Rc::new(on_change),
    })
    .key(key)
    .height(theme::CONTROL_HEIGHT)
    .role(Role::Slider)
}

impl<M> El<M> {
    /// A text box's Enter.
    pub fn on_enter(mut self, message: M) -> Self {
        if let Kind::Edit { on_enter, .. } = &mut self.kind {
            *on_enter = Some(message);
        }
        self
    }

    /// Escape empties the text box while it has text (else Escape closes what is open).
    pub fn clear_on_escape(mut self) -> Self {
        if let Kind::Edit {
            clear_on_escape, ..
        } = &mut self.kind
        {
            *clear_on_escape = true;
        }
        self
    }

    /// Takes a share of the space left in a row: `weight` against the others' (1 each).
    pub fn fill_width_by(mut self, weight: f32) -> Self {
        self.width = Size::Fill(weight);
        self
    }
}
