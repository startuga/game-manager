//! The Artwork dialog (the game menu's Artwork…, and the button on the game page's band): a game's
//! cover, banner (the wide picture at the top of its page), logo and icon in one place, each in a
//! tab (`gm_core::steamgriddb::Kind`), with SteamGridDB's pictures to choose from.
//!
//! - **What shows now** (`Previews`, read and decoded in the background when the dialog opens and
//!   after each change): each part's picture and where it comes from: your pick, SteamGridDB, the
//!   store, the game's files, made from the icon (a cover, `covers.rs`), or none yet.
//! - **Choose a file…** makes a picture the part's own (`covers::set_own`, `page_art::set_own`,
//!   `icon::set_own`); **Use the store's** (the game's, for an icon) drops it. Either way the
//!   Library, the page and the icons take it at once (`saved`).
//! - **SteamGridDB** (with the user's key, Settings ▸ SteamGridDB): the game is found when the
//!   dialog opens (`steamgriddb::find_game`), and another can be searched. Each tab lists its
//!   pictures a page at a time (animated ones first), their previews downloaded and decoded in
//!   `THUMB_JOBS` background jobs, the still ones first (`steamgriddb::preview`: an animated one
//!   is the picture itself), no larger than `THUMB_PIXELS`. A click downloads the picture and
//!   makes it the part's own, noted as SteamGridDB's (`steamgriddb::record`), so it is marked
//!   with a check.
//! - **The key** is read from Credential Manager in the background at start (`KeyRead`), kept in
//!   memory only, and never shown. Settings saves or removes it (`steamgriddb_card`).

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use gm_core::covers::Cover;
use gm_core::page_art::{self, Part};
use gm_core::steamgriddb::{self as sgdb, Kind, SgdbGame};
use gm_core::{Game, Launch};

use crate::app_view::{field_label, wide_dialog, wide_dialog_width};
use crate::shell::WindowInfo;
use crate::shell::ctx::{Ctx, FilePicker};
use crate::ui::el::{Align, El, Role, col, glyph, image, layers, row, text};
use crate::ui::image::{self, Picture};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    accent_button, button, card, card_header, chip, hint, link, segmented, text_box, working,
};
use crate::{GameManager, Message, Page};

/// The smallest thumbnail of each kind, in DIPs: as many as fit a row.
const COVER_THUMB: f32 = 130.0;
const BANNER_THUMB: f32 = 260.0;
const LOGO_THUMB: f32 = 200.0;
const ICON_THUMB: f32 = 88.0;
/// Banners are Steam's heroes' shape; logos sit on a tile this wide for its height.
const BANNER_SHAPE: f32 = 1920.0 / 620.0;
const LOGO_SHAPE: f32 = 2.1;
/// The current picture's preview, in DIPs.
const PREVIEW_HEIGHT: f32 = 160.0;
/// An icon's preview, in DIPs.
const ICON_PREVIEW: f32 = 96.0;
/// How many jobs download and decode thumbnails at once, and the most pixels a side one keeps.
const THUMB_JOBS: usize = 4;
const THUMB_PIXELS: u32 = 768;

#[derive(Clone)]
pub(crate) enum ArtworkMessage {
    /// Opens the dialog for a game, on a tab.
    Open(String, Kind),
    Close,
    Tab(Kind),
    /// What shows now, read for the game, and what came from SteamGridDB.
    Previewed(String, Previews, BTreeMap<Kind, u64>),
    /// Choose a file…: the Open dialog.
    ChooseFile,
    Picked(String, Kind, Result<Option<PathBuf>, String>),
    /// Use the store's (the game's, for an icon): the own picture goes.
    UseStore,
    /// A part's own picture was set or dropped, or why not.
    Saved(String, Result<Changed, String>),
    /// The key, read from Credential Manager at start.
    KeyRead(Result<Option<String>, String>),
    /// Settings: the key typed, Save, Remove, and the result.
    KeyDraft(String),
    SaveKey,
    RemoveKey,
    KeySaved(Result<Option<String>, String>),
    /// Settings: where a key is made, in the browser.
    OpenKeyPage,
    /// The dialog's game on SteamGridDB, or why not.
    GameFound(String, Result<sgdb::Found, String>),
    /// The search for another game: its text, Enter, the ✕, the games found, one picked.
    Query(String),
    Search,
    ClearSearch,
    Searched(Result<Vec<SgdbGame>, String>),
    PickGame(SgdbGame),
    /// A page of a game's pictures of a kind, and whether there may be more.
    Listed(u64, Kind, u32, Result<(Vec<sgdb::Picture>, bool), String>),
    /// Show more: the next page.
    More,
    /// A thumbnail, decoded; `None` when it can't be had.
    Thumb(u64, Option<Arc<Picture>>),
    /// A SteamGridDB picture chosen for the tab's part.
    Apply(sgdb::Picture),
    /// No key yet: Settings.
    OpenSettings,
}

impl From<ArtworkMessage> for Message {
    fn from(message: ArtworkMessage) -> Self {
        Message::Artwork(message)
    }
}

/// What a part's own picture changing means for the rest of the app.
#[derive(Clone)]
pub(crate) enum Changed {
    /// The game's own cover.
    Cover(Cover),
    /// The own cover dropped: it is looked up afresh.
    CoverReset,
    /// The banner or logo: the page's art is read again.
    PageArt,
    /// The icon: read again.
    Icon,
}

/// Where a part's picture comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Yours,
    SteamGridDb,
    Store,
    GameFiles,
    MadeFromIcon,
    Nothing,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Yours => "Your pick",
            Source::SteamGridDb => "From SteamGridDB",
            Source::Store => "From the store",
            Source::GameFiles => "From the game's files",
            Source::MadeFromIcon => "Made from the icon",
            Source::Nothing => "None yet",
        }
    }

    /// The user's own: Use the store's can drop it.
    fn own(self) -> bool {
        matches!(self, Source::Yours | Source::SteamGridDb)
    }
}

/// Each part's picture now, and where it comes from.
#[derive(Clone, Default)]
pub(crate) struct Previews {
    parts: BTreeMap<Kind, (Option<Arc<Picture>>, Source)>,
}

impl Previews {
    fn get(&self, kind: Kind) -> (Option<&Arc<Picture>>, Source) {
        self.parts
            .get(&kind)
            .map_or((None, Source::Nothing), |(picture, source)| {
                (picture.as_ref(), *source)
            })
    }
}

/// Looking something up on SteamGridDB.
#[derive(Clone, Default)]
enum Lookup<T> {
    #[default]
    Idle,
    Looking,
    Found(T),
    Failed(String),
}

/// A kind's pictures listed so far.
#[derive(Default)]
struct List {
    pictures: Vec<sgdb::Picture>,
    /// Pages asked for.
    pages: u32,
    more: bool,
    loading: bool,
    error: Option<String>,
}

/// A thumbnail: being made, ready, or not to be had.
enum Thumb {
    Loading,
    Ready(Arc<Picture>),
    Failed,
}

/// The open dialog.
struct Open {
    game: String,
    tab: Kind,
    previews: Option<Previews>,
    chosen: BTreeMap<Kind, u64>,
    /// The game on SteamGridDB: `Found(None)` when it has none.
    sgdb_game: Lookup<Option<SgdbGame>>,
    query: String,
    results: Lookup<Vec<SgdbGame>>,
    lists: HashMap<Kind, List>,
    /// A picture or file being made a part's own.
    applying: Option<Kind>,
}

#[derive(Default)]
pub(crate) struct ArtworkState {
    open: Option<Open>,
    /// The SteamGridDB key, once read: `Some(None)` when there is none.
    key: Option<Option<String>>,
    /// Settings: the key being typed.
    key_draft: String,
    key_busy: bool,
    /// Thumbnails, by picture id, kept while the app runs (small, and asked for again when
    /// another game shows the same).
    thumbs: HashMap<u64, Thumb>,
}

impl ArtworkState {
    /// Whether the dialog is open.
    pub fn is_open(&self) -> bool {
        self.open.is_some()
    }

    fn key(&self) -> Option<&str> {
        self.key.as_ref().and_then(Option::as_deref)
    }
}

/// A tab's name and what its part is.
fn describe(kind: Kind) -> (&'static str, &'static str) {
    match kind {
        Kind::Cover => ("Cover", "The picture on the game's tile in the Library."),
        Kind::Banner => ("Banner", "The wide picture at the top of the game's page."),
        Kind::Logo => ("Logo", "The title drawn on the banner."),
        Kind::Icon => ("Icon", "The small picture in lists and menus."),
    }
}

/// The page art part a tab sets.
fn page_part(kind: Kind) -> Option<Part> {
    match kind {
        Kind::Banner => Some(Part::Background),
        Kind::Logo => Some(Part::Logo),
        Kind::Cover | Kind::Icon => None,
    }
}

/// An error for the dialog: SteamGridDB refusing the key says so.
fn sgdb_error(error: &gm_core::Error) -> String {
    match error {
        gm_core::Error::Http { status: 401, .. } | gm_core::Error::Http { status: 403, .. } => {
            "SteamGridDB refused the key: check it in Settings.".to_owned()
        }
        error => format!("SteamGridDB: {error}"),
    }
}

/// Makes `file` the game's own `kind`. Blocking.
fn set_own(game: &Game, kind: Kind, file: &std::path::Path) -> gm_core::Result<Changed> {
    match (kind, page_part(kind)) {
        (Kind::Cover, _) => gm_core::covers::set_own(&game.id, file).map(Changed::Cover),
        (Kind::Icon, _) => gm_core::icon::set_own(&game.id, file).map(|()| Changed::Icon),
        (_, Some(part)) => page_art::set_own(&game.id, part, file).map(|()| Changed::PageArt),
        (_, None) => Ok(Changed::PageArt),
    }
}

/// Drops the game's own `kind`. Blocking.
fn clear_own(game: &Game, kind: Kind) -> gm_core::Result<Changed> {
    match (kind, page_part(kind)) {
        (Kind::Cover, _) => gm_core::covers::clear_own(game).map(|()| Changed::CoverReset),
        (Kind::Icon, _) => gm_core::icon::clear_own(&game.id).map(|()| Changed::Icon),
        (_, Some(part)) => page_art::clear_own(&game.id, part).map(|()| Changed::PageArt),
        (_, None) => Ok(Changed::PageArt),
    }
}

impl GameManager {
    /// Reads the SteamGridDB key at start, in the background.
    pub(crate) fn read_sgdb_key(context: &Ctx<Message>) {
        context.spawn_background(|| {
            ArtworkMessage::KeyRead(sgdb::key().map_err(|e| e.to_string())).into()
        });
    }

    /// Reads and decodes what each part shows now, for the open dialog (again when its icons have
    /// been read anew, `icons.rs`).
    pub(crate) fn request_previews(&self, context: &Ctx<Message>) {
        let Some(open) = &self.artwork.open else {
            return;
        };
        let Some(game) = self.games.iter().find(|game| game.id == open.game).cloned() else {
            return;
        };
        let cover = self.covers.file(&game.id).map(PathBuf::from);
        let own_cover = self.covers.is_own(&game.id);
        let icon = self.icons.bytes(&game.id).cloned();
        let icon_px = (ICON_PREVIEW * context.scale()).round() as u32;
        context.spawn_background(move || {
            let chosen = sgdb::chosen(&game.id);
            let mine = |kind: Kind| {
                if chosen.contains_key(&kind) {
                    Source::SteamGridDb
                } else {
                    Source::Yours
                }
            };
            let mut parts = BTreeMap::new();
            let decoded = |file: &std::path::Path, logo: bool| {
                let bytes = std::fs::read(file).ok()?;
                let picture = if logo {
                    image::decode_logo(&bytes)
                } else {
                    image::decode_cover(&bytes)
                };
                picture.map(Arc::new)
            };
            // The cover: the user's, the store's, else made from the icon.
            let cover = match cover {
                Some(file) => {
                    let source = if own_cover {
                        mine(Kind::Cover)
                    } else {
                        Source::Store
                    };
                    (decoded(&file, false), source)
                }
                None => match icon
                    .as_deref()
                    .and_then(|bytes| image::decode_capsule(bytes))
                {
                    Some(capsule) => (Some(Arc::new(capsule)), Source::MadeFromIcon),
                    None => (None, Source::Nothing),
                },
            };
            parts.insert(Kind::Cover, cover);
            // The banner and the logo, as the page has them on this PC.
            let (art, _) = page_art::local(&game);
            for (kind, part, logo) in [
                (Kind::Banner, art.background, false),
                (Kind::Logo, art.logo, true),
            ] {
                let preview = match part {
                    Some(part) => {
                        let source = if part.own { mine(kind) } else { Source::Store };
                        (decoded(&part.file, logo), source)
                    }
                    None => (None, Source::Nothing),
                };
                parts.insert(kind, preview);
            }
            // The icon.
            let icon_preview = match icon {
                Some(bytes) => {
                    let source = if gm_core::icon::is_own(&game.id) {
                        mine(Kind::Icon)
                    } else {
                        Source::GameFiles
                    };
                    let picture = image::decode_icons(vec![(game.id.clone(), bytes)], [icon_px; 2])
                        .pop()
                        .map(|(_, [picture, _])| Arc::new(picture));
                    (picture, source)
                }
                None => (None, Source::Nothing),
            };
            parts.insert(Kind::Icon, icon_preview);
            ArtworkMessage::Previewed(game.id, Previews { parts }, chosen).into()
        });
    }

    /// Finds the open dialog's game on SteamGridDB, when there is a key.
    fn find_sgdb_game(&mut self, context: &Ctx<Message>) {
        let Some(key) = self.artwork.key().map(str::to_owned) else {
            return;
        };
        let Some(open) = &mut self.artwork.open else {
            return;
        };
        if !matches!(open.sgdb_game, Lookup::Idle) {
            return;
        }
        let Some(game) = self.games.iter().find(|game| game.id == open.game).cloned() else {
            return;
        };
        open.sgdb_game = Lookup::Looking;
        context.spawn_background(move || {
            let found = sgdb::find_game(&key, &game).map_err(|e| sgdb_error(&e));
            ArtworkMessage::GameFound(game.id, found).into()
        });
    }

    /// Lists the next page of the open tab's pictures, when none is being listed.
    fn list_pictures(&mut self, context: &Ctx<Message>) {
        let Some(key) = self.artwork.key().map(str::to_owned) else {
            return;
        };
        let Some(open) = &mut self.artwork.open else {
            return;
        };
        let Lookup::Found(Some(game)) = &open.sgdb_game else {
            return;
        };
        let (game, kind) = (game.id, open.tab);
        let list = open.lists.entry(kind).or_default();
        if list.loading || (list.pages > 0 && !list.more) {
            return;
        }
        list.loading = true;
        let page = list.pages;
        context.spawn_background(move || {
            let listed = sgdb::pictures(&key, kind, game, page).map_err(|e| sgdb_error(&e));
            ArtworkMessage::Listed(game, kind, page, listed).into()
        });
    }

    /// Downloads and decodes the thumbnails of `pictures` not had yet, each sent when ready.
    fn load_thumbs(&mut self, pictures: &[sgdb::Picture], context: &Ctx<Message>) {
        let wanted: Vec<sgdb::Picture> = pictures
            .iter()
            .filter(|picture| !self.artwork.thumbs.contains_key(&picture.id))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return;
        }
        for picture in &wanted {
            self.artwork.thumbs.insert(picture.id, Thumb::Loading);
        }
        // The still ones first: an animated one downloads the picture itself.
        let (animated, still): (Vec<_>, Vec<_>) =
            wanted.into_iter().partition(|picture| picture.animated);
        // Dealt out in turn, so the first rows fill first.
        let mut lists = vec![Vec::new(); THUMB_JOBS];
        for (index, picture) in still.into_iter().chain(animated).enumerate() {
            lists[index % THUMB_JOBS].push(picture);
        }
        for list in lists.into_iter().filter(|list| !list.is_empty()) {
            context.spawn_stream(move |send| {
                for picture in list {
                    let thumb = sgdb::preview(&picture)
                        .and_then(|bytes| image::decode_thumbnail(&bytes, THUMB_PIXELS))
                        .map(Arc::new);
                    send(ArtworkMessage::Thumb(picture.id, thumb).into());
                }
            });
        }
    }

    /// The open dialog's game.
    fn artwork_game(&self) -> Option<Game> {
        let open = self.artwork.open.as_ref()?;
        self.games.iter().find(|game| game.id == open.game).cloned()
    }

    pub(crate) fn handle_artwork(&mut self, message: ArtworkMessage, context: &Ctx<Message>) {
        match message {
            ArtworkMessage::Open(id, tab) => {
                if !self.games.iter().any(|game| game.id == id) {
                    return;
                }
                self.artwork.open = Some(Open {
                    game: id,
                    tab,
                    previews: None,
                    chosen: BTreeMap::new(),
                    sgdb_game: Lookup::Idle,
                    query: String::new(),
                    results: Lookup::Idle,
                    lists: HashMap::new(),
                    applying: None,
                });
                self.request_previews(context);
                self.find_sgdb_game(context);
            }
            ArtworkMessage::Close => self.artwork.open = None,
            ArtworkMessage::Tab(tab) => {
                if let Some(open) = &mut self.artwork.open {
                    open.tab = tab;
                }
                self.list_pictures(context);
            }
            ArtworkMessage::Previewed(id, previews, chosen) => {
                if let Some(open) = &mut self.artwork.open
                    && open.game == id
                {
                    open.previews = Some(previews);
                    open.chosen = chosen;
                }
            }
            ArtworkMessage::ChooseFile => {
                let (Some(game), Some(open)) = (self.artwork_game(), &self.artwork.open) else {
                    return;
                };
                let kind = open.tab;
                let (name, _) = describe(kind);
                let extensions: &[&str] = if kind == Kind::Icon {
                    &["ico", "png", "jpg", "jpeg", "webp", "bmp"]
                } else {
                    &["jpg", "jpeg", "png", "webp", "bmp"]
                };
                // Owned by the app's window, which is disabled while the dialog is open.
                self.picking = true;
                let id = game.id.clone();
                FilePicker::new()
                    .title(format!(
                        "Choose a {} for {}",
                        name.to_lowercase(),
                        game.name
                    ))
                    .filter_extensions("Pictures", extensions.iter().copied())
                    .filter_all()
                    .request(context, move |picked| {
                        let picked = picked.map_err(|e| format!("could not choose a picture: {e}"));
                        ArtworkMessage::Picked(id, kind, picked).into()
                    });
            }
            ArtworkMessage::Picked(id, kind, Ok(Some(file))) => {
                self.picking = false;
                let Some(game) = self.games.iter().find(|game| game.id == id).cloned() else {
                    return;
                };
                if let Some(open) = &mut self.artwork.open {
                    open.applying = Some(kind);
                }
                // Reading and checking the picture are blocking work.
                context.spawn_background(move || {
                    let saved = set_own(&game, kind, &file).and_then(|changed| {
                        sgdb::record(&game.id, kind, None)?;
                        Ok(changed)
                    });
                    ArtworkMessage::Saved(game.id, saved.map_err(|e| e.to_string())).into()
                });
            }
            ArtworkMessage::Picked(_, _, Ok(None)) => self.picking = false,
            ArtworkMessage::Picked(_, _, Err(e)) => {
                self.picking = false;
                self.error = Some(e);
            }
            ArtworkMessage::UseStore => {
                let (Some(game), Some(open)) = (self.artwork_game(), &mut self.artwork.open) else {
                    return;
                };
                let kind = open.tab;
                open.applying = Some(kind);
                context.spawn_background(move || {
                    let saved = clear_own(&game, kind).and_then(|changed| {
                        sgdb::record(&game.id, kind, None)?;
                        Ok(changed)
                    });
                    ArtworkMessage::Saved(game.id, saved.map_err(|e| e.to_string())).into()
                });
            }
            ArtworkMessage::Apply(picture) => {
                let (Some(game), Some(open)) = (self.artwork_game(), &mut self.artwork.open) else {
                    return;
                };
                if open.applying.is_some() {
                    return;
                }
                let kind = open.tab;
                open.applying = Some(kind);
                context.spawn_background(move || {
                    let saved = sgdb::download(&picture)
                        .and_then(|file| set_own(&game, kind, &file))
                        .and_then(|changed| {
                            sgdb::record(&game.id, kind, Some(picture.id))?;
                            Ok(changed)
                        });
                    ArtworkMessage::Saved(game.id, saved.map_err(|e| e.to_string())).into()
                });
            }
            ArtworkMessage::Saved(id, saved) => {
                if let Some(open) = &mut self.artwork.open {
                    open.applying = None;
                }
                match saved {
                    Ok(Changed::Cover(cover)) => {
                        self.add_cover(id, cover);
                        self.decode_near_covers(context);
                    }
                    Ok(Changed::CoverReset) => {
                        // Without its own, the game's cover is looked up afresh.
                        self.covers.forget(&id);
                        self.load_covers(true, false, context);
                    }
                    Ok(Changed::PageArt) => self.page_art.reload(&id),
                    Ok(Changed::Icon) => {
                        if let Some(game) = self.games.iter().find(|game| game.id == id).cloned() {
                            self.icons.forget(&id);
                            Self::load_icons(vec![game], false, context);
                        }
                    }
                    Err(e) => self.error = Some(format!("Artwork: {e}")),
                }
                self.request_previews(context);
            }
            ArtworkMessage::KeyRead(Ok(key)) => {
                self.artwork.key = Some(key);
                self.find_sgdb_game(context);
            }
            ArtworkMessage::KeyRead(Err(e)) => {
                self.artwork.key = Some(None);
                self.error = Some(format!("SteamGridDB key: {e}"));
            }
            ArtworkMessage::KeyDraft(draft) => self.artwork.key_draft = draft,
            ArtworkMessage::SaveKey => {
                let key = self.artwork.key_draft.trim().to_owned();
                if key.is_empty() || self.artwork.key_busy {
                    return;
                }
                self.artwork.key_busy = true;
                context.spawn_background(move || {
                    let saved = sgdb::set_key(&key).map(|()| Some(key));
                    ArtworkMessage::KeySaved(saved.map_err(|e| e.to_string())).into()
                });
            }
            ArtworkMessage::RemoveKey => {
                if self.artwork.key_busy {
                    return;
                }
                self.artwork.key_busy = true;
                context.spawn_background(|| {
                    let removed = sgdb::remove_key().map(|()| None);
                    ArtworkMessage::KeySaved(removed.map_err(|e| e.to_string())).into()
                });
            }
            ArtworkMessage::KeySaved(saved) => {
                self.artwork.key_busy = false;
                match saved {
                    Ok(key) => {
                        self.artwork.key = Some(key);
                        self.artwork.key_draft.clear();
                    }
                    Err(e) => self.error = Some(format!("SteamGridDB key: {e}")),
                }
            }
            ArtworkMessage::OpenKeyPage => {
                self.error = gm_core::launch(&Launch::Uri(sgdb::KEY_PAGE.to_owned()))
                    .err()
                    .map(|e| format!("Could not open SteamGridDB: {e}"));
            }
            ArtworkMessage::GameFound(id, found) => {
                let Some(open) = &mut self.artwork.open else {
                    return;
                };
                if open.game != id || !matches!(open.sgdb_game, Lookup::Looking) {
                    return;
                }
                match found {
                    Ok(sgdb::Found::Game(game)) => open.sgdb_game = Lookup::Found(Some(game)),
                    // No game by its name: what was found, to choose from.
                    Ok(sgdb::Found::Choices(games)) => {
                        open.sgdb_game = Lookup::Found(None);
                        open.query = game_name(&self.games, &id);
                        open.results = Lookup::Found(games);
                    }
                    Err(e) => open.sgdb_game = Lookup::Failed(e),
                }
                self.list_pictures(context);
            }
            ArtworkMessage::Query(query) => {
                if let Some(open) = &mut self.artwork.open {
                    open.query = query;
                }
            }
            ArtworkMessage::Search => {
                let Some(key) = self.artwork.key().map(str::to_owned) else {
                    return;
                };
                let Some(open) = &mut self.artwork.open else {
                    return;
                };
                let query = open.query.trim().to_owned();
                if query.is_empty() {
                    return;
                }
                open.results = Lookup::Looking;
                context.spawn_background(move || {
                    let found = sgdb::search(&key, &query).map_err(|e| sgdb_error(&e));
                    ArtworkMessage::Searched(found).into()
                });
            }
            ArtworkMessage::ClearSearch => {
                if let Some(open) = &mut self.artwork.open {
                    open.query.clear();
                    open.results = Lookup::Idle;
                }
            }
            ArtworkMessage::Searched(found) => {
                if let Some(open) = &mut self.artwork.open
                    && matches!(open.results, Lookup::Looking)
                {
                    open.results = match found {
                        Ok(games) => Lookup::Found(games),
                        Err(e) => Lookup::Failed(e),
                    };
                }
            }
            ArtworkMessage::PickGame(game) => {
                if let Some(open) = &mut self.artwork.open {
                    open.sgdb_game = Lookup::Found(Some(game));
                    open.results = Lookup::Idle;
                    open.query.clear();
                    open.lists.clear();
                }
                self.list_pictures(context);
            }
            ArtworkMessage::Listed(game, kind, page, listed) => {
                let Some(open) = &mut self.artwork.open else {
                    return;
                };
                // Kept only for the game shown now.
                if !matches!(&open.sgdb_game, Lookup::Found(Some(found)) if found.id == game) {
                    return;
                }
                let list = open.lists.entry(kind).or_default();
                list.loading = false;
                match listed {
                    Ok((pictures, more)) => {
                        list.pages = page + 1;
                        list.more = more;
                        list.error = None;
                        list.pictures.extend(pictures.iter().cloned());
                        self.load_thumbs(&pictures, context);
                    }
                    Err(e) => list.error = Some(e),
                }
            }
            ArtworkMessage::More => self.list_pictures(context),
            ArtworkMessage::Thumb(id, thumb) => {
                let thumb = match thumb {
                    Some(picture) => Thumb::Ready(picture),
                    None => Thumb::Failed,
                };
                self.artwork.thumbs.insert(id, thumb);
            }
            ArtworkMessage::OpenSettings => {
                self.artwork.open = None;
                self.go_to(Page::Settings, context);
            }
        }
    }

    /// The Artwork dialog, while it is open.
    pub(crate) fn artwork_dialog(&self, window: &WindowInfo) -> Option<El<Message>> {
        let open = self.artwork.open.as_ref()?;
        let game = self.games.iter().find(|game| game.id == open.game)?;
        // The card's padding, and the body's room for its scroll bar (`app_view::dialog_card`).
        let width = wide_dialog_width(window) - 2.0 * theme::space::XL - theme::space::M;
        let tabs = segmented(
            Kind::ALL
                .iter()
                .map(|&kind| {
                    let (name, _) = describe(kind);
                    (name, open.tab == kind, ArtworkMessage::Tab(kind).into())
                })
                .collect(),
        );
        let body = vec![
            tabs,
            self.artwork_current(open, game),
            self.artwork_sgdb(open, game, width),
        ];
        Some(wide_dialog(
            window,
            &format!("Artwork of {}", game.name),
            body,
            ("Done", ArtworkMessage::Close.into()),
        ))
    }

    /// The tab's part now: its picture, where it comes from, and its buttons.
    fn artwork_current(&self, open: &Open, game: &Game) -> El<Message> {
        let kind = open.tab;
        let (name, line) = describe(kind);
        let (picture, source) = open
            .previews
            .as_ref()
            .map_or((None, Source::Nothing), |previews| previews.get(kind));
        let (w, h) = match kind {
            Kind::Cover => (PREVIEW_HEIGHT * 2.0 / 3.0, PREVIEW_HEIGHT),
            Kind::Banner => (PREVIEW_HEIGHT * BANNER_SHAPE, PREVIEW_HEIGHT),
            Kind::Logo => (PREVIEW_HEIGHT * LOGO_SHAPE, PREVIEW_HEIGHT),
            Kind::Icon => (PREVIEW_HEIGHT, PREVIEW_HEIGHT),
        };
        let key = format!("artwork-now:{kind:?}:{}", game.id);
        let ground = row([])
            .size(w, h)
            .bg(theme::DARK.base)
            .radius(theme::RADIUS_CARD);
        let shown = match (picture, kind) {
            (Some(picture), Kind::Cover | Kind::Banner) => image(key, Arc::clone(picture))
                .cover()
                .radius(theme::RADIUS_CARD)
                .size(w, h),
            (Some(picture), Kind::Logo) => layers([
                ground,
                row([image(key, Arc::clone(picture)).size(w * 0.85, h * 0.7)])
                    .justify(Align::Center)
                    .size(w, h),
            ]),
            (Some(picture), Kind::Icon) => layers([
                ground,
                row([image(key, Arc::clone(picture)).size(ICON_PREVIEW, ICON_PREVIEW)])
                    .justify(Align::Center)
                    .size(w, h),
            ]),
            (None, _) => layers([
                ground,
                row([glyph(g::PICTURE, theme::ICON_SIZE * 2.0).color(theme::DARK.text_tertiary)])
                    .justify(Align::Center)
                    .size(w, h),
            ]),
        };
        let tint = if source.own() {
            theme::accent()
        } else {
            theme::text_secondary()
        };
        let busy = open.applying.is_some();
        let store = if kind == Kind::Icon {
            "Use the game's"
        } else {
            "Use the store's"
        };
        let actions = row([
            accent_button("Choose a file…")
                .enabled(!busy && !self.picking)
                .on_click(ArtworkMessage::ChooseFile.into()),
            button(store)
                .enabled(!busy && source.own())
                .on_click(ArtworkMessage::UseStore.into()),
        ])
        .gap(theme::space::S);
        let mut about = vec![
            text(name).text_size(theme::TEXT_SUBTITLE).bold(),
            row([chip(source.label(), tint)]),
            hint(line),
            actions,
        ];
        if busy {
            about.push(working("Saving…"));
        }
        card([row([shown, col(about).gap(theme::space::S).fill_width()])
            .gap(theme::space::L)
            .valign(Align::Start)])
    }

    /// SteamGridDB's pictures for the tab, and the search for another game.
    fn artwork_sgdb(&self, open: &Open, game: &Game, width: f32) -> El<Message> {
        let subtitle = match (&self.artwork.key, &open.sgdb_game) {
            (Some(None), _) => "Covers, banners, logos and icons people made.".to_owned(),
            (_, Lookup::Found(Some(found))) => format!("Pictures for {}", found.name),
            (_, Lookup::Found(None)) => format!("No game named {} there", game.name),
            _ => format!("Pictures for {}", game.name),
        };
        let mut search = vec![
            text_box(
                "artwork:search",
                open.query.clone(),
                "Search another game",
                "Search another game on SteamGridDB",
                |query| ArtworkMessage::Query(query).into(),
            )
            .on_enter(ArtworkMessage::Search.into())
            .clear_on_escape()
            .fill_width(),
            button("Search")
                .enabled(!open.query.trim().is_empty())
                .on_click(ArtworkMessage::Search.into()),
        ];
        if !matches!(open.results, Lookup::Idle) {
            search.push(button("Cancel").on_click(ArtworkMessage::ClearSearch.into()));
        }
        let header = row([
            col([
                text("From SteamGridDB")
                    .text_size(theme::TEXT_SUBTITLE)
                    .bold(),
                hint(subtitle),
            ])
            .gap(theme::space::XXS)
            .fill_width(),
            row(search)
                .gap(theme::space::S)
                .width((width * 0.45).max(260.0))
                .enabled(self.artwork.key().is_some()),
        ])
        .gap(theme::space::L);
        let content = match (&self.artwork.key, &open.results, &open.sgdb_game) {
            (None, _, _) => working("Reading your SteamGridDB key…"),
            (Some(None), _, _) => col([
                hint(
                    "SteamGridDB needs your own API key: add it in Settings, then choose from its \
                     pictures here.",
                )
                .wrap(),
                row(
                    [
                        button("Add a key in Settings")
                            .on_click(ArtworkMessage::OpenSettings.into()),
                    ],
                ),
            ])
            .gap(theme::space::S),
            (_, Lookup::Looking, _) => working("Searching SteamGridDB…"),
            (_, Lookup::Failed(e), _) => hint(e.clone()).wrap(),
            (_, Lookup::Found(games), _) => search_results(games),
            (_, Lookup::Idle, Lookup::Idle | Lookup::Looking) => {
                working("Looking for the game on SteamGridDB…")
            }
            (_, Lookup::Idle, Lookup::Failed(e)) => hint(e.clone()).wrap(),
            (_, Lookup::Idle, Lookup::Found(None)) => {
                hint("SteamGridDB has no game by this name: search another one.").wrap()
            }
            (_, Lookup::Idle, Lookup::Found(Some(_))) => self.picture_grid(open, width),
        };
        col([header, content]).gap(theme::space::M)
    }

    /// The tab's pictures as a grid, with Show more while there may be more.
    fn picture_grid(&self, open: &Open, width: f32) -> El<Message> {
        let kind = open.tab;
        let Some(list) = open.lists.get(&kind) else {
            return working("Listing SteamGridDB's pictures…");
        };
        if list.pictures.is_empty() {
            return match (&list.error, list.loading) {
                (Some(e), _) => hint(e.clone()).wrap(),
                (None, true) => working("Listing SteamGridDB's pictures…"),
                (None, false) => {
                    let (name, _) = describe(kind);
                    hint(format!(
                        "SteamGridDB has no {} for this game yet.",
                        name.to_lowercase()
                    ))
                }
            };
        }
        let (smallest, shape) = match kind {
            Kind::Cover => (COVER_THUMB, 2.0 / 3.0),
            Kind::Banner => (BANNER_THUMB, BANNER_SHAPE),
            Kind::Logo => (LOGO_THUMB, LOGO_SHAPE),
            Kind::Icon => (ICON_THUMB, 1.0),
        };
        let gap = theme::space::M;
        let columns = (((width + gap) / (smallest + gap)).floor() as usize).max(1);
        let tile_w = (width - gap * (columns as f32 - 1.0)) / columns as f32;
        let tile_h = tile_w / shape;
        let chosen = open.chosen.get(&kind).copied();
        let mut rows: Vec<El<Message>> = list
            .pictures
            .chunks(columns)
            .map(|line| {
                row(line
                    .iter()
                    .map(|picture| self.thumb_tile(picture, kind, (tile_w, tile_h), chosen)))
                .gap(gap)
                .halign(Align::Start)
            })
            .collect();
        if list.loading {
            rows.push(working("Listing more…"));
        } else if list.more {
            rows.push(row([
                button("Show more").on_click(ArtworkMessage::More.into())
            ]));
        }
        if let Some(e) = &list.error {
            rows.push(hint(e.clone()).wrap());
        }
        col(rows).gap(gap)
    }

    /// One SteamGridDB picture: its thumbnail (on a dark tile for logos and icons), a check when it
    /// is the one chosen, a chip when it is animated. A click chooses it.
    fn thumb_tile(
        &self,
        picture: &sgdb::Picture,
        kind: Kind,
        (w, h): (f32, f32),
        chosen: Option<u64>,
    ) -> El<Message> {
        let key = format!("sgdb-thumb:{}", picture.id);
        let ground = row([])
            .size(w, h)
            .bg(theme::DARK.base)
            .radius(theme::RADIUS_CARD);
        let mut tile = vec![ground];
        match self.artwork.thumbs.get(&picture.id) {
            Some(Thumb::Ready(thumb)) => tile.push(match kind {
                Kind::Cover | Kind::Banner => image(key, Arc::clone(thumb))
                    .cover()
                    .radius(theme::RADIUS_CARD)
                    .size(w, h),
                Kind::Logo | Kind::Icon => {
                    let fit = if kind == Kind::Logo { 0.82 } else { 0.7 };
                    row([image(key, Arc::clone(thumb)).size(w * fit, h * fit)])
                        .justify(Align::Center)
                        .size(w, h)
                }
            }),
            Some(Thumb::Failed) => tile.push(
                row([glyph(g::PICTURE, theme::ICON_SIZE).color(theme::DARK.text_tertiary)])
                    .justify(Align::Center)
                    .size(w, h),
            ),
            Some(Thumb::Loading) | None => {}
        }
        let selected = chosen == Some(picture.id);
        let mut marks = Vec::new();
        if picture.animated {
            marks.push(chip("Animated", theme::DARK.text));
        }
        if !marks.is_empty() || selected {
            let mut corner = vec![row(marks).fill_width()];
            if selected {
                corner.push(
                    row([glyph(g::CHECK, 12.0).color(theme::text_on_accent())])
                        .pad(theme::space::XS)
                        .bg(theme::accent())
                        .radius(theme::RADIUS_CHIP),
                );
            }
            // Along the top: a row alone would centre them in the tile's height.
            tile.push(
                col([row(corner).fill_width()])
                    .pad(theme::space::S)
                    .size(w, h)
                    .valign(Align::Start),
            );
        }
        let mut name = format!("{}×{}", picture.width, picture.height);
        if picture.animated {
            name.push_str(", animated");
        }
        if let Some(author) = picture
            .author
            .as_ref()
            .filter(|author| !author.name.is_empty())
        {
            name.push_str(&format!(", by {}", author.name));
        }
        // The chosen one's outline, over the picture (an element's own stroke is under its
        // children).
        if selected {
            tile.push(
                row([])
                    .size(w, h)
                    .stroke(theme::accent(), 2.0)
                    .radius(theme::RADIUS_CARD),
            );
        }
        layers(tile)
            .size(w, h)
            .radius(theme::RADIUS_CARD)
            .hover_overlay(theme::OVER_PICTURE_HOVER)
            .role(Role::Button)
            .name(format!("Choose this picture ({name})"))
            .tooltip(name)
            .on_click(ArtworkMessage::Apply(picture.clone()).into())
    }

    /// Settings ▸ SteamGridDB: the user's API key, saved in Credential Manager.
    pub(crate) fn steamgriddb_card(&self) -> El<Message> {
        let header = card_header(
            g::PICTURE,
            theme::cyan(),
            "SteamGridDB",
            "Covers, banners, logos and icons people made, to choose from in a game's Artwork.",
        );
        let busy = self.artwork.key_busy;
        let content = match &self.artwork.key {
            None => working("Reading your key…"),
            Some(Some(_)) => col([
                field_label("API key"),
                row([
                    text("Your key is saved, encrypted for your Windows account (Credential Manager).")
                        .wrap()
                        .fill_width(),
                    button("Remove")
                        .enabled(!busy)
                        .on_click(ArtworkMessage::RemoveKey.into()),
                ])
                .gap(theme::space::M),
            ])
            .gap(theme::space::S),
            Some(None) => col([
                field_label("API key"),
                row([
                    text_box(
                        "settings:sgdb-key",
                        self.artwork.key_draft.clone(),
                        "Paste your SteamGridDB API key",
                        "SteamGridDB API key",
                        |draft| ArtworkMessage::KeyDraft(draft).into(),
                    )
                    .on_enter(ArtworkMessage::SaveKey.into())
                    .fill_width(),
                    accent_button("Save")
                        .enabled(!busy && !self.artwork.key_draft.trim().is_empty())
                        .on_click(ArtworkMessage::SaveKey.into()),
                ])
                .gap(theme::space::S),
                row([
                    hint("Make one in your SteamGridDB profile:"),
                    link("Preferences ▸ API", ArtworkMessage::OpenKeyPage.into()),
                ])
                .gap(theme::space::XS),
            ])
            .gap(theme::space::S),
        };
        card([header, content]).gap(theme::space::L)
    }
}

/// A game's name, by id.
fn game_name(games: &[Game], id: &str) -> String {
    games
        .iter()
        .find(|game| game.id == id)
        .map(|game| game.name.clone())
        .unwrap_or_default()
}

/// The games a search found, each a button that shows its pictures.
fn search_results(games: &[SgdbGame]) -> El<Message> {
    if games.is_empty() {
        return hint("SteamGridDB found no game by that name.");
    }
    let mut rows = vec![hint("Choose the game:")];
    for found in games {
        let mut label = found.name.clone();
        if let Some(year) = found.release_date.and_then(year_of) {
            label.push_str(&format!(" ({year})"));
        }
        rows.push(row([
            button(label).on_click(ArtworkMessage::PickGame(found.clone()).into())
        ]));
    }
    col(rows).gap(theme::space::XS)
}

/// The year of a date in seconds since 1970 (UTC), from the civil calendar.
fn year_of(seconds: i64) -> Option<i64> {
    let days = seconds.div_euclid(86_400);
    // Howard Hinnant's days-to-civil, for the year only.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year > 1900).then_some(year)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_years_come_from_seconds() {
        // Tomb Raider: Underworld, 2008-11-21.
        assert_eq!(year_of(1_227_225_600), Some(2008));
        assert_eq!(year_of(0), Some(1970));
        assert_eq!(year_of(951_782_400), Some(2000)); // 2000-02-29
        assert_eq!(year_of(-3_000_000_000), None);
    }

    #[test]
    fn a_part_says_where_it_comes_from() {
        assert!(Source::Yours.own() && Source::SteamGridDb.own());
        assert!(!Source::Store.own() && !Source::MadeFromIcon.own());
        let previews = Previews::default();
        assert_eq!(previews.get(Kind::Logo).1, Source::Nothing);
        assert_eq!(Source::SteamGridDb.label(), "From SteamGridDB");
    }
}
