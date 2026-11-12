//! Cover art on the Library's tiles: loading the covers the core finds (`gm_core::covers`),
//! decoding them for the tiles, and the game menu's Choose cover… and Reset cover.
//!
//! - **Loading** (`load_covers`), for the games that show no cover yet, after the snapshot, each
//!   scan and a game added by hand:
//!   1. One background job takes what needs no network (the user's own image, the cover cache,
//!      the stores' files on this PC) and sends the decoded covers in batches.
//!   2. After a scan it then names the games the network may still help (`Missing`). They go to at
//!      most `FETCHERS` jobs that ask the stores' servers one game at a time, each cover showing
//!      as it arrives (`Fetched`). A game is asked for once at a time (`fetching`); once a request
//!      fails for want of a connection, the job stops asking for the rest. Failures show nothing:
//!      the tile keeps its icon, and the next start tries again.
//! - **Sizes**: covers are decoded at the tile's pixel size for the window's display scaling
//!   (`Ctx::scale`), with the tile's rounded corners cut in (`ui::image::decode_covers`). When
//!   the scaling changes they are all made again (`ScaleChanged`).
//! - **Choose cover…** opens the Open dialog for a picture, which becomes the game's own cover
//!   (`covers::set_own`); **Reset cover** drops it and what was downloaded for the game, and
//!   looks its cover up afresh.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gm_core::Game;
use gm_core::covers::Cover;

use crate::shell::ctx::{Ctx, FilePicker};
use crate::ui::image::{self, Picture};
use crate::ui::theme;
use crate::{GameManager, Message};

/// A cover's size on a tile, in DIPs: 2:3, as the stores' portrait covers are.
pub(crate) const COVER_WIDTH: f32 = 200.0;
pub(crate) const COVER_HEIGHT: f32 = 300.0;
/// How many jobs ask the stores' servers at once.
const FETCHERS: usize = 4;
/// How many covers the local job decodes before it sends them on.
const BATCH: usize = 12;

/// A decoded cover for a game's tile.
#[derive(Clone)]
pub(crate) struct CoverPicture {
    id: String,
    picture: Arc<Picture>,
    /// The user's own image, not a store's.
    own: bool,
}

#[derive(Clone)]
pub(crate) enum CoverMessage {
    /// Covers that needed no network, decoded.
    Loaded(Vec<CoverPicture>),
    /// The games the stores' servers may have a cover for (or a sharper one).
    Missing(Vec<Game>),
    /// One game's lookup on the network ended: its cover, if one was found.
    Fetched(String, Option<CoverPicture>),
    /// The game menu's Choose cover….
    Choose(String),
    /// The picture picked for the game; `None` when the dialog was cancelled.
    Picked(String, Result<Option<PathBuf>, String>),
    /// The picked picture as the game's cover, or why it can't be one.
    Chosen(Result<CoverPicture, String>),
    /// The game menu's Reset cover.
    Reset(String),
    /// The game's own cover was dropped (or not).
    ResetDone(String, Result<(), String>),
    /// The window's display scaling changed: the covers are made again for it.
    ScaleChanged,
}

impl From<CoverMessage> for Message {
    fn from(message: CoverMessage) -> Self {
        Message::Cover(message)
    }
}

/// The covers shown, by game id.
#[derive(Default)]
pub(crate) struct CoverState {
    pub pictures: HashMap<String, Arc<Picture>>,
    /// Games showing the user's own image (the menu offers Reset cover).
    pub own: HashSet<String>,
    /// Games being looked up on the network.
    fetching: HashSet<String>,
}

impl CoverState {
    fn show(&mut self, cover: CoverPicture) {
        if cover.own {
            self.own.insert(cover.id.clone());
        } else {
            self.own.remove(&cover.id);
        }
        self.pictures.insert(cover.id, cover.picture);
    }

    fn forget(&mut self, id: &str) {
        self.pictures.remove(id);
        self.own.remove(id);
    }
}

/// A cover's size and corner radius in pixels at a display scaling.
fn pixels(scale: f32) -> ((u32, u32), u32) {
    let px = |dips: f32| (dips * scale).round().max(1.0) as u32;
    ((px(COVER_WIDTH), px(COVER_HEIGHT)), px(theme::RADIUS_CARD))
}

/// The core's covers decoded for tiles at `scale`; what is no picture is left out.
fn decode(covers: Vec<(String, Cover)>, scale: f32) -> Vec<CoverPicture> {
    let (size, radius) = pixels(scale);
    let own: HashSet<String> = covers
        .iter()
        .filter(|(_, cover)| cover.own)
        .map(|(id, _)| id.clone())
        .collect();
    let items = covers
        .into_iter()
        .map(|(id, cover)| (id, cover.jpeg))
        .collect();
    image::decode_covers(items, size, radius)
        .into_iter()
        .map(|(id, picture)| CoverPicture {
            own: own.contains(&id),
            id,
            picture: Arc::new(picture),
        })
        .collect()
}

/// `games` dealt out to at most `count` lists, so each job has about as many to ask for.
fn deal(games: Vec<Game>, count: usize) -> Vec<Vec<Game>> {
    let mut lists = vec![Vec::new(); count.min(games.len())];
    for (index, game) in games.into_iter().enumerate() {
        let list = index % lists.len();
        lists[list].push(game);
    }
    lists
}

impl GameManager {
    /// Loads the covers of the games that show none. With `network`, the stores' servers are
    /// asked for those still missing (after a scan; the snapshot's games are about to be
    /// scanned). With `prune` (a full, fresh library), cached covers no game uses are deleted.
    pub(crate) fn load_covers(&mut self, network: bool, prune: bool, context: &Ctx<Message>) {
        let wanted: Vec<Game> = self
            .games
            .iter()
            .filter(|game| !self.covers.pictures.contains_key(&game.id))
            .cloned()
            .collect();
        let library = prune.then(|| self.games.clone());
        let scale = context.scale();
        let want_width = pixels(scale).0.0;
        context.spawn_stream(move |send| {
            let mut batch = Vec::new();
            let rest = gm_core::covers::load_local(&wanted, want_width, &mut |game, cover| {
                batch.push((game.id.clone(), cover));
                if batch.len() >= BATCH {
                    let covers = decode(std::mem::take(&mut batch), scale);
                    send(CoverMessage::Loaded(covers).into());
                }
            });
            if !batch.is_empty() {
                send(CoverMessage::Loaded(decode(batch, scale)).into());
            }
            if let Some(library) = library {
                gm_core::covers::prune(&library);
            }
            if network && !rest.is_empty() {
                send(CoverMessage::Missing(rest).into());
            }
        });
    }

    pub(crate) fn handle_cover(&mut self, message: CoverMessage, context: &Ctx<Message>) {
        match message {
            CoverMessage::Loaded(covers) => {
                for cover in covers {
                    // A game gone since (removed, uninstalled) needs none.
                    if self.games.iter().any(|game| game.id == cover.id) {
                        self.covers.show(cover);
                    }
                }
            }
            CoverMessage::Missing(games) => {
                let games: Vec<Game> = games
                    .into_iter()
                    .filter(|game| self.covers.fetching.insert(game.id.clone()))
                    .collect();
                let scale = context.scale();
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
                            let cover = cover.and_then(|cover| {
                                decode(vec![(game.id.clone(), cover)], scale).pop()
                            });
                            send(CoverMessage::Fetched(game.id, cover).into());
                        }
                    });
                }
            }
            CoverMessage::Fetched(id, cover) => {
                self.covers.fetching.remove(&id);
                if let Some(cover) = cover
                    && self.games.iter().any(|game| game.id == id)
                {
                    self.covers.show(cover);
                }
            }
            CoverMessage::Choose(id) => {
                let Some(game) = self.games.iter().find(|game| game.id == id) else {
                    return;
                };
                // Owned by the app's window, which is disabled while the dialog is open.
                self.picking = FilePicker::new()
                    .title(format!("Choose a cover for {}", game.name))
                    .filter_extensions("Pictures", ["jpg", "jpeg", "png", "webp", "bmp", "gif"])
                    .filter_all()
                    .request(context, move |picked| {
                        let picked = picked.map_err(|e| format!("could not choose a picture: {e}"));
                        CoverMessage::Picked(id, picked).into()
                    });
            }
            CoverMessage::Picked(id, Ok(Some(file))) => {
                self.picking = false;
                let scale = context.scale();
                // Reading and scaling the picture are disk work.
                context.spawn_background(move |_cancel| {
                    let chosen = gm_core::covers::set_own(&id, &file)
                        .map_err(|e| e.to_string())
                        .and_then(|cover| {
                            decode(vec![(id, cover)], scale)
                                .pop()
                                .ok_or_else(|| "The cover could not be read back.".to_owned())
                        });
                    CoverMessage::Chosen(chosen).into()
                });
            }
            CoverMessage::Picked(_, Ok(None)) => self.picking = false,
            CoverMessage::Picked(_, Err(e)) => {
                self.picking = false;
                self.error = Some(e);
            }
            CoverMessage::Chosen(Ok(cover)) => self.covers.show(cover),
            CoverMessage::Chosen(Err(e)) => self.error = Some(e),
            CoverMessage::Reset(id) => {
                let Some(game) = self.games.iter().find(|game| game.id == id).cloned() else {
                    return;
                };
                context.spawn_background(move |_cancel| {
                    let reset = gm_core::covers::clear_own(&game).map_err(|e| e.to_string());
                    CoverMessage::ResetDone(game.id, reset).into()
                });
            }
            CoverMessage::ResetDone(id, Ok(())) => {
                // Without a cover the game is looked up afresh: its store's, or by name.
                self.covers.forget(&id);
                self.load_covers(true, false, context);
            }
            CoverMessage::ResetDone(_, Err(e)) => self.error = Some(e),
            CoverMessage::ScaleChanged => {
                self.covers.pictures.clear();
                self.load_covers(true, false, context);
            }
        }
    }

    /// After a scan: covers of games that are gone are dropped.
    pub(crate) fn drop_covers_of_missing_games(&mut self) {
        let games = &self.games;
        self.covers
            .pictures
            .retain(|id, _| games.iter().any(|game| game.id == *id));
        self.covers
            .own
            .retain(|id| games.iter().any(|game| game.id == *id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::{Launch, Store};

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
    fn covers_are_sized_for_the_display_scaling() {
        // 200 × 300 DIPs with the cards' 8 DIP corners.
        assert_eq!(pixels(1.0), ((200, 300), 8));
        assert_eq!(pixels(1.25), ((250, 375), 10));
        assert_eq!(pixels(1.5), ((300, 450), 12));
        assert_eq!(pixels(2.0), ((400, 600), 16));
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
        assert!(deal(Vec::new(), 4).is_empty());
    }

    #[test]
    fn a_shown_cover_remembers_whose_it_is() {
        let picture = Arc::new(Picture {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        });
        let cover = |own| CoverPicture {
            id: "steam:220".into(),
            picture: picture.clone(),
            own,
        };
        let mut state = CoverState::default();
        state.show(cover(true));
        assert!(state.pictures.contains_key("steam:220") && state.own.contains("steam:220"));
        // The store's cover replaces it after Reset cover.
        state.show(cover(false));
        assert!(state.pictures.contains_key("steam:220") && state.own.is_empty());
        state.forget("steam:220");
        assert!(state.pictures.is_empty());
    }
}
