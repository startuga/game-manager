//! The window's controls, built from elements and theme tokens so every view looks the same.
//!
//! Their look:
//! rimless translucent buttons, chips on a tint's wash, cards with an icon square, a title and a
//! subtitle, tabs with a tinted icon and a filled, underlined open tab,
//! and the Play button: a two-tone gradient face with a lighter top
//! edge, a gloss on its top half and a glow of its colour, lifted under the pointer and pressed on
//! click.

use windows_canvas::ColorF;

use super::el::{
    Align, El, Font, Interaction, Rect, Role, TextAlign, TextSpec, col, glyph, paint, row, spinner,
    text,
};
use super::render::{Painter, glyph_spec};
use super::theme;

/// A standard button.
pub fn button<M>(label: impl Into<String>) -> El<M> {
    row([text(label)])
        .pad_xy(12.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::CONTROL)
        .hover_bg(theme::CONTROL_HOVER)
        .pressed_bg(theme::CONTROL_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// A standard button with a glyph before its label.
pub fn icon_text_button<M>(icon: &str, label: impl Into<String>) -> El<M> {
    row([glyph(icon, theme::ICON_SIZE), text(label)])
        .gap(8.0)
        .pad_xy(12.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::CONTROL)
        .hover_bg(theme::CONTROL_HOVER)
        .pressed_bg(theme::CONTROL_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// The main action of a group, in the accent colour.
pub fn accent_button<M>(label: impl Into<String>) -> El<M> {
    row([text(label).bold()])
        .pad_xy(14.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::ACCENT)
        .hover_bg(lighten(theme::ACCENT, 0.10))
        .pressed_bg(darken(theme::ACCENT, 0.12))
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// A button without a fill until hovered, with a glyph and a label.
pub fn subtle_button<M>(icon: &str, label: impl Into<String>) -> El<M> {
    row([glyph(icon, theme::ICON_SIZE), text(label)])
        .gap(8.0)
        .pad_xy(10.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .hover_bg(theme::SUBTLE_HOVER)
        .pressed_bg(theme::SUBTLE_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// A square glyph button; `name` is its tooltip and its UI Automation name.
pub fn icon_button<M>(icon: &str, name: impl Into<String>) -> El<M> {
    let name = name.into();
    row([glyph(icon, theme::ICON_SIZE)
        .text_align(TextAlign::Center)
        .fill_width()])
    .size(theme::CONTROL_HEIGHT, theme::CONTROL_HEIGHT)
    .hover_bg(theme::SUBTLE_HOVER)
    .pressed_bg(theme::SUBTLE_PRESSED)
    .radius(theme::RADIUS_CONTROL)
    .role(Role::Button)
    .tooltip(name.clone())
    .name(name)
}

/// The red ✕ that removes something.
pub fn remove_button<M>(name: impl Into<String>) -> El<M> {
    let name = name.into();
    row([glyph(theme::glyph::CLOSE, 12.0)
        .color(theme::CRITICAL)
        .text_align(TextAlign::Center)
        .fill_width()])
    .size(theme::CONTROL_HEIGHT, theme::CONTROL_HEIGHT)
    .hover_bg(theme::SUBTLE_HOVER)
    .pressed_bg(theme::SUBTLE_PRESSED)
    .radius(theme::RADIUS_CONTROL)
    .role(Role::Button)
    .tooltip(name.clone())
    .name(name)
}

/// A text box: an EDIT control in a rounded frame. `key` keeps the same EDIT (and its caret)
/// across frames; `name` is its UI Automation name.
pub fn text_box<M>(
    key: impl Into<String>,
    value: impl Into<String>,
    placeholder: impl Into<String>,
    name: impl Into<String>,
    on_change: impl Fn(String) -> M + 'static,
) -> El<M> {
    super::el::edit(key, value, placeholder, on_change)
        .bg(theme::INPUT)
        .stroke(theme::INPUT_STROKE, 1.0)
        .radius(theme::RADIUS_CONTROL)
        .name(name)
}

/// A text link: a version that opens its download page, a web page.
pub fn link<M>(label: impl Into<String>, message: M) -> El<M> {
    row([text(label).color(theme::BLUE)])
        .pad_xy(4.0, 2.0)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::SUBTLE_HOVER)
        .pressed_bg(theme::SUBTLE_PRESSED)
        .role(Role::Button)
        .on_click(message)
}

/// A group's main action while it works: a spinner and "Working…" in place of its label.
pub fn working_button<M>() -> El<M> {
    row([spinner(14.0), text("Working…")])
        .gap(8.0)
        .pad_xy(14.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::CONTROL)
        .radius(theme::RADIUS_CONTROL)
        .enabled(false)
}

/// A choice among a few items, one per line, each with a round mark.
pub fn radio_group<M>(
    title: &str,
    items: Vec<String>,
    selected: Option<usize>,
    on_select: impl Fn(usize) -> M,
) -> El<M> {
    let mut lines = vec![text(title).bold()];
    for (index, item) in items.into_iter().enumerate() {
        let chosen = selected == Some(index);
        let mark = paint(
            move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
                let ring = Rect::new(rect.x, rect.y + (rect.h - 18.0) / 2.0, 18.0, 18.0);
                if chosen {
                    painter.fill_rounded(ring, 9.0, theme::ACCENT);
                    painter.fill_rounded(
                        Rect::new(ring.x + 5.0, ring.y + 5.0, 8.0, 8.0),
                        4.0,
                        theme::TEXT,
                    );
                } else {
                    painter.stroke_rounded(ring, 9.0, theme::TEXT_TERTIARY, 1.0);
                }
            },
        )
        .size(18.0, 22.0);
        lines.push(
            row([mark, text(item.clone()).fill_width()])
                .gap(8.0)
                .pad_xy(4.0, 3.0)
                .radius(theme::RADIUS_CONTROL)
                .hover_bg(theme::SUBTLE_HOVER)
                .role(Role::ListItem)
                .selected(chosen)
                .name(item)
                .on_click(on_select(index)),
        );
    }
    col(lines).gap(2.0)
}

/// A small label on its tint's wash: a store, an API, an upscaler's version.
pub fn chip<M>(label: impl Into<String>, tint: ColorF) -> El<M> {
    row([text(label)
        .text_size(theme::TEXT_CAPTION)
        .weight(700)
        .color(tint)])
    .pad_xy(7.0, 2.0)
    .bg(theme::wash(tint))
    .radius(4.0)
}

/// A card: a translucent panel with a faint outline.
pub fn card<M>(children: impl IntoIterator<Item = El<M>>) -> El<M> {
    col(children)
        .gap(theme::GAP)
        .pad(16.0)
        .bg(theme::CARD)
        .stroke(theme::CARD_STROKE, 1.0)
        .radius(theme::RADIUS_CARD)
        .role(Role::Group)
}

/// A square with a glyph in a tint, on the tint's wash.
pub fn icon_square<M>(icon: &str, tint: ColorF, size: f32) -> El<M> {
    row([glyph(icon, size * 0.5)
        .color(tint)
        .text_align(TextAlign::Center)
        .fill_width()])
    .size(size, size)
    .bg(theme::wash(tint))
    .radius(size * 0.22)
}

/// A card's header: icon square, title and subtitle.
pub fn card_header<M>(
    icon: &str,
    tint: ColorF,
    title: impl Into<String>,
    subtitle: impl Into<String>,
) -> El<M> {
    let subtitle = subtitle.into();
    let mut lines = vec![text(title).text_size(theme::TEXT_SUBTITLE).bold()];
    if !subtitle.is_empty() {
        lines.push(
            text(subtitle)
                .text_size(theme::TEXT_SMALL)
                .color(theme::TEXT_TERTIARY)
                .wrap(),
        );
    }
    row([
        icon_square(icon, tint, 32.0),
        col(lines).gap(2.0).fill_width(),
    ])
    .gap(12.0)
}

/// One tab of a tab strip.
pub struct Tab<M> {
    pub icon: &'static str,
    pub tint: ColorF,
    pub label: String,
    pub selected: bool,
    pub message: M,
}

/// A centred strip of tabs; the open one is filled with the accent, bold and underlined.
pub fn tabs<M>(tabs: Vec<Tab<M>>) -> El<M> {
    let items = tabs.into_iter().map(|tab| {
        let label = text(tab.label.clone())
            .text_size(13.5)
            .weight(if tab.selected { 600 } else { 400 })
            .color(if tab.selected {
                theme::TEXT
            } else {
                theme::TEXT_SECONDARY
            });
        let face = row([icon_square(tab.icon, tab.tint, 20.0), label])
            .gap(7.0)
            .padding(super::el::Edges::new(8.0, 5.0, 13.0, 6.0))
            .radius(theme::RADIUS_TAB)
            .hover_bg(theme::SUBTLE_HOVER)
            .pressed_bg(theme::SUBTLE_PRESSED)
            .role(Role::Tab)
            .selected(tab.selected)
            .name(tab.label)
            .on_click(tab.message);
        let underline = col([])
            .height(2.0)
            .width(24.0)
            .halign(Align::Center)
            .radius(1.0);
        let underline = if tab.selected {
            underline.bg(theme::ACCENT)
        } else {
            underline
        };
        col([face, underline]).gap(3.0)
    });
    row(items).gap(4.0).justify(Align::Center)
}

/// A sidebar page: glyph and label, selected with a fill and an accent bar.
pub fn nav_item<M>(icon: &str, label: impl Into<String>, selected: bool, message: M) -> El<M> {
    let bar = col([]).size(3.0, 16.0).radius(1.5);
    let bar = if selected { bar.bg(theme::ACCENT) } else { bar };
    row([bar, glyph(icon, theme::ICON_SIZE), text(label).fill_width()])
        .gap(10.0)
        .padding(super::el::Edges::new(2.0, 0.0, 10.0, 0.0))
        .height(34.0)
        .hover_bg(theme::SUBTLE_HOVER)
        .pressed_bg(theme::SUBTLE_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::ListItem)
        .selected(selected)
        .on_click(message)
}

/// A check box's square: checked (`Some(true)`), not (`Some(false)`), or partly (`None`: some of
/// a pack's effects).
pub fn check_square<M>(state: Option<bool>) -> El<M> {
    paint(
        move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
            let square = Rect::new(rect.x, rect.y + (rect.h - 18.0) / 2.0, 18.0, 18.0);
            match state {
                Some(true) => {
                    painter.fill_rounded(square, 4.0, theme::ACCENT);
                    painter.text(
                        &super::render::glyph_spec(theme::glyph::CHECK, 11.0, theme::TEXT),
                        square,
                    );
                }
                None => {
                    painter.fill_rounded(square, 4.0, theme::ACCENT);
                    painter.fill_rounded(
                        Rect::new(square.x + 4.0, square.y + 8.0, 10.0, 2.0),
                        1.0,
                        theme::TEXT,
                    );
                }
                Some(false) => painter.stroke_rounded(square, 4.0, theme::TEXT_TERTIARY, 1.0),
            }
        },
    )
    .size(18.0, 22.0)
}

/// A check box with its label; the whole row toggles it.
pub fn checkbox<M>(label: impl Into<String>, checked: bool, message: M) -> El<M> {
    row([check_square(Some(checked)), text(label).wrap().fill_width()])
        .gap(8.0)
        .pad_xy(4.0, 2.0)
        .hover_bg(theme::SUBTLE_HOVER)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::CheckBox)
        .selected(checked)
        .on_click(message)
}

/// A switch with its label.
pub fn toggle<M>(label: impl Into<String>, on: bool, message: M) -> El<M> {
    let knob = paint(
        move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
            let track = Rect::new(rect.x, rect.y + (rect.h - 20.0) / 2.0, 40.0, 20.0);
            if on {
                painter.fill_rounded(track, 10.0, theme::ACCENT);
            } else {
                painter.stroke_rounded(track, 10.0, theme::TEXT_TERTIARY, 1.0);
            }
            let x = if on {
                track.right() - 16.0
            } else {
                track.x + 4.0
            };
            let dot = Rect::new(x, track.y + 4.0, 12.0, 12.0);
            painter.fill_rounded(
                dot,
                6.0,
                if on {
                    theme::TEXT
                } else {
                    theme::TEXT_SECONDARY
                },
            );
        },
    )
    .size(40.0, 24.0);
    row([knob, text(label).fill_width()])
        .gap(10.0)
        .pad_xy(4.0, 2.0)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::SUBTLE_HOVER)
        .role(Role::CheckBox)
        .selected(on)
        .on_click(message)
}

/// A red bar with what went wrong, and ✕ to dismiss it.
pub fn error_bar<M>(title: &str, message: &str, on_close: M) -> El<M> {
    row([
        glyph(theme::glyph::ERROR, 16.0)
            .color(theme::CRITICAL)
            .valign(Align::Start),
        col([
            text(title).bold(),
            text(message).wrap().color(theme::TEXT_SECONDARY),
        ])
        .gap(2.0)
        .fill_width(),
        icon_button(theme::glyph::CLOSE, "Dismiss")
            .on_click(on_close)
            .valign(Align::Start),
    ])
    .gap(12.0)
    .pad_xy(14.0, 10.0)
    .bg(theme::ERROR_FILL)
    .radius(6.0)
}

/// A page's title, in the title font.
pub fn heading<M>(label: impl Into<String>) -> El<M> {
    text(label)
        .font(Font::Title)
        .text_size(theme::TEXT_TITLE)
        .weight(600)
}

/// A secondary line of explanation.
pub fn hint<M>(label: impl Into<String>) -> El<M> {
    text(label)
        .text_size(theme::TEXT_SMALL)
        .color(theme::TEXT_TERTIARY)
        .wrap()
}

/// A thin line between areas.
pub fn divider<M>() -> El<M> {
    col([]).height(1.0).bg(theme::DIVIDER)
}

/// A thin strip that resizes what is beside it by dragging (the sidebar): the left-right arrows
/// and a faint accent line under the pointer.
/// `on_drag` gets the pointer's x in window DIPs.
pub fn resize_grip<M>(name: &str, on_drag: impl Fn(f32) -> M + 'static) -> El<M> {
    paint(
        |painter: &mut Painter<'_>, rect: Rect, interaction: Interaction| {
            if interaction.hover || interaction.pressed {
                let line = Rect::new(rect.x + rect.w / 2.0 - 1.0, rect.y, 2.0, rect.h);
                painter.fill_rect(line, theme::with_alpha(theme::ACCENT, 0x99));
            }
        },
    )
    .width(6.0)
    .fill_height()
    .name(name)
    .on_drag(move |(x, _)| on_drag(x))
}

/// A usage bar, `ratio` (0–1) of it filled: the accent, or red from 90% (a drive nearly full).
pub fn meter<M>(ratio: f32) -> El<M> {
    let ratio = ratio.clamp(0.0, 1.0);
    paint(
        move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
            let bar = Rect::new(rect.x, rect.y + (rect.h - 4.0) / 2.0, rect.w, 4.0);
            painter.fill_rounded(bar, 2.0, theme::CONTROL_HOVER);
            let color = if ratio >= 0.9 {
                theme::CRITICAL
            } else {
                theme::ACCENT
            };
            painter.fill_rounded(Rect::new(bar.x, bar.y, bar.w * ratio, bar.h), 2.0, color);
        },
    )
    .height(8.0)
}

/// A label with a busy spinner.
pub fn working<M>(label: impl Into<String>) -> El<M> {
    row([spinner(16.0), text(label).color(theme::TEXT_SECONDARY)]).gap(8.0)
}

/// What the Play button shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Play,
    /// Just launched: a spinner in place of the icon.
    Starting,
}

/// The game page's big launch button.
pub fn play_button<M>(state: PlayState, message: M) -> El<M> {
    let label = match state {
        PlayState::Play => "PLAY",
        PlayState::Starting => "STARTING…",
    };
    paint(
        move |painter: &mut Painter<'_>, rect: Rect, interaction: Interaction| {
            draw_play(painter, rect, interaction, label, state);
        },
    )
    .size(168.0, 50.0)
    .role(Role::Button)
    .name("Play")
    .radius(6.0)
    .on_click(message)
}

fn draw_play(
    painter: &mut Painter<'_>,
    rect: Rect,
    interaction: Interaction,
    label: &str,
    state: PlayState,
) {
    const CORNER: f32 = 6.0;
    let color = theme::PLAY;
    let lift = if interaction.pressed {
        1.0
    } else if interaction.hover {
        -1.0
    } else {
        0.0
    };
    let face = Rect::new(rect.x, rect.y + lift, rect.w, rect.h);
    if interaction.enabled {
        painter.glow(
            face,
            CORNER,
            color,
            if interaction.hover { 14.0 } else { 10.0 },
        );
    }
    painter.fill_gradient(face, CORNER, lighten(color, 0.10), darken(color, 0.14));
    // The gloss on the top half.
    let gloss = Rect::new(face.x, face.y, face.w, face.h / 2.0);
    painter.fill_gradient(
        gloss,
        CORNER,
        theme::with_alpha(theme::TEXT, 0x1F),
        theme::with_alpha(theme::TEXT, 0x05),
    );
    // The crisp edge: lighter on top, darker at the bottom.
    painter.line(
        (face.x + CORNER + 4.0, face.y + 1.0),
        (face.right() - CORNER - 4.0, face.y + 1.0),
        theme::with_alpha(theme::TEXT, 0x59),
        1.0,
    );
    painter.line(
        (face.x + CORNER, face.bottom() - 0.5),
        (face.right() - CORNER, face.bottom() - 0.5),
        theme::with_alpha(ColorF::BLACK, 0x40),
        1.0,
    );
    if interaction.pressed {
        painter.fill_rounded(face, CORNER, theme::with_alpha(ColorF::BLACK, 0x1A));
    } else if interaction.hover {
        painter.fill_rounded(face, CORNER, theme::with_alpha(theme::TEXT, 0x14));
    }
    let word = TextSpec {
        text: label.to_owned(),
        size: 19.0,
        weight: 600,
        font: Font::Display,
        color: theme::TEXT,
        wrap: false,
        align: TextAlign::Start,
    };
    let (word_w, _) = painter.measure(&word, f32::INFINITY);
    let icon_w = 20.0;
    let total = icon_w + 10.0 + word_w;
    let x = face.x + ((face.w - total) / 2.0).max(8.0);
    match state {
        PlayState::Play => {
            painter.text(
                &glyph_spec(theme::glyph::PLAY, 18.0, theme::TEXT),
                Rect::new(x, face.y, icon_w, face.h),
            );
        }
        PlayState::Starting => {
            let dot = |i: f32| Rect::new(x + i * 7.0, face.y + face.h / 2.0 - 2.5, 5.0, 5.0);
            for i in 0..3 {
                painter.fill_rounded(
                    dot(i as f32),
                    2.5,
                    theme::with_alpha(theme::TEXT, 0x99 + i as u8 * 0x20),
                );
            }
        }
    }
    painter.text(
        &word,
        Rect::new(x + icon_w + 10.0, face.y, word_w + 2.0, face.h),
    );
}

/// `color` moved towards white by `amount` (0–1).
pub fn lighten(color: ColorF, amount: f32) -> ColorF {
    ColorF::new(
        color.r + (1.0 - color.r) * amount,
        color.g + (1.0 - color.g) * amount,
        color.b + (1.0 - color.b) * amount,
        color.a,
    )
}

/// `color` moved towards black by `amount` (0–1).
pub fn darken(color: ColorF, amount: f32) -> ColorF {
    ColorF::new(
        color.r * (1.0 - amount),
        color.g * (1.0 - amount),
        color.b * (1.0 - amount),
        color.a,
    )
}
