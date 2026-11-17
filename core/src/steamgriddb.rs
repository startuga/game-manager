//! SteamGridDB (steamgriddb.com): pictures people made for games (covers, which it calls grids;
//! wide pictures, heroes; logos; icons), through its API v2 with the user's own API key.
//!
//! - **The key**: each user's own, made in their SteamGridDB profile (Preferences ▸ API,
//!   `KEY_PAGE`), kept in Windows Credential Manager (`credentials.rs`, under `KEY_TARGET`). It is
//!   sent only as the `Authorization: Bearer` header, never in a URL, a file of Game Manager's or
//!   an error. None ships with Game Manager: SteamGridDB publishes no terms for sharing one, and one
//!   key would carry every user's requests.
//! - **The game** (`find_game`): a Steam game by its app id (`/games/steam/<id>`); any other, or a
//!   Steam id SteamGridDB doesn't know, by name (`/search/autocomplete/<name>`): the result with the
//!   same letters (`text::letters`). Without one, the results are offered to choose from rather
//!   than the first taken ("Artwork Check" found "Please, Touch The Artwork" first). `search`
//!   lists the games for another name.
//! - **Pictures** (`pictures`): `/{grids|heroes|logos|icons}/game/<id>`, `PAGE_SIZE` a page, the
//!   animated ones first (WebP only: the window plays WebP, and an APNG would show only its first
//!   frame), then the still ones. Covers are Steam's portrait sizes, wide pictures Steam's hero
//!   sizes, as SteamGridDB's own Steam Deck plugin asks. Adult pictures and ones flagged for
//!   flashing lights are left out.
//! - **Previews** (`preview`) are kept in `%LOCALAPPDATA%\Game Manager\cache\steamgriddb`, by
//!   the picture's id, byte for byte: a still picture's thumbnail; an animated one's is a WebM
//!   video (3 MB for one of Beat Saber's heroes), which the app can't show, so the animated WebP
//!   itself is downloaded and its first frame shown; choosing it then needs no download. A picture chosen is downloaded there (`download`), then made
//!   the game's own (covers, page art, icon), so it stays when the cache is cleared.
//! - **What was chosen** (`chosen`, `record`): which SteamGridDB picture each game's art came from,
//!   in `%APPDATA%\Game Manager\steamgriddb.json`, so the Artwork dialog marks it.
//!
//! Every function here is blocking (the network, Credential Manager, files).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::text::letters;
use crate::{Error, Game, Result, Store, credentials, http, json_file, whole_file};

/// The API's address.
const API: &str = "https://www.steamgriddb.com/api/v2";
/// Where the key is kept in Credential Manager.
const KEY_TARGET: &str = "Game Manager/SteamGridDB";
/// Where a user makes their key.
pub const KEY_PAGE: &str = "https://www.steamgriddb.com/profile/preferences/api";
/// How many pictures the API gives a page.
pub const PAGE_SIZE: usize = 50;
/// The record of what was chosen, in `%APPDATA%\Game Manager`.
const CHOICES_FILE: &str = "steamgriddb.json";

/// The user's API key; `None` when there is none.
pub fn key() -> Result<Option<String>> {
    Ok(credentials::read(KEY_TARGET)?.filter(|key| !key.trim().is_empty()))
}

/// Keeps `key` as the user's API key (spaces around it dropped).
pub fn set_key(key: &str) -> Result<()> {
    credentials::write(KEY_TARGET, key.trim())
}

/// Drops the user's API key.
pub fn remove_key() -> Result<()> {
    credentials::delete(KEY_TARGET)
}

/// The kinds of art SteamGridDB has, as the Artwork dialog's tabs name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// The portrait picture on a Library tile (a "grid").
    Cover,
    /// The wide picture at the top of the game's page (a "hero").
    Banner,
    Logo,
    Icon,
}

impl Kind {
    pub const ALL: [Kind; 4] = [Kind::Cover, Kind::Banner, Kind::Logo, Kind::Icon];

    fn endpoint(self) -> &'static str {
        match self {
            Kind::Cover => "grids",
            Kind::Banner => "heroes",
            Kind::Logo => "logos",
            Kind::Icon => "icons",
        }
    }

    /// The sizes asked for; none means any.
    fn dimensions(self) -> &'static [&'static str] {
        match self {
            Kind::Cover => &["600x900", "342x482", "660x930"],
            Kind::Banner => &["1920x620", "3840x1240", "1600x650"],
            Kind::Logo | Kind::Icon => &[],
        }
    }

    /// The formats of still pictures asked for.
    fn still_mimes(self) -> &'static [&'static str] {
        match self {
            Kind::Cover | Kind::Banner => &["image/png", "image/jpeg", "image/webp"],
            Kind::Logo => &["image/png", "image/webp"],
            Kind::Icon => &["image/png", "image/vnd.microsoft.icon"],
        }
    }

    /// Whether animated ones are asked for too.
    fn animates(self) -> bool {
        self != Kind::Icon
    }
}

/// A game on SteamGridDB.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SgdbGame {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub verified: bool,
    /// Its release date, seconds since 1970, when SteamGridDB knows it.
    #[serde(default)]
    pub release_date: Option<i64>,
}

/// A picture on SteamGridDB.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Picture {
    pub id: u64,
    /// The picture itself.
    pub url: String,
    /// A small copy, for choosing.
    pub thumb: String,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub mime: String,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub author: Option<Author>,
    /// Asked for as an animated picture.
    #[serde(skip)]
    pub animated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub name: String,
}

/// The API's answers: `{ "success": true, "data": … }`.
#[derive(Deserialize)]
struct Answer<T> {
    #[serde(default)]
    success: bool,
    data: Option<T>,
}

/// `GET` a path of the API with the key; `None` when SteamGridDB has no such thing (404).
fn call<T: DeserializeOwned>(key: &str, path: &str) -> Result<Option<T>> {
    let url = format!("{API}{path}");
    let body = match http::get_with(&url, &[format!("Authorization: Bearer {key}")]) {
        Ok(body) => body,
        Err(Error::Http { status: 404, .. }) => return Ok(None),
        Err(error) => return Err(error),
    };
    parse(&url, &body)
}

/// An answer's data, when it says it succeeded.
fn parse<T: DeserializeOwned>(url: &str, body: &[u8]) -> Result<Option<T>> {
    let answer: Answer<T> = serde_json::from_slice(body).map_err(|source| Error::WebJson {
        url: url.to_owned(),
        source,
    })?;
    Ok(answer.data.filter(|_| answer.success))
}

/// `text` for a URL's path or query: anything but letters, digits and `-._~` percent-encoded.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The games SteamGridDB finds for `name`, best first.
pub fn search(key: &str, name: &str) -> Result<Vec<SgdbGame>> {
    let name = name.trim();
    if name.is_empty() {
        return Ok(Vec::new());
    }
    Ok(call(key, &format!("/search/autocomplete/{}", encode(name)))?.unwrap_or_default())
}

/// What looking a game up on SteamGridDB found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Found {
    /// The game itself.
    Game(SgdbGame),
    /// No game by its name: these were found for it, to choose from (none, maybe).
    Choices(Vec<SgdbGame>),
}

/// The game's page on SteamGridDB (see the module's doc).
pub fn find_game(key: &str, game: &Game) -> Result<Found> {
    if game.store == Store::Steam
        && let Some(app) = game.id.strip_prefix("steam:")
        && app.bytes().all(|byte| byte.is_ascii_digit())
        && let Some(found) = call::<SgdbGame>(key, &format!("/games/steam/{app}"))?
    {
        return Ok(Found::Game(found));
    }
    Ok(pick(&game.name, search(key, &game.name)?))
}

/// Among `found`, the game named `name` (the same letters), else all of them to choose from.
fn pick(name: &str, found: Vec<SgdbGame>) -> Found {
    let wanted = letters(name);
    match found.iter().position(|game| letters(&game.name) == wanted) {
        Some(same) => Found::Game(found[same].clone()),
        None => Found::Choices(found),
    }
}

/// The query asking for `kind`'s pictures, animated or still, on page `page` (from 0).
fn query(kind: Kind, animated: bool, page: u32) -> String {
    let join = |values: &[&str]| {
        values
            .iter()
            .map(|value| encode(value))
            .collect::<Vec<_>>()
            .join(",")
    };
    let (types, mimes) = if animated {
        ("animated", join(&["image/webp"]))
    } else {
        ("static", join(kind.still_mimes()))
    };
    let mut query = format!("?types={types}&mimes={mimes}");
    if !kind.dimensions().is_empty() {
        query.push_str(&format!("&dimensions={}", join(kind.dimensions())));
    }
    query.push_str(&format!("&nsfw=false&humor=any&epilepsy=false&page={page}"));
    query
}

/// A page of the game's pictures of `kind` (from 0): the animated ones, then the still ones; and
/// whether there may be more.
pub fn pictures(key: &str, kind: Kind, game: u64, page: u32) -> Result<(Vec<Picture>, bool)> {
    let path = |animated| {
        format!(
            "/{}/game/{game}{}",
            kind.endpoint(),
            query(kind, animated, page)
        )
    };
    let mut all = Vec::new();
    let mut more = false;
    if kind.animates() {
        let animated: Vec<Picture> = call(key, &path(true))?.unwrap_or_default();
        more |= animated.len() >= PAGE_SIZE;
        all.extend(animated.into_iter().map(|picture| Picture {
            animated: true,
            ..picture
        }));
    }
    let still: Vec<Picture> = call(key, &path(false))?.unwrap_or_default();
    more |= still.len() >= PAGE_SIZE;
    all.extend(still);
    Ok((all, more))
}

/// `%LOCALAPPDATA%\Game Manager\cache\steamgriddb`.
fn cache_dir() -> Result<PathBuf> {
    let dir = json_file::local_file("cache")?.join("steamgriddb");
    fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
    Ok(dir)
}

/// A file name for a picture of SteamGridDB's: its id, and the extension of `url`'s file (only
/// letters and digits; "img" without one).
fn file_name(id: u64, url: &str, suffix: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .filter(|extension| {
            !extension.is_empty()
                && extension.len() <= 5
                && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .unwrap_or_else(|| "img".to_owned());
    format!("{id}{suffix}.{extension}")
}

/// A picture to show for `picture` while choosing (see the module's doc): its thumbnail, or for
/// an animated one the picture itself; from the cache or downloaded into it. `None` when it can't
/// be had.
pub fn preview(picture: &Picture) -> Option<Vec<u8>> {
    if picture.animated {
        return download(picture).ok().and_then(|file| fs::read(file).ok());
    }
    thumbnail(picture)
}

/// A still picture's thumbnail, from the cache or downloaded into it.
fn thumbnail(picture: &Picture) -> Option<Vec<u8>> {
    let file = cache_dir()
        .ok()?
        .join(file_name(picture.id, &picture.thumb, ".thumb"));
    if let Ok(bytes) = fs::read(&file)
        && !bytes.is_empty()
    {
        return Some(bytes);
    }
    let bytes = http::get(&picture.thumb).ok()?;
    let _ = whole_file::write(&file, &bytes);
    Some(bytes)
}

/// The picture itself, downloaded into the cache once: its file.
pub fn download(picture: &Picture) -> Result<PathBuf> {
    let file = cache_dir()?.join(file_name(picture.id, &picture.url, ""));
    if fs::metadata(&file).is_ok_and(|metadata| metadata.len() > 0) {
        return Ok(file);
    }
    let part = file.with_extension("part");
    http::download(&picture.url, &part)?;
    fs::rename(&part, &file).map_err(Error::io(&file))?;
    Ok(file)
}

/// What was chosen from SteamGridDB, by game id; other fields are kept.
#[derive(Default, Serialize, Deserialize)]
struct Choices {
    #[serde(default)]
    games: BTreeMap<String, BTreeMap<Kind, u64>>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

/// The SteamGridDB pictures the game's art came from, by kind.
pub fn chosen(game_id: &str) -> BTreeMap<Kind, u64> {
    json_file::user_file(CHOICES_FILE)
        .ok()
        .map_or_default(|file| chosen_in(&file, game_id))
}

fn chosen_in(file: &Path, game_id: &str) -> BTreeMap<Kind, u64> {
    json_file::load::<Choices>(file)
        .ok()
        .and_then(|mut choices| choices.games.remove(game_id))
        .unwrap_or_default()
}

/// Notes that the game's `kind` came from SteamGridDB's picture `id`, or (`None`) no longer does
/// (another file chosen, or the store's again).
pub fn record(game_id: &str, kind: Kind, id: Option<u64>) -> Result<()> {
    record_in(&json_file::user_file(CHOICES_FILE)?, game_id, kind, id)
}

fn record_in(file: &Path, game_id: &str, kind: Kind, id: Option<u64>) -> Result<()> {
    let mut choices: Choices = json_file::load(file)?;
    // Nothing to forget: the file isn't written (nor made, for one who never chose there).
    let noted = choices
        .games
        .get(game_id)
        .is_some_and(|kinds| kinds.contains_key(&kind));
    if id.is_none() && !noted {
        return Ok(());
    }
    let kinds = choices.games.entry(game_id.to_owned()).or_default();
    match id {
        Some(id) => kinds.insert(kind, id),
        None => kinds.remove(&kind),
    };
    if kinds.is_empty() {
        choices.games.remove(game_id);
    }
    json_file::save(file, &choices)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn game(id: u64, name: &str) -> SgdbGame {
        SgdbGame {
            id,
            name: name.to_owned(),
            verified: true,
            release_date: None,
        }
    }

    #[test]
    fn names_and_queries_are_encoded() {
        assert_eq!(
            encode("Tomb Raider: Underworld"),
            "Tomb%20Raider%3A%20Underworld"
        );
        assert_eq!(encode("Nier/Automata™"), "Nier%2FAutomata%E2%84%A2");
        assert_eq!(
            query(Kind::Cover, false, 0),
            "?types=static&mimes=image%2Fpng,image%2Fjpeg,image%2Fwebp\
             &dimensions=600x900,342x482,660x930&nsfw=false&humor=any&epilepsy=false&page=0"
        );
        // Animated: WebP only; logos and icons any size.
        assert_eq!(
            query(Kind::Logo, true, 2),
            "?types=animated&mimes=image%2Fwebp&nsfw=false&humor=any&epilepsy=false&page=2"
        );
        assert!(query(Kind::Icon, false, 0).contains("image%2Fvnd.microsoft.icon"));
        assert!(!Kind::Icon.animates());
    }

    #[test]
    fn a_game_is_found_by_its_letters_else_offered_to_choose() {
        let found = vec![game(1, "Tomb Raider"), game(2, "Tomb Raider: Underworld")];
        assert_eq!(
            pick("TOMB RAIDER™ Underworld", found.clone()),
            Found::Game(game(2, "Tomb Raider: Underworld"))
        );
        assert_eq!(pick("Lara Croft", found.clone()), Found::Choices(found));
        assert_eq!(pick("Anything", Vec::new()), Found::Choices(Vec::new()));
    }

    #[test]
    fn answers_are_read() {
        let search = br#"{"success":true,"data":[
            {"id":5247,"name":"Tomb Raider: Underworld","release_date":1227571200,"types":["steam"],"verified":true},
            {"id":9,"name":"Other","types":[],"verified":false}]}"#;
        let games: Vec<SgdbGame> = parse("search", search).unwrap().unwrap();
        assert_eq!(games[0].id, 5247);
        assert_eq!(games[0].release_date, Some(1_227_571_200));
        assert!(!games[1].verified);
        let grids = br#"{"success":true,"page":0,"total":1,"limit":50,"data":[
            {"id":80,"score":0,"style":"alternate","width":600,"height":900,"nsfw":false,"humor":false,
             "notes":null,"mime":"image/png","language":"en",
             "url":"https://cdn2.steamgriddb.com/grid/a.png","thumb":"https://cdn2.steamgriddb.com/thumb/a.jpg",
             "lock":false,"epilepsy":false,"upvotes":3,"downvotes":0,
             "author":{"name":"someone","steam64":"1","avatar":"x"}}]}"#;
        let pictures: Vec<Picture> = parse("grids", grids).unwrap().unwrap();
        assert_eq!(pictures[0].id, 80);
        assert_eq!((pictures[0].width, pictures[0].height), (600, 900));
        assert_eq!(pictures[0].author.as_ref().unwrap().name, "someone");
        assert!(!pictures[0].animated);
        // An answer that says it failed has no data; one that isn't JSON is an error.
        let failed = br#"{"success":false,"errors":["Game not found."]}"#;
        assert_eq!(parse::<Vec<Picture>>("x", failed).unwrap(), None);
        assert!(parse::<Vec<Picture>>("x", b"<html>").is_err());
    }

    #[test]
    fn pictures_are_named_after_their_ids() {
        assert_eq!(
            file_name(7, "https://cdn2.steamgriddb.com/thumb/ab.JPG", ".thumb"),
            "7.thumb.jpg"
        );
        assert_eq!(file_name(7, "https://x/grid/a.webp?v=2", ""), "7.webp");
        assert_eq!(file_name(7, "https://x/grid/noextension", ""), "7.img");
        assert_eq!(file_name(7, "https://x/a.../b.p/ng", ""), "7.img");
    }

    #[test]
    fn choices_are_kept_by_game_and_kind() {
        let dir = std::env::temp_dir().join(format!("gm-sgdb-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(CHOICES_FILE);
        // Forgetting what was never noted writes nothing, not even a file.
        record_in(&file, "steam:8000", Kind::Cover, None).unwrap();
        assert!(!file.exists());
        fs::write(&file, r#"{ "later": 1 }"#).unwrap();
        assert!(chosen_in(&file, "steam:8000").is_empty());
        record_in(&file, "steam:8000", Kind::Cover, Some(80)).unwrap();
        record_in(&file, "steam:8000", Kind::Logo, Some(81)).unwrap();
        let chosen = chosen_in(&file, "steam:8000");
        assert_eq!(chosen.get(&Kind::Cover), Some(&80));
        assert_eq!(chosen.get(&Kind::Logo), Some(&81));
        // Another file chosen: forgotten; the game's entry goes with its last kind.
        record_in(&file, "steam:8000", Kind::Cover, None).unwrap();
        record_in(&file, "steam:8000", Kind::Logo, None).unwrap();
        let saved: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["later"], 1);
        assert!(saved["games"].as_object().unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
