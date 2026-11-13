//! Cover art for the Library's tiles: each game's portrait cover, as the store's original file.
//!
//! **Where a cover comes from**, the first that exists:
//! 1. The user's own picture, `%APPDATA%\Game Manager\covers\<game id>.<jpg|png|webp|bmp|ico>`
//!    (`set_own`), kept as the file they chose.
//! 2. The store's original, downloaded once into the cover cache,
//!    `%LOCALAPPDATA%\Game Manager\cache\covers`:
//!    - by the game's own id, from the store's server: Steam's store API, the address in the Epic
//!      launcher's catalog cache, GOG's API, Microsoft's store catalog for Xbox games;
//!    - else, for a game of another store (EA and Ubisoft games and games added by hand have no
//!      source by id), from the Steam store by name. Only a result with the same name counts.
//! 3. A smaller copy the store keeps on this PC, read where it is: Steam's client cover
//!    (300 × 450), GOG Galaxy's (342 × 482 WebP), the small picture an older Ubisoft Connect
//!    cached (240 × 280) (`steam::cached_cover`, `gog::cached_cover`,
//!    `ubisoft::Launcher::cached_cover`). It shows only until the original has arrived, or when
//!    the store has none.
//!
//! `load_local` does 1 to 3 for a whole library and says which games the network may still help;
//! `fetch` does 2 for one game. Both block: call them off the UI thread. A `Cover` names its file,
//! so the app holds no picture it isn't showing; `read` gives the file's bytes when a tile needs
//! them.
//!
//! **Never re-encoded**: every cover is kept byte for byte as the store (or the user) has it:
//! Steam's 600 × 900 JPEG, Epic's 1200 × 1600 PNG, Xbox's 1440 × 2160 JPEG. A download or a chosen
//! file is decoded once here (`picture::decode`) only to prove it is a whole picture; the app
//! decodes it to draw it, and Direct2D scales it into its tile.
//!
//! **The cache** holds downloads under their format's extension, named after a hash of what was
//! asked: the game's id (by id), or its id and the letters of its name (by name). A lookup that
//! found nothing leaves an empty `.none` file and is not asked again for a week; a failed request
//! leaves nothing and is tried again at the next start.

use crate::picture::{self, Format};
use crate::text::{fnv1a, letters};
use crate::{Error, Game, Result, Store, epic, gog, http, json_file, steam, ubisoft, xbox};
use std::cell::OnceCell;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Bump when what is cached changes, so covers cached by an older version are not reused.
const CACHE_FORMAT: u32 = 2;
/// How long a lookup that found nothing is not asked again.
const MISS_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The extensions a cover is kept under, one per format.
const EXTENSIONS: [&str; 5] = ["jpg", "png", "webp", "bmp", "ico"];

/// A game's cover: a picture file, as the store (or the user) has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    /// The user's own file, the cache's copy of the store's original, or the store's copy on
    /// this PC. It held a whole picture when the cover was found.
    pub file: PathBuf,
    /// The user's own picture (`set_own`), not a store's.
    pub own: bool,
    /// A smaller copy kept on this PC, shown until the store's original has arrived: the app asks
    /// for that game again.
    pub stand_in: bool,
}

impl Cover {
    fn new(file: PathBuf, own: bool, stand_in: bool) -> Self {
        Self {
            file,
            own,
            stand_in,
        }
    }
}

/// A cover's file as it is now: its bytes, to decode; `None` when it can't be read (gone since).
/// Blocking.
pub fn read(cover: &Cover) -> Option<Vec<u8>> {
    read_file(&cover.file)
}

/// The covers that need no network: the user's own, the store's originals in the cache, and
/// smaller copies the stores keep on this PC. `found` gets each one; the games returned are those
/// whose original the network may still have (a copy on this PC is shown meanwhile). Blocking.
pub fn load_local(games: &[Game], found: &mut dyn FnMut(&Game, Cover)) -> Vec<Game> {
    let Some(cache) = Cache::open() else {
        return Vec::new();
    };
    let own = own_dir().ok();
    let stores = Stores::find();
    let mut rest = Vec::new();
    for game in games {
        let (cover, ask) = local_cover(game, &cache, own.as_deref(), &stores);
        if let Some(cover) = cover {
            found(game, cover);
        }
        if ask {
            rest.push(game.clone());
        }
    }
    rest
}

/// A game's original cover from its store's server, else from the Steam store by name; cached.
/// `None` when neither has one, which is remembered for a week. An error is a request that failed:
/// it is tried again next time. Blocking.
pub fn fetch(game: &Game) -> Result<Option<Cover>> {
    let Some(cache) = Cache::open() else {
        return Ok(None);
    };
    fetch_into(game, &cache)
}

/// Makes the picture in `image` the game's cover, whatever its store has: the file is kept as it
/// is. Blocking.
pub fn set_own(game_id: &str, image: &Path) -> Result<Cover> {
    set_own_in(&own_dir()?, game_id, image)
}

/// Drops the user's own cover of a game, and what was downloaded or looked up for it, so its
/// cover is found afresh (Reset cover; also when a game added by hand is removed). Blocking.
pub fn clear_own(game: &Game) -> Result<()> {
    clear_own_in(&own_dir()?, Cache::open().as_ref(), game)
}

/// Deletes cached covers no game uses any more. For a full library only: pruning after a partial
/// list would drop the other games' covers. The user's own pictures are never touched.
pub fn prune(games: &[Game]) {
    let Some(cache) = Cache::open() else {
        return;
    };
    let mut used = HashSet::new();
    for game in games {
        for identity in [net_identity(game), name_identity(game)] {
            used.extend(cache.files(&identity));
            used.insert(cache.miss_file(&identity));
        }
    }
    cache.prune(&used);
}

/// Where a game's cover comes from, or would be looked for, in a line: for `scan --covers`.
/// Reads only; nothing is downloaded or cached.
pub fn describe(game: &Game) -> String {
    let cache = Cache::existing();
    let cached = |identity: &str| cache.as_ref().and_then(|c| c.existing_file(identity));
    let missed = |identity: &str| cache.as_ref().is_some_and(|c| c.missed(identity));
    if let Ok(dir) = own_dir()
        && let Some(file) = own_files(&dir, &game.id).find(|file| file.is_file())
    {
        return format!("your own picture ({}): {}", details(&file), file.display());
    }
    let net = net_identity(game);
    let name = name_identity(game);
    if let Some(file) = cached(&net) {
        return format!(
            "{}'s original ({}): {}",
            game.store.name(),
            details(&file),
            file.display()
        );
    }
    if let Some(file) = cached(&name) {
        return format!(
            "the original from the Steam store, found by name ({}): {}",
            details(&file),
            file.display()
        );
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
    if let Some(file) = Stores::find().local_file(game) {
        let keeper = match game.store {
            Store::Gog => "GOG Galaxy",
            Store::Ubisoft => "Ubisoft Connect",
            _ => "Steam",
        };
        steps.push(format!(
            "meanwhile {keeper}'s copy on this PC ({}): {}",
            details(&file),
            file.display()
        ));
    }
    steps.join("; ")
}

/// A picture file's format and size, for `describe`.
fn details(file: &Path) -> String {
    let Some(bytes) = read_file(file) else {
        return "unreadable".to_owned();
    };
    match (picture::format(&bytes), picture::size(&bytes)) {
        (Some(format), Some((width, height))) => format!("{format} {width} × {height}"),
        _ => "not a picture".to_owned(),
    }
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

/// Steps 1 to 3 for one game: its cover, and whether the network may still have its original.
fn local_cover(
    game: &Game,
    cache: &Cache,
    own: Option<&Path>,
    stores: &Stores,
) -> (Option<Cover>, bool) {
    if let Some(file) = own.and_then(|dir| find_own(dir, &game.id)) {
        return (Some(Cover::new(file, true, false)), false);
    }
    let net = net_identity(game);
    let name = name_identity(game);
    if let Some(file) = cache.find(&net).or_else(|| cache.find(&name)) {
        return (Some(Cover::new(file, false, false)), false);
    }
    // The original is still to be asked for, unless the stores said they have none.
    let by_id = has_id_source(game.store) && !cache.missed(&net);
    let by_name = game.store != Store::Steam && !cache.missed(&name);
    // Meanwhile, the store's smaller copy on this PC.
    let copy = stores
        .local_file(game)
        .filter(|file| read_file(file).is_some_and(|bytes| picture::format(&bytes).is_some()))
        .map(|file| Cover::new(file, false, true));
    (copy, by_id || by_name)
}

fn fetch_into(game: &Game, cache: &Cache) -> Result<Option<Cover>> {
    let net = net_identity(game);
    if has_id_source(game.store) && !cache.missed(&net) {
        let url = match store_cover_url(game) {
            Err(error) if is_absent(&error) => None,
            url => url?,
        };
        let bytes = match url {
            Some(url) => download(&url)?,
            None => None,
        };
        match bytes {
            Some(bytes) => {
                let file = cache.write(&net, &bytes);
                return Ok(file.map(|file| Cover::new(file, false, false)));
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
    let bytes = match url {
        Some(url) => download(&url)?,
        None => None,
    };
    match bytes {
        Some(bytes) => {
            let file = cache.write(&name, &bytes);
            Ok(file.map(|file| Cover::new(file, false, false)))
        }
        None => {
            cache.mark_miss(&name);
            Ok(None)
        }
    }
}

/// The address of a game's original cover on its store's server; `None` when the store lists
/// none.
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

/// The picture at `url`, byte for byte; `None` when the server has none there, or what it sent is
/// no whole picture.
fn download(url: &str) -> Result<Option<Vec<u8>>> {
    match http::get(url) {
        Ok(bytes) => Ok(whole_picture(bytes)),
        Err(error) if is_absent(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

/// `bytes` when they decode as a picture (not an error page, nor a cut-off file).
fn whole_picture(bytes: Vec<u8>) -> Option<Vec<u8>> {
    picture::decode(&bytes).is_ok().then_some(bytes)
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

/// Where the user's own cover of a game may be: one file, under its format's extension.
fn own_files<'a>(dir: &'a Path, game_id: &str) -> impl Iterator<Item = PathBuf> + 'a {
    let name = safe_name(game_id);
    EXTENSIONS
        .into_iter()
        .map(move |extension| dir.join(format!("{name}.{extension}")))
}

/// The user's own cover of a game: the file that holds a picture of the format its name says.
fn find_own(dir: &Path, game_id: &str) -> Option<PathBuf> {
    own_files(dir, game_id).find(|file| read_picture(file).is_some())
}

fn set_own_in(dir: &Path, game_id: &str, image: &Path) -> Result<Cover> {
    let not_a_picture = || Error::NotAPicture(image.to_path_buf());
    let bytes = fs::read(image).map_err(Error::io(image))?;
    let format = picture::format(&bytes).ok_or_else(not_a_picture)?;
    picture::decode(&bytes).map_err(|_| not_a_picture())?;
    fs::create_dir_all(dir).map_err(Error::io(dir))?;
    let file = dir.join(format!("{}.{}", safe_name(game_id), format.extension()));
    write_file(&file, &bytes).map_err(Error::io(&file))?;
    // The earlier choice, in another format.
    for other in own_files(dir, game_id).filter(|other| *other != file) {
        let _ = fs::remove_file(other);
    }
    Ok(Cover::new(file, true, false))
}

fn clear_own_in(dir: &Path, cache: Option<&Cache>, game: &Game) -> Result<()> {
    for file in own_files(dir, &game.id) {
        match fs::remove_file(&file) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::io(&file)(error));
            }
            _ => {}
        }
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

/// A file that holds a picture of the format its extension says.
fn read_picture(file: &Path) -> Option<Vec<u8>> {
    let bytes = read_file(file)?;
    let extension = file.extension()?.to_str()?;
    (picture::format(&bytes).map(Format::extension) == Some(extension)).then_some(bytes)
}

/// Written through a temporary file, so a reader never sees half a picture.
fn write_file(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = file.with_extension("tmp");
    fs::write(&temp, bytes)?;
    fs::rename(&temp, file)
}

/// The stores' original covers in one folder, each under its format's extension, and the `.none`
/// markers of lookups that found nothing. Every failure only means "not cached".
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

    fn stem(&self, identity: &str) -> PathBuf {
        self.dir
            .join(format!("{:016x}", fnv1a(identity.as_bytes())))
    }

    /// Where a cover may be cached: under each format's extension.
    fn files(&self, identity: &str) -> Vec<PathBuf> {
        let stem = self.stem(identity);
        EXTENSIONS
            .into_iter()
            .map(|extension| stem.with_extension(extension))
            .collect()
    }

    fn existing_file(&self, identity: &str) -> Option<PathBuf> {
        self.files(identity).into_iter().find(|file| file.is_file())
    }

    fn miss_file(&self, identity: &str) -> PathBuf {
        self.stem(identity).with_extension("none")
    }

    /// The cached file that holds a picture of the format its name says.
    fn find(&self, identity: &str) -> Option<PathBuf> {
        self.files(identity)
            .into_iter()
            .find(|file| read_picture(file).is_some())
    }

    /// Kept byte for byte under its format's extension: the file, or `None` when it is no
    /// picture or can't be written. A found cover also ends the wait a miss started.
    fn write(&self, identity: &str, bytes: &[u8]) -> Option<PathBuf> {
        let format = picture::format(bytes)?;
        let file = self.stem(identity).with_extension(format.extension());
        for other in self
            .files(identity)
            .into_iter()
            .filter(|other| *other != file)
        {
            let _ = fs::remove_file(other);
        }
        let written = write_file(&file, bytes).is_ok();
        let _ = fs::remove_file(self.miss_file(identity));
        written.then_some(file)
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
        for file in self.files(identity) {
            let _ = fs::remove_file(file);
        }
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
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| EXTENSIONS.contains(&ext) || ext == "none" || ext == "tmp");
            if cached && !used.contains(&path) {
                let _ = fs::remove_file(&path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Launch;
    use crate::picture::Rgba;

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

    /// What the cache holds for `identity`, read from its file.
    fn cached(cache: &Cache, identity: &str) -> Option<Vec<u8>> {
        cache.find(identity).map(|file| fs::read(file).unwrap())
    }

    /// A PNG of one colour.
    fn png(width: u32, height: u32) -> Vec<u8> {
        picture::encode_png(&Rgba {
            width,
            height,
            pixels: [30, 60, 90, 255].repeat((width * height) as usize),
        })
        .unwrap()
    }

    /// An uncompressed 24-bit BMP: the left half red, the right half blue.
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
    fn only_whole_pictures_are_kept_as_they_are() {
        let picture = png(6, 9);
        assert_eq!(whole_picture(picture.clone()), Some(picture.clone()));
        assert_eq!(whole_picture(picture[..40].to_vec()), None);
        assert_eq!(whole_picture(b"<html>Not found</html>".to_vec()), None);
        assert_eq!(whole_picture(Vec::new()), None);
    }

    #[test]
    fn the_cache_keeps_originals_by_format_and_week_long_misses() {
        let cache = Cache::at(temp_dir("cache")).unwrap();
        let (half_life, portal) = ("net|steam:220|2", "net|steam:400|2");
        assert_eq!(cached(&cache, half_life), None);
        assert!(!cache.missed(half_life));
        // Kept byte for byte, under the extension its format has.
        let original = png(6, 9);
        let file = cache.write(half_life, &original).unwrap();
        assert_eq!(cached(&cache, half_life), Some(original.clone()));
        assert_eq!(cache.existing_file(half_life), Some(file.clone()));
        assert_eq!(cache.find(half_life), Some(file.clone()));
        assert_eq!(file.extension().unwrap(), "png");
        assert_eq!(fs::read(&file).unwrap(), original);
        // A new original in another format replaces it; what is no picture is not written.
        let bitmap = bmp(4, 6);
        cache.write(half_life, &bitmap);
        assert_eq!(cached(&cache, half_life), Some(bitmap.clone()));
        assert!(!file.exists());
        assert_eq!(cache.write(portal, b"<html>"), None);
        assert_eq!(cache.existing_file(portal), None);
        // A file whose content isn't what its name says is not read.
        fs::write(cache.stem(portal).with_extension("jpg"), png(2, 3)).unwrap();
        assert_eq!(cached(&cache, portal), None);
        cache.forget(portal);

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
        cache.write(portal, &original);
        assert!(!cache.missed(portal));
        cache.forget(portal);
        assert_eq!(cached(&cache, portal), None);

        // Pruning keeps what is in use, and files that are not the cache's.
        cache.mark_miss(portal);
        let stale = cache.dir.join("0123456789abcdef.jpg");
        let old_webp = cache.dir.join("0123456789abcdee.webp");
        let leftover = cache.dir.join("fedcba9876543210.tmp");
        let unrelated = cache.dir.join("notes.txt");
        for file in [&stale, &old_webp, &leftover, &unrelated] {
            fs::write(file, b"x").unwrap();
        }
        let mut used: HashSet<PathBuf> = cache.files(half_life).into_iter().collect();
        used.insert(cache.miss_file(half_life));
        cache.prune(&used);
        assert_eq!(cached(&cache, half_life), Some(bitmap));
        assert!(!cache.miss_file(portal).exists());
        assert!(!stale.exists() && !old_webp.exists() && !leftover.exists());
        assert!(unrelated.exists());
        fs::remove_dir_all(&cache.dir).unwrap();
    }

    #[test]
    fn identities_follow_the_game_and_its_name() {
        let dead_space = game("ea:1", "Dead Space™", Store::Ea);
        assert_eq!(net_identity(&dead_space), "net|ea:1|2");
        assert_eq!(name_identity(&dead_space), "name|ea:1|deadspace|2");
        let renamed = game("ea:1", "Dead Space (2023)", Store::Ea);
        assert_ne!(name_identity(&renamed), name_identity(&dead_space));
        assert_eq!(safe_name("steam:220"), "steam_220");
        assert_eq!(safe_name("ea:{64CF}"), "ea__64CF_");
    }

    #[test]
    fn a_cover_is_the_original_and_a_copy_on_this_pc_shows_meanwhile() {
        let dir = temp_dir("chain");
        let cache = Cache::at(dir.join("cache")).unwrap();
        let own = dir.join("own");
        let steam = dir.join("steam");
        let client_cover = steam.join(r"appcache\librarycache\220\library_600x900.jpg");
        fs::create_dir_all(client_cover.parent().unwrap()).unwrap();
        // What Steam's client keeps (a 300 × 450 picture, here a BMP under its JPEG name).
        let client = bmp(300, 450);
        fs::write(&client_cover, &client).unwrap();
        // What an older Ubisoft Connect cached: a 240 × 280 picture, named by the configuration.
        let launcher = dir.join("ubisoft");
        let thumb = launcher.join(r"cache\assets\thumb.jpg");
        fs::create_dir_all(thumb.parent().unwrap()).unwrap();
        let small = bmp(240, 280);
        fs::write(&thumb, &small).unwrap();
        let stores = Stores {
            steam: Some(steam),
            galaxy: None,
            ubisoft: OnceCell::from(Some(ubisoft::Launcher {
                dir: launcher,
                configurations: [(609, "root:\n  thumb_image: thumb.jpg\n".to_owned())].into(),
            })),
        };
        let find = |game: &Game| {
            let (cover, ask) = local_cover(game, &cache, Some(&own), &stores);
            let read = |cover: Cover| (read(&cover).unwrap(), cover.own);
            (cover.map(read), ask)
        };
        let half_life = game("steam:220", "Half-Life 2", Store::Steam);

        // Steam's client copy shows while the original is asked for, until Steam says it has none.
        assert_eq!(find(&half_life), (Some((client.clone(), false)), true));
        cache.mark_miss(&net_identity(&half_life));
        assert_eq!(find(&half_life), (Some((client.clone(), false)), false));
        // The original, once it has arrived, as it was sent.
        let original = png(600, 900);
        cache.write(&net_identity(&half_life), &original);
        assert_eq!(find(&half_life), (Some((original.clone(), false)), false));
        // Only the copy on this PC is a stand-in.
        let stand_in = |game: &Game| {
            local_cover(game, &cache, Some(&own), &stores)
                .0
                .map(|cover| cover.stand_in)
        };
        assert_eq!(stand_in(&half_life), Some(false));
        assert_eq!(
            stand_in(&game("ubisoft:609", "Child of Light", Store::Ubisoft)),
            Some(true)
        );
        // The user's own picture wins over everything, kept as the file they chose.
        let picked = dir.join("picked.png");
        let mine = png(200, 300);
        fs::write(&picked, &mine).unwrap();
        let set = set_own_in(&own, &half_life.id, &picked).unwrap();
        assert_eq!((read(&set).unwrap(), set.own), (mine.clone(), true));
        assert_eq!(set.file, own.join("steam_220.png"));
        assert_eq!(fs::read(own.join("steam_220.png")).unwrap(), mine);
        assert_eq!(find(&half_life), (Some((mine, true)), false));
        // Choosing again in another format replaces the earlier file.
        let picked_bmp = dir.join("picked.bmp");
        fs::write(&picked_bmp, bmp(20, 30)).unwrap();
        set_own_in(&own, &half_life.id, &picked_bmp).unwrap();
        assert!(own.join("steam_220.bmp").is_file() && !own.join("steam_220.png").exists());
        // Reset: the own picture goes, and the download with it, so the client's copy shows again
        // while the original is asked for anew.
        clear_own_in(&own, Some(&cache), &half_life).unwrap();
        clear_own_in(&own, Some(&cache), &half_life).unwrap();
        assert_eq!(find(&half_life), (Some((client, false)), true));

        // A Steam app the client has no cover of: the server, until it says it has none.
        let tool = game("steam:7", "A tool", Store::Steam);
        assert_eq!(find(&tool), (None, true));
        cache.mark_miss(&net_identity(&tool));
        assert_eq!(find(&tool), (None, false));
        // An EA game has only the lookup by name; an Epic game its store, then the name.
        let dead_space = game("ea:1", "Dead Space™", Store::Ea);
        assert_eq!(find(&dead_space), (None, true));
        cache.mark_miss(&name_identity(&dead_space));
        assert_eq!(find(&dead_space), (None, false));
        let sifu = game("epic:d363", "Sifu", Store::Epic);
        cache.mark_miss(&net_identity(&sifu));
        assert_eq!(find(&sifu), (None, true));
        let by_name = png(600, 900);
        cache.write(&name_identity(&sifu), &by_name);
        assert_eq!(find(&sifu), (Some((by_name.clone(), false)), false));
        // A Ubisoft game shows the launcher's small picture while the Steam store is asked by name;
        // what that finds wins, and a miss keeps the small one.
        let child_of_light = game("ubisoft:609", "Child of Light", Store::Ubisoft);
        assert_eq!(find(&child_of_light), (Some((small.clone(), false)), true));
        cache.mark_miss(&name_identity(&child_of_light));
        assert_eq!(find(&child_of_light), (Some((small, false)), false));
        cache.write(&name_identity(&child_of_light), &by_name);
        assert_eq!(find(&child_of_light), (Some((by_name, false)), false));
        // One the launcher has no picture of: the lookup by name alone.
        let anno = game("ubisoft:4553", "Anno 1800", Store::Ubisoft);
        assert_eq!(find(&anno), (None, true));

        // Not a picture: an error that names the file, and nothing saved.
        let text = dir.join("notes.txt");
        fs::write(&text, b"not a picture").unwrap();
        let error = set_own_in(&own, &tool.id, &text).unwrap_err();
        assert!(matches!(error, Error::NotAPicture(file) if file == text));
        assert!(own_files(&own, &tool.id).all(|file| !file.exists()));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// One real original cover from each network source. Run with
    /// `cargo test -p gm-core live_covers -- --ignored --nocapture`.
    #[test]
    #[ignore = "asks Steam's, GOG's, Microsoft's and Epic's servers"]
    fn live_covers() {
        // GOG's covers are 342 × 482 in every form its image server makes; the others at least
        // Steam's 600 × 900.
        let check = |source: &str, url: Option<String>| {
            let least = if source.starts_with("GOG") { 342 } else { 600 };
            let url = url.unwrap_or_else(|| panic!("{source}: no address"));
            let bytes = download(&url)
                .unwrap()
                .unwrap_or_else(|| panic!("{source}: no picture at {url}"));
            let format = picture::format(&bytes).unwrap();
            let (width, height) = picture::size(&bytes).unwrap();
            println!(
                "{source}: {format} {width} × {height}, {} bytes, {url}",
                bytes.len()
            );
            // Portrait, between 2:3 and 3:4.
            assert!(width >= least, "{source}: {width} × {height}");
            let ratio = f64::from(width) / f64::from(height);
            assert!((0.6..=0.78).contains(&ratio), "{source}: {ratio}");
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
        assert_eq!(download(gone).unwrap(), None);
    }
}
