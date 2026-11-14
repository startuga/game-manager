//! Nexus Mods (mods for a game): the address of a game's page there, opened in the browser.
//!
//! - **Pages** are `https://www.nexusmods.com/games/<domain>`, keyed by Nexus's "game domain"
//!   (`cyberpunk2077`, `witcher3`; the older `/<domain>` redirects there). Nexus keeps no store
//!   ids, so a game is found by name, unless `games.json` names its domain (`nexusMods`, matched by
//!   the game's binaries like its program), which wins: GTA V is "Grand Theft Auto V Legacy" on
//!   Nexus, and its Enhanced edition a game of its own.
//! - **Nexus's list of games** is the public `games.json` on its data server: every game's name and
//!   domain (5 836 games, 1.8 MB, on 2026-11-14), with no API key. The v1 API's list needs a
//!   personal key, and the v2 GraphQL API takes POSTs of about 2 500 games each. The file is kept
//!   byte for byte in `%LOCALAPPDATA%\Game Manager\cache\nexus-games.json` and downloaded again
//!   when it is a week old; the older copy stays in use while a download fails. It is read once
//!   per process, at the first lookup.
//! - **Names** are the same game when their words are, ignoring case, accents and punctuation, with
//!   Roman numerals as numbers: "Alan Wake II" is Nexus's "Alan Wake 2", and "DRAGON QUEST I & II
//!   HD-2D Remake" stays apart from "Dragon Quest III HD-2D Remake". Of games with one name, the
//!   one with the most mods counts. Otherwise a name that adds an edition ("The Witcher 3: Wild
//!   Hunt - Game of the Year Edition") is the longest listed name its words start with, when the
//!   words left name an edition (`edition`, `goty`, `game of the year`). A shorter name is never
//!   taken without one, so a sequel or another game of the series Nexus lacks ("Tomb Raider:
//!   Legend") never opens the first game's page.
//! - **No match** (a VR tool, a name Nexus writes otherwise, no list offline): Nexus's games search
//!   for the name, `/games?keyword=<name>`, which lists the games whose names hold every word.
//!
//! On a library of 38 (2026-11-14): 25 found by name, 3 by the name without its edition, 2 by
//! `games.json` (GTA V, and GOG's "Tomb Raider I-III Remastered Starring Lara Croft"); the 8 left
//! are VR tools and utilities Nexus doesn't list.

use crate::text::{letters, percent_encode, search_name};
use crate::{Game, config, http, json_file};
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

const SITE: &str = "https://www.nexusmods.com";
const LIST_URL: &str = "https://data.nexusmods.com/file/nexus-data/games.json";
const LIST_FILE: &str = "nexus-games.json";
/// How long a downloaded list is used before it is downloaded again.
const LIST_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// The list read in this process, and when.
static LIST: Mutex<Option<(Instant, Arc<NexusGames>)>> = Mutex::new(None);

/// The address of the game's Nexus Mods page, or of Nexus's search for its name when neither
/// `games.json` nor Nexus's list knows it. Blocking: reads `games.json` and the game's folder, and
/// may download Nexus's list.
pub fn page_url(game: &Game) -> String {
    let known = config::load_defaults()
        .ok()
        .and_then(|defaults| defaults.nexus_mods(game).map(str::to_owned));
    let games = if known.is_none() { games() } else { None };
    url_for(game, known.as_deref(), games.as_deref())
}

/// The page of `known` (from `games.json`), else of the game of that name in `games`, else the
/// search for the name.
fn url_for(game: &Game, known: Option<&str>, games: Option<&NexusGames>) -> String {
    match known.or_else(|| games.and_then(|games| games.domain(&game.name))) {
        Some(domain) => format!("{SITE}/games/{}", percent_encode(domain)),
        None => format!(
            "{SITE}/games?keyword={}",
            percent_encode(&search_name(&game.name))
        ),
    }
}

/// Nexus's list for this process: read or downloaded at the first call, again a week later.
/// `None` when there is none (offline at the first start, or a list that can't be read).
fn games() -> Option<Arc<NexusGames>> {
    let mut held = LIST.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, list)) = held.as_ref()
        && at.elapsed() < LIST_AGE
    {
        return Some(Arc::clone(list));
    }
    match load_list() {
        Some(list) => {
            let list = Arc::new(list);
            *held = Some((Instant::now(), Arc::clone(&list)));
            Some(list)
        }
        // A list read earlier stays in use.
        None => held.as_ref().map(|(_, list)| Arc::clone(list)),
    }
}

/// The cached list, downloaded again when missing or a week old; the older copy when that fails.
fn load_list() -> Option<NexusGames> {
    let file = json_file::local_file("cache").ok()?.join(LIST_FILE);
    let age = fs::metadata(&file)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok());
    if age.is_none_or(|age| age >= LIST_AGE)
        && let Ok(bytes) = http::get(LIST_URL)
        && let Some(games) = NexusGames::parse(&bytes)
    {
        // A list that can't be cached is downloaded again next time.
        if let Some(dir) = file.parent()
            && fs::create_dir_all(dir).is_ok()
        {
            let _ = crate::whole_file::write(&file, &bytes);
        }
        return Some(games);
    }
    NexusGames::parse(&fs::read(&file).ok()?)
}

/// Nexus's games by the words of their names (`name_key`).
#[derive(Debug, Default)]
struct NexusGames(HashMap<String, Listed>);

#[derive(Debug)]
struct Listed {
    domain: String,
    mods: u64,
}

/// An entry of Nexus's list (it has more fields: forum and site addresses, genre, counts).
#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    domain_name: Option<String>,
    #[serde(default)]
    mods: Option<u64>,
}

impl NexusGames {
    /// Nexus's `games.json`, an array of games; `None` when it is not one.
    fn parse(bytes: &[u8]) -> Option<Self> {
        let entries: Vec<Entry> = serde_json::from_slice(bytes).ok()?;
        let mut games = HashMap::with_capacity(entries.len());
        for entry in entries {
            let (Some(name), Some(domain)) = (entry.name, entry.domain_name) else {
                continue;
            };
            let key = name_key(&name_words(&name));
            if key.is_empty() || domain.is_empty() {
                continue;
            }
            let mods = entry.mods.unwrap_or_default();
            if games
                .get(&key)
                .is_none_or(|listed: &Listed| mods > listed.mods)
            {
                games.insert(key, Listed { domain, mods });
            }
        }
        (!games.is_empty()).then_some(Self(games))
    }

    /// The domain of the game of this name, or of the name without an edition after it (see the
    /// module's doc).
    fn domain(&self, name: &str) -> Option<&str> {
        let words = name_words(name);
        let found = self.0.get(&name_key(&words)).or_else(|| {
            (1..words.len())
                .rev()
                .filter(|&end| words.get(end..).is_some_and(names_edition))
                .find_map(|end| self.0.get(&name_key(words.get(..end)?)))
        });
        found.map(|listed| listed.domain.as_str())
    }
}

/// A name's words, lower case and without accents, Roman numerals as numbers ("Tomb Raider
/// I–III" is `tomb`, `raider`, `1`, `3`).
fn name_words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .map(letters)
        .filter(|word| !word.is_empty())
        .map(|word| roman(&word).map_or(word, |number| number.to_string()))
        .collect()
}

fn name_key(words: &[String]) -> String {
    words.concat()
}

/// Whether the words after a game's name name an edition of it.
fn names_edition(rest: &[String]) -> bool {
    rest.iter().any(|word| word == "edition" || word == "goty")
        || rest
            .windows(4)
            .any(|words| words == ["game", "of", "the", "year"])
}

/// A lower-case Roman numeral from 1 to 39 (I to XXXIX), written the usual way: `iv`, not `iiii`.
fn roman(word: &str) -> Option<u32> {
    const ONES: [&str; 10] = ["", "i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix"];
    let tens = word.bytes().take_while(|&b| b == b'x').count().min(3);
    let ones = word.get(tens..)?;
    let one = ONES.iter().position(|&numeral| numeral == ones)?;
    let number = u32::try_from(tens * 10 + one).ok()?;
    (number > 0).then_some(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Launch, Store};

    fn game(name: &str) -> Game {
        Game {
            id: "epic:x".into(),
            name: name.into(),
            store: Store::Epic,
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

    /// As Nexus lists them, with fewer fields.
    fn list() -> NexusGames {
        NexusGames::parse(
            br#"[
            {"id":3333,"name":"Cyberpunk 2077","name_lower":"cyberpunk 2077","domain_name":"cyberpunk2077","mods":26742},
            {"id":952,"name":"The Witcher 3: Wild Hunt","domain_name":"witcher3","mods":10887},
            {"id":150,"name":"The Witcher","domain_name":"witcher","mods":519},
            {"id":5054,"name":"Alan Wake","domain_name":"alanwake","mods":16},
            {"id":5835,"name":"Alan Wake 2","domain_name":"alanwake2","mods":69},
            {"id":1,"name":"Tomb Raider","domain_name":"tombraider","mods":300},
            {"id":2,"name":"Tomb Raider IV-VI Remastered","domain_name":"tombraidertrilogy2remastered","mods":348},
            {"id":3,"name":"Hitman World of Assassination","domain_name":"hitman3","mods":990},
            {"id":4,"name":"Ghostbusters: Spirits Unleashed","domain_name":"ghostbustersspiritsunleashed","mods":1},
            {"id":5,"name":"Dragon Quest III HD-2D Remake","domain_name":"dragonquest3remake","mods":99},
            {"id":6,"name":"DRAGON QUEST I & II HD-2D Remake","domain_name":"dragonquest1and2hd2dremake","mods":43},
            {"id":7,"name":"Same Name","domain_name":"samenamefew","mods":2},
            {"id":8,"name":"Same name","domain_name":"samenamemany","mods":50},
            {"id":9,"name":"Same-Name","domain_name":"samenamenull","mods":null},
            {"id":10,"name":"Baldi's Basics Classic Remastered","domain_name":"baldi'sbasicsclassicremastered","mods":3},
            {"id":11,"name":null,"domain_name":"nameless"},
            {"id":12,"name":"No domain"}
        ]"#,
        )
        .unwrap()
    }

    fn domain(name: &str) -> Option<&'static str> {
        static LIST: std::sync::OnceLock<NexusGames> = std::sync::OnceLock::new();
        LIST.get_or_init(list).domain(name)
    }

    #[test]
    fn same_names_ignore_case_signs_and_numeral_styles() {
        assert_eq!(domain("Cyberpunk 2077"), Some("cyberpunk2077"));
        assert_eq!(
            domain("HITMAN World of Assassination"),
            Some("hitman3"),
            "Nexus's name for Hitman 3"
        );
        assert_eq!(domain("Alan Wake II"), Some("alanwake2"));
        assert_eq!(domain("Alan Wake™"), Some("alanwake"));
        assert_eq!(
            domain("Tomb Raider IV–VI Remastered"),
            Some("tombraidertrilogy2remastered")
        );
        // Letters alone would make these one name.
        assert_eq!(
            domain("Dragon Quest III HD-2D Remake"),
            Some("dragonquest3remake")
        );
        assert_eq!(
            domain("DRAGON QUEST I & II HD-2D Remake"),
            Some("dragonquest1and2hd2dremake")
        );
        // Of games with one name, the one with the most mods.
        assert_eq!(domain("Same name"), Some("samenamemany"));
    }

    #[test]
    fn editions_are_the_game_they_are_of() {
        assert_eq!(
            domain("The Witcher 3: Wild Hunt - Game of the Year Edition"),
            Some("witcher3")
        );
        assert_eq!(domain("The Witcher 3: Wild Hunt GOTY"), Some("witcher3"));
        assert_eq!(
            domain("Ghostbusters: Spirits Unleashed Ecto Edition"),
            Some("ghostbustersspiritsunleashed")
        );
        assert_eq!(
            domain("Cyberpunk 2077: Ultimate Edition"),
            Some("cyberpunk2077")
        );
    }

    #[test]
    fn shorter_names_are_not_other_games() {
        // Nexus has Tomb Raider (2013) and Alan Wake, not these.
        assert_eq!(domain("Tomb Raider: Legend"), None);
        assert_eq!(domain("Alan Wake III"), None);
        assert_eq!(domain("The Witcher 3: Wild Hunt - Blood and Wine"), None);
        assert_eq!(domain("SteamVR"), None);
        assert_eq!(domain(""), None);
        assert_eq!(domain("Edition"), None);
    }

    #[test]
    fn reads_only_entries_with_a_name_and_a_domain() {
        let games = list();
        assert!(!games.0.values().any(|listed| listed.domain == "nameless"));
        assert_eq!(games.domain("No domain"), None);
        assert!(NexusGames::parse(b"[]").is_none());
        assert!(NexusGames::parse(b"{\"games\":[]}").is_none());
        assert!(NexusGames::parse(b"<html>").is_none());
    }

    #[test]
    fn roman_numerals_up_to_39() {
        assert_eq!(roman("i"), Some(1));
        assert_eq!(roman("iv"), Some(4));
        assert_eq!(roman("ix"), Some(9));
        assert_eq!(roman("xiv"), Some(14));
        assert_eq!(roman("xxxix"), Some(39));
        for word in ["", "iiii", "vv", "xl", "mix", "ivx", "xxxx", "civ", "2"] {
            assert_eq!(roman(word), None, "{word}");
        }
        assert_eq!(
            name_words("Tomb Raider I–III, Vol. ii"),
            ["tomb", "raider", "1", "3", "vol", "2"]
        );
        assert_eq!(name_words("Pokémon™ Légendes"), ["pokemon", "legendes"]);
    }

    #[test]
    fn links_the_page_or_the_search() {
        let games = list();
        assert_eq!(
            url_for(&game("Cyberpunk 2077"), None, Some(&games)),
            "https://www.nexusmods.com/games/cyberpunk2077"
        );
        // games.json wins over the name.
        assert_eq!(
            url_for(&game("Cyberpunk 2077"), Some("gta5"), Some(&games)),
            "https://www.nexusmods.com/games/gta5"
        );
        assert_eq!(
            url_for(&game("Grand Theft Auto V"), Some("gta5"), None),
            "https://www.nexusmods.com/games/gta5"
        );
        // Domains are not always letters and digits.
        assert_eq!(
            url_for(
                &game("Baldi's Basics Classic Remastered"),
                None,
                Some(&games)
            ),
            "https://www.nexusmods.com/games/baldi%27sbasicsclassicremastered"
        );
        // Not listed, or no list: the search.
        assert_eq!(
            url_for(&game("SteamVR"), None, Some(&games)),
            "https://www.nexusmods.com/games?keyword=SteamVR"
        );
        assert_eq!(
            url_for(&game("  STAR WARS™: Squadrons "), None, None),
            "https://www.nexusmods.com/games?keyword=STAR%20WARS%3A%20Squadrons"
        );
    }
}
