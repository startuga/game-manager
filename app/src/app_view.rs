//! The window's view: the title bar, the sidebar beside the page, the error bar, and the layers
//! over them: the Sort and filters flyout (`filter_menu.rs`, over the sidebar and the page), the
//! game's menu (`game_menu.rs`) and the dialogs.
//!
//! The title bar is drawn by the app. Its keys (`shell::CAPTION_*`) tell the window where it can
//! be dragged and where its minimize, maximize and close buttons are, so Windows 11 offers Snap
//! Layouts over maximize. Dialogs are a dimmed, modal layer with a card centred on it: nothing
//! under a dialog reacts, and Escape is its Cancel. Their buttons sit at the right, primary
//! first, each at least `theme::BUTTON_MIN_WIDTH` wide; Add a game's inputs are `field`s.

use crate::my_pc::MyPcMessage;
use crate::shell::ctx::WindowCommand;
use crate::shell::{CAPTION_CLOSE, CAPTION_DRAG, CAPTION_MAX, CAPTION_MIN, WindowInfo};
use crate::ui::el::{Align, El, Font, TextAlign, col, glyph, layers, row, scroll, spacer, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    accent_button, button, danger_button, error_bar, field, hint, icon_button, resize_grip,
    text_box, working,
};
use crate::{DialogResult, GameManager, Message};

impl GameManager {
    pub(crate) fn root_view(&self, window: &WindowInfo) -> El<Message> {
        let mut page = vec![self.title_bar(window)];
        if let Some(crash) = &self.crash {
            page.push(
                col([error_bar(
                    "Game Manager stopped unexpectedly last time",
                    &format!(
                        "{}. The details are in {}.",
                        crash.summary,
                        crash.file.display()
                    ),
                    Message::DismissCrash,
                )])
                .pad_xy(theme::space::M, theme::space::S),
            );
        }
        if let Some(error) = &self.error {
            page.push(
                col([error_bar(
                    "Something went wrong",
                    error,
                    Message::DismissError,
                )])
                .pad_xy(theme::space::M, theme::space::S),
            );
        }
        let grip = resize_grip("Resize the library", Message::SidebarWidth);
        let body = row([self.sidebar(), grip, self.page_view(window)])
            .grow()
            .valign(Align::Stretch);
        // The Sort and filters flyout lies over the sidebar and the page only, so it opens right
        // under its button whatever is above them. Always a layer, so the elements under it keep
        // their place in the tree when it opens.
        page.push(layers([body].into_iter().chain(self.filter_layer(window))).grow());
        let mut stack = vec![col(page).grow()];
        stack.extend(self.game_menu_layer(window));
        stack.extend(self.dialog_layer(window));
        layers(stack).grow()
    }

    /// The app's name, what it is doing, Rescan, and the caption buttons.
    fn title_bar(&self, window: &WindowInfo) -> El<Message> {
        let status: El<Message> = if self.scanning {
            working("Scanning libraries…")
        } else {
            text(format!("{} games", self.games.len()))
                .text_size(theme::TEXT_SMALL)
                .color(theme::text_tertiary())
        };
        let caption = |icon: &str, key: &str, name: &str, command: WindowCommand| {
            let face = glyph(icon, 10.0).text_align(TextAlign::Center).fill_width();
            let button = row([face])
                .size(theme::CAPTION_BUTTON_WIDTH, theme::TITLE_BAR_HEIGHT)
                .key(key)
                .name(name)
                .on_click(Message::Window(command));
            if command == WindowCommand::Close {
                button
                    .hover_bg(theme::DANGER)
                    .pressed_bg(theme::DANGER_PRESSED)
            } else {
                button
                    .hover_bg(theme::subtle_hover())
                    .pressed_bg(theme::subtle_pressed())
            }
        };
        let (max_icon, max_name) = if window.maximized {
            (g::RESTORE, "Restore")
        } else {
            (g::MAXIMIZE, "Maximize")
        };
        row([
            glyph(g::GAME, 16.0).color(theme::accent()),
            text("GAME MANAGER")
                .font(Font::Display)
                .text_size(theme::TEXT_SIZE)
                .weight(700),
            status,
            spacer(),
            icon_button(g::REFRESH, "Rescan the libraries")
                .enabled(!self.scanning)
                .on_click(Message::Scan),
            caption(
                g::MINIMIZE,
                CAPTION_MIN,
                "Minimize",
                WindowCommand::Minimize,
            ),
            caption(max_icon, CAPTION_MAX, max_name, WindowCommand::Maximize),
            caption(g::CLOSE, CAPTION_CLOSE, "Close", WindowCommand::Close),
        ])
        .gap(theme::space::S)
        .padding(crate::ui::el::Edges::new(theme::space::L, 0.0, 0.0, 0.0))
        .height(theme::TITLE_BAR_HEIGHT)
        .bg(theme::layer())
        .key(CAPTION_DRAG)
    }

    /// The open dialog: a new display mode to keep, renaming or uninstalling a game (its menu's),
    /// adding a game, removing one, or a mod's Info or settings.
    fn dialog_layer(&self, window: &WindowInfo) -> Option<El<Message>> {
        if let Some(trial) = &self.my_pc.trial {
            let mode = trial.after;
            return Some(dialog_with(
                window,
                "Keep these display settings?",
                vec![
                    text(format!(
                        "{} now runs at {} × {}, {} Hz. Keep it? The previous mode comes back \
                         in {} s.",
                        trial.display.name, mode.width, mode.height, mode.hz, trial.seconds
                    ))
                    .wrap(),
                ],
                Some(("Keep", !self.my_pc.busy, MyPcMessage::KeepMode.into())),
                ("Go back", MyPcMessage::RevertMode.into()),
            ));
        }
        if let Some(dialog) = self.game_menu_dialog(window) {
            return Some(dialog);
        }
        if let Some(draft) = &self.draft {
            let has_name = !draft.name.trim().is_empty();
            let body = vec![
                hint(draft.program.display().to_string()),
                field(
                    "Name",
                    text_box(
                        "draft:name",
                        draft.name.clone(),
                        "",
                        "Name",
                        Message::DraftName,
                    ),
                    "",
                ),
                field(
                    "Command-line arguments",
                    text_box(
                        "draft:args",
                        draft.args.clone(),
                        "For example: -windowed",
                        "Command-line arguments",
                        Message::DraftArgs,
                    ),
                    "Optional: passed to the program when Play starts it.",
                ),
            ];
            return Some(dialog(
                window,
                "Add a game",
                body,
                Some(("Add", has_name, Message::DraftClosed(DialogResult::Primary))),
                Message::DraftClosed(DialogResult::Cancel),
            ));
        }
        if let Some(id) = &self.removing {
            let name = self
                .games
                .iter()
                .find(|game| game.id == *id)
                .map_or_default(|game| game.name.clone());
            return Some(removal_dialog(
                window,
                &format!("Remove {name}?"),
                vec![
                    text("It is removed from the library only. The game's files are not deleted.")
                        .wrap(),
                ],
                ("Remove", Message::RemoveClosed(DialogResult::Primary)),
                Message::RemoveClosed(DialogResult::Cancel),
            ));
        }
        if let Some(artwork) = self.artwork_dialog(window) {
            return Some(artwork);
        }
        self.mod_dialog_view(window)
    }
}

/// A modal dialog: a dimmed backdrop and a card with a title, a body (scrolled when it doesn't fit
/// the window) and its buttons (the main one, if any, then Cancel or Close).
pub(crate) fn dialog(
    window: &WindowInfo,
    title: &str,
    body: Vec<El<Message>>,
    primary: Option<(&str, bool, Message)>,
    cancel: Message,
) -> El<Message> {
    let label = if primary.is_some() { "Cancel" } else { "Close" };
    dialog_with(window, title, body, primary, (label, cancel))
}

/// A modal dialog whose closing button has its own label ("Go back").
pub(crate) fn dialog_with(
    window: &WindowInfo,
    title: &str,
    body: Vec<El<Message>>,
    primary: Option<(&str, bool, Message)>,
    (cancel_label, cancel): (&str, Message),
) -> El<Message> {
    let primary = primary
        .map(|(label, enabled, message)| accent_button(label).enabled(enabled).on_click(message));
    dialog_card(
        window,
        title,
        body,
        primary,
        (cancel_label, cancel),
        DIALOG_WIDTH,
    )
}

/// A dialog's width.
const DIALOG_WIDTH: f32 = 520.0;
/// A wide dialog's (the Artwork dialog's), at most, and its margin to the window's edges.
const WIDE_DIALOG_WIDTH: f32 = 1000.0;
const WIDE_DIALOG_MARGIN: f32 = 48.0;

/// A wide dialog's width in this window: most of it, from `DIALOG_WIDTH` to `WIDE_DIALOG_WIDTH`.
pub(crate) fn wide_dialog_width(window: &WindowInfo) -> f32 {
    (window.size.0 - 2.0 * WIDE_DIALOG_MARGIN).clamp(DIALOG_WIDTH, WIDE_DIALOG_WIDTH)
}

/// A wide dialog (`wide_dialog_width`) with only the button that closes it.
pub(crate) fn wide_dialog(
    window: &WindowInfo,
    title: &str,
    body: Vec<El<Message>>,
    (close_label, close): (&str, Message),
) -> El<Message> {
    let width = wide_dialog_width(window);
    dialog_card(window, title, body, None, (close_label, close), width)
}

/// A dialog that asks before something is removed: its main button is red (`danger_button`),
/// then Cancel. Every removal that loses something a click can't bring back asks this way.
pub(crate) fn removal_dialog(
    window: &WindowInfo,
    title: &str,
    body: Vec<El<Message>>,
    (label, remove): (&str, Message),
    cancel: Message,
) -> El<Message> {
    let primary = danger_button(label).on_click(remove);
    dialog_card(
        window,
        title,
        body,
        Some(primary),
        ("Cancel", cancel),
        DIALOG_WIDTH,
    )
}

/// The dialog's card over the dimmed window: the title, the body, the main button and the one
/// that closes it.
fn dialog_card(
    window: &WindowInfo,
    title: &str,
    body: Vec<El<Message>>,
    primary: Option<El<Message>>,
    (cancel_label, cancel): (&str, Message),
    width: f32,
) -> El<Message> {
    let mut buttons = vec![spacer()];
    if let Some(primary) = primary {
        buttons.push(primary.min_width(theme::BUTTON_MIN_WIDTH));
    }
    buttons.push(
        button(cancel_label)
            .on_click(cancel)
            .min_width(theme::BUTTON_MIN_WIDTH),
    );
    // The title, the buttons and the padding take about 170 DIPs.
    let body_height = (window.size.1 - 200.0).max(160.0);
    let body = scroll(
        "dialog:body",
        col(body)
            .gap(theme::space::S)
            .padding(crate::ui::el::Edges::new(0.0, 0.0, theme::space::M, 0.0)),
    )
    .max_height(body_height);
    let card = col([
        text(title).text_size(theme::TEXT_HEADLINE).bold(),
        body,
        row(buttons)
            .gap(theme::space::S)
            .padding(crate::ui::el::Edges::new(0.0, theme::space::M, 0.0, 0.0)),
    ])
    .gap(theme::space::M)
    .pad(theme::space::XL)
    .width(width)
    .bg(theme::flyout())
    .stroke(theme::flyout_stroke(), 1.0)
    .radius(theme::RADIUS_FLYOUT)
    .center();
    layers([col([]).grow().bg(theme::scrim()).modal(), card]).grow()
}
