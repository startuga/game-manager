//! The Library page (the sidebar's Library, and the page the window opens on): every game as a
//! tile.
//!
//! - **Which games**: the sidebar's search and its Sort and filters (`library::shown`), in the
//!   sidebar's sections (`library::sections`): the pinned games first under PINNED, in the order
//!   they were pinned; the others by store, or in one list, in the view's order. A store folded
//!   in the sidebar is folded here too: both headers send `ToggleStore`. While searching every
//!   store shows open, and the headers can't fold. The caption over the count names the filters
//!   or the collection that is on ("LIBRARY · STEAM, GOG").
//! - **A tile**: the game's 200 × 300 cover with rounded corners (`covers.rs`), or its icon on a
//!   card while it has none, a pin on a dark chip when it is pinned, and its name on one line
//!   under it (whole in the tooltip). Under the pointer a light tint goes over all of it
//!   (`hover_overlay`: the picture would hide a fill). A click opens the game's page; a
//!   right-click (or the Menu key) its menu, as in the sidebar (`game_menu.rs`).
//! - **The grid**: as many tiles a row as the page's width holds (`library::columns`): 4 at the
//!   window's first size, more when it is wider.

use crate::covers::{COVER_HEIGHT, COVER_WIDTH};
use crate::game_menu::MenuMessage;
use crate::library;
use crate::shell::WindowInfo;
use crate::ui::el::{Align, Edges, El, col, glyph, image, layers, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{heading, hint, working};
use crate::{GameManager, Message};
use gm_core::Game;

/// The space between tiles: 4 fit across at the window's first size.
const TILE_GAP: f32 = 16.0;
/// The tint over a tile under the pointer, and the chip behind a pinned tile's pin.
const TILE_HOVER: windows_canvas::ColorF = theme::argb(0x1FFF_FFFF);
const PIN_CHIP: windows_canvas::ColorF = theme::argb(0xC717_181C);

impl GameManager {
    pub(crate) fn library_page(&self, window: &WindowInfo) -> El<Message> {
        let visible = self.shown_games();
        let total = library::total(&self.games, &self.view);
        let shown = if visible.len() == total {
            library::games_label(total)
        } else {
            format!("{} of {}", visible.len(), library::games_label(total))
        };
        let header = col([
            text(match library::describe(&self.view) {
                Some(what) => format!("LIBRARY · {}", what.to_uppercase()),
                None => "LIBRARY".to_owned(),
            })
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
            let nothing = library::empty_hint(&self.view, &self.query);
            return col([header, hint(nothing)]).gap(16.0);
        }
        let columns = library::columns(self.page_width(window), COVER_WIDTH, TILE_GAP);
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
        // The sidebar's sections, folded as there.
        for section in library::sections(&self.games, &visible, &self.view) {
            let (header, folded) = self.section_header("library-", &section);
            rows.push(header);
            if !folded {
                let games: Vec<&Game> = section.games.iter().map(|&i| &self.games[i]).collect();
                grid(&mut rows, &games);
            }
        }
        col(rows).gap(12.0)
    }

    /// A game's tile: its cover (or its icon on a card until it has one), a pin on a dark chip
    /// when it is pinned, and its name on one line under it. A click opens its page.
    fn tile(&self, game: &Game) -> El<Message> {
        let id = game.id.clone();
        // Decoded while the tile is near the screen (`.watch()`), else its icon meanwhile.
        let art = match self.covers.picture(&game.id) {
            Some(picture) => image(format!("cover:{}", game.id), picture.clone())
                .cover()
                .radius(theme::RADIUS_CARD),
            None => row([self.icon(game, crate::icons::LARGE)])
                .justify(Align::Center)
                .bg(theme::CARD)
                .stroke(theme::CARD_STROKE, 1.0)
                .radius(theme::RADIUS_CARD),
        };
        let mut cover = vec![art.size(COVER_WIDTH, COVER_HEIGHT)];
        if game.pinned.is_some() {
            let chip = row([glyph(g::PIN, 11.0).color(theme::ACCENT)])
                .pad(6.0)
                .bg(PIN_CHIP)
                .radius(6.0);
            cover.push(col([chip]).pad(8.0).halign(Align::End).valign(Align::Start));
        }
        col([
            layers(cover).size(COVER_WIDTH, COVER_HEIGHT),
            // One line, cut with "…"; the tooltip has it whole.
            text(game.name.clone()).padding(Edges::new(4.0, 0.0, 4.0, 0.0)),
        ])
        .gap(8.0)
        .width(COVER_WIDTH)
        .radius(theme::RADIUS_CARD)
        .hover_overlay(TILE_HOVER)
        .role(crate::ui::el::Role::ListItem)
        .key(format!("{}{}", crate::covers::TILE_KEY, game.id))
        .watch()
        .name(game.name.clone())
        .tooltip(game.name.clone())
        .on_click(Message::Select(Some(game.id.clone())))
        .on_context(move |at| MenuMessage::Open(id.clone(), at).into())
    }
}
