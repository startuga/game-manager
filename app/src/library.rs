//! Library logic behind the sidebar: search, store filter and selection. No UI code.

use gm_core::{Game, Store};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Indices of the games matching the search text and store filter, in list order.
pub fn filter(games: &[Game], query: &str, store: Option<Store>) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    games
        .iter()
        .enumerate()
        .filter(|(_, game)| store.is_none_or(|store| game.store == store))
        .filter(|(_, game)| query.is_empty() || game.name.to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

/// Store filter entries: "All stores", then each store that has games, with game counts.
pub fn store_options(games: &[Game]) -> Vec<(Option<Store>, String)> {
    let mut options = vec![(None, format!("All stores ({})", games.len()))];
    for store in Store::ALL {
        let count = games.iter().filter(|game| game.store == store).count();
        if count > 0 {
            options.push((Some(store), format!("{} ({count})", store.name())));
        }
    }
    options
}

/// A key for the list showing exactly these game ids, in this order. The sidebar builds a new list
/// whenever it changes rather than changing a live list's rows (see `sidebar` in `main.rs`).
pub fn list_key(ids: &[String]) -> String {
    let mut hasher = DefaultHasher::new();
    ids.hash(&mut hasher);
    format!("games-{:016x}", hasher.finish())
}

/// The selection after a rescan: the same game if it is still installed, else the first game.
pub fn keep_selection(games: &[Game], selected: Option<&str>) -> Option<String> {
    selected
        .filter(|id| games.iter().any(|game| game.id == *id))
        .map(str::to_owned)
        .or_else(|| games.first().map(|game| game.id.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gm_core::Launch;

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
        }
    }

    fn library() -> Vec<Game> {
        vec![
            game("steam:244210", "Assetto Corsa", Store::Steam),
            game("ea:1025169", "Theme Hospital™", Store::Ea),
            game("gog:1407750955", "Tomb Raider I-III Remastered", Store::Gog),
            game("steam:8000", "Tomb Raider: Anniversary", Store::Steam),
        ]
    }

    #[test]
    fn filters_by_name_and_store() {
        let games = library();
        assert_eq!(filter(&games, "", None), [0, 1, 2, 3]);
        assert_eq!(filter(&games, "  tomb RAIDER ", None), [2, 3]);
        assert_eq!(filter(&games, "theme hospital", None), [1]);
        assert_eq!(filter(&games, "", Some(Store::Steam)), [0, 3]);
        assert_eq!(filter(&games, "tomb", Some(Store::Steam)), [3]);
        assert!(filter(&games, "tomb", Some(Store::Xbox)).is_empty());
    }

    #[test]
    fn store_options_list_only_stores_with_games() {
        assert_eq!(
            store_options(&library()),
            [
                (None, "All stores (4)".to_owned()),
                (Some(Store::Steam), "Steam (2)".to_owned()),
                (Some(Store::Gog), "GOG (1)".to_owned()),
                (Some(Store::Ea), "EA (1)".to_owned()),
            ]
        );
        assert_eq!(store_options(&[]), [(None, "All stores (0)".to_owned())]);
    }

    #[test]
    fn selection_survives_rescan_or_falls_back_to_first_game() {
        let games = library();
        assert_eq!(
            keep_selection(&games, Some("gog:1407750955")).as_deref(),
            Some("gog:1407750955")
        );
        assert_eq!(
            keep_selection(&games, Some("uninstalled:1")).as_deref(),
            Some("steam:244210")
        );
        assert_eq!(
            keep_selection(&games, None).as_deref(),
            Some("steam:244210")
        );
        assert_eq!(keep_selection(&[], Some("steam:244210")), None);
    }

    #[test]
    fn list_key_changes_with_the_visible_games() {
        let ids = |list: &[&str]| list.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        let all = ids(&["steam:1", "gog:2", "ea:3"]);
        // Same games: the same list keeps its rows, e.g. when icons arrive or a rescan finds the
        // same library.
        assert_eq!(list_key(&all), list_key(&all.clone()));
        // A search, a store filter, an added or removed game, or another order: a new list.
        assert_ne!(list_key(&all), list_key(&ids(&["steam:1", "ea:3"])));
        assert_ne!(
            list_key(&all),
            list_key(&ids(&["gog:2", "steam:1", "ea:3"]))
        );
        assert_ne!(list_key(&all), list_key(&[]));
    }
}
