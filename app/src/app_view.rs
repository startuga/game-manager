//! The window's view: the title bar, the sidebar beside the page, the error bar, and the layers
//! over them: the game's right-click menu and the dialogs.
//!
//! The title bar is drawn by the app. Its keys (`shell::CAPTION_*`) tell the window where it can
//! be dragged and where its minimize, maximize and close buttons are, so Windows 11 offers Snap
//! Layouts over maximize. Dialogs are a dimmed, modal layer with a card centred on it: nothing
//! under a dialog reacts, and Escape is its Cancel.

use crate::shell::ctx::WindowCommand;
use crate::shell::{CAPTION_CLOSE, CAPTION_DRAG, CAPTION_MAX, CAPTION_MIN, WindowInfo};
use crate::ui::el::{Align, El, Font, TextAlign, col, glyph, layers, row, scroll, spacer, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    accent_button, button, error_bar, icon_button, resize_grip, text_box, working,
};
use crate::{DialogResult, GameManager, Message};

/// The game menu's width and its height (two items).
const MENU_WIDTH: f32 = 240.0;
const MENU_HEIGHT: f32 = 78.0;
/// The close button's hover colour, as Windows draws it.
const CLOSE_HOVER: windows_canvas::ColorF = theme::argb(0xFFC4_2B1C);

impl GameManager {
    pub(crate) fn root_view(&self, window: &WindowInfo) -> El<Message> {
        let mut page = vec![self.title_bar(window)];
        if let Some(error) = &self.error {
            page.push(
                col([error_bar(
                    "Something went wrong",
                    error,
                    Message::DismissError,
                )])
                .pad_xy(12.0, 8.0),
            );
        }
        let grip = resize_grip("Resize the library", Message::SidebarWidth);
        page.push(
            row([self.sidebar(), grip, self.page_view()])
                .grow()
                .valign(Align::Stretch),
        );
        let mut stack = vec![col(page).grow()];
        stack.extend(self.row_menu_layer(window));
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
                .color(theme::TEXT_TERTIARY)
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
                    .hover_bg(CLOSE_HOVER)
                    .pressed_bg(theme::argb(0xFFA0_2318))
            } else {
                button
                    .hover_bg(theme::SUBTLE_HOVER)
                    .pressed_bg(theme::SUBTLE_PRESSED)
            }
        };
        let (max_icon, max_name) = if window.maximized {
            (g::RESTORE, "Restore")
        } else {
            (g::MAXIMIZE, "Maximize")
        };
        row([
            glyph(g::GAME, 16.0).color(theme::ACCENT),
            text("GAME MANAGER")
                .font(Font::Title)
                .text_size(13.0)
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
        .gap(10.0)
        .padding(crate::ui::el::Edges::new(14.0, 0.0, 0.0, 0.0))
        .height(theme::TITLE_BAR_HEIGHT)
        .bg(theme::LAYER)
        .key(CAPTION_DRAG)
    }

    /// The game's menu at the pointer, kept inside the window, over a backdrop that closes it.
    fn row_menu_layer(&self, window: &WindowInfo) -> Option<El<Message>> {
        let menu = self.row_menu.as_ref()?;
        let game = self.games.iter().find(|game| game.id == menu.id)?;
        let (width, height) = window.size;
        let x = if menu.x + MENU_WIDTH > width {
            (menu.x - MENU_WIDTH).max(0.0)
        } else {
            menu.x
        };
        let y = if menu.y + MENU_HEIGHT > height {
            (menu.y - MENU_HEIGHT).max(0.0)
        } else {
            menu.y
        };
        let item = |icon: &str, label: &str, name: String, message: Message| {
            row([glyph(icon, theme::ICON_SIZE), text(label)])
                .gap(10.0)
                .pad_xy(10.0, 0.0)
                .height(32.0)
                .hover_bg(theme::SUBTLE_HOVER)
                .pressed_bg(theme::SUBTLE_PRESSED)
                .radius(theme::RADIUS_CONTROL)
                .name(name)
                .on_click(message)
        };
        let favorite = if game.favorite {
            item(
                g::STAR_FILLED,
                "Remove from Favorites",
                format!("Remove {} from Favorites", game.name),
                Message::RowMenuFavorite,
            )
        } else {
            item(
                g::STAR,
                "Add to Favorites",
                format!("Add {} to Favorites", game.name),
                Message::RowMenuFavorite,
            )
        };
        let panel = col([
            item(
                g::PLAY,
                "Play",
                format!("Play {}", game.name),
                Message::RowMenuPlay,
            ),
            favorite,
        ])
        .gap(2.0)
        .pad(4.0)
        .width(MENU_WIDTH)
        .bg(theme::FLYOUT)
        .stroke(theme::FLYOUT_STROKE, 1.0)
        .radius(theme::RADIUS_FLYOUT);
        let backdrop = col([]).grow().modal().on_click(Message::CloseRowMenu);
        let placed = col([panel])
            .padding(crate::ui::el::Edges::new(x, y, 0.0, 0.0))
            .halign(Align::Start)
            .valign(Align::Start);
        Some(layers([backdrop, placed]).grow())
    }

    /// The open dialog: adding a game, removing one, or a mod's Info or settings.
    fn dialog_layer(&self, window: &WindowInfo) -> Option<El<Message>> {
        if let Some(draft) = &self.draft {
            let has_name = !draft.name.trim().is_empty();
            let body = vec![
                text(draft.program.display().to_string())
                    .color(theme::TEXT_TERTIARY)
                    .wrap(),
                field_label("Name"),
                text_box(
                    "draft:name",
                    draft.name.clone(),
                    "",
                    "Name",
                    Message::DraftName,
                ),
                field_label("Command-line arguments (optional)"),
                text_box(
                    "draft:args",
                    draft.args.clone(),
                    "For example: -windowed",
                    "Command-line arguments",
                    Message::DraftArgs,
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
                .map(|game| game.name.clone())
                .unwrap_or_default();
            return Some(dialog(
                window,
                &format!("Remove {name}?"),
                vec![
                    text("It is removed from the library only. The game's files are not deleted.")
                        .wrap(),
                ],
                Some(("Remove", true, Message::RemoveClosed(DialogResult::Primary))),
                Message::RemoveClosed(DialogResult::Cancel),
            ));
        }
        self.mod_dialog_view(window)
    }
}

/// A form field's label.
pub(crate) fn field_label(label: &str) -> El<Message> {
    text(label)
        .bold()
        .padding(crate::ui::el::Edges::new(0.0, 6.0, 0.0, 0.0))
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
    let mut buttons = vec![spacer()];
    let has_primary = primary.is_some();
    if let Some((label, enabled, message)) = primary {
        buttons.push(
            accent_button(label)
                .enabled(enabled)
                .on_click(message)
                .min_width(96.0),
        );
    }
    buttons.push(
        button(if has_primary { "Cancel" } else { "Close" })
            .on_click(cancel)
            .min_width(96.0),
    );
    // The title, the buttons and the padding take about 170 DIPs.
    let body_height = (window.size.1 - 200.0).max(160.0);
    let body = scroll(
        "dialog:body",
        col(body)
            .gap(6.0)
            .padding(crate::ui::el::Edges::new(0.0, 0.0, 12.0, 0.0)),
    )
    .max_height(body_height);
    let card = col([
        text(title).text_size(theme::TEXT_SUBTITLE + 3.0).bold(),
        body,
        row(buttons)
            .gap(8.0)
            .padding(crate::ui::el::Edges::new(0.0, 12.0, 0.0, 0.0)),
    ])
    .gap(12.0)
    .pad(24.0)
    .width(520.0)
    .bg(theme::FLYOUT)
    .stroke(theme::FLYOUT_STROKE, 1.0)
    .radius(theme::RADIUS_FLYOUT)
    .center();
    layers([col([]).grow().bg(theme::SCRIM).modal(), card]).grow()
}
