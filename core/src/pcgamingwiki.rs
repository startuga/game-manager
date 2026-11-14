//! PCGamingWiki (fixes, settings, save locations, technical data):
//!
//! - **Links** to a game's page, opened in the browser, need no request. Steam and GOG games use
//!   the wiki's id redirects, which go straight to the article without a Cloudflare check (an id
//!   the wiki does not know shows "No such AppID"). Other games use the wiki search, which opens
//!   the article when the name is its title and the results otherwise, behind Cloudflare's
//!   "confirm you are human" check in a fresh browser.
//! - **A game's page** (`page`) is read through the wiki's API without an account: `idlookup` by
//!   Steam or GOG id, else the wiki's full-text search (`list=search`, five results) by name, then
//!   `parse&prop=wikitext`. The wiki's structured data (`cargoquery`, its `GameData` table of save
//!   and config paths too) needs a bot-password login since August 2026, hence the wikitext.
//!   Requests carry a descriptive User-Agent and are spaced one second apart, as the wiki asks (at
//!   most 60 a minute; HTTP 429 blocks for a minute). The pages read in the last ten minutes are
//!   kept in memory, so the graphics API and the game's folders, looked up together when a game is
//!   shown, cost one page.
//! - **By name**: the search is fuzzy, so a title counts only when its letters (ignoring case,
//!   spaces and punctuation, `&` read as "and") are the game's name or begin it, and the longest
//!   such title wins ("Ghostbusters: Spirits Unleashed" over "Ghostbusters"). Names are searched
//!   with plain apostrophes ("Assassin’s" finds nothing), and a name ending in "Edition" again
//!   without it ("… Ecto Edition"). The prefix search (`opensearch`) found 27 of 38 games on the PC
//!   this was written on, the full-text search 31: "Ghostwire Tokyo" is "GhostWire: Tokyo".
//! - **The API table** (`{{API}}`: Direct3D, OpenGL and Vulkan versions): `api_table`.
//! - **Game data** (`game_data`): the paths of the `{{Game data/saves|<platform>|<path>|…}}` and
//!   `{{Game data/config|…}}` templates as the wiki writes them, with their `{{p|…}}` placeholders
//!   (`game_data.rs` fills them in for this PC).

use crate::graphics::{GraphicsApi, GraphicsInfo, GraphicsSource};
use crate::text::{percent_encode, search_name};
use crate::{Error, Game, Result, Store, http};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const SITE: &str = "https://www.pcgamingwiki.com";
/// Time between two requests to the wiki's API.
const REQUEST_INTERVAL: Duration = Duration::from_secs(1);
/// When the last request was made, shared by every thread.
static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

/// How long a page read is kept in memory (`page`).
const PAGE_LIFE: Duration = Duration::from_secs(10 * 60);
/// The pages read lately, by game.
static PAGES: Mutex<Vec<Remembered>> = Mutex::new(Vec::new());

/// A page read lately for a game (`None`: the wiki has none for it).
struct Remembered {
    game_id: String,
    at: Instant,
    page: Option<Page>,
}

/// A game's page on the wiki: its title and its wikitext.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Page {
    pub title: String,
    pub wikitext: String,
}

/// The APIs in the game's API table on the wiki; `None` when the wiki has no page for the game, or
/// its page lists no API. A page found by name may be another game of the same name (Dead Space,
/// 2008, for the 2023 remake): the user can choose the right API in the game's settings.
/// Blocking: up to five web requests (`page`).
pub fn api_table(game: &Game) -> Result<Option<GraphicsInfo>> {
    let Some(page) = page(game)? else {
        return Ok(None);
    };
    let apis = parse_api_table(&page.wikitext);
    Ok((!apis.is_empty()).then_some(GraphicsInfo {
        apis,
        source: GraphicsSource::PcGamingWiki { page: page.title },
    }))
}

/// The game's page (see the module's doc); `None` when the wiki has none. One lookup at a time, so
/// a second one for the same game meanwhile gets the page the first read. Blocking: up to five
/// web requests (an id lookup, three searches, the page), none for a page read in the last ten
/// minutes.
pub(crate) fn page(game: &Game) -> Result<Option<Page>> {
    let mut pages = PAGES.lock().unwrap_or_else(PoisonError::into_inner);
    pages.retain(|remembered| remembered.at.elapsed() < PAGE_LIFE);
    if let Some(remembered) = pages
        .iter()
        .find(|remembered| remembered.game_id == game.id)
    {
        return Ok(remembered.page.clone());
    }
    let page = read_page(game, &pages)?;
    pages.push(Remembered {
        game_id: game.id.clone(),
        at: Instant::now(),
        page: page.clone(),
    });
    Ok(page)
}

/// Reads the game's page from the wiki; `_held` is the memo's lock, held meanwhile.
fn read_page(game: &Game, _held: &MutexGuard<'_, Vec<Remembered>>) -> Result<Option<Page>> {
    let Some(title) = find_page(game)? else {
        return Ok(None);
    };
    let path = format!(
        "/w/api.php?action=parse&page={}&prop=wikitext&redirects=1&format=json&formatversion=2",
        percent_encode(&title)
    );
    Ok(wikitext(&request(&path)?).map(|wikitext| Page { title, wikitext }))
}

/// The title of the game's page: by store id for Steam and GOG, else (or when the wiki does not
/// know the id) by name, searched as it is and, ending in "Edition", without it.
fn find_page(game: &Game) -> Result<Option<String>> {
    let field = match game.store {
        Store::Steam => Some("steamappid"),
        Store::Gog => Some("gogid"),
        _ => None,
    };
    if let (Some(field), Some(id)) = (field, store_number(&game.id)) {
        let path = format!("/w/api.php?action=idlookup&field={field}&value={id}&format=json");
        if let Some(page) = idlookup_page(&request(&path)?) {
            return Ok(Some(page));
        }
    }
    let name = wiki_name(&game.name);
    for search in search_names(&name) {
        let path = format!(
            "/w/api.php?action=query&list=search&srsearch={}&srlimit=5&srnamespace=0&srprop=\
             &format=json&formatversion=2",
            percent_encode(&search)
        );
        if let Some(title) = best_title(&search_titles(&request(&path)?), &name) {
            return Ok(Some(title));
        }
    }
    Ok(None)
}

/// A game's name as the wiki writes it: without ™ ® ©, with plain apostrophes.
fn wiki_name(name: &str) -> String {
    search_name(name).replace(['’', '‘'], "'")
}

/// What to search for: the name, then, when it ends in "Edition", the name without its last two
/// words ("Spirits Unleashed Ecto Edition") and the name before its last `:` or ` - `.
fn search_names(name: &str) -> Vec<String> {
    let mut names = vec![name.to_owned()];
    let words: Vec<&str> = name.split_whitespace().collect();
    let edition = words
        .last()
        .is_some_and(|word| word.eq_ignore_ascii_case("edition"));
    if edition && words.len() > 2 {
        let shorter = words[..words.len() - 2].join(" ");
        names.push(shorter.trim_end_matches([':', '-', '–', ' ']).to_owned());
        let cut = [": ", " - ", " – "]
            .iter()
            .filter_map(|separator| name.rfind(separator))
            .max();
        if let Some(cut) = cut {
            names.push(name[..cut].trim().to_owned());
        }
    }
    names.retain(|name| !name.is_empty());
    names.dedup();
    names
}

/// `{"query":{"search":[{"title":"…"}, …]}}`: the titles found, best first.
fn search_titles(answer: &Value) -> Vec<String> {
    answer["query"]["search"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|result| result["title"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The longest of `titles` that is the game called `name` (`same_game`).
fn best_title(titles: &[String], name: &str) -> Option<String> {
    titles
        .iter()
        .filter(|title| same_game(title, name))
        .max_by_key(|title| letters(title).len())
        .cloned()
}

/// A request to the wiki's API, at most one a second, answered with JSON.
fn request(path: &str) -> Result<Value> {
    {
        let mut last = LAST_REQUEST.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(wait) =
            last.and_then(|at| (at + REQUEST_INTERVAL).checked_duration_since(Instant::now()))
        {
            std::thread::sleep(wait);
        }
        *last = Some(Instant::now());
    }
    let url = format!("{SITE}{path}");
    let body = http::get(&url)?;
    serde_json::from_slice(&body).map_err(|source| Error::WebJson { url, source })
}

/// `{"idlookup":[{"title":{"Page":"…"}}]}`; an empty list when the wiki does not know the id.
fn idlookup_page(answer: &Value) -> Option<String> {
    answer["idlookup"][0]["title"]["Page"]
        .as_str()
        .map(str::to_owned)
}

/// `{"parse":{"wikitext":"…"}}`; an error object when the page does not exist.
fn wikitext(answer: &Value) -> Option<String> {
    answer["parse"]["wikitext"].as_str().map(str::to_owned)
}

/// The wiki search is fuzzy ("SteamVR" finds "VR Karts SteamVR"), so a title found by name is used
/// only when, ignoring case, spaces and punctuation, it is the game's name or begins it ("Tomb
/// Raider I–III Remastered" for "Tomb Raider I-III Remastered Starring Lara Croft").
fn same_game(title: &str, name: &str) -> bool {
    let (title, name) = (letters(title), letters(name));
    !title.is_empty() && name.starts_with(&title)
}

/// A name's letters and digits in lower case, `&` read as "and" ("Beyond Good & Evil").
fn letters(text: &str) -> String {
    text.replace('&', "and")
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A path the wiki gives for a game's saves or settings, as written (`{{p|…}}` placeholders, notes
/// and all), and the platform it is for ("Windows", "Steam", "GOG.com", "Microsoft Store"…).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiPath {
    pub platform: String,
    pub path: String,
}

/// The paths of a page's `{{Game data/saves|…}}` and `{{Game data/config|…}}` templates, in the
/// page's order: `(saves, config)`.
pub(crate) fn game_data(wikitext: &str) -> (Vec<WikiPath>, Vec<WikiPath>) {
    (
        game_data_paths(wikitext, "saves"),
        game_data_paths(wikitext, "config"),
    )
}

/// Every `{{Game data/<kind>|<platform>|<path>|…}}`'s paths: its arguments split at the `|`s of its
/// own level, so the placeholders inside (`{{p|game}}`) stay whole.
fn game_data_paths(wikitext: &str, kind: &str) -> Vec<WikiPath> {
    let open = format!("{{{{Game data/{kind}|");
    let mut paths = Vec::new();
    let mut rest = wikitext;
    while let Some(at) = rest.find(&open) {
        let (arguments, after) = template_arguments(&rest[at + open.len()..]);
        rest = after;
        let mut arguments = arguments
            .into_iter()
            .map(|argument| argument.trim().to_owned());
        let Some(platform) = arguments.next() else {
            continue;
        };
        paths.extend(
            arguments
                .filter(|path| !path.is_empty())
                .map(|path| WikiPath {
                    platform: platform.clone(),
                    path,
                }),
        );
    }
    paths
}

/// The `|`-separated arguments of a template whose name and first `|` are already read, up to its
/// closing `}}`, and the text after it (none when it is never closed).
fn template_arguments(text: &str) -> (Vec<String>, &str) {
    let mut arguments = vec![String::new()];
    let mut depth = 0usize;
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let current = arguments.len() - 1;
        if rest.starts_with("{{") {
            depth += 1;
            arguments[current].push_str("{{");
            rest = &rest[2..];
        } else if rest.starts_with("}}") {
            if depth == 0 {
                return (arguments, &rest[2..]);
            }
            depth -= 1;
            arguments[current].push_str("}}");
            rest = &rest[2..];
        } else {
            if c == '|' && depth == 0 {
                arguments.push(String::new());
            } else {
                arguments[current].push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    (arguments, "")
}

/// The APIs in a page's `{{API}}` template: each `direct3d versions` entry's major version (`9.0c`,
/// `9Ex` → 9; `10.1` → 10), and OpenGL or Vulkan when their versions are given (`true`, `4.5`).
/// Unknown Direct3D versions (`true`) and older ones than 8 are left out.
fn parse_api_table(wikitext: &str) -> Vec<GraphicsApi> {
    let Some(start) = wikitext.match_indices("{{API").find_map(|(at, _)| {
        let rest = wikitext.get(at + "{{API".len()..)?;
        // Not `{{API/switch …}}` and the like.
        rest.starts_with(|c: char| c.is_whitespace() || c == '|')
            .then_some(rest)
    }) else {
        return Vec::new();
    };
    let mut apis = BTreeSet::new();
    for line in start.lines().map(str::trim) {
        if line.starts_with("}}") {
            break;
        }
        let Some((key, value)) = line.trim_start_matches('|').split_once('=') else {
            continue;
        };
        let value = value.trim();
        let given = !value.is_empty() && !value.eq_ignore_ascii_case("false");
        match key.trim().to_ascii_lowercase().as_str() {
            "direct3d versions" => apis.extend(value.split(',').filter_map(direct3d_version)),
            "opengl versions" if given => {
                apis.insert(GraphicsApi::OpenGl);
            }
            "vulkan versions" if given => {
                apis.insert(GraphicsApi::Vulkan);
            }
            _ => {}
        }
    }
    apis.into_iter().collect()
}

fn direct3d_version(entry: &str) -> Option<GraphicsApi> {
    let digits: String = entry
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    match digits.parse().ok()? {
        8 => Some(GraphicsApi::DirectX8),
        9 => Some(GraphicsApi::DirectX9),
        10 => Some(GraphicsApi::DirectX10),
        11 => Some(GraphicsApi::DirectX11),
        12 => Some(GraphicsApi::DirectX12),
        _ => None,
    }
}

/// The address of the game's PCGamingWiki page.
pub fn page_url(game: &Game) -> String {
    match (game.store, store_number(&game.id)) {
        (Store::Steam, Some(app_id)) => format!("{SITE}/api/appid.php?appid={app_id}"),
        (Store::Gog, Some(product_id)) => format!("{SITE}/api/gog.php?page={product_id}"),
        _ => format!(
            "{SITE}/w/index.php?search={}",
            percent_encode(&search_name(&game.name))
        ),
    }
}

/// The numeric store id of a `<store>:<id>` game id (Steam AppID, GOG product id).
fn store_number(id: &str) -> Option<&str> {
    let (_, number) = id.split_once(':')?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Launch;

    fn game(store: Store, id: &str, name: &str) -> Game {
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

    #[test]
    fn steam_and_gog_games_use_the_id_redirects() {
        assert_eq!(
            page_url(&game(Store::Steam, "steam:292030", "The Witcher 3")),
            "https://www.pcgamingwiki.com/api/appid.php?appid=292030"
        );
        assert_eq!(
            page_url(&game(Store::Gog, "gog:1495134320", "The Witcher 3 - GOTY")),
            "https://www.pcgamingwiki.com/api/gog.php?page=1495134320"
        );
    }

    #[test]
    fn other_games_are_searched_by_name() {
        let search = |store, id| page_url(&game(store, id, "Cyberpunk 2077"));
        let expected = "https://www.pcgamingwiki.com/w/index.php?search=Cyberpunk%202077";
        assert_eq!(search(Store::Epic, "epic:Ginger"), expected);
        assert_eq!(search(Store::Ea, "ea:198402"), expected);
        assert_eq!(search(Store::Xbox, "xbox:Family_8wekyb3d8bbwe"), expected);
        assert_eq!(search(Store::Custom, "custom:1792317898684"), expected);
        // An id that is not a store number is searched too.
        assert_eq!(search(Store::Steam, "steam:"), expected);
        assert_eq!(search(Store::Gog, "gog:abc"), expected);
    }

    #[test]
    fn search_names_drop_trademark_signs_and_are_encoded() {
        assert_eq!(
            search_name("  STAR WARS™: Squadrons "),
            "STAR WARS: Squadrons"
        );
        assert_eq!(search_name("Far Cry® 3"), "Far Cry 3");
        assert_eq!(search_name("Burnout™  Paradise ©"), "Burnout Paradise");
        assert_eq!(
            page_url(&game(Store::Epic, "epic:x", "Tomb Raider I–III & Lara's ™")),
            "https://www.pcgamingwiki.com/w/index.php?search=Tomb%20Raider%20I%E2%80%93III%20%26%20Lara%27s"
        );
    }

    #[test]
    fn reads_the_api_template() {
        use GraphicsApi::*;
        // As the wiki writes them (Cyberpunk 2077, GTA V, Tomb Raider: Legend).
        let cyberpunk = "{{API\n|direct3d versions      = 12\n|direct3d notes         = \n}}";
        assert_eq!(parse_api_table(cyberpunk), [DirectX12]);
        let gta = "==Other information==\n===API===\n{{API\n|direct3d versions      = 11\n\
                   |directdraw versions    =\n|opengl versions        =\n|vulkan versions        =\n\
                   |windows 64-bit exe     = true\n}}\n{{Middleware\n|opengl versions = 4.5\n}}";
        assert_eq!(parse_api_table(gta), [DirectX11]);
        let legend = "{{API\n|direct3d versions      = 9.0c\n|opengl versions        = \n}}";
        assert_eq!(parse_api_table(legend), [DirectX9]);

        let many = "{{API\r\n|direct3d versions = 9Ex, 10.1, 11, 12{{cn}}\r\n\
                    |opengl versions = true\r\n|vulkan versions = 1.1\r\n}}";
        assert_eq!(
            parse_api_table(many),
            [DirectX9, DirectX10, DirectX11, DirectX12, OpenGl, Vulkan]
        );
        let unknown = "{{API\n|direct3d versions = true, 7\n|opengl versions = false\n\
                       |vulkan versions = FALSE\n}}";
        assert!(parse_api_table(unknown).is_empty());
        // Other templates starting with "API" are not the table.
        assert!(parse_api_table("{{API/switch Direct3D|11}}").is_empty());
        assert!(parse_api_table("no template at all").is_empty());
    }

    #[test]
    fn reads_lookup_answers() {
        let found: Value =
            serde_json::from_str(r#"{"idlookup":[{"title":{"Page":"The Witcher 3: Wild Hunt"}}]}"#)
                .unwrap();
        assert_eq!(
            idlookup_page(&found).as_deref(),
            Some("The Witcher 3: Wild Hunt")
        );
        let unknown: Value = serde_json::from_str(r#"{"idlookup":[]}"#).unwrap();
        assert_eq!(idlookup_page(&unknown), None);

        let search: Value = serde_json::from_str(
            r#"{"batchcomplete":true,"query":{"search":[{"ns":0,"title":"GhostWire: Tokyo"},
                {"ns":0,"title":"Ghostwire: Tokyo - Prelude"}]}}"#,
        )
        .unwrap();
        assert_eq!(
            search_titles(&search),
            ["GhostWire: Tokyo", "Ghostwire: Tokyo - Prelude"]
        );
        let nothing: Value = serde_json::from_str(r#"{"query":{"search":[]}}"#).unwrap();
        assert!(search_titles(&nothing).is_empty());

        let page: Value =
            serde_json::from_str(r#"{"parse":{"title":"X","wikitext":"{{API}}"}}"#).unwrap();
        assert_eq!(wikitext(&page).as_deref(), Some("{{API}}"));
        let missing: Value = serde_json::from_str(r#"{"error":{"code":"missingtitle"}}"#).unwrap();
        assert_eq!(wikitext(&missing), None);
    }

    #[test]
    fn titles_found_by_name_must_be_the_game() {
        assert!(same_game("Black Myth: Wukong", "Black Myth Wukong"));
        assert!(same_game("GhostWire: Tokyo", "Ghostwire Tokyo"));
        assert!(same_game(
            "Tomb Raider I–III Remastered",
            "Tomb Raider I-III Remastered Starring Lara Croft"
        ));
        assert!(!same_game("VR Karts SteamVR", "SteamVR"));
        assert!(!same_game("Alan Wake II", "Alan Wake 2"));
        assert!(!same_game("", "Anything"));
        assert!(same_game("Beyond Good & Evil", "Beyond Good and Evil"));
        // The longest title that is the game wins.
        let titles = [
            "Ghostbusters",
            "Ghostbusters: Spirits Unleashed",
            "Ghostbusters II",
        ]
        .map(str::to_owned);
        assert_eq!(
            best_title(&titles, "Ghostbusters: Spirits Unleashed Ecto Edition").as_deref(),
            Some("Ghostbusters: Spirits Unleashed")
        );
        assert_eq!(best_title(&titles, "SteamVR"), None);
    }

    #[test]
    fn names_are_searched_as_the_wiki_writes_them() {
        assert_eq!(
            wiki_name("Assassin’s Creed® Chronicles Russia"),
            "Assassin's Creed Chronicles Russia"
        );
        assert_eq!(search_names("Cyberpunk 2077"), ["Cyberpunk 2077"]);
        assert_eq!(
            search_names("Ghostbusters: Spirits Unleashed Ecto Edition"),
            [
                "Ghostbusters: Spirits Unleashed Ecto Edition",
                "Ghostbusters: Spirits Unleashed",
                "Ghostbusters"
            ]
        );
        assert_eq!(
            search_names("The Witcher 3: Wild Hunt - Game of the Year Edition"),
            [
                "The Witcher 3: Wild Hunt - Game of the Year Edition",
                "The Witcher 3: Wild Hunt - Game of the",
                "The Witcher 3: Wild Hunt"
            ]
        );
    }

    #[test]
    fn reads_the_game_data_templates() {
        // As The Witcher 3's page writes them, with a Steam path and a note added.
        let page = concat!(
            "==Game data==\n===Configuration file(s) location===\n{{Game data|\n",
            r"{{Game data/config|Windows|{{p|userprofile\Documents}}\The Witcher 3\*.settings}}",
            "\n}}\n===Save game data location===\n{{Game data|\n",
            r"{{Game data/saves|Windows|{{p|userprofile\Documents}}\The Witcher 3\gamesaves\*.png|",
            r"{{p|userprofile\Documents}}\The Witcher 3\gamesaves\*.sav}}",
            "\n",
            r"{{Game data/saves|Steam|{{p|steam}}\userdata\{{p|uid}}\292030\remote{{Note|Cloud.}}}}",
            "\n}}"
        );
        let (saves, config) = game_data(page);
        let path = |platform: &str, path: &str| WikiPath {
            platform: platform.into(),
            path: path.into(),
        };
        assert_eq!(
            config,
            [path(
                "Windows",
                r"{{p|userprofile\Documents}}\The Witcher 3\*.settings"
            )]
        );
        assert_eq!(
            saves,
            [
                path(
                    "Windows",
                    r"{{p|userprofile\Documents}}\The Witcher 3\gamesaves\*.png"
                ),
                path(
                    "Windows",
                    r"{{p|userprofile\Documents}}\The Witcher 3\gamesaves\*.sav"
                ),
                path(
                    "Steam",
                    r"{{p|steam}}\userdata\{{p|uid}}\292030\remote{{Note|Cloud.}}"
                ),
            ]
        );
        // An empty path, no template, a template never closed.
        let none = (Vec::new(), Vec::new());
        assert_eq!(game_data("{{Game data/saves|Windows|}}"), none);
        assert_eq!(game_data("no data"), none);
        assert_eq!(
            game_data(r"{{Game data/saves|Windows|{{p|game}}\save"),
            (vec![path("Windows", r"{{p|game}}\save")], Vec::new())
        );
    }
}
