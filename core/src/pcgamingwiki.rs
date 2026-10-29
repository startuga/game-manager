//! PCGamingWiki (fixes, settings, save locations, technical data):
//!
//! - **Links** to a game's page, opened in the browser, need no request. Steam and GOG games use
//!   the wiki's id redirects, which go straight to the article. Other games use the wiki search,
//!   which opens the article when the name is its title and the results otherwise.
//! - **The API table** (`{{API}}`: Direct3D, OpenGL and Vulkan versions) is read through the
//!   wiki's API without an account. Requests carry a descriptive User-Agent and are spaced one
//!   second apart, as the wiki asks (at most 60 a minute).

use crate::graphics::{GraphicsApi, GraphicsInfo, GraphicsSource};
use crate::text::percent_encode;
use crate::{Error, Game, Result, Store, http};
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

const SITE: &str = "https://www.pcgamingwiki.com";
/// Time between two requests to the wiki's API.
const REQUEST_INTERVAL: Duration = Duration::from_secs(1);
/// When the last request was made, shared by every thread.
static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

/// The APIs in the game's API table on the wiki; `None` when the wiki has no page for the game, or
/// its page lists no API. A page found by name may be another game of the same name (Dead Space,
/// 2008, for the 2023 remake): the user can choose the right API in the game's settings.
/// Blocking: up to three web requests.
pub fn api_table(game: &Game) -> Result<Option<GraphicsInfo>> {
    let Some(page) = find_page(game)? else {
        return Ok(None);
    };
    let path = format!(
        "/w/api.php?action=parse&page={}&prop=wikitext&redirects=1&format=json&formatversion=2",
        percent_encode(&page)
    );
    let apis = wikitext(&request(&path)?)
        .map(|text| parse_api_table(&text))
        .unwrap_or_default();
    Ok((!apis.is_empty()).then_some(GraphicsInfo {
        apis,
        source: GraphicsSource::PcGamingWiki { page },
    }))
}

/// The title of the game's page: by store id for Steam and GOG, else (or when the wiki does not
/// know the id) by name.
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
    let name = search_name(&game.name);
    let path = format!(
        "/w/api.php?action=opensearch&search={}&limit=1&namespace=0&redirects=resolve&format=json",
        percent_encode(&name)
    );
    Ok(opensearch_title(&request(&path)?).filter(|title| same_game(title, &name)))
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

/// `[query, [titles], [descriptions], [urls]]`: the first title found.
fn opensearch_title(answer: &Value) -> Option<String> {
    answer[1][0].as_str().map(str::to_owned)
}

/// `{"parse":{"wikitext":"…"}}`; an error object when the page does not exist.
fn wikitext(answer: &Value) -> Option<String> {
    answer["parse"]["wikitext"].as_str().map(str::to_owned)
}

/// The wiki search is fuzzy ("SteamVR" finds "VR Karts SteamVR"), so a title found by name is used
/// only when, ignoring case, spaces and punctuation, it is the game's name or begins it ("Tomb
/// Raider I–III Remastered" for "Tomb Raider I-III Remastered Starring Lara Croft").
fn same_game(title: &str, name: &str) -> bool {
    let letters = |text: &str| -> String {
        text.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (title, name) = (letters(title), letters(name));
    !title.is_empty() && name.starts_with(&title)
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

/// The name as the wiki titles it: stores add ™ ® ©, which no title has.
fn search_name(name: &str) -> String {
    name.replace(['™', '®', '©'], "")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
            favorite: false,
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
            r#"["Alan Wake 2",["Alan Wake II"],[""],["https://www.pcgamingwiki.com/wiki/Alan_Wake_II"]]"#,
        )
        .unwrap();
        assert_eq!(opensearch_title(&search).as_deref(), Some("Alan Wake II"));
        let nothing: Value = serde_json::from_str(r#"["YUR",[],[],[]]"#).unwrap();
        assert_eq!(opensearch_title(&nothing), None);

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
    }
}
