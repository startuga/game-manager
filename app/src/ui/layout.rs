//! Lays out an element tree in a rectangle: a small flexbox.
//!
//! - **Columns and rows** give each child its size along their direction: fixed (`Size::Px`), as
//!   large as its content (`Auto`, measured), or a weighted share of what is left (`Fill`).
//!   Across, a child stretches or sits at the start, center or end (`halign` in a column, `valign`
//!   in a row). Without fill children, `justify` places the group.
//! - **Layers** give every child the whole box, placed by both alignments.
//! - **Scroll views** lay their child out at its full height, moved up by the offset kept in
//!   `input::UiState` (clamped here), and clip it to the view.
//!
//! Text is measured through `Measure`, so this module is plain arithmetic and is tested without
//! DirectWrite. Measurements are memoised per element and width within one pass.
//!
//! Every placed element gets an id: from its `key` when it has one (stable across frames even when
//! rows come and go), else from its parent's id and its index.

use std::cell::RefCell;
use std::collections::HashMap;

use super::el::{Align, Axis, El, Kind, Rect, Size, TextSpec};
use super::theme;

/// Measures text: its size laid out at most `max_width` wide (`f32::INFINITY` for one line).
pub trait Measure {
    fn text(&self, spec: &TextSpec, max_width: f32) -> (f32, f32);
}

/// A scroll view's state after layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollInfo {
    /// The offset actually used (the kept one, clamped to the content).
    pub offset: f32,
    /// The content's full height.
    pub content: f32,
    /// The visible area.
    pub viewport: Rect,
}

impl ScrollInfo {
    pub fn max_offset(&self) -> f32 {
        (self.content - self.viewport.h).max(0.0)
    }
}

/// An element's place: the same shape as the element tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub id: u64,
    pub rect: Rect,
    /// The part of `rect` that can be seen (inside every scroll view around it).
    pub clip: Rect,
    pub scroll: Option<ScrollInfo>,
    pub children: Vec<Placed>,
}

/// FNV-1a: stable across runs, so ids don't change from one frame to the next.
fn hash_bytes(seed: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01B3)
    })
}

/// The id of an element with this key.
pub fn key_id(key: &str) -> u64 {
    hash_bytes(0xCBF2_9CE4_8422_2325, key.as_bytes())
}

fn child_id(parent: u64, index: usize) -> u64 {
    hash_bytes(
        hash_bytes(0xCBF2_9CE4_8422_2325, &parent.to_le_bytes()),
        &index.to_le_bytes(),
    )
}

/// The root's id.
pub const ROOT_ID: u64 = 1;

pub struct Layout<'a, T: Measure> {
    measure: &'a T,
    scrolls: &'a HashMap<u64, f32>,
    memo: RefCell<HashMap<(usize, u32), (f32, f32)>>,
}

impl<'a, T: Measure> Layout<'a, T> {
    pub fn new(measure: &'a T, scrolls: &'a HashMap<u64, f32>) -> Self {
        Self {
            measure,
            scrolls,
            memo: RefCell::new(HashMap::new()),
        }
    }

    /// Lays `root` out in `rect`.
    pub fn place<M>(&self, root: &El<M>, rect: Rect) -> Placed {
        let id = root.key.as_deref().map_or(ROOT_ID, key_id);
        self.arrange(root, rect, id, rect)
    }

    /// The size `el` wants when it may be at most `available` wide.
    pub fn measure<M>(&self, el: &El<M>, available: f32) -> (f32, f32) {
        let key = (std::ptr::from_ref(el) as usize, available.to_bits());
        if let Some(size) = self.memo.borrow().get(&key) {
            return *size;
        }
        let size = self.measure_uncached(el, available);
        self.memo.borrow_mut().insert(key, size);
        size
    }

    fn measure_uncached<M>(&self, el: &El<M>, available: f32) -> (f32, f32) {
        let pad = el.padding;
        let outer = match el.width {
            Size::Px(width) => width,
            _ => available.min(el.max_width),
        };
        let inner = (outer - pad.horizontal()).max(0.0);
        let (content_w, content_h) = match &el.kind {
            Kind::Text(spec) => self
                .measure
                .text(spec, if spec.wrap { inner } else { f32::INFINITY }),
            Kind::DropDown { items, .. } => {
                let widest = items
                    .iter()
                    .map(|item| self.measure.text(&item_spec(item), f32::INFINITY).0)
                    .fold(0.0, f32::max);
                // The text's padding and the chevron.
                (widest + 40.0, theme::CONTROL_HEIGHT)
            }
            Kind::Box | Kind::Scroll => self.measure_children(el, inner),
            Kind::Image { .. }
            | Kind::Spinner
            | Kind::Edit { .. }
            | Kind::Paint(_)
            | Kind::Slider { .. } => (0.0, 0.0),
        };
        let width = match el.width {
            Size::Px(width) => width,
            _ => content_w + pad.horizontal(),
        }
        .clamp(el.min_width, el.max_width.max(el.min_width));
        let height = match el.height {
            Size::Px(height) => height,
            _ => (content_h + pad.vertical()).min(el.max_height),
        }
        .max(el.min_height);
        (width, height)
    }

    fn measure_children<M>(&self, el: &El<M>, inner: f32) -> (f32, f32) {
        let gaps = el.gap * el.children.len().saturating_sub(1) as f32;
        match el.axis {
            Axis::Column => {
                let (w, h) = el
                    .children
                    .iter()
                    .map(|child| self.measure(child, inner))
                    .fold((0.0f32, 0.0f32), |(w, h), (cw, ch)| (w.max(cw), h + ch));
                (w, h + gaps)
            }
            Axis::Row => {
                let (w, h) = self
                    .row_sizes(el, inner)
                    .into_iter()
                    .fold((0.0f32, 0.0f32), |(w, h), (cw, ch)| (w + cw, h.max(ch)));
                (w + gaps, h)
            }
            Axis::Layers => el
                .children
                .iter()
                .map(|child| self.measure(child, inner))
                .fold((0.0f32, 0.0f32), |(w, h), (cw, ch)| (w.max(cw), h.max(ch))),
        }
    }

    /// A row's children measured as `distribute` places them: the others first, then each
    /// filling child in its share of what they leave, so wrapped text is measured at the width it
    /// gets.
    fn row_sizes<M>(&self, el: &El<M>, inner: f32) -> Vec<(f32, f32)> {
        let gaps = el.gap * el.children.len().saturating_sub(1) as f32;
        let mut sizes = vec![(0.0f32, 0.0f32); el.children.len()];
        let mut used = gaps;
        let mut weights = 0.0;
        for (i, child) in el.children.iter().enumerate() {
            match child.width {
                Size::Fill(weight) => weights += weight,
                _ => {
                    sizes[i] = self.measure(child, inner);
                    used += sizes[i].0;
                }
            }
        }
        let free = (inner - used).max(0.0);
        for (i, child) in el.children.iter().enumerate() {
            if let Size::Fill(weight) = child.width {
                let share = (free * weight / weights)
                    .max(child.min_width)
                    .min(child.max_width.max(child.min_width));
                sizes[i] = self.measure(child, share);
            }
        }
        sizes
    }

    fn arrange<M>(&self, el: &El<M>, rect: Rect, id: u64, clip: Rect) -> Placed {
        let inner = rect.inset(el.padding);
        let visible = clip.intersect(&rect);
        let mut placed = Placed {
            id,
            rect,
            clip: visible,
            scroll: None,
            children: Vec::with_capacity(el.children.len()),
        };
        if let Kind::Scroll = el.kind {
            if let Some(child) = el.children.first() {
                let (_, content) = self.measure(child, inner.w);
                let content = content.max(inner.h);
                let max = (content - inner.h).max(0.0);
                let offset = self
                    .scrolls
                    .get(&id)
                    .copied()
                    .unwrap_or(0.0)
                    .clamp(0.0, max);
                let viewport = inner;
                let child_rect = Rect::new(inner.x, inner.y - offset, inner.w, content);
                let child_clip = visible.intersect(&viewport);
                placed.children.push(self.arrange(
                    child,
                    child_rect,
                    self.id_of(child, id, 0),
                    child_clip,
                ));
                placed.scroll = Some(ScrollInfo {
                    offset,
                    content,
                    viewport,
                });
            }
            return placed;
        }
        let rects = match el.axis {
            Axis::Column => self.distribute(el, inner, false),
            Axis::Row => self.distribute(el, inner, true),
            Axis::Layers => el
                .children
                .iter()
                .map(|child| self.place_in(child, inner))
                .collect(),
        };
        for (index, (child, child_rect)) in el.children.iter().zip(rects).enumerate() {
            let child_id = self.id_of(child, id, index);
            placed
                .children
                .push(self.arrange(child, child_rect, child_id, visible));
        }
        placed
    }

    fn id_of<M>(&self, el: &El<M>, parent: u64, index: usize) -> u64 {
        el.key
            .as_deref()
            .map_or_else(|| child_id(parent, index), key_id)
    }

    /// A child's size along one axis and across, given the space it has.
    fn cross_size<M>(
        &self,
        child: &El<M>,
        available: f32,
        size: Size,
        align: Align,
        horizontal: bool,
    ) -> f32 {
        let (min, max) = if horizontal {
            (child.min_width, child.max_width.max(child.min_width))
        } else {
            (child.min_height, f32::INFINITY)
        };
        let natural = |width: f32| {
            let (w, h) = self.measure(child, width);
            if horizontal { w } else { h }
        };
        match size {
            Size::Px(px) => px,
            Size::Fill(_) => available.clamp(min, max),
            Size::Auto if align == Align::Stretch => available.clamp(min, max),
            Size::Auto => natural(available).min(available.max(min)),
        }
    }

    /// Places a layer: its size from its own width and height, its position from both
    /// alignments.
    fn place_in<M>(&self, child: &El<M>, inner: Rect) -> Rect {
        let width = self.cross_size(child, inner.w, child.width, child.halign, true);
        let height = match child.height {
            Size::Px(px) => px,
            Size::Fill(_) => inner.h,
            Size::Auto if child.valign == Align::Stretch => inner.h,
            Size::Auto => self
                .measure(child, width)
                .1
                .min(inner.h.max(child.min_height)),
        };
        Rect::new(
            align_in(inner.x, inner.w, width, child.halign),
            align_in(inner.y, inner.h, height, child.valign),
            width,
            height,
        )
    }

    /// Sizes and places a row's (`horizontal`) or a column's children.
    fn distribute<M>(&self, el: &El<M>, inner: Rect, horizontal: bool) -> Vec<Rect> {
        let count = el.children.len();
        let (main_space, cross_space) = if horizontal {
            (inner.w, inner.h)
        } else {
            (inner.h, inner.w)
        };
        let gaps = el.gap * count.saturating_sub(1) as f32;

        // Across first: a column's children need their width before their height is measured.
        let cross: Vec<f32> = el
            .children
            .iter()
            .map(|child| {
                if horizontal {
                    // A row's child: its width decides its height, so it is computed below.
                    0.0
                } else {
                    self.cross_size(child, cross_space, child.width, child.halign, true)
                }
            })
            .collect();

        let mut main = vec![0.0f32; count];
        let mut weights = 0.0;
        let mut used = gaps;
        for (i, child) in el.children.iter().enumerate() {
            let size = if horizontal {
                child.width
            } else {
                child.height
            };
            match size {
                Size::Px(px) => main[i] = px,
                Size::Fill(weight) => weights += weight,
                Size::Auto => {
                    main[i] = if horizontal {
                        self.measure(child, main_space).0
                    } else {
                        self.measure(child, cross[i]).1
                    };
                }
            }
            if !matches!(size, Size::Fill(_)) {
                used += main[i];
            }
        }
        let free = (main_space - used).max(0.0);
        if weights > 0.0 {
            for (i, child) in el.children.iter().enumerate() {
                let size = if horizontal {
                    child.width
                } else {
                    child.height
                };
                if let Size::Fill(weight) = size {
                    let min = if horizontal {
                        child.min_width
                    } else {
                        child.min_height
                    };
                    let mut share = (free * weight / weights).max(min);
                    if horizontal {
                        share = share.min(child.max_width.max(child.min_width));
                    }
                    main[i] = share;
                    used += share;
                }
            }
        }

        let total = used;
        let mut cursor = match el.justify {
            Align::Center => ((main_space - total) / 2.0).max(0.0),
            Align::End => (main_space - total).max(0.0),
            Align::Start | Align::Stretch => 0.0,
        };
        let origin = if horizontal { inner.x } else { inner.y };
        let mut rects = Vec::with_capacity(count);
        for (i, child) in el.children.iter().enumerate() {
            let rect = if horizontal {
                let width = main[i];
                let height = match child.height {
                    Size::Px(px) => px,
                    Size::Fill(_) => cross_space,
                    Size::Auto if child.valign == Align::Stretch => cross_space,
                    Size::Auto => self
                        .measure(child, width)
                        .1
                        .min(cross_space.max(child.min_height)),
                };
                Rect::new(
                    origin + cursor,
                    align_in(inner.y, cross_space, height, child.valign),
                    width,
                    height,
                )
            } else {
                let width = cross[i];
                Rect::new(
                    align_in(inner.x, cross_space, width, child.halign),
                    origin + cursor,
                    width,
                    main[i],
                )
            };
            cursor += main[i] + el.gap;
            rects.push(rect);
        }
        rects
    }
}

/// Where something `size` long starts in `space` from `start`.
fn align_in(start: f32, space: f32, size: f32, align: Align) -> f32 {
    match align {
        Align::Start | Align::Stretch => start,
        Align::Center => start + (space - size) / 2.0,
        Align::End => start + space - size,
    }
}

/// How a drop-down item is measured and drawn.
pub fn item_spec(text: &str) -> TextSpec {
    TextSpec {
        text: text.to_owned(),
        size: theme::TEXT_SIZE,
        weight: 400,
        font: super::el::Font::Text,
        color: theme::TEXT,
        wrap: false,
        align: super::el::TextAlign::Start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::el::{col, layers, row, scroll, spacer, text};

    /// Text is 7 DIPs a character and 16 high; wrapped text breaks at the width.
    struct Fixed;
    impl Measure for Fixed {
        fn text(&self, spec: &TextSpec, max_width: f32) -> (f32, f32) {
            let width = spec.text.chars().count() as f32 * 7.0;
            if width <= max_width {
                (width, 16.0)
            } else {
                let lines = (width / max_width).ceil();
                (max_width, 16.0 * lines)
            }
        }
    }

    fn place(root: &El<()>, w: f32, h: f32) -> Placed {
        let scrolls = HashMap::new();
        Layout::new(&Fixed, &scrolls).place(root, Rect::new(0.0, 0.0, w, h))
    }

    #[test]
    fn a_column_gives_fixed_and_content_sizes_then_shares_the_rest() {
        let root: El<()> = col([
            text("title").height(30.0),
            text("auto"),
            col([]).fill_height(),
            col([]).height(10.0),
        ])
        .gap(4.0)
        .pad(10.0);
        let placed = place(&root, 200.0, 200.0);
        let rects: Vec<Rect> = placed.children.iter().map(|c| c.rect).collect();
        assert_eq!(rects[0], Rect::new(10.0, 10.0, 180.0, 30.0));
        assert_eq!(rects[1], Rect::new(10.0, 44.0, 180.0, 16.0));
        // 180 inner high, less 30 + 16 + 10 and three gaps of 4.
        assert_eq!(rects[2], Rect::new(10.0, 64.0, 180.0, 112.0));
        assert_eq!(rects[3], Rect::new(10.0, 180.0, 180.0, 10.0));
    }

    #[test]
    fn a_row_fills_between_content_and_centers_children_vertically() {
        let root: El<()> = row([text("abc"), spacer(), text("de").width(40.0)]).gap(8.0);
        let placed = place(&root, 300.0, 40.0);
        let rects: Vec<Rect> = placed.children.iter().map(|c| c.rect).collect();
        assert_eq!(rects[0], Rect::new(0.0, 12.0, 21.0, 16.0));
        assert_eq!(rects[1].x, 29.0);
        assert_eq!(rects[1].w, 300.0 - 21.0 - 40.0 - 16.0);
        assert_eq!(rects[2], Rect::new(260.0, 12.0, 40.0, 16.0));
    }

    #[test]
    fn justify_centers_a_group_without_fill_children() {
        let root: El<()> = row([text("ab"), text("cd")])
            .gap(6.0)
            .justify(Align::Center);
        let placed = place(&root, 100.0, 20.0);
        // 14 + 6 + 14 = 34 wide, centered in 100.
        assert_eq!(placed.children[0].rect.x, 33.0);
        assert_eq!(placed.children[1].rect.x, 53.0);
    }

    #[test]
    fn wrapped_text_takes_the_lines_it_needs_at_its_width() {
        let root: El<()> = col([col([text("a".repeat(30)).wrap()]).width(100.0)]);
        let placed = place(&root, 400.0, 400.0);
        // 210 DIPs of text in 100: three lines.
        assert_eq!(placed.children[0].children[0].rect.h, 48.0);
    }

    #[test]
    fn a_row_is_as_tall_as_its_wrapped_text_at_the_width_left_to_it() {
        // A fixed label beside wrapping text, in a column: the row's height counts the lines the
        // text needs in the 90 DIPs it is left (200 less 100 and the gap), not in all 200.
        let root: El<()> = col([
            row([
                text("label").width(100.0),
                text("a".repeat(20)).wrap().fill_width(),
            ])
            .gap(10.0),
            text("next"),
        ]);
        let placed = place(&root, 200.0, 400.0);
        // 140 DIPs of text in 90: two lines.
        assert_eq!(placed.children[0].rect.h, 32.0);
        assert_eq!(placed.children[1].rect.y, 32.0);
    }

    #[test]
    fn layers_are_placed_by_both_alignments() {
        let root: El<()> = layers([col([]).grow(), col([]).size(50.0, 20.0).center()]);
        let placed = place(&root, 200.0, 100.0);
        assert_eq!(placed.children[0].rect, Rect::new(0.0, 0.0, 200.0, 100.0));
        assert_eq!(placed.children[1].rect, Rect::new(75.0, 40.0, 50.0, 20.0));
    }

    #[test]
    fn a_scroll_view_clamps_its_offset_and_clips_its_content() {
        let rows: Vec<El<()>> = (0..10).map(|_| text("row").height(20.0)).collect();
        let root: El<()> = scroll("list", col(rows)).height(50.0);
        let mut scrolls = HashMap::new();
        scrolls.insert(key_id("list"), 1000.0);
        let placed = Layout::new(&Fixed, &scrolls).place(&root, Rect::new(0.0, 0.0, 100.0, 50.0));
        let info = placed.scroll.unwrap();
        assert_eq!(info.content, 200.0);
        assert_eq!(info.offset, 150.0);
        let content = &placed.children[0];
        assert_eq!(content.rect.y, -150.0);
        // The last row is in view; the first is clipped away.
        assert!(content.children[0].clip.is_empty());
        assert_eq!(content.children[9].clip, Rect::new(0.0, 30.0, 100.0, 20.0));
    }

    #[test]
    fn max_height_caps_a_content_sized_scroll_view() {
        let rows: Vec<El<()>> = (0..10).map(|_| text("row").height(20.0)).collect();
        let root: El<()> = col([scroll("body", col(rows)).max_height(70.0)]);
        let placed = place(&root, 100.0, 400.0);
        assert_eq!(placed.children[0].rect.h, 70.0);
        assert_eq!(placed.children[0].scroll.unwrap().content, 200.0);
    }

    #[test]
    fn keyed_elements_keep_their_id_when_rows_move() {
        let first: El<()> = col([text("a").key("row:a"), text("b").key("row:b")]);
        let second: El<()> = col([text("b").key("row:b")]);
        let a = place(&first, 100.0, 100.0);
        let b = place(&second, 100.0, 100.0);
        assert_eq!(a.children[1].id, b.children[0].id);
        assert_ne!(a.children[0].id, a.children[1].id);
    }

    #[test]
    fn max_width_caps_a_filling_child_and_alignment_places_it() {
        let root: El<()> = col([col([])
            .height(10.0)
            .fill_width()
            .max_width(120.0)
            .halign(Align::Center)]);
        let placed = place(&root, 400.0, 100.0);
        assert_eq!(placed.children[0].rect, Rect::new(140.0, 0.0, 120.0, 10.0));
    }
}
