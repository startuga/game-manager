//! The library sidebar: search and the filter, the pages (My PC, then Library: the games grid),
//! the games grouped by store, and Add a game. ReShade's Shaders and Add-ons and the upscaler
//! versions are in the game page's tabs. The selected game's row is highlighted while its page
//! shows.
//!
//! Store groups fold with a click on their header. Each row is
//! keyed by its game's id, so hover, focus and the scroll position stay with the game when the
//! list changes; a right-click opens the game's menu (`app_view.rs`).
//!
//! **Search** (`widgets::search_box`; matching in `library.rs`): Ctrl+F goes to it, Escape
//! empties it, Enter opens the first game shown, ✕ clears it. While it has text, a line under it
//! says how many games match, and every store shows open (its header can't fold until the search
//! is cleared, then the folding the user chose comes back), here and on the Library page.

use gm_core::{Game, Store};

use crate::library::{self, LibraryFilter};
use crate::ui::el::{El, TextAlign, col, dropdown, glyph, image, row, scroll, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{divider, hint, nav_item, search_box, subtle_button, working};
use crate::{GameManager, Message, Page};

/// The search box's key, which Ctrl+F looks for (`App::search_box`).
pub(crate) const SEARCH_KEY: &str = "search";

impl GameManager {
    pub(crate) fn sidebar(&self) -> El<Message> {
        let search = search_box(
            SEARCH_KEY,
            &self.query,
            "Search games (Ctrl+F)",
            Message::Search,
            Message::SearchSubmit,
            Message::ClearSearch,
        );
        let searching = !self.query.trim().is_empty();
        let options = library::filter_options(&self.games);
        let filters: Vec<LibraryFilter> = options.iter().map(|(filter, _)| *filter).collect();
        let index = filters.iter().position(|filter| *filter == self.filter);
        let labels = options.into_iter().map(|(_, label)| label).collect();
        let filter = dropdown("filter", labels, index, move |i| {
            Message::Filter(filters.get(i).copied())
        })
        .name("Filter");
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
        ])
        .gap(2.0);
        let add = subtle_button(g::ADD, "Add a game")
            .enabled(self.can_add_game())
            .tooltip("Or drop a game's .exe or its shortcut on the window")
            .on_click(Message::AddGame);
        let mut items = vec![search];
        if searching {
            // How many the search and the filter leave, while searching.
            let shown = library::filter(&self.games, &self.query, self.filter).len();
            items.push(
                text(match shown {
                    0 => "No game matches".to_owned(),
                    1 => format!("1 of {} games", self.games.len()),
                    n => format!("{n} of {} games", self.games.len()),
                })
                .text_size(theme::TEXT_SMALL)
                .color(theme::TEXT_TERTIARY)
                .padding(crate::ui::el::Edges::new(4.0, 0.0, 0.0, 0.0)),
            );
        }
        items.extend([
            filter,
            pages,
            divider(),
            self.game_list().fill_height(),
            divider(),
            add,
        ]);
        col(items)
            .gap(8.0)
            .pad(10.0)
            .width(self.sidebar_width)
            .fill_height()
            .bg(theme::LAYER)
    }

    fn game_list(&self) -> El<Message> {
        if self.games.is_empty() {
            return col([if self.scanning {
                working("Looking for installed games…")
            } else {
                hint("No games found yet. Add one, or drop its .exe or shortcut here.")
            }])
            .pad(8.0);
        }
        let visible = library::filter(&self.games, &self.query, self.filter);
        if visible.is_empty() {
            let message = if self.filter == LibraryFilter::Favorites && self.query.trim().is_empty()
            {
                "No favorite games yet. Right-click a game and choose Add to Favorites."
            } else {
                "No games match the search."
            };
            return col([hint(message)]).pad(8.0);
        }
        let mut rows = Vec::new();
        let searching = !self.query.trim().is_empty();
        for store in Store::ALL {
            let games: Vec<&Game> = visible
                .iter()
                .map(|&i| &self.games[i])
                .filter(|game| game.store == store)
                .collect();
            if games.is_empty() {
                continue;
            }
            // A search shows every store open.
            let folded = !searching && self.folded.contains(&store);
            rows.push(store_header(
                "store",
                store,
                games.len(),
                folded,
                !searching,
            ));
            if !folded {
                rows.extend(games.into_iter().map(|game| self.game_row(game)));
            }
        }
        scroll("games", col(rows).gap(1.0))
    }

    fn game_row(&self, game: &Game) -> El<Message> {
        let selected =
            self.page == Page::Game && self.selected.as_deref() == Some(game.id.as_str());
        let id = game.id.clone();
        let mut parts = vec![self.icon(game, 24.0), text(game.name.clone()).fill_width()];
        if game.favorite {
            parts.push(glyph(g::STAR_FILLED, 11.0).color(theme::GOLD));
        }
        row(parts)
            .gap(10.0)
            .pad_xy(8.0, 0.0)
            .height(36.0)
            .radius(theme::RADIUS_CONTROL)
            .hover_bg(theme::SUBTLE_HOVER)
            .pressed_bg(theme::SUBTLE_PRESSED)
            .role(crate::ui::el::Role::ListItem)
            .selected(selected)
            .key(format!("game:{}", game.id))
            .name(game.name.clone())
            .on_click(Message::Select(Some(game.id.clone())))
            .on_context(move |at| Message::RowContext(id.clone(), at))
    }

    /// A game's icon, or the game glyph while it loads or when it has none.
    pub(crate) fn icon(&self, game: &Game, size: f32) -> El<Message> {
        match self.icons.get(&game.id) {
            Some(picture) => image(format!("icon:{}", game.id), picture.clone()).size(size, size),
            None => glyph(g::GAME, size * 0.6)
                .color(theme::TEXT_TERTIARY)
                .text_align(TextAlign::Center)
                .size(size, size),
        }
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
    let chevron = if folded {
        g::CHEVRON_RIGHT
    } else {
        g::CHEVRON_DOWN
    };
    let header = row([
        glyph(if foldable { chevron } else { "" }, 9.0)
            .color(theme::TEXT_TERTIARY)
            .width(12.0),
        text(store.name().to_uppercase())
            .text_size(theme::TEXT_CAPTION)
            .weight(700)
            .color(theme::TEXT_SECONDARY),
        row([text(count.to_string())
            .text_size(theme::TEXT_CAPTION)
            .color(theme::TEXT_TERTIARY)])
        .pad_xy(6.0, 1.0)
        .bg(theme::CONTROL)
        .radius(8.0),
    ])
    .gap(8.0)
    .pad_xy(6.0, 0.0)
    .height(30.0)
    .radius(theme::RADIUS_CONTROL)
    .key(format!("{key}:{}", store.name()));
    if !foldable {
        return header.name(format!("{} games", store.name()));
    }
    header
        .hover_bg(theme::SUBTLE_HOVER)
        .name(format!(
            "{} games, {}",
            store.name(),
            if folded { "folded" } else { "unfolded" }
        ))
        .on_click(Message::ToggleStore(store))
}
