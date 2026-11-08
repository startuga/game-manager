//! The Library page (the sidebar's Library, and the page the window opens on): every game as a
//! tile.
//!
//! - **Which games**: the sidebar's search and filter (`library::filter`). The pinned games come
//!   first under PINNED, in the order they were pinned; the others are grouped by store in the
//!   sidebar's order. A store folded in the sidebar is folded here too: both headers send
//!   `ToggleStore`. While searching every store shows open, and the headers can't fold.
//! - **A tile**: the game's icon and its name, with a pin when it is pinned. A click opens the
//!   game's page; a right-click (or the Menu key) its menu, as in the sidebar (`game_menu.rs`).
//!   Store artwork (covers) comes later, as its own change.
//! - **The grid**: as many tiles a row as the page's width holds (`library::columns`); each row's
//!   tiles are as tall as the row, so a long name doesn't make a ragged row.

use crate::game_menu::MenuMessage;
use crate::library;
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
        let total = library::total(&self.games, self.filter);
        let shown = if visible.len() == total {
            library::games_label(total)
        } else {
            format!("{} of {}", visible.len(), library::games_label(total))
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
            return col([header, hint(self.filter.empty_hint(&self.query))]).gap(16.0);
        }
        let columns = library::columns(self.page_width(window), TILE_WIDTH, TILE_GAP);
        let mut rows = vec![header];
        let grid = |rows: &mut Vec<El<Message>>, games: &[&Game]| {
            for line in games.chunks(columns) {
                rows.push(
                    row(line.iter().map(|game| self.tile(game)))
                        .gap(TILE_GAP)
                        .halign(Align::Start),
                );
            }
        };
        // Quick access: the pinned games first, in the order they were pinned.
        let (pinned, rest) = library::split_pinned(&self.games, &visible);
        if !pinned.is_empty() {
            rows.push(crate::sidebar::pinned_header(
                "library-pinned",
                pinned.len(),
            ));
            let games: Vec<&Game> = pinned.iter().map(|&i| &self.games[i]).collect();
            grid(&mut rows, &games);
        }
        for store in Store::ALL {
            let games: Vec<&Game> = rest
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
            if !folded {
                grid(&mut rows, &games);
            }
        }
        col(rows).gap(12.0)
    }

    /// A game's tile: its icon and name, and a pin when it is pinned; a click opens its page.
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
        if game.pinned.is_some() {
            parts.push(
                glyph(g::PIN, 12.0)
                    .color(theme::ACCENT)
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
            .on_context(move |at| MenuMessage::Open(id.clone(), at).into())
    }
}
