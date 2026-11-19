//! The window's controls, built from elements and theme tokens so every view looks the same.
//!
//! The design system's components (`app/CLAUDE.md` lists them, with the rules for using them): a
//! page's header (`page_header`: a caption, a title, a line, actions), a section's label in small
//! capitals (`section_label`, with a `count_badge`), cards with `card_header` (or
//! `card_header_with`: a status line and actions at the right, Info last), `hint` for every
//! secondary line, and `working` for every lookup still running. Then:
//! - **A choice** (a switch, a drop-down, option pills) is a `setting_row`: title and hint on the
//!   left, the control at the right (a drop-down `SETTING_CONTROL_WIDTH` wide, a `switch`). **A
//!   text value** is a `field`: its label, the text box, a hint, stacked.
//! - **Opening** a web page or Windows Settings is a `link_button`, a folder a `folder_button`;
//!   a `link` is only text inside a sentence.
//! - **A form's buttons** sit in a right-aligned row, primary first, each at least
//!   `BUTTON_MIN_WIDTH` wide and never stretched across a column; Reset to … has `glyph::RESET`
//!   (Refresh keeps its own). A button busy with its job becomes `working_button`, as wide as
//!   `busy_width` keeps it.
//! - **The rest**: errors are an `error_card`; key and value lists are `facts`; folding headers
//!   are `fold_header`, other group headers `group_row`; searches `search_box`; a check box
//!   alone is `check_button` (with a label, `checkbox` or `tri_checkbox`); a button over a
//!   picture is `picture_icon_button`; chips are `chip`s in sentence case. A page's blocks are
//!   `space::L` apart, and a card's parts `card`'s own gap.
//!
//! `theme::tests::views_use_the_shared_controls` fails on a hover or pressed fill, a card's fill
//! or an inline `link` made in a view, outside the few exceptions it lists with why.
//!
//! Their look:
//! rimless translucent buttons, chips on a tint's wash, cards with an icon square, a title and a
//! subtitle, tabs with a tinted icon and a filled, underlined open tab,
//! and the Play button: a two-tone gradient face with a lighter top
//! edge, a gloss on its top half and a glow of its colour, lifted under the pointer and pressed on
//! click.

use windows_canvas::ColorF;

use super::el::{
    Align, El, Font, Interaction, Rect, Role, TextAlign, TextSpec, col, glyph, layers, paint, row,
    spinner, text,
};
use super::render::{Painter, glyph_spec};
use super::theme;

/// A standard button.
pub fn button<M>(label: impl Into<String>) -> El<M> {
    row([text(label)])
        .pad_xy(12.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::control())
        .hover_bg(theme::control_hover())
        .pressed_bg(theme::control_pressed())
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// A standard button with a glyph before its label.
pub fn icon_text_button<M>(icon: &str, label: impl Into<String>) -> El<M> {
    row([glyph(icon, theme::ICON_SIZE), text(label)])
        .gap(8.0)
        .pad_xy(12.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::control())
        .hover_bg(theme::control_hover())
        .pressed_bg(theme::control_pressed())
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// The main action of a group, in the accent colour.
pub fn accent_button<M>(label: impl Into<String>) -> El<M> {
    row([text(label).bold().color(theme::text_on_accent())])
        .pad_xy(14.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::accent())
        .hover_bg(lighten(theme::accent(), 0.10))
        .pressed_bg(darken(theme::accent(), 0.12))
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// The button that removes something, in a dialog that asks first: Windows' red, white text.
pub fn danger_button<M>(label: impl Into<String>) -> El<M> {
    row([text(label).bold().color(ColorF::WHITE)])
        .pad_xy(14.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::DANGER)
        .hover_bg(lighten(theme::DANGER, 0.10))
        .pressed_bg(theme::DANGER_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// A button without a fill until hovered, with a glyph and a label.
pub fn subtle_button<M>(icon: &str, label: impl Into<String>) -> El<M> {
    row([glyph(icon, theme::ICON_SIZE), text(label)])
        .gap(8.0)
        .pad_xy(10.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
}

/// `icon_button` over a picture (the game page's band), in the dark palette whatever the theme:
/// no fill of its own, so it blends into the art as the other icon buttons do into the page, and
/// the dark palette's subtle fills under the pointer and on a press.
pub fn picture_icon_button<M>(icon: &str, name: impl Into<String>) -> El<M> {
    let name = name.into();
    row([glyph(icon, theme::ICON_SIZE)
        .color(theme::DARK.text)
        .text_align(TextAlign::Center)
        .fill_width()])
    .size(theme::CONTROL_HEIGHT, theme::CONTROL_HEIGHT)
    .hover_bg(theme::DARK.subtle_hover)
    .pressed_bg(theme::DARK.subtle_pressed)
    .radius(theme::RADIUS_CONTROL)
    .role(Role::Button)
    .tooltip(name.clone())
    .name(name)
}

/// A square glyph button; `name` is its tooltip and its UI Automation name.
pub fn icon_button<M>(icon: &str, name: impl Into<String>) -> El<M> {
    let name = name.into();
    row([glyph(icon, theme::ICON_SIZE)
        .text_align(TextAlign::Center)
        .fill_width()])
    .size(theme::CONTROL_HEIGHT, theme::CONTROL_HEIGHT)
    .hover_bg(theme::subtle_hover())
    .pressed_bg(theme::subtle_pressed())
    .radius(theme::RADIUS_CONTROL)
    .role(Role::Button)
    .tooltip(name.clone())
    .name(name)
}

/// A button that opens a flyout: its glyph and a small chevron, with a dot at the glyph's corner
/// while what it opens is in effect (`active`). `name` is its tooltip and its UI Automation name.
pub fn dropdown_button<M>(icon: &str, name: impl Into<String>, active: bool) -> El<M> {
    let name = name.into();
    let mut face = vec![
        glyph(icon, theme::ICON_SIZE)
            .text_align(TextAlign::Center)
            .size(18.0, 18.0)
            .center(),
    ];
    if active {
        face.push(
            col([])
                .size(6.0, 6.0)
                .radius(3.0)
                .bg(theme::accent())
                .halign(Align::End)
                .valign(Align::Start),
        );
    }
    row([
        layers(face).size(20.0, 20.0),
        glyph(theme::glyph::CHEVRON_DOWN, 8.0).color(theme::text_secondary()),
    ])
    .gap(5.0)
    .pad_xy(8.0, 0.0)
    .height(theme::CONTROL_HEIGHT)
    .bg(theme::control())
    .stroke(theme::input_stroke(), 1.0)
    .hover_bg(theme::control_hover())
    .pressed_bg(theme::control_pressed())
    .radius(theme::RADIUS_CONTROL)
    .role(Role::Button)
    .tooltip(name.clone())
    .name(name)
}

/// The red ✕ that removes something.
pub fn remove_button<M>(name: impl Into<String>) -> El<M> {
    let name = name.into();
    row([glyph(theme::glyph::CLOSE, 12.0)
        .color(theme::critical())
        .text_align(TextAlign::Center)
        .fill_width()])
    .size(theme::CONTROL_HEIGHT, theme::CONTROL_HEIGHT)
    .hover_bg(theme::subtle_hover())
    .pressed_bg(theme::subtle_pressed())
    .radius(theme::RADIUS_CONTROL)
    .role(Role::Button)
    .tooltip(name.clone())
    .name(name)
}

/// A text box in a rounded frame. `key` keeps its caret, selection and undo across frames;
/// `name` is its UI Automation name.
pub fn text_box<M>(
    key: impl Into<String>,
    value: impl Into<String>,
    placeholder: impl Into<String>,
    name: impl Into<String>,
    on_change: impl Fn(String) -> M + 'static,
) -> El<M> {
    super::el::edit(key, value, placeholder, on_change)
        .bg(theme::input())
        .stroke(theme::input_stroke(), 1.0)
        .radius(theme::RADIUS_CONTROL)
        .name(name)
}

/// The search box, wherever something is searched: a magnifier, the text, and a ✕ that sends
/// `on_clear` while there is text, in one input frame. Escape empties it (`clear_on_escape`);
/// Enter sends `on_enter`. `name` is its UI Automation name.
pub fn search_box<M>(
    key: impl Into<String>,
    value: &str,
    placeholder: impl Into<String>,
    name: impl Into<String>,
    on_change: impl Fn(String) -> M + 'static,
    on_enter: M,
    on_clear: M,
) -> El<M> {
    let mut parts = vec![
        glyph(theme::glyph::SEARCH, theme::ICON_SIZE)
            .color(theme::text_tertiary())
            .padding(super::el::Edges::new(10.0, 0.0, 0.0, 0.0)),
        super::el::edit(key, value, placeholder, on_change)
            .on_enter(on_enter)
            .clear_on_escape()
            .name(name)
            .fill_width(),
    ];
    if !value.is_empty() {
        parts.push(
            row([glyph(theme::glyph::CLOSE, 10.0)
                .color(theme::text_secondary())
                .text_align(TextAlign::Center)
                .fill_width()])
            .size(24.0, 24.0)
            .radius(theme::RADIUS_CONTROL)
            .hover_bg(theme::subtle_hover())
            .pressed_bg(theme::subtle_pressed())
            .role(Role::Button)
            .name("Clear the search")
            .tooltip("Clear the search (Escape)")
            .on_click(on_clear),
        );
    }
    row(parts)
        .pad_xy(0.0, 0.0)
        .padding(super::el::Edges::new(0.0, 0.0, 3.0, 0.0))
        .height(theme::CONTROL_HEIGHT)
        .bg(theme::input())
        .stroke(theme::input_stroke(), 1.0)
        .radius(theme::RADIUS_CONTROL)
}

/// A text link: a version that opens its download page, a web page.
pub fn link<M>(label: impl Into<String>, message: M) -> El<M> {
    row([text(label).color(theme::blue())])
        .pad_xy(4.0, 2.0)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .role(Role::Button)
        .on_click(message)
}

/// A button while its job runs: a spinner and "Working…" in place of its label, as wide as the
/// button it replaces (`BUTTON_MIN_WIDTH`, which that button takes too: `busy_width`).
pub fn working_button<M>() -> El<M> {
    row([spinner(14.0), text("Working…")])
        .gap(8.0)
        .pad_xy(14.0, 0.0)
        .height(theme::CONTROL_HEIGHT)
        .min_width(theme::BUTTON_MIN_WIDTH)
        .justify(Align::Center)
        .bg(theme::control())
        .radius(theme::RADIUS_CONTROL)
        .enabled(false)
}

/// A button that becomes `working_button` while its job runs: `button` while idle, as wide as
/// it will be busy, so the row doesn't move.
pub fn busy_width<M>(button: El<M>) -> El<M> {
    button
        .min_width(theme::BUTTON_MIN_WIDTH)
        .justify(Align::Center)
}

/// A choice among a few items, one per line, each with a round mark.
pub fn radio_group<M>(
    title: &str,
    items: Vec<String>,
    selected: Option<usize>,
    on_select: impl Fn(usize) -> M,
) -> El<M> {
    let mut lines = vec![section_label(title)];
    for (index, item) in items.into_iter().enumerate() {
        let chosen = selected == Some(index);
        let mark = paint(
            move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
                let ring = Rect::new(rect.x, rect.y + (rect.h - 18.0) / 2.0, 18.0, 18.0);
                if chosen {
                    painter.fill_rounded(ring, 9.0, theme::accent());
                    painter.fill_rounded(
                        Rect::new(ring.x + 5.0, ring.y + 5.0, 8.0, 8.0),
                        4.0,
                        theme::text(),
                    );
                } else {
                    painter.stroke_rounded(ring, 9.0, theme::text_tertiary(), 1.0);
                }
            },
        )
        .size(18.0, 22.0);
        lines.push(
            row([mark, text(item.clone()).fill_width()])
                .gap(8.0)
                .pad_xy(4.0, 3.0)
                .radius(theme::RADIUS_CONTROL)
                .hover_bg(theme::subtle_hover())
                .role(Role::ListItem)
                .selected(chosen)
                .name(item)
                .on_click(on_select(index)),
        );
    }
    col(lines).gap(2.0)
}

/// A small label on its tint's wash, in sentence case: a store, an API, an upscaler's version,
/// "Main display".
pub fn chip<M>(label: impl Into<String>, tint: ColorF) -> El<M> {
    row([text(label)
        .text_size(theme::TEXT_CAPTION)
        .weight(700)
        .color(tint)])
    .pad_xy(7.0, 2.0)
    .bg(theme::wash(tint))
    .radius(theme::RADIUS_CHIP)
}

/// A card: a translucent panel with a faint outline.
pub fn card<M>(children: impl IntoIterator<Item = El<M>>) -> El<M> {
    col(children)
        .gap(theme::space::S)
        .pad(theme::space::L)
        .bg(theme::card())
        .stroke(theme::card_stroke(), 1.0)
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
    let status = (!subtitle.is_empty()).then(|| hint(subtitle));
    card_header_with(icon, tint, title, status, Vec::new())
}

/// A card's header with its state and actions: icon square, title, a status line under it (a
/// hint, a version link, "Not installed"), and the card's actions at the right, Info last.
pub fn card_header_with<M>(
    icon: &str,
    tint: ColorF,
    title: impl Into<String>,
    status: Option<El<M>>,
    actions: Vec<El<M>>,
) -> El<M> {
    let mut lines = vec![text(title).text_size(theme::TEXT_SUBTITLE).bold()];
    lines.extend(status);
    let mut parts = vec![
        icon_square(icon, tint, 32.0),
        col(lines).gap(theme::space::XXS).fill_width(),
    ];
    if !actions.is_empty() {
        parts.push(row(actions).gap(theme::space::S));
    }
    row(parts).gap(theme::space::M)
}

/// A card saying what couldn't be read or done: the part's header, and why.
pub fn error_card<M>(icon: &str, tint: ColorF, title: &str, why: impl Into<String>) -> El<M> {
    card([card_header(icon, tint, title, ""), hint(why).wrap()])
}

/// A setting, as Windows 11's settings are: its title and what it does on the left, its control
/// at the right (a `switch`, a drop-down `SETTING_CONTROL_WIDTH` wide, option pills), on a filled
/// row. Every choice is one; a text value is a `field`. Without a control, only its text.
pub fn setting_row<M>(title: &str, description: &str, control: Option<El<M>>) -> El<M> {
    let mut texts = vec![text(title).bold().wrap()];
    if !description.is_empty() {
        texts.push(hint(description.to_owned()));
    }
    let mut items = vec![col(texts).gap(theme::space::XXS).fill_width()];
    items.extend(control);
    row(items)
        .gap(theme::space::L)
        .pad_xy(theme::space::M, theme::space::S)
        .bg(theme::card())
        .radius(theme::RADIUS_CONTROL)
}

/// An On/Off switch, `SWITCH_WIDTH` wide: a setting row's control. `name` is its UI Automation
/// name (the setting's).
pub fn switch<M>(name: impl Into<String>, on: bool, message: M) -> El<M> {
    toggle(if on { "On" } else { "Off" }, on, message)
        .name(name)
        .width(theme::SWITCH_WIDTH)
}

/// A form field's label (over its text box).
pub fn field_label<M>(label: &str) -> El<M> {
    text(label)
        .bold()
        .padding(super::el::Edges::new(0.0, theme::space::S, 0.0, 0.0))
}

/// A text value: its label, the text box (or a row with it and its Browse…), and a hint under
/// it when there is one, stacked.
pub fn field<M>(label: &str, control: El<M>, line: &str) -> El<M> {
    let mut parts = vec![field_label(label), control];
    if !line.is_empty() {
        parts.push(hint(line.to_owned()).wrap());
    }
    col(parts).gap(theme::space::S)
}

/// Opens a web page or a page of Windows' Settings.
pub fn link_button<M>(label: impl Into<String>) -> El<M> {
    icon_text_button(theme::glyph::LINK, label)
}

/// Opens a folder.
pub fn folder_button<M>(label: impl Into<String>) -> El<M> {
    icon_text_button(theme::glyph::FOLDER, label)
}

/// Label and value pairs in two aligned columns (the label `FACT_LABEL_WIDTH` wide); empty values
/// are left out.
pub fn facts<M>(rows: Vec<(&str, String)>) -> El<M> {
    col(rows
        .into_iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(label, value)| {
            row([
                text(label)
                    .color(theme::text_tertiary())
                    .width(theme::FACT_LABEL_WIDTH),
                text(value).wrap().fill_width(),
            ])
            .gap(theme::space::M)
        }))
    .gap(theme::space::XS)
}

/// A group's header row: a glyph in a narrow slot (a chevron, a pin), its label (a
/// `section_label`, a title) and what follows it (a `count_badge`, a hint).
pub fn group_row<M>(lead: El<M>, label: El<M>, detail: Option<El<M>>) -> El<M> {
    let mut parts = vec![lead.width(FOLD_SLOT), label];
    parts.extend(detail);
    // As tall as a control for one line, taller for a title with a hint under it.
    row(parts)
        .gap(theme::space::S)
        .pad_xy(theme::space::S, 7.0)
        .radius(theme::RADIUS_CONTROL)
}

/// A fold header's chevron and its slot.
const FOLD_GLYPH: f32 = 10.0;
const FOLD_SLOT: f32 = 16.0;

/// A header that folds what follows (`group_row` with a chevron: right while folded, down while
/// open); a click sends `message`.
pub fn fold_header<M>(open: bool, label: El<M>, detail: Option<El<M>>, message: M) -> El<M> {
    let chevron = if open {
        theme::glyph::CHEVRON_DOWN
    } else {
        theme::glyph::CHEVRON_RIGHT
    };
    let lead = glyph(chevron, FOLD_GLYPH)
        .color(theme::text_secondary())
        .text_align(TextAlign::Center);
    group_row(lead, label, detail)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .role(Role::Button)
        .on_click(message)
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
            .text_size(theme::TEXT_SIZE)
            .weight(if tab.selected { 600 } else { 400 })
            .color(if tab.selected {
                theme::text()
            } else {
                theme::text_secondary()
            });
        let face = row([icon_square(tab.icon, tab.tint, 20.0), label])
            .gap(7.0)
            .padding(super::el::Edges::new(8.0, 5.0, 13.0, 6.0))
            .radius(theme::RADIUS_TAB)
            .hover_bg(theme::subtle_hover())
            .pressed_bg(theme::subtle_pressed())
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
            underline.bg(theme::accent())
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
    let bar = if selected {
        bar.bg(theme::accent())
    } else {
        bar
    };
    row([bar, glyph(icon, theme::ICON_SIZE), text(label).fill_width()])
        .gap(10.0)
        .padding(super::el::Edges::new(2.0, 0.0, 10.0, 0.0))
        .height(34.0)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
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
                    painter.fill_rounded(square, 4.0, theme::accent());
                    painter.text(
                        &super::render::glyph_spec(theme::glyph::CHECK, 11.0, theme::text()),
                        square,
                    );
                }
                None => {
                    painter.fill_rounded(square, 4.0, theme::accent());
                    painter.fill_rounded(
                        Rect::new(square.x + 4.0, square.y + 8.0, 10.0, 2.0),
                        1.0,
                        theme::text(),
                    );
                }
                Some(false) => painter.stroke_rounded(square, 4.0, theme::text_tertiary(), 1.0),
            }
        },
    )
    .size(18.0, 22.0)
}

/// A check box with its label; the whole row toggles it.
pub fn checkbox<M>(label: impl Into<String>, checked: bool, message: M) -> El<M> {
    tri_checkbox(label, Some(checked), message)
}

/// A check box alone, its label elsewhere (a shader pack's, beside its fold header); `name` is its
/// UI Automation name. It can be partly checked (`None`).
pub fn check_button<M>(state: Option<bool>, name: impl Into<String>, message: M) -> El<M> {
    row([check_square(state)])
        .pad_xy(4.0, 4.0)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .radius(theme::RADIUS_CONTROL)
        .role(Role::CheckBox)
        .selected(state == Some(true))
        .name(name)
        .on_click(message)
}

/// A check box that can be partly checked (`None`: some of a pack's effects), with its label; the
/// whole row toggles it.
pub fn tri_checkbox<M>(label: impl Into<String>, state: Option<bool>, message: M) -> El<M> {
    row([check_square(state), text(label).wrap().fill_width()])
        .gap(8.0)
        .pad_xy(4.0, 2.0)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .radius(theme::RADIUS_CONTROL)
        .role(Role::CheckBox)
        .selected(state == Some(true))
        .on_click(message)
}

/// A switch with its label.
pub fn toggle<M>(label: impl Into<String>, on: bool, message: M) -> El<M> {
    let knob = paint(
        move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
            let track = Rect::new(rect.x, rect.y + (rect.h - 20.0) / 2.0, 40.0, 20.0);
            if on {
                painter.fill_rounded(track, 10.0, theme::accent());
            } else {
                painter.stroke_rounded(track, 10.0, theme::text_tertiary(), 1.0);
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
                    theme::text()
                } else {
                    theme::text_secondary()
                },
            );
        },
    )
    .size(40.0, 24.0);
    row([knob, text(label).fill_width()])
        .gap(10.0)
        .pad_xy(4.0, 2.0)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::subtle_hover())
        .role(Role::CheckBox)
        .selected(on)
        .on_click(message)
}

/// A red bar with what went wrong, and ✕ to dismiss it.
pub fn error_bar<M>(title: &str, message: &str, on_close: M) -> El<M> {
    message_bar(
        (theme::glyph::ERROR, theme::critical()),
        theme::error_fill(),
        title,
        message,
        on_close,
    )
}

/// A bar under the title bar saying something went as asked (a shortcut made where it says), with
/// a close button: the error bar's layout in the accent's tint.
pub fn notice_bar<M>(title: &str, message: &str, on_close: M) -> El<M> {
    message_bar(
        (theme::glyph::INFO, theme::accent()),
        theme::wash(theme::accent()),
        title,
        message,
        on_close,
    )
}

fn message_bar<M>(
    (icon, tint): (&str, ColorF),
    fill: ColorF,
    title: &str,
    message: &str,
    on_close: M,
) -> El<M> {
    row([
        glyph(icon, 16.0).color(tint).valign(Align::Start),
        col([
            text(title).bold(),
            text(message).wrap().color(theme::text_secondary()),
        ])
        .gap(2.0)
        .fill_width(),
        icon_button(theme::glyph::CLOSE, "Dismiss")
            .on_click(on_close)
            .valign(Align::Start),
    ])
    .gap(12.0)
    .pad_xy(14.0, 10.0)
    .bg(fill)
    .radius(6.0)
}

/// A page's title, in the display font.
pub fn heading<M>(label: impl Into<String>) -> El<M> {
    text(label)
        .font(Font::Display)
        .text_size(theme::TEXT_TITLE)
        .weight(600)
}

/// A label in small capitals over a part of a page, a list or a menu ("PINNED", "HARDWARE",
/// "SORT").
pub fn section_label<M>(label: &str) -> El<M> {
    text(label.to_uppercase())
        .text_size(theme::TEXT_CAPTION)
        .weight(700)
        .color(theme::text_tertiary())
}

/// How many a section holds, beside its label ("PINNED 3").
pub fn count_badge<M>(count: usize) -> El<M> {
    row([text(count.to_string())
        .text_size(theme::TEXT_CAPTION)
        .color(theme::text_tertiary())])
    .pad_xy(6.0, 1.0)
    .bg(theme::control())
    .radius(theme::RADIUS_CARD)
}

/// A page's header: what the page is in small capitals (`caption`), its title, a line under it,
/// and its actions on the right. Library, My PC and Settings share it; the game page has its own.
pub fn page_header<M>(
    caption: &str,
    title: impl Into<String>,
    line: Option<String>,
    actions: Vec<El<M>>,
) -> El<M> {
    let mut lines = vec![section_label(caption), heading(title).wrap()];
    if let Some(line) = line {
        lines.push(hint(line));
    }
    let mut parts = vec![col(lines).gap(theme::space::XS).fill_width()];
    parts.extend(
        actions
            .into_iter()
            .map(|action| action.valign(Align::Start)),
    );
    row(parts).gap(theme::space::M)
}

/// A secondary line of explanation.
pub fn hint<M>(label: impl Into<String>) -> El<M> {
    text(label)
        .text_size(theme::TEXT_SMALL)
        .color(theme::text_tertiary())
        .wrap()
}

/// A thin line between areas.
pub fn divider<M>() -> El<M> {
    col([]).height(1.0).bg(theme::divider())
}

/// A thin strip that resizes what is beside it by dragging (the sidebar): the left-right arrows
/// and a faint accent line under the pointer.
/// `on_drag` gets the pointer's x in window DIPs.
pub fn resize_grip<M>(name: &str, on_drag: impl Fn(f32) -> M + 'static) -> El<M> {
    paint(
        |painter: &mut Painter<'_>, rect: Rect, interaction: Interaction| {
            if interaction.hover || interaction.pressed {
                let line = Rect::new(rect.x + rect.w / 2.0 - 1.0, rect.y, 2.0, rect.h);
                painter.fill_rect(line, theme::with_alpha(theme::accent(), 0x99));
            }
        },
    )
    .width(6.0)
    .fill_height()
    .name(name)
    .on_drag(move |(x, _)| on_drag(x))
}

/// Pills switching between a tab's views (the ReShade tab's ReShade, Shaders and Add-ons): left
/// aligned in a faint track, the selected one filled. They are tabs to UI Automation.
pub fn segmented<M>(items: Vec<(&str, bool, M)>) -> El<M> {
    row(items.into_iter().map(|(label, selected, message)| {
        row([text(label)
            .weight(if selected { 600 } else { 400 })
            .color(if selected {
                theme::text()
            } else {
                theme::text_secondary()
            })])
        .pad_xy(16.0, 0.0)
        .height(28.0)
        .radius(14.0)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .role(Role::Tab)
        .selected(selected)
        .on_click(message)
    }))
    .gap(2.0)
    .pad(3.0)
    .bg(theme::card())
    .stroke(theme::card_stroke(), 1.0)
    .radius(17.0)
    .halign(Align::Start)
}

/// A usage bar, `ratio` (0–1) of it filled: the accent, or red from 90% (a drive nearly full).
pub fn meter<M>(ratio: f32) -> El<M> {
    let ratio = ratio.clamp(0.0, 1.0);
    paint(
        move |painter: &mut Painter<'_>, rect: Rect, _: Interaction| {
            let bar = Rect::new(rect.x, rect.y + (rect.h - 4.0) / 2.0, rect.w, 4.0);
            painter.fill_rounded(bar, 2.0, theme::control_hover());
            let color = if ratio >= 0.9 {
                theme::critical()
            } else {
                theme::accent()
            };
            painter.fill_rounded(Rect::new(bar.x, bar.y, bar.w * ratio, bar.h), 2.0, color);
        },
    )
    .height(8.0)
}

/// A label with a busy spinner.
pub fn working<M>(label: impl Into<String>) -> El<M> {
    row([spinner(16.0), text(label).color(theme::text_secondary())]).gap(8.0)
}

/// What the Play button shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlayState {
    Play,
    /// Just launched: a spinner in place of the icon.
    Starting,
    /// The game is running (`play.rs`): a dot in place of the icon; the button is off.
    Running,
}

/// The game page's big launch button.
pub fn play_button<M>(state: PlayState, message: M) -> El<M> {
    let label = match state {
        PlayState::Play => "PLAY",
        PlayState::Starting => "STARTING…",
        PlayState::Running => "RUNNING",
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
    let color = theme::play();
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
        theme::with_alpha(theme::text(), 0x1F),
        theme::with_alpha(theme::text(), 0x05),
    );
    // The crisp edge: lighter on top, darker at the bottom.
    painter.line(
        (face.x + CORNER + 4.0, face.y + 1.0),
        (face.right() - CORNER - 4.0, face.y + 1.0),
        theme::with_alpha(theme::text(), 0x59),
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
        painter.fill_rounded(face, CORNER, theme::with_alpha(theme::text(), 0x14));
    }
    let word = TextSpec {
        text: label.to_owned(),
        size: 19.0,
        weight: 600,
        font: Font::Display,
        color: theme::text(),
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
                &glyph_spec(theme::glyph::PLAY, 18.0, theme::text()),
                Rect::new(x, face.y, icon_w, face.h),
            );
        }
        PlayState::Starting => {
            let dot = |i: f32| Rect::new(x + i * 7.0, face.y + face.h / 2.0 - 2.5, 5.0, 5.0);
            for i in 0..3 {
                painter.fill_rounded(
                    dot(i as f32),
                    2.5,
                    theme::with_alpha(theme::text(), 0x99 + i as u8 * 0x20),
                );
            }
        }
        PlayState::Running => {
            // A steady dot: the game is on.
            painter.fill_rounded(
                Rect::new(x + 5.0, face.y + face.h / 2.0 - 5.0, 10.0, 10.0),
                5.0,
                theme::text(),
            );
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
