//! Cover art for the Library's tiles: each game's portrait cover as a small JPEG.
//!
//! **Where a cover comes from**, the first that exists:
//! 1. The user's own image, `%APPDATA%\Game Manager\covers\<game id>.jpg` (`set_own`).
//! 2. The cover cache, `%LOCALAPPDATA%\Game Manager\cache\covers`.
//! 3. The store's own files on this PC, with no network: Steam's library cache and GOG Galaxy's
//!    web cache (`steam::cached_cover`, `gog::cached_cover`), and the small picture an older
//!    Ubisoft Connect cached (`ubisoft::Launcher::cached_cover`). That one is 240 × 280, less
//!    than a tile: it shows until step 5 has answered, and a cover found there replaces it.
//! 4. The store's server, by the game's own id: Steam's store API, the address in the Epic
//!    launcher's catalog cache, GOG's API, Microsoft's store catalog for Xbox games.
//! 5. The Steam store by name, for a game of another store that still has none (EA and Ubisoft
//!    games and games added by hand have no other source). Only a result with the same name
//!    counts.
//!
//! `load_local` does 1 to 3 for a whole library and says which games the network may still
//! help; `fetch` does 4 and 5 for one game. Both block: call them off the UI thread.
//!
//! **Thumbnails**: every cover is kept once as a JPEG cropped to 2:3 around its centre and at
//! most 400 × 600, made with WIC (decode, convert, clip, scale, encode) as `icon.rs` makes PNGs.
//! WIC reads JPEG, PNG and BMP itself and WebP (GOG Galaxy's) through Windows' WebP extension;
//! without it a GOG game's cover comes from GOG's API instead.
//!
//! **The cache** holds only thumbnails, named after a hash of what they were made from: a
//! store's file (its path, length and modified time, so new art replaces it), a download (the
//! game's id) or a name lookup (the id and the name's letters). A lookup that found nothing
//! leaves an empty `.none` file and is not asked again for a week; a failed request leaves
//! nothing and is tried again at the next start.
//!
//! **Steam at high display scaling**: its client keeps 300 × 450 covers. When a tile needs more
//! pixels than that (`want_width`), the 600 × 900 one is fetched once, and replaces it.

use crate::com::ComGuard;
use crate::text::{fnv1a, letters};
use crate::{Error, Game, Result, Store, epic, gog, http, json_file, steam, ubisoft, xbox};
use std::cell::OnceCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use windows::Win32::{
    CLSCTX_INPROC_SERVER, CLSID_WICImagingFactory, CoCreateInstance, GENERIC_READ,
    GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR, IPropertyBag2, IStream,
    IWICBitmapDecoder, IWICBitmapFrameEncode, IWICImagingFactory, IWICPalette, LARGE_INTEGER,
    SHCreateMemStream, STATFLAG_NONAME, STATSTG, STREAM_SEEK_SET, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
    WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::core::HSTRING;

/// The size of a cached cover at most: enough for a 200 DIP wide tile at 200% display scaling.
pub const THUMB_WIDTH: u32 = 400;
pub const THUMB_HEIGHT: u32 = 600;
/// Bump when thumbnails are made differently, so covers cached by an older version are not
/// reused.
const CACHE_FORMAT: u32 = 1;
/// How long a lookup that found nothing is not asked again.
const MISS_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// A game's cover: a 2:3 JPEG, at most `THUMB_WIDTH` × `THUMB_HEIGHT`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    pub jpeg: Vec<u8>,
    /// The picture's width in pixels.
    pub width: u32,
    /// The user's own image (`set_own`), not a store's.
    pub own: bool,
}

impl Cover {
    fn new(jpeg: Vec<u8>, own: bool) -> Self {
        let width = jpeg_size(&jpeg).map_or(0, |(width, _)| width);
        Self { jpeg, width, own }
    }
}

/// The covers that need no network: the user's own, the cache, and the stores' files on this PC
/// (made into thumbnails and cached). `found` gets each one; the games returned are those the
/// network may still help: without a cover, or (Steam) with one narrower than `want_width`
/// pixels. Blocking.
pub fn load_local(
    games: &[Game],
    want_width: u32,
    found: &mut dyn FnMut(&Game, Cover),
) -> Vec<Game> {
    let Some(cache) = Cache::open() else {
        return Vec::new();
    };
    let own = own_dir().ok();
    let stores = Stores::find();
    // COM and WIC start only when a thumbnail really has to be made.
    let mut thumbnailer: Option<Option<Thumbnailer>> = None;
    let mut rest = Vec::new();
    for game in games {
        let mut make = |file: &Path| {
            thumbnailer
                .get_or_insert_with(|| Thumbnailer::new().ok())
                .as_ref()?
                .of_file(file)
                .ok()
        };
        let (cover, ask) =
            local_cover(game, want_width, &cache, own.as_deref(), &stores, &mut make);
        if let Some(cover) = cover {
            found(game, cover);
        }
        if ask {
            rest.push(game.clone());
        }
    }
    rest
}

/// A game's cover from its store's server, else from the Steam store by name; cached. `None`
/// when neither has one, which is remembered for a week. An error is a request that failed: it
/// is tried again next time. Blocking.
pub fn fetch(game: &Game) -> Result<Option<Cover>> {
    let Some(cache) = Cache::open() else {
        return Ok(None);
    };
    let thumbnailer = Thumbnailer::new().map_err(Error::windows("Windows Imaging"))?;
    fetch_into(game, &cache, &thumbnailer)
}

/// Makes `image` the game's cover, whatever its store has. Blocking.
pub fn set_own(game_id: &str, image: &Path) -> Result<Cover> {
    let thumbnailer = Thumbnailer::new().map_err(Error::windows("Windows Imaging"))?;
    set_own_in(&own_dir()?, game_id, image, &thumbnailer)
}

/// Drops the user's own cover of a game, and what was downloaded or looked up for it, so its
/// cover is found afresh (Reset cover; also when a game added by hand is removed). Blocking.
pub fn clear_own(game: &Game) -> Result<()> {
    clear_own_in(&own_dir()?, Cache::open().as_ref(), game)
}

/// Deletes cached covers no game uses any more. For a full library only: pruning after a partial
/// list would drop the other games' covers. The user's own images are never touched.
pub fn prune(games: &[Game]) {
    let Some(cache) = Cache::open() else {
        return;
    };
    let stores = Stores::find();
    let mut used = HashSet::new();
    for game in games {
        let local = stores
            .local_file(game)
            .and_then(|file| local_identity(&file));
        for identity in [Some(net_identity(game)), Some(name_identity(game)), local]
            .into_iter()
            .flatten()
        {
            used.insert(cache.file(&identity));
            used.insert(cache.miss_file(&identity));
        }
    }
    cache.prune(&used);
}

/// Where a game's cover comes from, or would be looked for, in a line: for `scan --covers`.
/// Reads only; nothing is downloaded or cached.
pub fn describe(game: &Game) -> String {
    let cache = Cache::existing();
    let cached = |identity: &str| cache.as_ref().is_some_and(|c| c.file(identity).is_file());
    let missed = |identity: &str| cache.as_ref().is_some_and(|c| c.missed(identity));
    if let Ok(dir) = own_dir()
        && own_file(&dir, &game.id).is_file()
    {
        return format!("your own image: {}", own_file(&dir, &game.id).display());
    }
    let net = net_identity(game);
    let name = name_identity(game);
    if cached(&net) {
        return format!("downloaded from {}", game.store.name());
    }
    if has_small_local(game.store) && cached(&name) {
        return "found in the Steam store by name".to_owned();
    }
    if let Some(file) = Stores::find().local_file(game) {
        // Only Steam's server has a sharper one than its client keeps.
        let (keeper, sharper) = match game.store {
            Store::Gog => ("GOG Galaxy", ""),
            Store::Ubisoft if missed(&name) => (
                "Ubisoft Connect",
                " (the Steam store has no game of this name)",
            ),
            Store::Ubisoft => (
                "Ubisoft Connect",
                " (a cover is looked up in the Steam store by name)",
            ),
            _ if missed(&net) => ("Steam", ""),
            _ => (
                "Steam",
                " (a sharper one is fetched for tiles wider than it)",
            ),
        };
        return format!("{keeper}'s own copy: {}{sharper}", file.display());
    }
    if cached(&name) {
        return "found in the Steam store by name".to_owned();
    }
    let mut steps = Vec::new();
    if has_id_source(game.store) {
        steps.push(match (missed(&net), game.store) {
            (true, _) => format!("{} had none", game.store.name()),
            (false, Store::Epic) => match epic::cover_url(game) {
                Some(url) => format!("to download: {url}"),
                None => "not in the Epic launcher's catalog cache".to_owned(),
            },
            (false, _) => format!("to ask {}", game.store.name()),
        });
    }
    if game.store != Store::Steam {
        steps.push(if missed(&name) {
            "the Steam store has no game of this name".to_owned()
        } else {
            "to look up in the Steam store by name".to_owned()
        });
    }
    steps.join("; ")
}

/// Where the stores keep covers on this PC, looked up once for a batch of games.
struct Stores {
    steam: Option<PathBuf>,
    galaxy: Option<PathBuf>,
    /// Ubisoft Connect's folder and old configuration cache, read when a Ubisoft game asks.
    ubisoft: OnceCell<Option<ubisoft::Launcher>>,
}

impl Stores {
    fn find() -> Self {
        Self {
            steam: steam::install_dir(),
            galaxy: gog::galaxy_webcache(),
            ubisoft: OnceCell::new(),
        }
    }

    /// The store's own copy of a game's cover on this PC.
    fn local_file(&self, game: &Game) -> Option<PathBuf> {
        match game.store {
            Store::Steam => {
                steam::cached_cover(self.steam.as_deref()?, store_number(game)?.parse().ok()?)
            }
            Store::Gog => gog::cached_cover(self.galaxy.as_deref()?, store_number(game)?),
            Store::Ubisoft => self
                .ubisoft
                .get_or_init(ubisoft::Launcher::find)
                .as_ref()?
                .cached_cover(store_number(game)?),
            Store::Epic | Store::Ea | Store::Xbox | Store::Custom => None,
        }
    }
}

/// Stores whose own picture is smaller than a tile and not a cover's shape (Ubisoft Connect's
/// 240 × 280): a cover found in the Steam store by name wins over it.
fn has_small_local(store: Store) -> bool {
    store == Store::Ubisoft
}

/// The store's own id in a `<store>:<id>` game id.
fn store_number(game: &Game) -> Option<&str> {
    game.id
        .split_once(':')
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty())
}

/// Stores whose server has a game's cover by its id; EA and Ubisoft games and games added by hand
/// have only the lookup by name.
fn has_id_source(store: Store) -> bool {
    matches!(store, Store::Steam | Store::Epic | Store::Gog | Store::Xbox)
}

fn net_identity(game: &Game) -> String {
    format!("net|{}|{CACHE_FORMAT}", game.id)
}

/// A renamed game is looked up again under its new name.
fn name_identity(game: &Game) -> String {
    format!("name|{}|{}|{CACHE_FORMAT}", game.id, letters(&game.name))
}

/// Changes with the file's length and modified time, so a store's new art gets a new thumbnail.
fn local_identity(file: &Path) -> Option<String> {
    let metadata = fs::metadata(file).ok()?;
    let modified = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(format!(
        "local|{}|{}|{modified}|{CACHE_FORMAT}",
        file.to_string_lossy().to_lowercase(),
        metadata.len()
    ))
}

/// Steps 1 to 3 (and a cover an earlier lookup by name found) for one game: its cover, and
/// whether the network may have one, or a sharper one. `make` turns a store's file into a
/// thumbnail.
fn local_cover(
    game: &Game,
    want_width: u32,
    cache: &Cache,
    own: Option<&Path>,
    stores: &Stores,
    make: &mut dyn FnMut(&Path) -> Option<Vec<u8>>,
) -> (Option<Cover>, bool) {
    if let Some(jpeg) = own.and_then(|dir| read_file(&own_file(dir, &game.id))) {
        return (Some(Cover::new(jpeg, true)), false);
    }
    // A download by id is the store's own art at full size: nothing beats it.
    let net = net_identity(game);
    if let Some(jpeg) = cache.read(&net) {
        return (Some(Cover::new(jpeg, false)), false);
    }
    let name = name_identity(game);
    let by_name = || cache.read(&name).map(|jpeg| Cover::new(jpeg, false));
    // A store's small picture gives way to the cover a lookup by name found.
    let small = has_small_local(game.store);
    if small && let Some(cover) = by_name() {
        return (Some(cover), false);
    }
    if let Some(file) = stores.local_file(game)
        && let Some(identity) = local_identity(&file)
    {
        let jpeg = cache.read(&identity).or_else(|| {
            let jpeg = make(&file)?;
            cache.write(&identity, &jpeg);
            Some(jpeg)
        });
        if let Some(jpeg) = jpeg {
            let cover = Cover::new(jpeg, false);
            let better = match game.store {
                // Steam's server has a sharper one than its client keeps.
                Store::Steam => cover.width < want_width.min(THUMB_WIDTH) && !cache.missed(&net),
                // The Steam store may have a real cover under this name.
                _ => small && !cache.missed(&name),
            };
            return (Some(cover), better);
        }
    }
    if let Some(cover) = by_name() {
        return (Some(cover), false);
    }
    let by_id = has_id_source(game.store) && !cache.missed(&net);
    let by_name = game.store != Store::Steam && !cache.missed(&name);
    (None, by_id || by_name)
}

fn fetch_into(game: &Game, cache: &Cache, thumbnailer: &Thumbnailer) -> Result<Option<Cover>> {
    let net = net_identity(game);
    if has_id_source(game.store) && !cache.missed(&net) {
        let url = match store_cover_url(game) {
            Err(error) if is_absent(&error) => None,
            url => url?,
        };
        let jpeg = match url {
            Some(url) => download(thumbnailer, &url)?,
            None => None,
        };
        match jpeg {
            Some(jpeg) => {
                cache.write(&net, &jpeg);
                return Ok(Some(Cover::new(jpeg, false)));
            }
            None => cache.mark_miss(&net),
        }
    }
    if game.store == Store::Steam {
        return Ok(None);
    }
    let name = name_identity(game);
    if cache.missed(&name) {
        return Ok(None);
    }
    let url = match steam::find_app(&game.name)? {
        Some(app_id) => steam::cover_url(app_id)?,
        None => None,
    };
    let jpeg = match url {
        Some(url) => download(thumbnailer, &url)?,
        None => None,
    };
    match jpeg {
        Some(jpeg) => {
            cache.write(&name, &jpeg);
            Ok(Some(Cover::new(jpeg, false)))
        }
        None => {
            cache.mark_miss(&name);
            Ok(None)
        }
    }
}

/// The address of a game's cover on its store's server; `None` when the store lists none.
fn store_cover_url(game: &Game) -> Result<Option<String>> {
    match game.store {
        Store::Steam => match store_number(game).and_then(|id| id.parse().ok()) {
            Some(app_id) => steam::cover_url(app_id),
            None => Ok(None),
        },
        Store::Epic => Ok(epic::cover_url(game)),
        Store::Gog => match store_number(game) {
            Some(id) => gog::cover_url(id),
            None => Ok(None),
        },
        Store::Xbox => match game.install_dir.as_deref().and_then(xbox::store_id) {
            Some(id) => xbox::cover_url(&id),
            None => Ok(None),
        },
        Store::Ea | Store::Ubisoft | Store::Custom => Ok(None),
    }
}

/// The picture at `url` as a thumbnail; `None` when the server has none there, or what it sent
/// is no picture.
fn download(thumbnailer: &Thumbnailer, url: &str) -> Result<Option<Vec<u8>>> {
    match http::get(url) {
        Ok(bytes) => Ok(thumbnailer.of_bytes(&bytes).ok()),
        Err(error) if is_absent(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The server answered that there is no such thing, which asking again will not change (unlike a
/// failed connection, a busy server or a rate limit).
fn is_absent(error: &Error) -> bool {
    matches!(
        error,
        Error::Http {
            status: 400 | 403 | 404 | 410,
            ..
        }
    )
}

/// `%APPDATA%\Game Manager\covers`: the user's own covers.
fn own_dir() -> Result<PathBuf> {
    json_file::user_file("covers")
}

fn own_file(dir: &Path, game_id: &str) -> PathBuf {
    dir.join(format!("{}.jpg", safe_name(game_id)))
}

/// A game id as a file name: "steam:220" is "steam_220".
fn safe_name(game_id: &str) -> String {
    game_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn set_own_in(dir: &Path, game_id: &str, image: &Path, thumbnailer: &Thumbnailer) -> Result<Cover> {
    let jpeg = thumbnailer
        .of_file(image)
        .map_err(|_| Error::NotAPicture(image.to_path_buf()))?;
    fs::create_dir_all(dir).map_err(Error::io(dir))?;
    let file = own_file(dir, game_id);
    write_file(&file, &jpeg).map_err(Error::io(&file))?;
    Ok(Cover::new(jpeg, true))
}

fn clear_own_in(dir: &Path, cache: Option<&Cache>, game: &Game) -> Result<()> {
    let file = own_file(dir, &game.id);
    match fs::remove_file(&file) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::io(&file)(error));
        }
        _ => {}
    }
    if let Some(cache) = cache {
        cache.forget(&net_identity(game));
        cache.forget(&name_identity(game));
    }
    Ok(())
}

fn read_file(file: &Path) -> Option<Vec<u8>> {
    fs::read(file).ok().filter(|bytes| !bytes.is_empty())
}

/// Written through a temporary file, so a reader never sees half a picture.
fn write_file(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = file.with_extension("tmp");
    fs::write(&temp, bytes)?;
    fs::rename(&temp, file)
}

/// Cover thumbnails as JPEG files in one folder, and the `.none` markers of lookups that found
/// nothing. Every failure only means "not cached".
struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// `%LOCALAPPDATA%\Game Manager\cache\covers`; `None` when it cannot be used.
    fn open() -> Option<Self> {
        Self::at(Self::dir()?)
    }

    /// The cache as it is, for reading only: nothing is created.
    fn existing() -> Option<Self> {
        Some(Self { dir: Self::dir()? }).filter(|cache| cache.dir.is_dir())
    }

    fn dir() -> Option<PathBuf> {
        Some(json_file::local_file("cache").ok()?.join("covers"))
    }

    fn at(dir: PathBuf) -> Option<Self> {
        fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    fn stem(identity: &str) -> String {
        format!("{:016x}", fnv1a(identity.as_bytes()))
    }

    fn file(&self, identity: &str) -> PathBuf {
        self.dir.join(format!("{}.jpg", Self::stem(identity)))
    }

    fn miss_file(&self, identity: &str) -> PathBuf {
        self.dir.join(format!("{}.none", Self::stem(identity)))
    }

    fn read(&self, identity: &str) -> Option<Vec<u8>> {
        read_file(&self.file(identity))
    }

    /// A found cover also ends the wait a miss started.
    fn write(&self, identity: &str, jpeg: &[u8]) {
        let _ = write_file(&self.file(identity), jpeg);
        let _ = fs::remove_file(self.miss_file(identity));
    }

    /// Whether this was looked up and not found less than `MISS_AGE` ago.
    fn missed(&self, identity: &str) -> bool {
        fs::metadata(self.miss_file(identity))
            .and_then(|marker| marker.modified())
            .ok()
            .and_then(|at| SystemTime::now().duration_since(at).ok())
            .is_some_and(|age| age < MISS_AGE)
    }

    fn mark_miss(&self, identity: &str) {
        let _ = fs::write(self.miss_file(identity), b"");
    }

    fn forget(&self, identity: &str) {
        let _ = fs::remove_file(self.file(identity));
        let _ = fs::remove_file(self.miss_file(identity));
    }

    /// Deletes cached covers, markers and leftover temporary files that are not in `used`.
    fn prune(&self, used: &HashSet<PathBuf>) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            let cached = path
                .extension()
                .is_some_and(|ext| ext == "jpg" || ext == "none" || ext == "tmp");
            if cached && !used.contains(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

/// Makes cover thumbnails with WIC. Create one per thread and reuse it for a batch.
struct Thumbnailer {
    factory: IWICImagingFactory,
    _com: ComGuard,
}

impl Thumbnailer {
    fn new() -> windows::core::Result<Self> {
        let com = ComGuard::new();
        // SAFETY: COM is initialized on this thread for the lifetime of `_com`.
        let factory = unsafe {
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER as u32)?
        };
        Ok(Self { factory, _com: com })
    }

    /// The picture in `file` (any format WIC reads) as a cover thumbnail.
    fn of_file(&self, file: &Path) -> windows::core::Result<Vec<u8>> {
        let path = HSTRING::from(file.as_os_str());
        // SAFETY: `path` is a valid wide string for the call; no vendor preference.
        let decoder = unsafe {
            self.factory.CreateDecoderFromFilename(
                &path,
                None,
                GENERIC_READ,
                WICDecodeMetadataCacheOnDemand,
            )
        }?;
        self.thumbnail(&decoder)
    }

    /// The picture in `bytes` (a download) as a cover thumbnail.
    fn of_bytes(&self, bytes: &[u8]) -> windows::core::Result<Vec<u8>> {
        let length = u32::try_from(bytes.len()).map_err(|_| windows::core::Error::empty())?;
        // SAFETY: the stream copies `bytes`, which are valid for `length`.
        let stream = unsafe { SHCreateMemStream(Some(bytes.as_ptr()), length) }
            .ok_or_else(windows::core::Error::empty)?;
        // SAFETY: `stream` is live; no vendor preference.
        let decoder = unsafe {
            self.factory
                .CreateDecoderFromStream(&stream, None, WICDecodeMetadataCacheOnDemand)
        }?;
        self.thumbnail(&decoder)
    }

    /// The decoder's first frame, without its alpha, cropped to 2:3 around its centre, scaled
    /// down to at most `THUMB_WIDTH` × `THUMB_HEIGHT` and encoded as a JPEG (WIC's default
    /// quality, 90%). Every interface used comes from the WIC call just before it, which
    /// succeeded.
    fn thumbnail(&self, decoder: &IWICBitmapDecoder) -> windows::core::Result<Vec<u8>> {
        // SAFETY: frame 0 exists in any picture that decoded.
        let frame = unsafe { decoder.GetFrame(0) }?;
        let (mut width, mut height) = (0u32, 0u32);
        // SAFETY: both out-pointers are valid locals.
        unsafe { frame.GetSize(&mut width, &mut height) }.ok()?;
        if width == 0 || height == 0 || width > 32_768 || height > 32_768 {
            return Err(windows::core::Error::empty());
        }
        let (x, y, crop_width, crop_height) = crop(width, height);
        let (thumb_width, thumb_height) = thumb_size(crop_width, crop_height);

        // SAFETY: a plain factory call.
        let converter = unsafe { self.factory.CreateFormatConverter() }?;
        // SAFETY: `frame` is live; no palette is needed for 24-bit output.
        unsafe {
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat24bppBGR,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
        }
        .ok()?;
        let rect = WICRect {
            X: x as i32,
            Y: y as i32,
            Width: crop_width as i32,
            Height: crop_height as i32,
        };
        // SAFETY: a plain factory call.
        let clipper = unsafe { self.factory.CreateBitmapClipper() }?;
        // SAFETY: `converter` is live, and `rect` lies inside the picture (`crop`).
        unsafe { clipper.Initialize(&converter, &rect) }.ok()?;
        // SAFETY: a plain factory call.
        let scaler = unsafe { self.factory.CreateBitmapScaler() }?;
        // SAFETY: `clipper` is live; the size is not zero (`thumb_size`).
        unsafe {
            scaler.Initialize(
                &clipper,
                thumb_width,
                thumb_height,
                WICBitmapInterpolationModeFant,
            )
        }
        .ok()?;

        // SAFETY: no initial data, so the stream starts empty.
        let stream =
            unsafe { SHCreateMemStream(None, 0) }.ok_or_else(windows::core::Error::from_thread)?;
        // SAFETY: no vendor means "any encoder", as WIC documents.
        let encoder = unsafe { self.factory.CreateEncoder(&GUID_ContainerFormatJpeg, None) }?;
        // SAFETY: `stream` is the live stream created above.
        unsafe { encoder.Initialize(&stream, WICBitmapEncoderNoCache) }.ok()?;
        let mut out: Option<IWICBitmapFrameEncode> = None;
        let mut options: Option<IPropertyBag2> = None;
        // SAFETY: both out-pointers are valid locals.
        unsafe { encoder.CreateNewFrame(&mut out, Some(&raw mut options)) }.ok()?;
        let out = out.ok_or_else(windows::core::Error::empty)?;
        // SAFETY: `options` is the property bag CreateNewFrame returned with this frame.
        unsafe { out.Initialize(options.as_ref()) }.ok()?;
        // SAFETY: `scaler` is live; no rectangle means all of it.
        unsafe { out.WriteSource(&scaler, None) }.ok()?;
        // SAFETY: the frame was initialized and written.
        unsafe { out.Commit() }.ok()?;
        // SAFETY: the encoder's only frame was committed.
        unsafe { encoder.Commit() }.ok()?;
        read_all(&stream)
    }
}

/// Reads a whole memory stream from the beginning.
fn read_all(stream: &IStream) -> windows::core::Result<Vec<u8>> {
    let too_large = |_| windows::core::Error::empty();
    let mut stat = STATSTG::default();
    // SAFETY: `stat` is a valid out-pointer; with STATFLAG_NONAME, Stat allocates no name that
    // would need freeing.
    unsafe { stream.Stat(&mut stat, STATFLAG_NONAME as u32) }.ok()?;
    // SAFETY: Stat filled in the size, and every member of the union holds a valid integer.
    let size = unsafe { stat.cbSize.QuadPart };
    // SAFETY: seeking a live stream to its start; no out-pointer.
    unsafe { stream.Seek(LARGE_INTEGER { QuadPart: 0 }, STREAM_SEEK_SET as u32, None) }.ok()?;
    let mut buffer = vec![0u8; usize::try_from(size).map_err(too_large)?];
    let len = u32::try_from(buffer.len()).map_err(too_large)?;
    let mut read = 0u32;
    // SAFETY: `buffer` has exactly `len` bytes, so the stream cannot write past it, and `read` is
    // a valid out-pointer.
    unsafe { stream.Read(buffer.as_mut_ptr().cast(), len, Some(&mut read)) }.ok()?;
    buffer.truncate(read as usize);
    Ok(buffer)
}

/// The largest 2:3 rectangle around the centre of a `width` × `height` picture, as
/// `(x, y, width, height)`.
fn crop(width: u32, height: u32) -> (u32, u32, u32, u32) {
    if u64::from(width) * 3 > u64::from(height) * 2 {
        let kept = (height * 2 / 3).max(1);
        ((width - kept) / 2, 0, kept, height)
    } else {
        let kept = (width * 3 / 2).max(1);
        (0, (height - kept) / 2, width, kept)
    }
}

/// The size a 2:3 crop is kept at: `THUMB_WIDTH` × `THUMB_HEIGHT` at most, never enlarged.
fn thumb_size(width: u32, height: u32) -> (u32, u32) {
    if width > THUMB_WIDTH {
        (THUMB_WIDTH, THUMB_HEIGHT)
    } else {
        (width.max(1), height.max(1))
    }
}

/// The width and height in a JPEG's frame header (the first `SOFn` segment).
fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut rest = jpeg.strip_prefix(&[0xFF, 0xD8])?;
    loop {
        let [0xFF, marker, after @ ..] = rest else {
            return None;
        };
        match *marker {
            // A fill byte before a marker, then markers that stand alone.
            0xFF => {
                rest = &rest[1..];
                continue;
            }
            0x01 | 0xD0..=0xD9 => {
                rest = after;
                continue;
            }
            _ => {}
        }
        let length = usize::from(u16::from_be_bytes([*after.first()?, *after.get(1)?]));
        // Start of frame: every C0 to CF but the tables among them.
        if matches!(*marker, 0xC0..=0xCF) && !matches!(*marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([*after.get(3)?, *after.get(4)?]);
            let width = u16::from_be_bytes([*after.get(5)?, *after.get(6)?]);
            return Some((u32::from(width), u32::from(height)));
        }
        rest = after.get(length..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Launch;

    fn temp_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("GM-Covers-{test}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn game(id: &str, name: &str, store: Store) -> Game {
        Game {
            id: id.into(),
            name: name.into(),
            store,
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

    /// An uncompressed 24-bit BMP, which WIC reads itself: the left half red, the right half
    /// blue.
    fn bmp(width: u32, height: u32) -> Vec<u8> {
        let row = (width * 3).div_ceil(4) * 4;
        let size = 54 + row * height;
        let mut file = Vec::with_capacity(size as usize);
        file.extend_from_slice(b"BM");
        file.extend_from_slice(&size.to_le_bytes());
        file.extend_from_slice(&[0; 4]);
        file.extend_from_slice(&54u32.to_le_bytes());
        file.extend_from_slice(&40u32.to_le_bytes());
        file.extend_from_slice(&width.to_le_bytes());
        file.extend_from_slice(&height.to_le_bytes());
        file.extend_from_slice(&1u16.to_le_bytes());
        file.extend_from_slice(&24u16.to_le_bytes());
        file.extend_from_slice(&[0; 24]);
        for _ in 0..height {
            for x in 0..width {
                // Blue, green, red.
                file.extend_from_slice(if x < width / 2 {
                    &[0, 0, 255]
                } else {
                    &[255, 0, 0]
                });
            }
            file.extend_from_slice(&vec![0; (row - width * 3) as usize]);
        }
        file
    }

    #[test]
    fn crops_to_two_by_three_around_the_centre() {
        // Already 2:3; too wide (Epic's 3:4, a landscape); too tall.
        assert_eq!(crop(600, 900), (0, 0, 600, 900));
        assert_eq!(crop(1200, 1600), (67, 0, 1066, 1600));
        assert_eq!(crop(900, 600), (250, 0, 400, 600));
        assert_eq!(crop(100, 300), (0, 75, 100, 150));
        assert_eq!(crop(1, 1), (0, 0, 1, 1));
        // Kept as it is up to the thumbnail's size, never enlarged.
        assert_eq!(thumb_size(600, 900), (400, 600));
        assert_eq!(thumb_size(1066, 1600), (400, 600));
        assert_eq!(thumb_size(300, 450), (300, 450));
        assert_eq!(thumb_size(342, 513), (342, 513));
    }

    #[test]
    fn makes_thumbnails_from_files_and_downloads() {
        let dir = temp_dir("thumbnail");
        let thumbnailer = Thumbnailer::new().unwrap();
        // A landscape picture: cropped to its middle, scaled down.
        let wide = dir.join("wide.bmp");
        fs::write(&wide, bmp(1800, 1200)).unwrap();
        let jpeg = thumbnailer.of_file(&wide).unwrap();
        assert_eq!(jpeg[..2], [0xFF, 0xD8]);
        assert_eq!(jpeg_size(&jpeg), Some((400, 600)));
        // A small cover stays its size; a download is read from memory.
        let small = thumbnailer.of_bytes(&bmp(300, 450)).unwrap();
        assert_eq!(jpeg_size(&small), Some((300, 450)));
        let tall = thumbnailer.of_bytes(&bmp(100, 300)).unwrap();
        assert_eq!(jpeg_size(&tall), Some((100, 150)));
        // A thumbnail of a thumbnail is the same size: JPEGs are read too.
        assert_eq!(
            jpeg_size(&thumbnailer.of_bytes(&jpeg).unwrap()),
            Some((400, 600))
        );
        // Not a picture: an error page, an empty answer, a missing file.
        assert!(thumbnailer.of_bytes(b"<html>Not found</html>").is_err());
        assert!(thumbnailer.of_bytes(b"").is_err());
        assert!(thumbnailer.of_file(&dir.join("missing.jpg")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn reads_a_jpegs_size() {
        // SOI, an APP0 segment, a quantization table, then a baseline frame of 300 × 450.
        let mut jpeg = vec![
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 1, 2, 0xFF, 0xDB, 0x00, 0x03, 9,
        ];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 8, 0x01, 0xC2, 0x01, 0x2C, 3]);
        assert_eq!(jpeg_size(&jpeg), Some((300, 450)));
        // A progressive frame after a fill byte.
        let progressive = [
            0xFF, 0xD8, 0xFF, 0xFF, 0xC2, 0x00, 0x11, 8, 0x02, 0x58, 0x01, 0x90,
        ];
        assert_eq!(jpeg_size(&progressive), Some((400, 600)));
        assert_eq!(jpeg_size(b"\x89PNG\r\n"), None);
        assert_eq!(jpeg_size(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x40, 1]), None);
        assert_eq!(jpeg_size(&[0xFF, 0xD8]), None);
        assert_eq!(jpeg_size(&[]), None);
    }

    #[test]
    fn the_cache_keeps_covers_and_week_long_misses() {
        let cache = Cache::at(temp_dir("cache")).unwrap();
        let (half_life, portal) = ("net|steam:220|1", "net|steam:400|1");
        assert_eq!(cache.read(half_life), None);
        assert!(!cache.missed(half_life));
        cache.write(half_life, b"jpeg");
        assert_eq!(cache.read(half_life).as_deref(), Some(&b"jpeg"[..]));
        assert_eq!(cache.read(portal), None);

        // A miss holds for a week, then the lookup may run again.
        cache.mark_miss(portal);
        assert!(cache.missed(portal));
        assert!(!cache.missed(half_life));
        let marker = fs::File::options()
            .write(true)
            .open(cache.miss_file(portal))
            .unwrap();
        let eight_days = Duration::from_secs(8 * 24 * 60 * 60);
        marker.set_modified(SystemTime::now() - eight_days).unwrap();
        drop(marker);
        assert!(!cache.missed(portal));
        // A cover found later ends the miss; forgetting drops both.
        cache.mark_miss(portal);
        cache.write(portal, b"found");
        assert!(!cache.missed(portal));
        cache.forget(portal);
        assert_eq!(cache.read(portal), None);

        // Pruning keeps what is in use, and files that are not the cache's.
        cache.mark_miss(portal);
        let stale = cache.dir.join("0123456789abcdef.jpg");
        let leftover = cache.dir.join("fedcba9876543210.tmp");
        let unrelated = cache.dir.join("notes.txt");
        for file in [&stale, &leftover, &unrelated] {
            fs::write(file, b"x").unwrap();
        }
        let used = HashSet::from([cache.file(half_life), cache.miss_file(half_life)]);
        cache.prune(&used);
        assert!(cache.file(half_life).is_file());
        assert!(!cache.miss_file(portal).exists());
        assert!(!stale.exists() && !leftover.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn identities_follow_the_game_its_name_and_the_stores_file() {
        let dead_space = game("ea:1", "Dead Space™", Store::Ea);
        assert_eq!(net_identity(&dead_space), "net|ea:1|1");
        assert_eq!(name_identity(&dead_space), "name|ea:1|deadspace|1");
        let renamed = game("ea:1", "Dead Space (2023)", Store::Ea);
        assert_ne!(name_identity(&renamed), name_identity(&dead_space));
        // A store's file: its path in any case, and a new identity once it changes.
        let dir = temp_dir("identity");
        let file = dir.join("Library_600x900.jpg");
        fs::write(&file, b"first").unwrap();
        let before = local_identity(&file).unwrap();
        assert_eq!(
            local_identity(&dir.join("LIBRARY_600X900.JPG")).as_ref(),
            Some(&before)
        );
        fs::write(&file, b"second version").unwrap();
        assert_ne!(local_identity(&file).unwrap(), before);
        assert_eq!(local_identity(&dir.join("missing.jpg")), None);
        assert_eq!(safe_name("steam:220"), "steam_220");
        assert_eq!(safe_name("ea:{64CF}"), "ea__64CF_");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cover_comes_from_the_first_source_that_has_one() {
        let dir = temp_dir("chain");
        let cache = Cache::at(dir.join("cache")).unwrap();
        let own = dir.join("own");
        let steam = dir.join("steam");
        let cover_file = steam.join(r"appcache\librarycache\220\library_600x900.jpg");
        fs::create_dir_all(cover_file.parent().unwrap()).unwrap();
        // What Steam's client keeps: 300 × 450 under the 600 × 900 name.
        fs::write(&cover_file, bmp(300, 450)).unwrap();
        // What an older Ubisoft Connect cached: a 240 × 280 picture, named by the configuration.
        let launcher = dir.join("ubisoft");
        let thumb = launcher.join(r"cache\assets\thumb.jpg");
        fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        fs::write(&thumb, bmp(240, 280)).unwrap();
        let stores = Stores {
            steam: Some(steam),
            galaxy: None,
            ubisoft: OnceCell::from(Some(ubisoft::Launcher {
                dir: launcher,
                configurations: [(609, "root:\n  thumb_image: thumb.jpg\n".to_owned())].into(),
            })),
        };
        let thumbnailer = Thumbnailer::new().unwrap();
        let made = std::cell::Cell::new(0);
        let find = |game: &Game, want_width: u32| {
            let mut make = |file: &Path| {
                made.set(made.get() + 1);
                thumbnailer.of_file(file).ok()
            };
            local_cover(game, want_width, &cache, Some(&own), &stores, &mut make)
        };
        let half_life = game("steam:220", "Half-Life 2", Store::Steam);

        // Steam's own copy, made into a thumbnail once; enough for a 200 pixel tile.
        let (cover, ask) = find(&half_life, 200);
        let cover = cover.unwrap();
        assert_eq!((cover.width, cover.own, ask), (300, false, false));
        assert_eq!(find(&half_life, 200).0, Some(cover.clone()));
        // A tile needing more asks Steam's server once, and not again after a miss.
        assert!(find(&half_life, 350).1);
        assert!(find(&half_life, 900).1);
        cache.mark_miss(&net_identity(&half_life));
        assert!(!find(&half_life, 350).1);
        // What the server sent wins over the client's copy.
        cache.write(
            &net_identity(&half_life),
            &thumbnailer.of_bytes(&bmp(600, 900)).unwrap(),
        );
        let (sharp, ask) = find(&half_life, 350);
        assert_eq!((sharp.unwrap().width, ask), (400, false));
        // The user's own image wins over everything.
        let picked = dir.join("picked.bmp");
        fs::write(&picked, bmp(200, 300)).unwrap();
        let set = set_own_in(&own, &half_life.id, &picked, &thumbnailer).unwrap();
        assert_eq!((set.width, set.own), (200, true));
        assert_eq!(find(&half_life, 350), (Some(set), false));
        // Reset: the own image goes, and the download with it, so the client's copy shows again.
        clear_own_in(&own, Some(&cache), &half_life).unwrap();
        clear_own_in(&own, Some(&cache), &half_life).unwrap();
        let (again, ask) = find(&half_life, 200);
        assert_eq!((again.unwrap().width, ask), (300, false));
        assert_eq!(made.get(), 1);

        // A Steam app the client has no cover of: the server, until it says it has none.
        let tool = game("steam:7", "A tool", Store::Steam);
        assert_eq!(find(&tool, 200), (None, true));
        cache.mark_miss(&net_identity(&tool));
        assert_eq!(find(&tool, 200), (None, false));
        // An EA game has only the lookup by name; an Epic game its store, then the name.
        let dead_space = game("ea:1", "Dead Space™", Store::Ea);
        assert_eq!(find(&dead_space, 200), (None, true));
        cache.mark_miss(&name_identity(&dead_space));
        assert_eq!(find(&dead_space, 200), (None, false));
        let sifu = game("epic:d363", "Sifu", Store::Epic);
        cache.mark_miss(&net_identity(&sifu));
        assert_eq!(find(&sifu, 200), (None, true));
        cache.write(
            &name_identity(&sifu),
            &thumbnailer.of_bytes(&bmp(300, 450)).unwrap(),
        );
        let (by_name, ask) = find(&sifu, 200);
        assert_eq!((by_name.unwrap().width, ask), (300, false));
        // A Ubisoft game shows the launcher's small picture (cut to 2:3) while the Steam store
        // is asked by name; what that finds wins, and a miss keeps the small one.
        let child_of_light = game("ubisoft:609", "Child of Light", Store::Ubisoft);
        let (small, ask) = find(&child_of_light, 200);
        assert_eq!((small.unwrap().width, ask), (186, true));
        cache.mark_miss(&name_identity(&child_of_light));
        let (small, ask) = find(&child_of_light, 200);
        assert_eq!((small.unwrap().width, ask), (186, false));
        cache.write(
            &name_identity(&child_of_light),
            &thumbnailer.of_bytes(&bmp(600, 900)).unwrap(),
        );
        let (by_name, ask) = find(&child_of_light, 200);
        assert_eq!((by_name.unwrap().width, ask), (400, false));
        // One the launcher has no picture of: the lookup by name alone.
        let anno = game("ubisoft:4553", "Anno 1800", Store::Ubisoft);
        assert_eq!(find(&anno, 200), (None, true));

        // Not a picture: an error that names the file, and nothing saved.
        let text = dir.join("notes.txt");
        fs::write(&text, b"not a picture").unwrap();
        let error = set_own_in(&own, &tool.id, &text, &thumbnailer).unwrap_err();
        assert!(matches!(error, Error::NotAPicture(file) if file == text));
        assert!(!own_file(&own, &tool.id).exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    /// One real cover from each network source. Run with
    /// `cargo test -p gm-core live_covers -- --ignored --nocapture`.
    #[test]
    #[ignore = "asks Steam's, GOG's, Microsoft's and Epic's servers"]
    fn live_covers() {
        let thumbnailer = Thumbnailer::new().unwrap();
        let check = |source: &str, url: Option<String>| {
            let url = url.unwrap_or_else(|| panic!("{source}: no address"));
            let jpeg = download(&thumbnailer, &url)
                .unwrap()
                .unwrap_or_else(|| panic!("{source}: no picture at {url}"));
            let size = jpeg_size(&jpeg).unwrap();
            println!("{source}: {size:?}, {} bytes, {url}", jpeg.len());
            // About 2:3 (an odd height rounds), and no smaller than what the stores keep.
            let off = (size.0 * 3).abs_diff(size.1 * 2);
            assert!(size.0 >= 300 && off <= 3, "{source}: {size:?}");
        };
        check("Steam (Half-Life 2)", steam::cover_url(220).unwrap());
        let found = steam::find_app("Half-Life 2").unwrap();
        assert_eq!(found, Some(220));
        assert_eq!(steam::find_app("No Such Game Zzyzx 9000").unwrap(), None);
        check("GOG (The Witcher 3)", gog::cover_url("1495134320").unwrap());
        check(
            "Xbox (Forza Horizon 5)",
            xbox::cover_url("9NKX70BBCDRN").unwrap(),
        );
        // Epic's addresses come from the launcher's catalog on this PC: the first game it lists.
        match epic::scan()
            .unwrap_or_default()
            .iter()
            .find_map(epic::cover_url)
        {
            Some(url) => check("Epic (an installed game)", Some(url)),
            None => println!("Epic: no installed game with a cover in the launcher's catalog"),
        }
        // An address with nothing behind it is a miss, not an error.
        let gone =
            "https://shared.fastly.steamstatic.com/store_item_assets/steam/apps/220/none.jpg";
        assert_eq!(download(&thumbnailer, gone).unwrap(), None);
    }
}
