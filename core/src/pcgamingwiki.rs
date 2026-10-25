//! Links to a game's page on PCGamingWiki (fixes, settings, save locations), opened in the
//! browser. No requests are made here: Steam and GOG games use the wiki's id redirects, which go
//! straight to the article; other games use the wiki search, which opens the article when the
//! name is its title and the results otherwise.

use crate::text::percent_encode;
use crate::{Game, Store};

const SITE: &str = "https://www.pcgamingwiki.com";

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
}
