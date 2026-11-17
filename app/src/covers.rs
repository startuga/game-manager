//! Cover art on the Library's tiles: the covers the core finds (`gm_core::covers`), decoded only
//! while their tiles are near the screen. The user's own cover is chosen and dropped in the Artwork
//! dialog (`artwork.rs`), which then calls `add_cover`, or `forget` and `load_covers`.
//!
//! - **Sources** (`load_covers`), for the games that have none yet, or only a stand-in, after the
//!   snapshot, each scan and a game added by hand:
//!   1. One background job takes what needs no network (the user's own picture, the cover cache,
//!      the stores' files on this PC) and sends each cover's file in batches (`Loaded`).
//!   2. After a scan it then names the games the network may still help (`Missing`). They go to at
//!      most `FETCHERS` jobs that ask the stores' servers one game at a time, each cover arriving
//!      on its own (`Fetched`). A game is asked for once at a time (`fetching`); once a request
//!      fails for want of a connection, the job stops asking for the rest. Failures show nothing:
//!      the tile keeps its capsule, and the next start tries again.
//! - **Pixels only near the screen**: a cover is the store's original file, and only its path is
//!   kept (`CoverState::sources`). The shell says which tiles are on the screen or within one
//!   window height of it (`App::near_screen`, tiles made with `El::watch`): their covers are
//!   decoded at full size in up to `DECODERS` background jobs (`Decoded`), and the others' pixels
//!   are dropped, so the Library left behind frees all of them. The renderer drops their bitmaps
//!   too. Direct2D scales each cover into its tile as it draws it (`el::El::cover`), so the display
//!   scaling changes nothing here. A tile shows its icon until its cover is decoded.
//! - **Animated covers** (an animated WebP, the user's own): the tile shows its first frame, and
//!   plays it as Settings say (`library_page.rs`, `ui::animation`), its frames made no larger than
//!   the tile's pixels. The animation is kept with the decoded cover and goes
//!   with it.
//! - **Generations**: each new source of a game (the original after a stand-in, the user's own)
//!   gets a new generation, and a decoded cover is kept only for the generation it was decoded
//!   from, so a stand-in decoded late never hides the original.
//! - **Stand-ins**: a smaller copy the store keeps on this PC shows until the original has
//!   arrived; such a game stays among those `load_covers` asks for (`CoverState::wants`).
//! - **Capsules**: a game the local job found nothing for (`NoneHere`, `CoverState::bare`) has
//!   its icon made into a cover, as Steam makes one from a wide picture: the icon across the
//!   cover's top part, its edges drawn out around it and blurred (`image::decode_capsule`). Like a
//!   cover it is made only while its tile is near the screen, in the decoders' jobs (`Capsule`),
//!   and again when the icon changes (a scan reads it again; the same bytes count as the same
//!   icon). A cover found later replaces it once decoded.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gm_core::Game;
use gm_core::covers::Cover;
use gm_core::picture;

use crate::shell::ctx::Ctx;
use crate::ui::animation::Animation;
use crate::ui::image::{self, Picture};
use crate::{GameManager, Message};

/// A cover's size on a tile, in DIPs: 2:3, as the stores' portrait covers are.
pub(crate) const COVER_WIDTH: f32 = 200.0;
pub(crate) const COVER_HEIGHT: f32 = 300.0;
/// The key prefix of a game's tile, which the shell reports when it is near the screen.
pub(crate) const TILE_KEY: &str = "tile:";
/// How many jobs ask the stores' servers at once.
const FETCHERS: usize = 4;
/// How many jobs decode covers at once.
const DECODERS: usize = 4;
/// How many covers the local job finds before it sends them on.
const BATCH: usize = 12;

#[derive(Clone)]
pub(crate) enum CoverMessage {
    /// Covers that needed no network: their files.
    Loaded(Vec<(String, Cover)>),
    /// The games the stores' servers may have a cover for (or the original of a stand-in).
    Missing(Vec<Game>),
    /// One game's lookup on the network ended: its cover, if one was found.
    Fetched(String, Option<Cover>),
    /// The games whose tiles are on the screen or near it (`App::near_screen`).
    Near(HashSet<String>),
    /// A cover decoded from the source of that generation, and its animation when it is an
    /// animated picture; `None` when it couldn't be decoded.
    Decoded(String, u64, Option<Arc<Picture>>, Option<Arc<Animation>>),
    /// The games the local job found no cover for, not even a stand-in.
    NoneHere(Vec<String>),
    /// A game's icon made into a capsule, with the icon it was made from; `None` when it couldn't
    /// be.
    Capsule(String, Arc<Vec<u8>>, Option<Arc<Picture>>),
}

impl From<CoverMessage> for Message {
    fn from(message: CoverMessage) -> Self {
        Message::Cover(message)
    }
}

/// The games' covers: their files, and the pixels of those near the screen.
#[derive(Default)]
pub(crate) struct CoverState {
    /// Each game's cover and its generation.
    sources: HashMap<String, (Cover, u64)>,
    /// Decoded covers, only for games near the screen, with the generation each came from.
    pictures: HashMap<String, (Arc<Picture>, u64)>,
    /// The animations of animated covers among those.
    animations: HashMap<String, Arc<Animation>>,
    /// The games whose tiles are near the screen.
    near: HashSet<String>,
    /// The generation being decoded, by game.
    decoding: HashMap<String, u64>,
    /// Games being looked up on the network.
    fetching: HashSet<String>,
    /// The last generation given out.
    generation: u64,
    /// Games the local job found no cover for, and none found since: their tiles show capsules.
    bare: HashSet<String>,
    /// Capsules, only for games near the screen, with the icon each was made from.
    capsules: HashMap<String, (Arc<Picture>, Arc<Vec<u8>>)>,
    /// The icons being made into capsules, by game.
    capsuling: HashMap<String, Arc<Vec<u8>>>,
}

/// Whether two icons are the same: the same bytes, read again or not.
fn same_icon(a: &Arc<Vec<u8>>, b: &Arc<Vec<u8>>) -> bool {
    Arc::ptr_eq(a, b) || a == b
}

impl CoverState {
    /// The cover to draw on a game's tile, once decoded.
    pub fn picture(&self, id: &str) -> Option<&Arc<Picture>> {
        self.pictures.get(id).map(|(picture, _)| picture)
    }

    /// A game's icon made into a cover, for its tile while it has none (or until it is decoded).
    pub fn capsule(&self, id: &str) -> Option<&Arc<Picture>> {
        self.capsules.get(id).map(|(capsule, _)| capsule)
    }

    /// A game's cover's animation, when it is an animated picture and decoded.
    pub fn animation(&self, id: &str) -> Option<&Arc<Animation>> {
        self.animations.get(id)
    }

    /// The games whose tiles are near the screen (their icons are kept whole too, `icons.rs`).
    pub fn near(&self) -> &HashSet<String> {
        &self.near
    }

    /// The file of a game's cover, once found (the game page blurs it when it has no wide picture).
    pub fn file(&self, id: &str) -> Option<&std::path::Path> {
        self.sources.get(id).map(|(cover, _)| cover.file.as_path())
    }

    /// Whether the game's cover is the user's own (the menu offers Reset cover).
    pub fn is_own(&self, id: &str) -> bool {
        self.sources.get(id).is_some_and(|(cover, _)| cover.own)
    }

    /// Whether a game still needs its cover: none found, or only a stand-in.
    fn wants(&self, id: &str) -> bool {
        self.sources.get(id).is_none_or(|(cover, _)| cover.stand_in)
    }

    /// A game's new cover, as a new generation. Its picture stays until the new one is decoded.
    fn set_source(&mut self, id: String, cover: Cover) {
        self.generation += 1;
        self.bare.remove(&id);
        self.sources.insert(id, (cover, self.generation));
    }

    pub(crate) fn forget(&mut self, id: &str) {
        self.sources.remove(id);
        self.pictures.remove(id);
        self.animations.remove(id);
        self.decoding.remove(id);
        self.bare.remove(id);
        self.capsules.remove(id);
        self.capsuling.remove(id);
    }

    /// The games now near the screen: the others' pictures are dropped.
    fn set_near(&mut self, near: HashSet<String>) {
        self.pictures.retain(|id, _| near.contains(id));
        self.animations.retain(|id, _| near.contains(id));
        self.capsules.retain(|id, _| near.contains(id));
        self.near = near;
    }

    /// The games the local job found nothing for: those still without a cover get capsules.
    fn none_here(&mut self, ids: Vec<String>) {
        for id in ids {
            if !self.sources.contains_key(&id) {
                self.bare.insert(id);
            }
        }
    }

    /// The icons to make into capsules (`icon` gives a game's): games near the screen without a
    /// cover whose capsule is missing or from another icon, and not being made from this one
    /// already. They are marked as being made.
    fn start_capsules(
        &mut self,
        icon: impl Fn(&str) -> Option<Arc<Vec<u8>>>,
    ) -> Vec<(String, Arc<Vec<u8>>)> {
        let mut wanted = Vec::new();
        for id in self.near.intersection(&self.bare) {
            let Some(bytes) = icon(id) else {
                continue;
            };
            let made = self
                .capsules
                .get(id)
                .is_some_and(|(_, from)| same_icon(from, &bytes));
            let making = self
                .capsuling
                .get(id)
                .is_some_and(|from| same_icon(from, &bytes));
            if !made && !making {
                wanted.push((id.clone(), bytes));
            }
        }
        for (id, bytes) in &wanted {
            self.capsuling.insert(id.clone(), Arc::clone(bytes));
        }
        wanted
    }

    /// A capsule made from `from`: kept while its game is near the screen, still without a
    /// cover, and `from` still its icon (`now`).
    fn capsule_made(
        &mut self,
        id: String,
        from: Arc<Vec<u8>>,
        capsule: Option<Arc<Picture>>,
        now: Option<&Arc<Vec<u8>>>,
    ) {
        if self
            .capsuling
            .get(&id)
            .is_some_and(|making| Arc::ptr_eq(making, &from))
        {
            self.capsuling.remove(&id);
        }
        if let Some(capsule) = capsule
            && self.near.contains(&id)
            && self.bare.contains(&id)
            && now.is_some_and(|now| same_icon(now, &from))
        {
            self.capsules.insert(id, (capsule, from));
        }
    }

    /// The covers near the screen to decode: those whose picture is missing or older than their
    /// source, and not being decoded already. They are marked as being decoded.
    fn start_decoding(&mut self) -> Vec<(String, Cover, u64)> {
        let mut wanted = Vec::new();
        for id in &self.near {
            let Some((cover, generation)) = self.sources.get(id) else {
                continue;
            };
            let shown = self.pictures.get(id).map(|(_, shown)| shown);
            if shown == Some(generation) || self.decoding.get(id) == Some(generation) {
                continue;
            }
            wanted.push((id.clone(), cover.clone(), *generation));
        }
        for (id, _, generation) in &wanted {
            self.decoding.insert(id.clone(), *generation);
        }
        wanted
    }

    /// A decoded cover: kept when its game is still near the screen and its source the same.
    fn decoded(&mut self, id: String, generation: u64, picture: Option<Arc<Picture>>) {
        if self.decoding.get(&id) == Some(&generation) {
            self.decoding.remove(&id);
        }
        let current = self.sources.get(&id).map(|(_, current)| *current);
        if let Some(picture) = picture
            && current == Some(generation)
            && self.near.contains(&id)
        {
            // The cover shows now: its capsule is done with.
            self.capsules.remove(&id);
            self.pictures.insert(id, (picture, generation));
        }
    }

    /// The animation of the cover decoded for `generation`: kept while that cover shows.
    fn animated(&mut self, id: &str, generation: u64, animation: Option<Arc<Animation>>) {
        if self
            .pictures
            .get(id)
            .is_none_or(|(_, shown)| *shown != generation)
        {
            return;
        }
        match animation {
            Some(animation) => self.animations.insert(id.to_owned(), animation),
            None => self.animations.remove(id),
        };
    }

    /// Keeps only what belongs to `ids` (after a scan, games that are gone).
    fn retain(&mut self, ids: &HashSet<&str>) {
        self.sources.retain(|id, _| ids.contains(id.as_str()));
        self.pictures.retain(|id, _| ids.contains(id.as_str()));
        self.animations.retain(|id, _| ids.contains(id.as_str()));
        self.decoding.retain(|id, _| ids.contains(id.as_str()));
        self.bare.retain(|id| ids.contains(id.as_str()));
        self.capsules.retain(|id, _| ids.contains(id.as_str()));
        self.capsuling.retain(|id, _| ids.contains(id.as_str()));
    }
}

/// `items` dealt out to at most `count` lists, so each job has about as much to do.
fn deal<T: Clone>(items: Vec<T>, count: usize) -> Vec<Vec<T>> {
    let mut lists = vec![Vec::new(); count.min(items.len())];
    for (index, item) in items.into_iter().enumerate() {
        let list = index % lists.len();
        lists[list].push(item);
    }
    lists
}

/// The game ids among the keys the shell reports near the screen (`TILE_KEY`).
pub(crate) fn near_tiles(keys: &HashSet<String>) -> HashSet<String> {
    keys.iter()
        .filter_map(|key| key.strip_prefix(TILE_KEY))
        .map(str::to_owned)
        .collect()
}

impl GameManager {
    /// Finds the covers of the games that have none, or only a stand-in. With `network`, the
    /// stores' servers are asked for the originals still missing (after a scan; the snapshot's
    /// games are about to be scanned). With `prune` (a full, fresh library), cached covers no
    /// game uses are deleted.
    pub(crate) fn load_covers(&mut self, network: bool, prune: bool, context: &Ctx<Message>) {
        let wanted: Vec<Game> = self
            .games
            .iter()
            .filter(|game| self.covers.wants(&game.id))
            .cloned()
            .collect();
        let library = prune.then(|| self.games.clone());
        context.spawn_stream(move |send| {
            let mut batch = Vec::new();
            let mut found = HashSet::new();
            let rest = gm_core::covers::load_local(&wanted, &mut |game, cover| {
                found.insert(game.id.clone());
                batch.push((game.id.clone(), cover));
                if batch.len() >= BATCH {
                    send(CoverMessage::Loaded(std::mem::take(&mut batch)).into());
                }
            });
            if !batch.is_empty() {
                send(CoverMessage::Loaded(batch).into());
            }
            let none: Vec<String> = wanted
                .iter()
                .filter(|game| !found.contains(&game.id))
                .map(|game| game.id.clone())
                .collect();
            if !none.is_empty() {
                send(CoverMessage::NoneHere(none).into());
            }
            if let Some(library) = library {
                gm_core::covers::prune(&library);
                gm_core::page_art::prune(&library);
            }
            if network && !rest.is_empty() {
                send(CoverMessage::Missing(rest).into());
            }
        });
    }

    /// Decodes the covers near the screen that aren't yet, and makes the capsules of those near it
    /// without a cover, off the UI thread, each sent as soon as it is ready. Also once icons have
    /// been read (`icons.rs`): a capsule needs its game's.
    pub(crate) fn decode_near_covers(&mut self, context: &Ctx<Message>) {
        // An animation's frames are made for the tile's pixels.
        let scale = context.scale();
        let tile = (
            (COVER_WIDTH * scale).round() as u32,
            (COVER_HEIGHT * scale).round() as u32,
        );
        for list in deal(self.covers.start_decoding(), DECODERS) {
            context.spawn_stream(move |send| {
                for (id, cover, generation) in list {
                    let bytes = gm_core::covers::read(&cover);
                    let picture = bytes.as_deref().and_then(image::decode_cover).map(Arc::new);
                    // Played from its file: the bytes go once the first frame is made.
                    let animated = bytes.is_some_and(|bytes| picture::is_animated(&bytes));
                    let animation = (animated && picture.is_some())
                        .then(|| Animation::open(&cover.file, tile))
                        .flatten();
                    send(CoverMessage::Decoded(id, generation, picture, animation).into());
                }
            });
        }
        let capsules = self
            .covers
            .start_capsules(|id| self.icons.bytes(id).cloned());
        for list in deal(capsules, DECODERS) {
            context.spawn_stream(move |send| {
                for (id, icon) in list {
                    let capsule = image::decode_capsule(&icon).map(Arc::new);
                    send(CoverMessage::Capsule(id, icon, capsule).into());
                }
            });
        }
    }

    /// A game's cover found or chosen, unless the game is gone since (removed, uninstalled).
    pub(crate) fn add_cover(&mut self, id: String, cover: Cover) {
        if self.games.iter().any(|game| game.id == id) {
            self.covers.set_source(id, cover);
        }
    }

    pub(crate) fn handle_cover(&mut self, message: CoverMessage, context: &Ctx<Message>) {
        match message {
            CoverMessage::Loaded(covers) => {
                for (id, cover) in covers {
                    self.add_cover(id, cover);
                }
                self.decode_near_covers(context);
            }
            CoverMessage::Missing(games) => {
                let games: Vec<Game> = games
                    .into_iter()
                    .filter(|game| self.covers.fetching.insert(game.id.clone()))
                    .collect();
                for list in deal(games, FETCHERS) {
                    context.spawn_stream(move |send| {
                        let mut offline = false;
                        for game in list {
                            let cover = if offline {
                                None
                            } else {
                                match gm_core::covers::fetch(&game) {
                                    Ok(cover) => cover,
                                    // No connection: the others would only wait as long.
                                    Err(gm_core::Error::Windows { .. }) => {
                                        offline = true;
                                        None
                                    }
                                    Err(_) => None,
                                }
                            };
                            send(CoverMessage::Fetched(game.id, cover).into());
                        }
                    });
                }
            }
            CoverMessage::Fetched(id, cover) => {
                self.covers.fetching.remove(&id);
                if let Some(cover) = cover {
                    self.add_cover(id, cover);
                    self.decode_near_covers(context);
                }
            }
            CoverMessage::Near(ids) => {
                self.covers.set_near(ids);
                self.decode_near_covers(context);
            }
            CoverMessage::Decoded(id, generation, picture, animation) => {
                self.covers.decoded(id.clone(), generation, picture);
                self.covers.animated(&id, generation, animation);
            }
            CoverMessage::NoneHere(ids) => {
                self.covers.none_here(ids);
                self.decode_near_covers(context);
            }
            CoverMessage::Capsule(id, icon, capsule) => {
                let now = self.icons.bytes(&id).cloned();
                self.covers.capsule_made(id, icon, capsule, now.as_ref());
            }
        }
    }

    /// After a scan: covers of games that are gone are dropped.
    pub(crate) fn drop_covers_of_missing_games(&mut self) {
        let ids: HashSet<&str> = self.games.iter().map(|game| game.id.as_str()).collect();
        self.covers.retain(&ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::{Launch, Store};
    use std::path::PathBuf;

    fn game(id: &str) -> Game {
        Game {
            id: id.into(),
            name: id.into(),
            store: Store::Steam,
            install_dir: None,
            launch: Launch::Uri(String::new()),
            alt_launches: Vec::new(),
            icon: None,
            program: None,
            arch: None,
            edits: None,
            pinned: None,
            hidden: false,
            installed: None,
        }
    }

    #[test]
    fn games_are_dealt_out_to_the_jobs() {
        let ids = |lists: Vec<Vec<Game>>| -> Vec<Vec<String>> {
            lists
                .into_iter()
                .map(|list| list.into_iter().map(|game| game.id).collect())
                .collect()
        };
        let six = (1..=6).map(|n| game(&n.to_string())).collect();
        assert_eq!(
            ids(deal(six, 4)),
            [vec!["1", "5"], vec!["2", "6"], vec!["3"], vec!["4"]]
        );
        // Fewer games than jobs: a job each, and none for nothing.
        assert_eq!(ids(deal(vec![game("1"), game("2")], 4)), [["1"], ["2"]]);
        assert!(deal(Vec::<Game>::new(), 4).is_empty());
    }

    fn cover(file: &str, own: bool, stand_in: bool) -> Cover {
        Cover {
            file: PathBuf::from(file),
            own,
            stand_in,
        }
    }

    fn picture() -> Arc<Picture> {
        Arc::new(Picture {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        })
    }

    fn near(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    /// What `start_decoding` hands out: game ids and generations, sorted.
    fn decode(state: &mut CoverState) -> Vec<(String, u64)> {
        let mut out: Vec<(String, u64)> = state
            .start_decoding()
            .into_iter()
            .map(|(id, _, generation)| (id, generation))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn covers_are_decoded_only_near_the_screen() {
        let mut state = CoverState::default();
        state.set_source("steam:220".into(), cover("hl2.jpg", false, false));
        state.set_source("epic:sifu".into(), cover("sifu.png", false, false));
        // Nothing near the screen: nothing to decode.
        assert!(decode(&mut state).is_empty());
        state.set_near(near(&["steam:220", "steam:400"]));
        // Only the near game with a source; once handed out, not again while it decodes.
        assert_eq!(decode(&mut state), [("steam:220".to_owned(), 1)]);
        assert!(decode(&mut state).is_empty());
        state.decoded("steam:220".into(), 1, Some(picture()));
        assert!(state.picture("steam:220").is_some());
        assert!(decode(&mut state).is_empty());
        // Scrolled away (or the Library left): its pixels go; back near, decoded again.
        state.set_near(near(&["epic:sifu"]));
        assert!(state.picture("steam:220").is_none());
        assert_eq!(decode(&mut state), [("epic:sifu".to_owned(), 2)]);
        // A decode that finishes after its tile left is dropped.
        state.set_near(HashSet::new());
        state.decoded("epic:sifu".into(), 2, Some(picture()));
        assert!(state.picture("epic:sifu").is_none());
        // One that failed can be tried again.
        state.set_near(near(&["epic:sifu"]));
        assert_eq!(decode(&mut state), [("epic:sifu".to_owned(), 2)]);
        state.decoded("epic:sifu".into(), 2, None);
        assert_eq!(decode(&mut state), [("epic:sifu".to_owned(), 2)]);
    }

    #[test]
    fn a_stand_in_decoded_late_never_hides_the_original() {
        let mut state = CoverState::default();
        state.set_near(near(&["steam:220"]));
        state.set_source("steam:220".into(), cover("client.jpg", false, true));
        assert!(state.wants("steam:220"));
        assert_eq!(decode(&mut state), [("steam:220".to_owned(), 1)]);
        // The original arrives while the stand-in is being decoded.
        state.set_source("steam:220".into(), cover("original.jpg", false, false));
        assert!(!state.wants("steam:220"));
        assert_eq!(decode(&mut state), [("steam:220".to_owned(), 2)]);
        state.decoded("steam:220".into(), 2, Some(picture()));
        let original = Arc::clone(state.picture("steam:220").unwrap());
        // The stand-in's late result is refused.
        state.decoded("steam:220".into(), 1, Some(picture()));
        assert!(Arc::ptr_eq(state.picture("steam:220").unwrap(), &original));
        // The user's own replaces it; the original shows until it is decoded.
        state.set_source("steam:220".into(), cover("mine.png", true, false));
        assert!(state.is_own("steam:220"));
        assert!(Arc::ptr_eq(state.picture("steam:220").unwrap(), &original));
        assert_eq!(decode(&mut state), [("steam:220".to_owned(), 3)]);
        // Reset cover: nothing left, and wanted again.
        state.forget("steam:220");
        assert!(state.picture("steam:220").is_none() && state.wants("steam:220"));
        assert!(!state.is_own("steam:220"));
    }

    fn icon(bytes: &[u8]) -> Arc<Vec<u8>> {
        Arc::new(bytes.to_vec())
    }

    /// What `start_capsules` hands out when every game's icon is `icon`: game ids, sorted.
    fn capsule_jobs(state: &mut CoverState, icon: &Arc<Vec<u8>>) -> Vec<String> {
        let mut out: Vec<String> = state
            .start_capsules(|_| Some(Arc::clone(icon)))
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        out.sort();
        out
    }

    #[test]
    fn games_without_a_cover_get_capsules_near_the_screen_until_one_is_decoded() {
        let mut state = CoverState::default();
        state.set_source("steam:220".into(), cover("hl2.jpg", false, false));
        state.set_near(near(&["steam:220", "custom:1", "custom:2"]));
        // Before the local job has answered, no game is known to have none.
        let first = icon(b"one");
        assert!(capsule_jobs(&mut state, &first).is_empty());
        // A game with a cover is never made into one; one far from the screen waits.
        state.none_here(vec![
            "steam:220".into(),
            "custom:1".into(),
            "custom:3".into(),
        ]);
        assert_eq!(capsule_jobs(&mut state, &first), ["custom:1"]);
        assert!(capsule_jobs(&mut state, &first).is_empty());
        state.capsule_made(
            "custom:1".into(),
            Arc::clone(&first),
            Some(picture()),
            Some(&first),
        );
        assert!(state.capsule("custom:1").is_some());
        // The same bytes read again by a scan are the same icon: nothing to make.
        assert!(capsule_jobs(&mut state, &icon(b"one")).is_empty());
        // Another icon: made again, the old capsule showing meanwhile.
        let second = icon(b"two");
        assert_eq!(capsule_jobs(&mut state, &second), ["custom:1"]);
        assert!(state.capsule("custom:1").is_some());
        // A capsule from an icon that has changed since is refused.
        let shown = Arc::clone(state.capsule("custom:1").unwrap());
        state.capsule_made(
            "custom:1".into(),
            Arc::clone(&first),
            Some(picture()),
            Some(&second),
        );
        assert!(Arc::ptr_eq(state.capsule("custom:1").unwrap(), &shown));
        state.capsule_made(
            "custom:1".into(),
            Arc::clone(&second),
            Some(picture()),
            Some(&second),
        );
        assert!(!Arc::ptr_eq(state.capsule("custom:1").unwrap(), &shown));
        // Scrolled away: its pixels go; back near, made again.
        state.set_near(near(&["steam:220"]));
        assert!(state.capsule("custom:1").is_none());
        state.set_near(near(&["custom:1"]));
        assert_eq!(capsule_jobs(&mut state, &second), ["custom:1"]);
        state.capsule_made(
            "custom:1".into(),
            Arc::clone(&second),
            Some(picture()),
            Some(&second),
        );
        // A cover found later: no more capsules, and the one shown stays until it is decoded.
        state.set_source("custom:1".into(), cover("found.jpg", false, false));
        assert!(capsule_jobs(&mut state, &icon(b"three")).is_empty());
        assert!(state.capsule("custom:1").is_some());
        let generation = state.generation;
        assert_eq!(decode(&mut state), [("custom:1".to_owned(), generation)]);
        state.decoded("custom:1".into(), generation, Some(picture()));
        assert!(state.capsule("custom:1").is_none() && state.picture("custom:1").is_some());
    }

    #[test]
    fn games_gone_lose_their_covers_and_tile_keys_name_games() {
        let mut state = CoverState::default();
        state.set_source("steam:220".into(), cover("a.jpg", false, false));
        state.set_source("ea:1".into(), cover("b.jpg", false, false));
        state.set_near(near(&["steam:220", "ea:1"]));
        state.decoded("ea:1".into(), 2, Some(picture()));
        state.retain(&HashSet::from(["steam:220"]));
        assert!(state.picture("ea:1").is_none() && state.wants("ea:1"));
        assert!(!state.wants("steam:220"));
        let keys = near(&["tile:steam:220", "tile:ea:1", "row:steam:220", "search"]);
        assert_eq!(near_tiles(&keys), near(&["steam:220", "ea:1"]));
    }
}
