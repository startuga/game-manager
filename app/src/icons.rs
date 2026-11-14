//! Game icons for the window. The core reads each icon's best image as it is in the game's files
//! (`gm_core::icon`); here it is decoded for the two sizes the window draws icons at, in pixels at
//! the window's display scaling, so each is drawn pixel for pixel:
//! - `SMALL`: the sidebar's rows and the game menu's header;
//! - `LARGE`: a game's page and a Library tile without a cover.
//!
//! An image of exactly that many pixels shows untouched; any other is scaled once, from that best
//! image (`ui::image::decode_icons`). The icons' bytes are kept, so a new
//! scaling (another monitor, a new setting) decodes them again without reading any file; a decode
//! made for a scaling that has changed since is dropped.

use std::collections::HashMap;
use std::sync::Arc;

use gm_core::Game;

use crate::shell::ctx::Ctx;
use crate::ui::el::{El, TextAlign, glyph, image};
use crate::ui::image::{Picture, decode_icons};
use crate::ui::theme::{self, glyph as g};
use crate::{GameManager, Message};

/// The sizes icons are drawn at, in DIPs.
pub(crate) const SMALL: f32 = 24.0;
pub(crate) const LARGE: f32 = 64.0;

/// One icon decoded at both sizes.
type Pictures = [Arc<Picture>; 2];

#[derive(Clone)]
pub(crate) enum IconMessage {
    /// Icons read and decoded at a scaling (its bits), with their bytes.
    Loaded(u32, Vec<(String, Arc<Vec<u8>>, Pictures)>),
    /// Icons decoded again for a new scaling (its bits).
    Decoded(u32, Vec<(String, Pictures)>),
}

impl From<IconMessage> for Message {
    fn from(message: IconMessage) -> Self {
        Message::Icon(message)
    }
}

/// The icons' bytes and pictures, by game id.
#[derive(Default)]
pub(crate) struct IconState {
    sources: HashMap<String, Arc<Vec<u8>>>,
    pictures: HashMap<String, Pictures>,
}

impl IconState {
    /// A game that is gone.
    pub fn forget(&mut self, id: &str) {
        self.sources.remove(id);
        self.pictures.remove(id);
    }
}

/// The sizes icons are drawn at, in pixels at `scale`.
fn pixel_sizes(scale: f32) -> [u32; 2] {
    [SMALL, LARGE].map(|size| ((size * scale).round() as u32).max(1))
}

/// Decodes icons at both sizes for `scale`.
fn decode(items: Vec<(String, Arc<Vec<u8>>)>, scale: f32) -> Vec<(String, Pictures)> {
    decode_icons(items, pixel_sizes(scale))
        .into_iter()
        .map(|(id, [small, large])| (id, [Arc::new(small), Arc::new(large)]))
        .collect()
}

impl GameManager {
    /// Reads the games' icons after the list is shown, so a large library appears without
    /// waiting, and decodes them there too. `prune` (full library only) also drops cached icons
    /// no game uses any more.
    pub(crate) fn load_icons(games: Vec<Game>, prune: bool, context: &Ctx<Message>) {
        let scale = context.scale();
        context.spawn_background(move || {
            let read = if prune {
                gm_core::icon::refresh_all(&games)
            } else {
                gm_core::icon::extract_all(&games)
            };
            let sources: Vec<(String, Arc<Vec<u8>>)> = read
                .into_iter()
                .map(|(id, bytes)| (id, Arc::new(bytes)))
                .collect();
            let decoded: HashMap<String, Pictures> =
                decode(sources.clone(), scale).into_iter().collect();
            let loaded = sources
                .into_iter()
                .filter_map(|(id, bytes)| {
                    let pictures = decoded.get(&id)?.clone();
                    Some((id, bytes, pictures))
                })
                .collect();
            IconMessage::Loaded(scale.to_bits(), loaded).into()
        });
    }

    /// Decodes every icon again for the window's new scaling, from the bytes kept.
    pub(crate) fn redecode_icons(&self, context: &Ctx<Message>) {
        let scale = context.scale();
        let sources: Vec<(String, Arc<Vec<u8>>)> = self
            .icons
            .sources
            .iter()
            .map(|(id, bytes)| (id.clone(), Arc::clone(bytes)))
            .collect();
        if sources.is_empty() {
            return;
        }
        context.spawn_background(move || {
            IconMessage::Decoded(scale.to_bits(), decode(sources, scale)).into()
        });
    }

    pub(crate) fn handle_icon(&mut self, message: IconMessage, context: &Ctx<Message>) {
        let current = context.scale().to_bits();
        match message {
            IconMessage::Loaded(scale, icons) => {
                for (id, bytes, pictures) in icons {
                    self.icons.sources.insert(id.clone(), bytes);
                    self.icons.pictures.insert(id, pictures);
                }
                // The scaling changed while they were read.
                if scale != current {
                    self.redecode_icons(context);
                }
            }
            IconMessage::Decoded(scale, icons) if scale == current => {
                self.icons.pictures.extend(icons);
            }
            IconMessage::Decoded(..) => {}
        }
    }

    /// A game's icon `size` DIPs big (`SMALL` or `LARGE`), or the game glyph while it loads or
    /// when it has none.
    pub(crate) fn icon(&self, game: &Game, size: f32) -> El<Message> {
        let large = usize::from(size > SMALL);
        match self.icons.pictures.get(&game.id) {
            Some(pictures) => image(
                format!("icon{large}:{}", game.id),
                Arc::clone(&pictures[large]),
            )
            .size(size, size),
            None => glyph(g::GAME, size * 0.6)
                .color(theme::TEXT_TERTIARY)
                .text_align(TextAlign::Center)
                .size(size, size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_made_for_the_pixels_they_take() {
        assert_eq!(pixel_sizes(1.0), [24, 64]);
        assert_eq!(pixel_sizes(1.5), [36, 96]);
        assert_eq!(pixel_sizes(2.25), [54, 144]);
        assert_eq!(pixel_sizes(1.25), [30, 80]);
    }
}
