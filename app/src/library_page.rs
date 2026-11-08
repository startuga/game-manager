//! The Library page (the sidebar's Library, and the page the window opens on): every game as a
//! tile.
//!
//! - **Which games**: the sidebar's search and filter (`library::filter`), grouped by store in the
//!   sidebar's order. A store folded in the sidebar is folded here too: both headers send
//!   `ToggleStore`. While searching every store shows open, and the headers can't fold.
//! - **A tile**: the game's icon and its name, with a gold star for a favorite. A click opens the
//!   game's page; a right-click its menu, as in the sidebar. Store artwork (covers) comes later,
//!   as its own change.
//! - **The grid**: as many tiles a row as the page's width holds (`library::columns`); each row's
//!   tiles are as tall as the row, so a long name doesn't make a ragged row.

use crate::library::{self, LibraryFilter};
use crate::shell::WindowInfo;
use crate::ui::el::{Align, El, TextAlign, col, glyph, layers, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{heading, hint, working};
use crate::{GameManager, Message};
use gm_core::{Game, Store};

/// A tile's width, and the space between tiles.
const TILE_WIDTH: f32 = 140.0;
const TILE_GAP: f32 = 12.0;

impl GameManager {
    pub(crate) fn library_page(&self, window: &WindowInfo) -> El<Message> {
        let visible = library::filter(&self.games, &self.query, self.filter);
        let shown = if visible.len() == self.games.len() {
            format!("{} games", self.games.len())
        } else {
            format!("{} of {} games", visible.len(), self.games.len())
        };
        let header = col([
            text("LIBRARY")
                .text_size(theme::TEXT_CAPTION)
                .weight(700)
                .color(theme::TEXT_TERTIARY),
            heading(shown),
        ])
        .gap(4.0);
        if self.games.is_empty() {
            return col([
                header,
                if self.scanning {
                    working("Looking for installed games…")
                } else {
                    hint("No games found yet. Add one, or drop its .exe or shortcut here.")
                },
            ])
            .gap(16.0);
        }
        if visible.is_empty() {
            let message = if self.filter == LibraryFilter::Favorites && self.query.trim().is_empty()
            {
                "No favorite games yet. Right-click a game and choose Add to Favorites."
            } else {
                "No games match the search."
            };
            return col([header, hint(message)]).gap(16.0);
        }
        let columns = library::columns(self.page_width(window), TILE_WIDTH, TILE_GAP);
        let mut rows = vec![header];
        for store in Store::ALL {
            let games: Vec<&Game> = visible
                .iter()
                .map(|&i| &self.games[i])
                .filter(|game| game.store == store)
                .collect();
            if games.is_empty() {
                continue;
            }
            // A search shows every store open, as in the sidebar.
            let searching = !self.query.trim().is_empty();
            let folded = !searching && self.folded.contains(&store);
            rows.push(crate::sidebar::store_header(
                "library-store",
                store,
                games.len(),
                folded,
                !searching,
            ));
            if folded {
                continue;
            }
            for line in games.chunks(columns) {
                rows.push(
                    row(line.iter().map(|game| self.tile(game)))
                        .gap(TILE_GAP)
                        .halign(Align::Start),
                );
            }
        }
        col(rows).gap(12.0)
    }

    /// A game's tile: its icon and name; a click opens its page.
    fn tile(&self, game: &Game) -> El<Message> {
        let id = game.id.clone();
        let body = col([
            row([self.icon(game, 64.0)]).justify(Align::Center),
            text(game.name.clone())
                .wrap()
                .text_align(TextAlign::Center)
                .fill_width(),
        ])
        .gap(10.0)
        .padding(crate::ui::el::Edges::new(10.0, 16.0, 10.0, 12.0));
        let mut parts = vec![body];
        if game.favorite {
            parts.push(
                glyph(g::STAR_FILLED, 12.0)
                    .color(theme::GOLD)
                    .padding(crate::ui::el::Edges::all(8.0))
                    .halign(Align::End)
                    .valign(Align::Start),
            );
        }
        layers(parts)
            .width(TILE_WIDTH)
            .fill_height()
            .bg(theme::CARD)
            .hover_bg(theme::CONTROL_HOVER)
            .pressed_bg(theme::CONTROL_PRESSED)
            .stroke(theme::CARD_STROKE, 1.0)
            .radius(theme::RADIUS_CARD)
            .role(crate::ui::el::Role::ListItem)
            .key(format!("tile:{}", game.id))
            .name(game.name.clone())
            .on_click(Message::Select(Some(game.id.clone())))
            .on_context(move |at| Message::RowContext(id.clone(), at))
    }
}
