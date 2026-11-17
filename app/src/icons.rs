//! Game icons for the window. The core reads each icon's best image as it is in the game's files
//! (`gm_core::icon`); here it is decoded for the two sizes the window draws icons at, in pixels at
//! the window's display scaling, so each is drawn pixel for pixel:
//! - `SMALL`: the sidebar's rows and the game menu's header;
//! - `LARGE`: a game's page and a Library tile without a cover.
//!
//! An image of exactly that many pixels shows untouched; any other is scaled once, from that best
//! image (`ui::image::decode_icon`).
//!
//! **What is kept**, so a large library doesn't hold every icon's file (about 100 KB each) and
//! both sizes of it (about 90 KB at 225%):
//! - every game's `SMALL` picture, read for all games after the list shows (`load_icons`; the core
//!   hands the icons over one at a time, so not even loading holds all their files);
//! - for the games that want more (`want_icons`, after every message: the tiles near the screen,
//!   the selected game and the Artwork dialog's), the icon's bytes, which make capsules
//!   (`covers.rs`), blur the page's band (`page_art.rs`) and show in the dialog, and its `LARGE`
//!   picture. They are read from the icon cache when a game comes to want them and dropped when it
//!   no longer does; until they are there, a `LARGE` place shows the `SMALL` picture scaled up.
//!
//! When icons are read anew (a scan, an icon chosen) what is kept for those games is read again,
//! and shown until then; the same bytes read again keep the old copy, so nothing made from them
//! (the band's blur) is made again. A new scaling reads and decodes every icon again; what a
//! slower read found for an older scaling, or before the icons were read anew, is dropped.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gm_core::Game;

use crate::shell::ctx::Ctx;
use crate::ui::el::{El, TextAlign, glyph, image};
use crate::ui::image::{Picture, decode_icon};
use crate::ui::theme::{self, glyph as g};
use crate::{GameManager, Message};

/// The sizes icons are drawn at, in DIPs.
pub(crate) const SMALL: f32 = 24.0;
pub(crate) const LARGE: f32 = 64.0;

/// A wanted game's icon kept whole: its bytes and its `LARGE` picture.
type Kept = (Arc<Vec<u8>>, Arc<Picture>);

#[derive(Clone)]
pub(crate) enum IconMessage {
    /// Games' icons read anew at a scaling (its bits): their `SMALL` pictures.
    Loaded(u32, Vec<(String, Arc<Picture>)>),
    /// Wanted games' icons read for the icons' generation: each one's bytes and `LARGE` picture,
    /// or `None` when it has none that can be read.
    Kept(u64, Vec<(String, Option<Kept>)>),
}

impl From<IconMessage> for Message {
    fn from(message: IconMessage) -> Self {
        Message::Icon(message)
    }
}

/// The games' icons, by game id (see the module's doc).
#[derive(Default)]
pub(crate) struct IconState {
    /// Every game's icon at `SMALL`.
    small: HashMap<String, Arc<Picture>>,
    /// The wanted games' icons, whole.
    kept: HashMap<String, Kept>,
    /// Kept icons read anew since: shown until their new read arrives.
    stale: HashSet<String>,
    /// Wanted games whose icon is being read.
    reading: HashSet<String>,
    /// Wanted games whose icon couldn't be read whole: not asked for again until it is read anew.
    unreadable: HashSet<String>,
    /// Counts the times icons were read anew or rescaled: a read asked for before is dropped.
    generation: u64,
}

impl IconState {
    /// The bytes of a game's icon, as the core read them, while the game wants them (a tile near
    /// the screen makes a capsule of them, the page's band blurs them, the Artwork dialog shows
    /// them).
    pub fn bytes(&self, id: &str) -> Option<&Arc<Vec<u8>>> {
        self.kept.get(id).map(|(bytes, _)| bytes)
    }

    /// A game that is gone.
    pub fn forget(&mut self, id: &str) {
        self.small.remove(id);
        self.kept.remove(id);
        self.stale.remove(id);
        self.unreadable.remove(id);
    }

    /// Icons read anew: their `SMALL` pictures replace the old, and what is kept for those games
    /// is read again (the icon may have changed), shown until then.
    fn loaded(&mut self, icons: Vec<(String, Arc<Picture>)>) {
        self.generation += 1;
        for (id, picture) in icons {
            if self.kept.contains_key(&id) {
                self.stale.insert(id.clone());
            }
            self.unreadable.remove(&id);
            self.small.insert(id, picture);
        }
    }

    /// A new scaling: the `LARGE` pictures kept are for the old one, and reads asked for before
    /// are dropped.
    fn rescaled(&mut self) {
        self.generation += 1;
        self.kept.clear();
        self.stale.clear();
    }

    /// Of `wanted`, the games whose icon to read now, marked as being read; what is kept for games
    /// no longer wanted goes. Only games with an icon (a `SMALL` picture) are read.
    fn start_reading(&mut self, wanted: &HashSet<String>) -> HashSet<String> {
        self.kept.retain(|id, _| wanted.contains(id));
        self.stale.retain(|id| wanted.contains(id));
        let ids: HashSet<String> = wanted
            .iter()
            .filter(|id| {
                self.small.contains_key(*id)
                    && (!self.kept.contains_key(*id) || self.stale.contains(*id))
                    && !self.reading.contains(*id)
                    && !self.unreadable.contains(*id)
            })
            .cloned()
            .collect();
        self.reading.extend(ids.iter().cloned());
        ids
    }

    /// Wanted icons read for `generation`: kept, unless the icons were read anew or rescaled since
    /// (then they are asked for again). The same bytes as those kept keep the kept copy. Whether
    /// any icon was kept.
    fn kept_read(&mut self, generation: u64, icons: Vec<(String, Option<Kept>)>) -> bool {
        for (id, _) in &icons {
            self.reading.remove(id);
        }
        if generation != self.generation {
            return false;
        }
        let mut any = false;
        for (id, read) in icons {
            self.stale.remove(&id);
            let Some((bytes, large)) = read else {
                self.kept.remove(&id);
                self.unreadable.insert(id);
                continue;
            };
            let bytes = match self.kept.get(&id) {
                Some((old, _)) if **old == *bytes => Arc::clone(old),
                _ => bytes,
            };
            self.kept.insert(id, (bytes, large));
            any = true;
        }
        any
    }

    /// A game's icon to draw `large` or not: its `LARGE` picture while it is kept, else its
    /// `SMALL` one (scaled up in a large place). The bool says which.
    fn picture(&self, id: &str, large: bool) -> Option<(&Arc<Picture>, bool)> {
        let kept = large
            .then(|| self.kept.get(id))
            .flatten()
            .map(|(_, picture)| (picture, true));
        kept.or_else(|| self.small.get(id).map(|picture| (picture, false)))
    }
}

/// The sizes icons are drawn at, in pixels at `scale`.
fn pixel_sizes(scale: f32) -> [u32; 2] {
    [SMALL, LARGE].map(|size| ((size * scale).round() as u32).max(1))
}

impl GameManager {
    /// Reads `games`' icons after the list is shown, so a large library appears without waiting,
    /// and decodes their `SMALL` pictures there. `prune` (full library only) also drops cached
    /// icons no game uses any more.
    pub(crate) fn load_icons(games: Vec<Game>, prune: bool, context: &Ctx<Message>) {
        let scale = context.scale();
        let [size, _] = pixel_sizes(scale);
        context.spawn_background(move || {
            let mut small = Vec::new();
            gm_core::icon::each(&games, prune, |id, bytes| {
                if let Some(picture) = decode_icon(&bytes, size) {
                    small.push((id, Arc::new(picture)));
                }
            });
            IconMessage::Loaded(scale.to_bits(), small).into()
        });
    }

    /// The window's scaling changed: every icon is read and decoded again for it, the old
    /// pictures shown meanwhile.
    pub(crate) fn rescale_icons(&mut self, context: &Ctx<Message>) {
        self.icons.rescaled();
        Self::load_icons(self.games.clone(), false, context);
    }

    /// Keeps the wanted games' icons whole (see the module's doc): the tiles near the screen, the
    /// selected game and the Artwork dialog's. After every message; reads only what is missing.
    pub(crate) fn want_icons(&mut self, context: &Ctx<Message>) {
        let mut wanted = self.covers.near().clone();
        wanted.extend(self.selected.clone());
        wanted.extend(self.artwork.game().map(str::to_owned));
        let ids = self.icons.start_reading(&wanted);
        if ids.is_empty() {
            return;
        }
        let games: Vec<Game> = self
            .games
            .iter()
            .filter(|game| ids.contains(&game.id))
            .cloned()
            .collect();
        let generation = self.icons.generation;
        let [_, size] = pixel_sizes(context.scale());
        context.spawn_background(move || {
            let mut read = Vec::new();
            gm_core::icon::each(&games, false, |id, bytes| {
                let large = decode_icon(&bytes, size).map(Arc::new);
                read.push((id, large.map(|large| (Arc::new(bytes), large))));
            });
            // A game whose icon is no longer there at all.
            for id in ids {
                if !read.iter().any(|(read, _)| *read == id) {
                    read.push((id, None));
                }
            }
            IconMessage::Kept(generation, read).into()
        });
    }

    pub(crate) fn handle_icon(&mut self, message: IconMessage, context: &Ctx<Message>) {
        match message {
            IconMessage::Loaded(scale, icons) => {
                let ids: HashSet<String> = icons.iter().map(|(id, _)| id.clone()).collect();
                self.icons.loaded(icons);
                // The scaling changed while they were read: they are read again for this one.
                if scale != context.scale().to_bits() {
                    let games = self
                        .games
                        .iter()
                        .filter(|game| ids.contains(&game.id))
                        .cloned()
                        .collect();
                    Self::load_icons(games, false, context);
                }
            }
            IconMessage::Kept(generation, icons) => {
                if self.icons.kept_read(generation, icons) {
                    // Tiles without a cover are made from their icons; the Artwork dialog shows
                    // them.
                    self.decode_near_covers(context);
                    self.request_previews(context);
                }
            }
        }
    }

    /// A game's icon `size` DIPs big (`SMALL` or `LARGE`), or the game glyph while it loads or
    /// when it has none.
    pub(crate) fn icon(&self, game: &Game, size: f32) -> El<Message> {
        match self.icons.picture(&game.id, size > SMALL) {
            Some((picture, large)) => image(
                format!("icon{}:{}", usize::from(large), game.id),
                Arc::clone(picture),
            )
            .size(size, size),
            None => glyph(g::GAME, size * 0.6)
                .color(theme::text_tertiary())
                .text_align(TextAlign::Center)
                .size(size, size),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(width: u32) -> Arc<Picture> {
        Arc::new(Picture {
            width,
            height: width,
            pixels: vec![0; (width * width * 4) as usize],
        })
    }

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    fn kept(bytes: &[u8]) -> Option<Kept> {
        Some((Arc::new(bytes.to_vec()), picture(144)))
    }

    #[test]
    fn icons_are_made_for_the_pixels_they_take() {
        assert_eq!(pixel_sizes(1.0), [24, 64]);
        assert_eq!(pixel_sizes(1.5), [36, 96]);
        assert_eq!(pixel_sizes(2.25), [54, 144]);
        assert_eq!(pixel_sizes(1.25), [30, 80]);
    }

    #[test]
    fn only_wanted_games_keep_their_icons_whole() {
        let mut icons = IconState::default();
        icons.loaded(
            ["a", "b", "c"]
                .map(|id| (id.to_owned(), picture(54)))
                .to_vec(),
        );
        // Only games with an icon are read, each once while it is being read.
        assert_eq!(
            icons.start_reading(&set(&["a", "b", "none"])),
            set(&["a", "b"])
        );
        assert!(icons.start_reading(&set(&["a", "b"])).is_empty());
        let generation = icons.generation;
        assert!(icons.kept_read(
            generation,
            vec![("a".into(), kept(b"A")), ("b".into(), None)]
        ));
        assert!(icons.bytes("a").is_some());
        // Unreadable: not asked for again.
        assert!(icons.start_reading(&set(&["a", "b"])).is_empty());
        // Large shows the kept picture, else the small one.
        assert_eq!(
            icons.picture("a", true).map(|(p, large)| (p.width, large)),
            Some((144, true))
        );
        assert_eq!(
            icons.picture("c", true).map(|(p, large)| (p.width, large)),
            Some((54, false))
        );
        // No longer wanted: dropped.
        assert!(icons.start_reading(&set(&["c"])).contains("c"));
        assert!(icons.bytes("a").is_none());
    }

    #[test]
    fn icons_read_anew_are_kept_until_read_again() {
        let mut icons = IconState::default();
        icons.loaded(vec![("a".into(), picture(54))]);
        icons.start_reading(&set(&["a"]));
        let first = icons.generation;
        icons.kept_read(first, vec![("a".into(), kept(b"A"))]);
        let old = Arc::clone(icons.bytes("a").unwrap());
        // A scan reads every icon anew: "a" stays shown, and is read again.
        icons.loaded(vec![("a".into(), picture(54))]);
        assert!(icons.bytes("a").is_some());
        assert_eq!(icons.start_reading(&set(&["a"])), set(&["a"]));
        // A read asked for before the icons were read anew is dropped.
        assert!(!icons.kept_read(first, vec![("a".into(), kept(b"old"))]));
        assert!(Arc::ptr_eq(icons.bytes("a").unwrap(), &old));
        // The same bytes again keep the same copy, so nothing made from them is made again.
        icons.start_reading(&set(&["a"]));
        assert!(icons.kept_read(icons.generation, vec![("a".into(), kept(b"A"))]));
        assert!(Arc::ptr_eq(icons.bytes("a").unwrap(), &old));
        // Other bytes replace them.
        icons.loaded(vec![("a".into(), picture(54))]);
        icons.start_reading(&set(&["a"]));
        icons.kept_read(icons.generation, vec![("a".into(), kept(b"B"))]);
        assert_eq!(icons.bytes("a").unwrap().as_slice(), b"B");
    }

    #[test]
    fn a_new_scaling_drops_the_large_pictures_and_older_reads() {
        let mut icons = IconState::default();
        icons.loaded(vec![("a".into(), picture(54))]);
        icons.start_reading(&set(&["a"]));
        let before = icons.generation;
        icons.rescaled();
        assert!(!icons.kept_read(before, vec![("a".into(), kept(b"A"))]));
        assert!(icons.bytes("a").is_none());
        assert_eq!(icons.start_reading(&set(&["a"])), set(&["a"]));
    }
}
