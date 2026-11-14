//! A game's menu: a right-click on its sidebar row or Library tile, or the Menu key or Shift+F10
//! on one (`ui::input`, `Key::Menu`). It opens where it was asked for, over the page, which stays
//! as it was. Escape or a click outside closes it; a right-click outside opens the menu of the
//! game there instead (or only closes it), as Windows' menus do. Its header and lines take clicks
//! without closing it (`opaque`).
//!
//! - **What it holds**: a header with the game's
//!   icon, name and store; Pin to quick access (or Unpin), Play and its alternative launches
//!   ("Skip launcher"); Open folder, PCGamingWiki, Nexus Mods, Saves folder and Config folder (as
//!   the game page's buttons; a folder not looked up yet is looked up, then opened or the error
//!   bar says why not);
//!   Rename…, Choose cover… (and Reset cover for a game with the user's own, `covers.rs`), Hide
//!   from the list (or Show in the list);
//!   Uninstall… for a store's game, Remove from Game Manager for one added by hand. Groups are
//!   split by lines, and the two destructive items are red.
//! - **Its size** is worked out from its rows, so it flips up or left to stay in the window.
//! - **Keys**: ↑↓ go round its items (`Role::MenuItem`), Enter picks one.
//! - **Pin to quick access**: pinned games come first in the sidebar and on Library, in the order
//!   they were pinned (`library::split_pinned`). Shown at once (`library::set_pin`), saved in
//!   `game-settings.json` in the background (`config::set_pinned`), set back on failure. Pins
//!   replaced the favorites of earlier versions, which `config` reads as pins.
//! - **Rename…**: a dialog with the name; Save stores it like Game settings does
//!   (`config::save_edits` with only the name changed; empty is the store's name), then rescans.
//! - **Hide from the list**: shown at once, saved in `game-settings.json` in the background
//!   (`config::set_hidden`, set back on failure). Hidden games show only with Hidden games on in
//!   Sort and filters (`library.rs`). Hiding the game whose page shows goes back to Library.
//! - **Uninstall…**: the route (`gm_core::uninstall`, registry reads, in the background), then a
//!   dialog saying who removes the game and that mods outside its folder stay; confirming hands
//!   over to Steam, the game's uninstaller or the store's app, which asks before anything goes.

use gm_core::Store;
use gm_core::game_data::FolderKind;
use gm_core::uninstall::Route;

use crate::app_view::dialog;
use crate::covers::CoverMessage;
use crate::shell::WindowInfo;
use crate::shell::ctx::Ctx;
use crate::ui::el::{Align, Edges, El, Role, col, glyph, layers, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{hint, text_box};
use crate::{DialogResult, GameManager, Message, Page, library};

/// The menu's width, and its rows' heights.
const WIDTH: f32 = 260.0;
const ITEM_HEIGHT: f32 = 32.0;
const HEADER_HEIGHT: f32 = 48.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const GAP: f32 = 2.0;
const PADDING: f32 = 4.0;
/// The space kept between the menu and the window's edges.
const MARGIN: f32 = 8.0;

/// What the menu's items do, to the game it was opened for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    /// Pins it to quick access, or unpins it.
    Pin,
    /// Launches it: its default action, or the alternative at this index of `alt_launches`.
    Play(Option<usize>),
    Folder,
    Wiki,
    NexusMods,
    /// Opens its saves or config folder, as PCGamingWiki names it (looked up first if need be).
    GameFolder(FolderKind),
    Rename,
    /// A picture of the user's as the game's cover (`covers.rs`), or back to the store's.
    ChooseCover,
    ResetCover,
    /// Hides it from the list, or shows it again.
    Hide,
    Uninstall,
    /// Removes a game added by hand from Game Manager (after asking).
    Remove,
}

#[derive(Clone)]
pub(crate) enum MenuMessage {
    /// A right-click on a game, or the Menu key on it, at this point of the window (DIPs).
    Open(String, (f32, f32)),
    /// An item picked: the menu closes, then it acts.
    Pick(MenuAction),
    /// A click outside the menu, or Escape.
    Close,
    /// The name typed in Rename.
    RenameText(String),
    /// The Rename dialog closed: `Primary` is Save.
    RenameClosed(DialogResult),
    /// How the game is uninstalled (read in the background); `None` for a game added by hand.
    UninstallRoute(String, Option<Route>),
    /// The Uninstall dialog closed: `Primary` hands over.
    UninstallClosed(DialogResult),
    /// The game's hidden flag was saved (or not; then it is set back).
    HiddenSaved(String, bool, Result<(), String>),
    /// The game's pin was saved (or not; then it is set back).
    PinSaved(String, bool, Result<(), String>),
}

impl From<MenuMessage> for Message {
    fn from(message: MenuMessage) -> Self {
        Message::Menu(message)
    }
}

/// The open menu: which game, and where it was asked for (window DIPs).
pub(crate) struct OpenMenu {
    id: String,
    x: f32,
    y: f32,
}

/// The game being renamed, and the name typed.
pub(crate) struct Rename {
    id: String,
    name: String,
}

/// The game about to be uninstalled, and how.
pub(crate) struct Uninstall {
    id: String,
    route: Route,
}

/// The menu and the dialogs it opens.
#[derive(Default)]
pub(crate) struct MenuState {
    pub open: Option<OpenMenu>,
    pub renaming: Option<Rename>,
    pub uninstalling: Option<Uninstall>,
}

impl MenuState {
    /// What Escape closes first: the menu, then its dialogs.
    pub fn escape(&self) -> Option<Message> {
        if self.open.is_some() {
            Some(MenuMessage::Close.into())
        } else if self.uninstalling.is_some() {
            Some(MenuMessage::UninstallClosed(DialogResult::Cancel).into())
        } else if self.renaming.is_some() {
            Some(MenuMessage::RenameClosed(DialogResult::Cancel).into())
        } else {
            None
        }
    }
}

/// The rows of the menu, with the height they take.
struct Rows {
    rows: Vec<El<Message>>,
    height: f32,
}

impl Rows {
    fn push(&mut self, row: El<Message>, height: f32) {
        if !self.rows.is_empty() {
            self.height += GAP;
        }
        self.rows.push(row);
        self.height += height;
    }

    fn item(&mut self, icon: &str, label: &str, name: String, action: MenuAction) {
        self.push(item(icon, label, name, action, false), ITEM_HEIGHT);
    }

    fn separator(&mut self) {
        let line = col([]).height(1.0).bg(theme::DIVIDER);
        self.push(col([line]).pad_xy(6.0, 4.0), SEPARATOR_HEIGHT);
    }
}

/// An item: its glyph and label (red for a destructive one), `name` for UI Automation.
fn item(icon: &str, label: &str, name: String, action: MenuAction, danger: bool) -> El<Message> {
    let mut face = glyph(icon, theme::ICON_SIZE).width(theme::ICON_SIZE);
    let mut label = text(label).fill_width();
    if danger {
        face = face.color(theme::CRITICAL);
        label = label.color(theme::CRITICAL);
    }
    row([face, label])
        .gap(10.0)
        .pad_xy(10.0, 0.0)
        .height(ITEM_HEIGHT)
        .hover_bg(theme::SUBTLE_HOVER)
        .pressed_bg(theme::SUBTLE_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::MenuItem)
        .name(name)
        .on_click(MenuMessage::Pick(action).into())
}

/// Where a menu `size` big opens for a point: at it, else flipped left or up to stay inside the
/// window; with room on neither side, against the far edge, never past the top-left corner.
fn menu_origin(at: (f32, f32), size: (f32, f32), window: (f32, f32)) -> (f32, f32) {
    let place = |at: f32, size: f32, room: f32| {
        if at + size <= room - MARGIN {
            at
        } else if at - size >= MARGIN {
            at - size
        } else {
            (room - MARGIN - size).max(MARGIN)
        }
    };
    (place(at.0, size.0, window.0), place(at.1, size.1, window.1))
}

impl GameManager {
    /// The open menu over a backdrop that closes it.
    pub(crate) fn game_menu_layer(&self, window: &WindowInfo) -> Option<El<Message>> {
        let menu = self.menu.open.as_ref()?;
        let game = self.games.iter().find(|game| game.id == menu.id)?;
        let mut rows = Rows {
            rows: Vec::new(),
            height: 0.0,
        };
        let header = row([
            self.icon(game, crate::icons::SMALL),
            col([
                text(game.name.clone()).bold(),
                text(game.store.name())
                    .text_size(theme::TEXT_CAPTION)
                    .color(theme::TEXT_TERTIARY),
            ])
            .gap(2.0)
            .fill_width(),
        ])
        .gap(10.0)
        .pad_xy(10.0, 0.0)
        .height(HEADER_HEIGHT);
        rows.push(header, HEADER_HEIGHT);
        rows.separator();

        // First, above Play.
        let (pin, pin_label) = if game.pinned.is_some() {
            (g::UNPIN, "Unpin from quick access")
        } else {
            (g::PIN, "Pin to quick access")
        };
        rows.item(
            pin,
            pin_label,
            format!("{pin_label}: {}", game.name),
            MenuAction::Pin,
        );
        if self.launching.as_deref() == Some(game.id.as_str()) {
            let starting = item(
                g::PLAY,
                "Starting…",
                format!("{} is starting", game.name),
                MenuAction::Play(None),
                false,
            )
            .enabled(false);
            rows.push(starting, ITEM_HEIGHT);
        } else {
            rows.item(
                g::PLAY,
                "Play",
                format!("Play {}", game.name),
                MenuAction::Play(None),
            );
            for (index, alt) in game.alt_launches.iter().enumerate() {
                // Under Play, lined up with its label.
                rows.item(
                    "",
                    &alt.label,
                    format!("{}: {}", game.name, alt.label),
                    MenuAction::Play(Some(index)),
                );
            }
        }
        rows.separator();

        if game.install_dir.is_some() {
            rows.item(
                g::FOLDER,
                "Open folder",
                format!("Open {}'s folder", game.name),
                MenuAction::Folder,
            );
        }
        rows.item(
            g::LINK,
            "PCGamingWiki",
            format!("{} on PCGamingWiki", game.name),
            MenuAction::Wiki,
        );
        rows.item(
            g::LINK,
            "Nexus Mods",
            format!("{} on Nexus Mods", game.name),
            MenuAction::NexusMods,
        );
        rows.item(
            g::SAVE,
            "Saves folder",
            format!("Open the saves folder of {}", game.name),
            MenuAction::GameFolder(FolderKind::Saves),
        );
        rows.item(
            g::SETTINGS,
            "Config folder",
            format!("Open the config folder of {}", game.name),
            MenuAction::GameFolder(FolderKind::Config),
        );
        rows.separator();

        rows.item(
            g::RENAME,
            "Rename…",
            format!("Rename {}", game.name),
            MenuAction::Rename,
        );
        rows.item(
            g::PICTURE,
            "Choose cover…",
            format!("Choose a cover for {}", game.name),
            MenuAction::ChooseCover,
        );
        if self.covers.is_own(&game.id) {
            rows.item(
                g::UNDO,
                "Reset cover",
                format!("Reset {}'s cover", game.name),
                MenuAction::ResetCover,
            );
        }
        let (eye, hide) = if game.hidden {
            (g::VIEW, "Show in the list")
        } else {
            (g::HIDE, "Hide from the list")
        };
        rows.item(
            eye,
            hide,
            format!("{hide}: {}", game.name),
            MenuAction::Hide,
        );
        rows.separator();

        let destructive = if game.store == Store::Custom {
            item(
                g::REMOVE,
                "Remove from Game Manager",
                format!("Remove {} from Game Manager", game.name),
                MenuAction::Remove,
                true,
            )
        } else {
            item(
                g::DELETE,
                "Uninstall…",
                format!("Uninstall {}", game.name),
                MenuAction::Uninstall,
                true,
            )
        };
        rows.push(destructive, ITEM_HEIGHT);

        let height = rows.height + 2.0 * PADDING;
        let (x, y) = menu_origin((menu.x, menu.y), (WIDTH, height), window.size);
        let panel = col(rows.rows)
            .gap(GAP)
            .pad(PADDING)
            .width(WIDTH)
            .bg(theme::FLYOUT)
            .stroke(theme::FLYOUT_STROKE, 1.0)
            .radius(theme::RADIUS_FLYOUT)
            .role(Role::Group)
            .name(format!("{}'s menu", game.name))
            // A click on its header or lines leaves it open.
            .opaque();
        let backdrop = col([]).grow().modal().on_click(MenuMessage::Close.into());
        let placed = col([panel])
            .padding(Edges::new(x, y, 0.0, 0.0))
            .halign(Align::Start)
            .valign(Align::Start);
        Some(layers([backdrop, placed]).grow())
    }

    /// The Rename or Uninstall dialog, while one is open.
    pub(crate) fn game_menu_dialog(&self, window: &WindowInfo) -> Option<El<Message>> {
        if let Some(uninstall) = &self.menu.uninstalling {
            let game = self.games.iter().find(|game| game.id == uninstall.id)?;
            let primary = match &uninstall.route {
                Route::Steam(_) | Route::Uninstaller { .. } => "Uninstall",
                Route::StoreApp {
                    store: Store::Epic, ..
                } => "Open the Epic Games Launcher",
                Route::StoreApp {
                    store: Store::Gog, ..
                } => "Open GOG Galaxy",
                Route::StoreApp {
                    store: Store::Ubisoft,
                    ..
                } => "Open Ubisoft Connect",
                Route::StoreApp { .. } => "Open Installed apps",
            };
            return Some(dialog(
                window,
                &format!("Uninstall {}?", game.name),
                vec![
                    text(uninstall.route.describe()).wrap(),
                    hint(
                        "Mods Game Manager set up outside the game's folder stay: ReShade's \
                         Vulkan layer entry and NVIDIA profile settings. Remove them from the \
                         game's tabs first if you want them gone. Once the game is uninstalled, \
                         Rescan takes it off the list.",
                    ),
                ],
                Some((
                    primary,
                    true,
                    MenuMessage::UninstallClosed(DialogResult::Primary).into(),
                )),
                MenuMessage::UninstallClosed(DialogResult::Cancel).into(),
            ));
        }
        let rename = self.menu.renaming.as_ref()?;
        let game = self.games.iter().find(|game| game.id == rename.id)?;
        let empty = if game.store == Store::Custom {
            "Leave it empty for the name it was added with.".to_owned()
        } else {
            format!("Leave it empty for {}'s name.", game.store.name())
        };
        Some(dialog(
            window,
            &format!("Rename {}", game.name),
            vec![
                text_box("rename:name", rename.name.clone(), "", "Name", |name| {
                    MenuMessage::RenameText(name).into()
                })
                .on_enter(MenuMessage::RenameClosed(DialogResult::Primary).into()),
                hint(empty),
            ],
            Some((
                "Save",
                !self.saving,
                MenuMessage::RenameClosed(DialogResult::Primary).into(),
            )),
            MenuMessage::RenameClosed(DialogResult::Cancel).into(),
        ))
    }

    pub(crate) fn handle_menu(&mut self, message: MenuMessage, context: &Ctx<Message>) {
        match message {
            MenuMessage::Open(id, (x, y)) => self.menu.open = Some(OpenMenu { id, x, y }),
            MenuMessage::Close => self.menu.open = None,
            MenuMessage::Pick(action) => {
                if let Some(menu) = self.menu.open.take() {
                    self.menu_action(menu.id, action, context);
                }
            }
            MenuMessage::RenameText(name) => {
                if let Some(rename) = &mut self.menu.renaming {
                    rename.name = name;
                }
            }
            MenuMessage::RenameClosed(DialogResult::Cancel) => self.menu.renaming = None,
            MenuMessage::RenameClosed(DialogResult::Primary) => {
                if self.saving {
                    return;
                }
                let Some(rename) = self.menu.renaming.take() else {
                    return;
                };
                let Some(game) = self.games.iter().find(|game| game.id == rename.id) else {
                    return;
                };
                let name = rename.name.trim();
                if name == game.name {
                    return;
                }
                // Only the name changes; empty is the store's. The rescan after the save
                // (`EditsSaved`) shows it everywhere, in the list's order.
                let mut edits = game.edits.clone().unwrap_or_default();
                edits.name = (!name.is_empty()).then(|| name.to_owned());
                let id = rename.id;
                self.saving = true;
                context.spawn_background(move || {
                    Message::EditsSaved(
                        gm_core::config::save_edits(&id, &edits).map_err(|e| e.to_string()),
                    )
                });
            }
            MenuMessage::UninstallRoute(id, route) => {
                if let Some(route) = route {
                    self.menu.uninstalling = Some(Uninstall { id, route });
                }
            }
            MenuMessage::UninstallClosed(result) => {
                if let Some(uninstall) = self.menu.uninstalling.take()
                    && result == DialogResult::Primary
                {
                    // Handed over through the Shell, on this thread as `launch` is.
                    self.error = gm_core::uninstall::run(&uninstall.route)
                        .err()
                        .map(|e| e.to_string());
                }
            }
            MenuMessage::HiddenSaved(_, _, Ok(())) => {}
            MenuMessage::HiddenSaved(id, hidden, Err(e)) => {
                if let Some(game) = self.games.iter_mut().find(|game| game.id == id) {
                    game.hidden = !hidden;
                }
                self.tidy_view();
                self.error = Some(e);
            }
            MenuMessage::PinSaved(_, _, Ok(())) => {}
            MenuMessage::PinSaved(id, pinned, Err(e)) => {
                library::set_pin(&mut self.games, &id, !pinned);
                self.error = Some(e);
            }
        }
    }

    fn menu_action(&mut self, id: String, action: MenuAction, context: &Ctx<Message>) {
        let Some(game) = self.games.iter().find(|game| game.id == id) else {
            return;
        };
        match action {
            MenuAction::Pin => {
                // Shown at once, written in the background, set back on failure.
                let pinned = game.pinned.is_none();
                library::set_pin(&mut self.games, &id, pinned);
                context.spawn_background(move || {
                    let saved = gm_core::config::set_pinned(&id, pinned).map_err(|e| e.to_string());
                    MenuMessage::PinSaved(id, pinned, saved).into()
                });
            }
            MenuAction::Play(option) => self.handle(Message::Launch { id, option }, context),
            MenuAction::Folder => self.handle(Message::OpenFolder(id), context),
            MenuAction::Wiki => self.handle(Message::OpenWiki(id), context),
            MenuAction::NexusMods => self.handle(Message::OpenNexusMods(id), context),
            MenuAction::GameFolder(kind) => self.handle(Message::OpenGameFolder(id, kind), context),
            MenuAction::Rename => {
                let name = game.name.clone();
                self.menu.renaming = Some(Rename { id, name });
            }
            MenuAction::ChooseCover => self.handle_cover(CoverMessage::Choose(id), context),
            MenuAction::ResetCover => self.handle_cover(CoverMessage::Reset(id), context),
            MenuAction::Hide => self.toggle_hidden(id, context),
            MenuAction::Uninstall => {
                let game = game.clone();
                // The registry's Uninstall entries are read in the background.
                context.spawn_background(move || {
                    let route = gm_core::uninstall::route(&game);
                    MenuMessage::UninstallRoute(game.id, route).into()
                });
            }
            MenuAction::Remove => self.handle(Message::RemoveGame(id), context),
        }
    }

    /// Hides the game from the list, or shows it again. Shown at once; the settings file is
    /// written in the background, and a failure sets it back. Hiding the game whose page shows
    /// goes back to Library.
    fn toggle_hidden(&mut self, id: String, context: &Ctx<Message>) {
        let Some(game) = self.games.iter_mut().find(|game| game.id == id) else {
            return;
        };
        game.hidden = !game.hidden;
        let hidden = game.hidden;
        if hidden && self.page == Page::Game && self.selected.as_deref() == Some(id.as_str()) {
            self.go_to(Page::Library, context);
        }
        // The last hidden game shown again: the Hidden filter goes.
        self.tidy_view();
        context.spawn_background(move || {
            let saved = gm_core::config::set_hidden(&id, hidden).map_err(|e| e.to_string());
            MenuMessage::HiddenSaved(id, hidden, saved).into()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_opens_at_the_point_or_flips_to_stay_in_the_window() {
        let window = (1000.0, 700.0);
        // Room on both sides: at the point.
        assert_eq!(
            menu_origin((100.0, 100.0), (260.0, 400.0), window),
            (100.0, 100.0)
        );
        // Near the right and bottom edges: to the left of and above the point.
        assert_eq!(
            menu_origin((900.0, 650.0), (260.0, 400.0), window),
            (640.0, 250.0)
        );
        // Taller than the room above and below: as high as it fits, never past the top.
        assert_eq!(
            menu_origin((100.0, 300.0), (260.0, 650.0), window),
            (100.0, 42.0)
        );
        assert_eq!(
            menu_origin((100.0, 300.0), (260.0, 900.0), window),
            (100.0, MARGIN)
        );
    }
}
