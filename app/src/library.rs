//! Library logic behind the sidebar: search, filter and selection. No UI code.

use gm_core::{Game, Store};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Which games the list shows, before the search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LibraryFilter {
    #[default]
    All,
    Favorites,
    Store(Store),
}

impl LibraryFilter {
    pub fn matches(self, game: &Game) -> bool {
        match self {
            LibraryFilter::All => true,
            LibraryFilter::Favorites => game.favorite,
            LibraryFilter::Store(store) => game.store == store,
        }
    }
}

/// Indices of the games matching the search text and filter, in list order.
pub fn filter(games: &[Game], query: &str, filter: LibraryFilter) -> Vec<usize> {
    let query = query.trim().to_lowercase();
    games
        .iter()
        .enumerate()
        .filter(|(_, game)| filter.matches(game))
        .filter(|(_, game)| query.is_empty() || game.name.to_lowercase().contains(&query))
        .map(|(index, _)| index)
        .collect()
}

/// Filter entries with game counts: all games, the favorites (listed even when there are none,
/// so the filter can be found), then each store that has games.
pub fn filter_options(games: &[Game]) -> Vec<(LibraryFilter, String)> {
    let count = |filter: LibraryFilter| games.iter().filter(|game| filter.matches(game)).count();
    let mut options = vec![
        (LibraryFilter::All, format!("All games ({})", games.len())),
        (
            LibraryFilter::Favorites,
            format!("Favorites ({})", count(LibraryFilter::Favorites)),
        ),
    ];
    for store in Store::ALL {
        let filter = LibraryFilter::Store(store);
        let count = count(filter);
        if count > 0 {
            options.push((filter, format!("{} ({count})", store.name())));
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
            favorite: false,
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
        let steam = LibraryFilter::Store(Store::Steam);
        assert_eq!(filter(&games, "", LibraryFilter::All), [0, 1, 2, 3]);
        assert_eq!(filter(&games, "  tomb RAIDER ", LibraryFilter::All), [2, 3]);
        assert_eq!(filter(&games, "theme hospital", LibraryFilter::All), [1]);
        assert_eq!(filter(&games, "", steam), [0, 3]);
        assert_eq!(filter(&games, "tomb", steam), [3]);
        assert!(filter(&games, "tomb", LibraryFilter::Store(Store::Xbox)).is_empty());
    }

    #[test]
    fn filters_favorites() {
        let mut games = library();
        assert!(filter(&games, "", LibraryFilter::Favorites).is_empty());
        games[1].favorite = true;
        games[3].favorite = true;
        assert_eq!(filter(&games, "", LibraryFilter::Favorites), [1, 3]);
        assert_eq!(filter(&games, "tomb", LibraryFilter::Favorites), [3]);
    }

    #[test]
    fn filter_options_list_favorites_and_only_stores_with_games() {
        let mut games = library();
        games[0].favorite = true;
        assert_eq!(
            filter_options(&games),
            [
                (LibraryFilter::All, "All games (4)".to_owned()),
                (LibraryFilter::Favorites, "Favorites (1)".to_owned()),
                (LibraryFilter::Store(Store::Steam), "Steam (2)".to_owned()),
                (LibraryFilter::Store(Store::Gog), "GOG (1)".to_owned()),
                (LibraryFilter::Store(Store::Ea), "EA (1)".to_owned()),
            ]
        );
        assert_eq!(
            filter_options(&[]),
            [
                (LibraryFilter::All, "All games (0)".to_owned()),
                (LibraryFilter::Favorites, "Favorites (0)".to_owned()),
            ]
        );
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
