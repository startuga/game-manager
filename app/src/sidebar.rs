//! The library sidebar: search and the filter, the pages (Library, Shaders, Add-ons, Upscalers),
//! the games grouped by store, and Add a game.
//!
//! Store groups fold with a click on their header. Each row is
//! keyed by its game's id, so hover, focus and the scroll position stay with the game when the
//! list changes; a right-click opens the game's menu (`app_view.rs`).

use gm_core::{Game, Store};

use crate::library::{self, LibraryFilter};
use crate::ui::el::{El, TextAlign, col, dropdown, glyph, image, row, scroll, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{divider, hint, nav_item, subtle_button, text_box, working};
use crate::{GameManager, Message, Page};

impl GameManager {
    pub(crate) fn sidebar(&self) -> El<Message> {
        let search = text_box(
            "search",
            self.query.clone(),
            "Search games",
            "Search games",
            Message::Search,
        );
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
                g::LIBRARY,
                "Library",
                self.page == Page::Game,
                Message::Page(Page::Game),
            ),
            nav_item(
                g::BRUSH,
                "Shaders",
                self.page == Page::Shaders,
                Message::Page(Page::Shaders),
            ),
            nav_item(
                g::PUZZLE,
                "Add-ons",
                self.page == Page::Addons,
                Message::Page(Page::Addons),
            ),
            nav_item(
                g::CHIP,
                "Upscalers",
                self.page == Page::Upscalers,
                Message::Page(Page::Upscalers),
            ),
        ])
        .gap(2.0);
        let add = subtle_button(g::ADD, "Add a game")
            .enabled(self.can_add_game())
            .tooltip("Or drop a game's .exe or its shortcut on the window")
            .on_click(Message::AddGame);
        col([
            search,
            filter,
            pages,
            divider(),
            self.game_list().fill_height(),
            divider(),
            add,
        ])
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
        for store in Store::ALL {
            let games: Vec<&Game> = visible
                .iter()
                .map(|&i| &self.games[i])
                .filter(|game| game.store == store)
                .collect();
            if games.is_empty() {
                continue;
            }
            let folded = self.folded.contains(&store);
            rows.push(store_header(store, games.len(), folded));
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

/// A store's group header: its name, how many games, and a chevron that folds it.
fn store_header(store: Store, count: usize, folded: bool) -> El<Message> {
    let chevron = if folded {
        g::CHEVRON_RIGHT
    } else {
        g::CHEVRON_DOWN
    };
    row([
        glyph(chevron, 9.0).color(theme::TEXT_TERTIARY).width(12.0),
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
    .hover_bg(theme::SUBTLE_HOVER)
    .key(format!("store:{}", store.name()))
    .name(format!(
        "{} games, {}",
        store.name(),
        if folded { "folded" } else { "unfolded" }
    ))
    .on_click(Message::ToggleStore(store))
}
