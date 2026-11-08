//! Library logic behind the sidebar and the Library page: search, filter, selection, and how
//! many tiles a row of the grid holds. No UI code.
//!
//! **Search**: names and queries are compared `normalize`d: lower case, Latin accents taken off,
//! apostrophes dropped, ™ ® © and other punctuation as spaces. A game matches (`matches`) when
//! every word of the query is in its name (in any order), when the query is in its name with the
//! spaces taken out ("halflife"), when a one-word query starts its name's initials ("gta",
//! "tw3"; a word of digits counts whole), or when a word is in its program's file name ("ffxv").

use gm_core::{Game, Store};

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
    let query = normalize(query);
    games
        .iter()
        .enumerate()
        .filter(|(_, game)| filter.matches(game))
        .filter(|(_, game)| matches(game, &query))
        .map(|(index, _)| index)
        .collect()
}

/// The first game shown for the search and filter, in the sidebar's order: by store
/// (`Store::ALL`), then the list's order.
pub fn first_shown(games: &[Game], query: &str, filter: LibraryFilter) -> Option<usize> {
    self::filter(games, query, filter)
        .into_iter()
        .min_by_key(|&index| {
            let store = Store::ALL
                .iter()
                .position(|store| *store == games[index].store)
                .unwrap_or(usize::MAX);
            (store, index)
        })
}

/// Text as the search compares it: lower case, Latin accents taken off, apostrophes dropped,
/// ™ ® © and other punctuation as spaces, runs of spaces as one, trimmed.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars().flat_map(char::to_lowercase) {
        if matches!(c, '\'' | '’' | '‘' | '`' | '´') {
            continue;
        }
        if let Some(plain) = unaccented(c) {
            out.push_str(plain);
        } else if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with(' ') {
            out.push(' ');
        }
    }
    out.trim().to_owned()
}

/// A lower-case Latin letter with an accent, without it.
fn unaccented(c: char) -> Option<&'static str> {
    Some(match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'ç' | 'ć' | 'č' => "c",
        'ď' | 'đ' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => "i",
        'ł' => "l",
        'ñ' | 'ń' | 'ň' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ő' => "o",
        'ř' => "r",
        'ś' | 'š' => "s",
        'ť' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ů' | 'ű' => "u",
        'ý' | 'ÿ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'æ' => "ae",
        'œ' => "oe",
        'ß' => "ss",
        _ => return None,
    })
}

/// The first letter of each word, and a word of digits whole ("red dead redemption 2" →
/// "rdr2").
fn initials(normalized: &str) -> String {
    normalized
        .split(' ')
        .filter(|word| !word.is_empty())
        .map(|word| {
            if word.chars().all(|c| c.is_ascii_digit()) {
                word.to_owned()
            } else {
                word.chars().next().map(String::from).unwrap_or_default()
            }
        })
        .collect()
}

/// Whether `game` matches an already `normalize`d query (an empty one matches every game).
pub fn matches(game: &Game, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let name = normalize(&game.name);
    let program = game
        .program
        .as_deref()
        .and_then(|program| program.file_stem())
        .map(|stem| normalize(&stem.to_string_lossy()))
        .unwrap_or_default();
    let words: Vec<&str> = query.split(' ').collect();
    // Every word somewhere in the name, in any order, or in the program's file name.
    if words
        .iter()
        .all(|word| name.contains(word) || program.contains(word))
    {
        return true;
    }
    // "halflife" for "Half-Life", "farcry" for "Far Cry".
    let joined: String = query.split(' ').collect();
    if name.replace(' ', "").contains(&joined) {
        return true;
    }
    // One word starting the name's initials: "gta", "tw3".
    words.len() == 1 && query.chars().count() >= 2 && initials(&name).starts_with(query)
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

/// How many tiles `tile` wide, `gap` apart, fit in `width`: at least one.
pub fn columns(width: f32, tile: f32, gap: f32) -> usize {
    (((width + gap) / (tile + gap)).floor() as usize).max(1)
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
    fn normalizes_accents_marks_and_punctuation() {
        assert_eq!(normalize("Far Cry® 3"), "far cry 3");
        assert_eq!(
            normalize("Burnout™ Paradise Remastered"),
            "burnout paradise remastered"
        );
        assert_eq!(normalize("Pokémon: Ça Va – Ñandú"), "pokemon ca va nandu");
        assert_eq!(normalize("Assassin's Creed"), "assassins creed");
        assert_eq!(normalize("Half-Life: Alyx"), "half life alyx");
        assert_eq!(normalize("  Straße  Œuvre  "), "strasse oeuvre");
        assert_eq!(normalize("   "), "");
    }

    #[test]
    fn matches_words_spaces_initials_and_programs() {
        let mut games = vec![
            game("steam:1", "Far Cry® 3", Store::Steam),
            game("steam:2", "Half-Life 2", Store::Steam),
            game("epic:1", "Grand Theft Auto V", Store::Epic),
            game(
                "gog:1",
                "The Witcher 3: Wild Hunt - Game of the Year Edition",
                Store::Gog,
            ),
            game("custom:1", "Final Fantasy XV", Store::Custom),
        ];
        games[4].program = Some(r"D:\Games\FFXV\ffxv_s.exe".into());
        let found = |query: &str| -> Vec<usize> { filter(&games, query, LibraryFilter::All) };
        assert_eq!(found("cry far 3"), [0]);
        assert_eq!(found("FAR CRY 3"), [0]);
        assert_eq!(found("farcry"), [0]);
        assert_eq!(found("halflife"), [1]);
        assert_eq!(found("half life 2"), [1]);
        assert_eq!(found("gta"), [2]);
        assert_eq!(found("gtav"), [2]);
        assert_eq!(found("tw3"), [3]);
        assert_eq!(found("witcher wild"), [3]);
        assert_eq!(found("ffxv"), [4]);
        assert_eq!(found("cyberpunk"), Vec::<usize>::new());
        // A one-letter query is a substring, never initials.
        assert_eq!(found("x"), [4]);
        // Every game for nothing typed.
        assert_eq!(found("  "), [0, 1, 2, 3, 4]);
    }

    #[test]
    fn enter_opens_the_first_game_in_the_sidebars_order() {
        let games = vec![
            game("custom:1", "Tomb Raider Custom", Store::Custom),
            game("gog:1", "Tomb Raider I-III", Store::Gog),
            game("steam:1", "Tomb Raider: Legend", Store::Steam),
            game("steam:2", "Tomb Raider: Underworld", Store::Steam),
        ];
        // Steam comes first in the sidebar, then its games in list order.
        assert_eq!(first_shown(&games, "tomb", LibraryFilter::All), Some(2));
        assert_eq!(
            first_shown(&games, "tomb", LibraryFilter::Store(Store::Gog)),
            Some(1)
        );
        assert_eq!(first_shown(&games, "nothing", LibraryFilter::All), None);
    }

    #[test]
    fn the_grid_holds_as_many_tiles_as_fit() {
        // 140-wide tiles 12 apart: 4 need 596, 5 need 748.
        assert_eq!(columns(596.0, 140.0, 12.0), 4);
        assert_eq!(columns(747.0, 140.0, 12.0), 4);
        assert_eq!(columns(748.0, 140.0, 12.0), 5);
        // Never none, however narrow.
        assert_eq!(columns(80.0, 140.0, 12.0), 1);
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
}
