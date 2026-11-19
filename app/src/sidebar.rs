//! The library sidebar: search with the Sort and filters button beside it (`filter_menu.rs`), the
//! pages (My PC, then Library: the games grid), the games in the view's sections
//! (`library::sections`: pinned, then by store or in one list), and Add a game. ReShade's Shaders and Add-ons and the upscaler
//! versions are in the game page's tabs. The selected game's row is highlighted while its page
//! shows.
//!
//! Pinned games (the game menu's Pin to quick access) come first under a PINNED header, in the
//! order they were pinned, and leave their store's group. Store
//! groups fold with a click on their header (`widgets::fold_header`, as every folding header).
//! Each row is
//! keyed by its game's id, so hover, focus and the scroll position stay with the game when the
//! list changes; a right-click, or the Menu key on a focused row, opens the game's menu
//! (`game_menu.rs`) without leaving the page.
//!
//! **Search** (`widgets::search_box`; matching in `library.rs`): Ctrl+F goes to it, Escape
//! empties it, Enter opens the first game shown, ✕ clears it. While it has text, a line under it
//! says how many games match, and every store shows open (its header can't fold until the search
//! is cleared, then the folding the user chose comes back), here and on the Library page.

use gm_core::{Game, Store};

use crate::game_menu::MenuMessage;
use crate::library::{self, Section, SectionKind};
use crate::ui::el::{El, Role, col, glyph, row, scroll, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    count_badge, divider, fold_header, group_row, hint, nav_item, search_box, section_label,
    subtle_button, working,
};
use crate::{GameManager, Message, Page};

/// The search box's key, which Ctrl+F looks for (`App::search_box`).
pub(crate) const SEARCH_KEY: &str = "search";

impl GameManager {
    pub(crate) fn sidebar(&self) -> El<Message> {
        let search = search_box(
            SEARCH_KEY,
            &self.query,
            "Search games (Ctrl+F)",
            "Search games",
            Message::Search,
            Message::SearchSubmit,
            Message::ClearSearch,
        );
        let search = row([search.fill_width(), self.filter_button()]).gap(theme::space::S);
        let searching = !self.query.trim().is_empty();
        let pages = col([
            nav_item(
                g::PC,
                "My PC",
                self.page == Page::MyPc,
                Message::Page(Page::MyPc),
            ),
            nav_item(
                g::LIBRARY,
                "Library",
                self.page == Page::Library,
                Message::Page(Page::Library),
            ),
            nav_item(
                g::SETTINGS,
                "Settings",
                self.page == Page::Settings,
                Message::Page(Page::Settings),
            ),
        ])
        .gap(theme::space::XXS);
        let add = subtle_button(g::ADD, "Add a game")
            .enabled(self.can_add_game())
            .tooltip("Or drop a game's .exe or its shortcut on the window")
            .on_click(Message::AddGame);
        let mut items = vec![search];
        if searching {
            // How many the search and the filters leave, while searching.
            let shown = self.shown_games().len();
            let total = library::total(&self.games, &self.view);
            items.push(
                hint(match shown {
                    0 => "No game matches".to_owned(),
                    n => format!("{n} of {}", library::games_label(total)),
                })
                .padding(crate::ui::el::Edges::new(
                    theme::space::XS,
                    0.0,
                    0.0,
                    0.0,
                )),
            );
        }
        items.extend([
            pages,
            divider(),
            self.game_list().fill_height(),
            divider(),
            add,
        ]);
        col(items)
            .gap(theme::space::S)
            .pad(theme::space::S)
            .width(self.sidebar_width)
            .fill_height()
            .bg(theme::layer())
    }

    fn game_list(&self) -> El<Message> {
        if self.games.is_empty() {
            return col([if self.scanning {
                working("Looking for installed games…")
            } else {
                hint("No games found yet. Add one, or drop its .exe or shortcut here.")
            }])
            .pad(theme::space::S);
        }
        let shown = self.shown_games();
        if shown.is_empty() {
            return col([hint(library::empty_hint(&self.view, &self.query))]).pad(theme::space::S);
        }
        let mut rows = Vec::new();
        for section in library::sections(&self.games, &shown, &self.view, self.facts()) {
            let (header, folded) = self.section_header("", &section);
            rows.push(header);
            if !folded {
                rows.extend(
                    section
                        .games
                        .iter()
                        .map(|&index| self.game_row(&self.games[index])),
                );
            }
        }
        scroll("games", col(rows).gap(theme::space::XXS))
    }

    /// A section's header, and whether the section is folded: PINNED, a store's (which folds,
    /// except while searching: a search shows every store open), or GAMES for the one list of
    /// Group by store off. `prefix` keeps the sidebar's and the Library page's headers apart.
    pub(crate) fn section_header(&self, prefix: &str, section: &Section) -> (El<Message>, bool) {
        let count = section.games.len();
        match section.kind {
            SectionKind::Pinned => (pinned_header(&format!("{prefix}pinned"), count), false),
            SectionKind::Store(store) => {
                let searching = !self.query.trim().is_empty();
                let folded = !searching && self.folded.contains(&store);
                let key = format!("{prefix}store");
                (store_header(&key, store, count, folded, !searching), folded)
            }
            SectionKind::All => (games_header(&format!("{prefix}all"), count), false),
        }
    }

    fn game_row(&self, game: &Game) -> El<Message> {
        let selected =
            self.page == Page::Game && self.selected.as_deref() == Some(game.id.as_str());
        let id = game.id.clone();
        row([
            self.icon(game, crate::icons::SMALL),
            text(game.name.clone()).fill_width(),
        ])
        .gap(theme::space::S)
        .pad_xy(theme::space::S, 0.0)
        .height(36.0)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::subtle_hover())
        .pressed_bg(theme::subtle_pressed())
        .role(crate::ui::el::Role::ListItem)
        .selected(selected)
        .key(format!("game:{}", game.id))
        .name(game.name.clone())
        .on_click(Message::Select(Some(game.id.clone())))
        .on_context(move |at| MenuMessage::Open(id.clone(), at).into())
    }
}

/// A store's group header: its name, how many games, and a chevron that folds it. `key` keeps
/// the sidebar's and the Library page's headers apart. While searching it can't fold
/// (`foldable` false): no chevron, no click.
pub(crate) fn store_header(
    key: &str,
    store: Store,
    count: usize,
    folded: bool,
    foldable: bool,
) -> El<Message> {
    let key = format!("{key}:{}", store.name());
    if !foldable {
        return group_header(glyph("", 9.0), store.name(), count)
            .key(key)
            .role(Role::Group)
            .name(format!("{} games", store.name()));
    }
    // The shared fold header (`widgets::fold_header`), as every folding header is.
    fold_header(
        !folded,
        section_label(store.name()),
        Some(count_badge(count)),
        Message::ToggleStore(store),
    )
    .key(key)
    .name(format!(
        "{} games, {}",
        store.name(),
        if folded { "folded" } else { "unfolded" }
    ))
}

/// The pinned games' header (quick access): a pin, PINNED and how many. It doesn't fold.
pub(crate) fn pinned_header(key: &str, count: usize) -> El<Message> {
    let pin = glyph(g::PIN, 11.0).color(theme::accent());
    group_header(pin, "Pinned", count)
        .key(format!("{key}:pinned"))
        .role(Role::Group)
        .name("Pinned games")
}

/// The header of the one list that Group by store off shows: GAMES and how many. It doesn't
/// fold.
pub(crate) fn games_header(key: &str, count: usize) -> El<Message> {
    group_header(glyph("", 9.0), "Games", count)
        .key(format!("{key}:games"))
        .role(Role::Group)
        .name("Games")
}

/// A group's header that doesn't fold: a glyph in the fold header's slot (a pin, or none), its
/// label in capitals and a count badge (`widgets::group_row`).
fn group_header(lead: El<Message>, label: &str, count: usize) -> El<Message> {
    group_row(lead, section_label(label), Some(count_badge(count)))
}
