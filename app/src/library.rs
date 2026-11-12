//! Library logic behind the sidebar and the Library page: search, filter, selection, and how
//! many tiles a row of the grid holds. No UI code.
//!
//! **Search**: names and queries are compared `normalize`d: lower case, Latin accents taken off,
//! apostrophes dropped, ™ ® © and other punctuation as spaces. A game matches (`matches`) when
//! every word of the query is in its name (in any order), when the query is in its name with the
//! spaces taken out ("halflife"), when a one-word query starts its name's initials ("gta",
//! "tw3"; a word of digits counts whole), or when a word is in its program's file name ("ffxv").
//!
//! **The view** (`LibraryView`, the sidebar's Sort and filters; saved by `gm_core::config`):
//! - **Smart filters**: the stores ticked (none ticked is every store) and Hidden games.
//!   **Hidden games** (the game menu's Hide from the list) show only with Hidden on, which is
//!   offered while there are some; otherwise the lists, the search and the counts leave them out.
//! - **Smart Collections** (`in_collection`): sets that fill themselves, one at most. Picking one
//!   clears the filters, and a filter turns the collection off (`pick_collection`,
//!   `toggle_store`, `set_hidden`).
//! - **Sort**: by name either way, or the most recently installed first (`Game::installed`; a
//!   game without a date comes last), with the games under their store or in one list.
//!
//! **Sections** (`sections`): what the sidebar and the Library page list, in order. **Pinned
//! games** (the game menu's Pin to quick access) come first, in a section of their own, in the
//! order they were pinned whatever the sort; their store's group no longer lists them.

use gm_core::mods::ModsFile;
use gm_core::{Game, Store};

pub use gm_core::config::{Collection, LibraryView, Sort};

/// How long a game counts as recently installed (the collection): 30 days.
const RECENT: u64 = 30 * 24 * 60 * 60;

/// What a view needs to know beyond the games: their mods, and the time.
#[derive(Clone, Copy)]
pub struct Facts<'a> {
    pub mods: &'a ModsFile,
    /// Now, in seconds since 1970.
    pub now: u64,
}

/// Now, in seconds since 1970, for `Facts`.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// Whether the view's filters and collection show the game, before the search.
pub fn in_view(game: &Game, view: &LibraryView, facts: Facts) -> bool {
    game.hidden == view.hidden
        && (view.stores.is_empty() || view.stores.contains(&game.store))
        && view
            .collection
            .is_none_or(|collection| in_collection(game, collection, facts))
}

/// Whether a game belongs to a Smart Collection.
pub fn in_collection(game: &Game, collection: Collection, facts: Facts) -> bool {
    let mods = facts.mods;
    let reshade = || {
        mods.reshade(&game.id)
            .is_some_and(|state| state.installed.is_some() || state.layer.is_some())
    };
    let dxvk = || {
        mods.dxvk(&game.id)
            .is_some_and(|state| state.installed.is_some())
    };
    match collection {
        Collection::Pinned => game.pinned.is_some(),
        Collection::Modded => {
            reshade()
                || dxvk()
                || mods.laa(&game.id).is_some()
                || !mods.upscalers(&game.id).is_empty()
                || mods.dlss_override(&game.id).is_some()
        }
        Collection::Reshade => reshade(),
        Collection::Dxvk => dxvk(),
        Collection::Bits32 => game.arch.is_some_and(|arch| arch.bits() == 32),
        Collection::RecentlyInstalled => game
            .installed
            .is_some_and(|at| facts.now.saturating_sub(at) <= RECENT),
    }
}

/// A collection's name in the flyout and on the Library page.
pub fn collection_label(collection: Collection) -> &'static str {
    match collection {
        Collection::Pinned => "Pinned",
        Collection::Modded => "Modded",
        Collection::Reshade => "With ReShade",
        Collection::Dxvk => "With DXVK",
        Collection::Bits32 => "32-bit",
        Collection::RecentlyInstalled => "Recently installed",
    }
}

/// A sort's name in the flyout.
pub fn sort_label(sort: Sort) -> &'static str {
    match sort {
        Sort::Name => "Name (A–Z)",
        Sort::NameDescending => "Name (Z–A)",
        Sort::RecentlyInstalled => "Recently installed",
    }
}

/// Every sort, in the flyout's order.
pub const SORTS: [Sort; 3] = [Sort::Name, Sort::NameDescending, Sort::RecentlyInstalled];

/// Whether a filter or a collection narrows the lists (the sort doesn't).
pub fn is_filtered(view: &LibraryView) -> bool {
    !view.stores.is_empty() || view.hidden || view.collection.is_some()
}

/// Clear all: no filters and no collection; the sort stays.
pub fn clear(view: &mut LibraryView) {
    view.stores.clear();
    view.hidden = false;
    view.collection = None;
}

/// Ticks or unticks a store. A filter turns the collection off.
pub fn toggle_store(view: &mut LibraryView, store: Store) {
    match view.stores.iter().position(|ticked| *ticked == store) {
        Some(at) => {
            view.stores.remove(at);
        }
        None => view.stores.push(store),
    }
    view.collection = None;
}

/// Shows the hidden games, or the others again. A filter turns the collection off.
pub fn set_hidden(view: &mut LibraryView, hidden: bool) {
    view.hidden = hidden;
    view.collection = None;
}

/// Picks a collection, which clears the filters; picking the one that is on turns it off.
pub fn pick_collection(view: &mut LibraryView, collection: Collection) {
    if view.collection == Some(collection) {
        view.collection = None;
    } else {
        clear(view);
        view.collection = Some(collection);
    }
}

/// What narrows the lists, for the Library page's caption ("Steam, GOG", "Hidden", "Modded");
/// `None` when nothing does.
pub fn describe(view: &LibraryView) -> Option<String> {
    let mut parts: Vec<&str> = Store::ALL
        .into_iter()
        .filter(|store| view.stores.contains(store))
        .map(Store::name)
        .collect();
    let mut text = parts.join(", ");
    parts.clear();
    for part in [
        view.hidden.then_some("Hidden"),
        view.collection.map(collection_label),
    ]
    .into_iter()
    .flatten()
    {
        if !text.is_empty() {
            text.push_str(" · ");
        }
        text.push_str(part);
    }
    (!text.is_empty()).then_some(text)
}

/// The hint where no game shows.
pub fn empty_hint(view: &LibraryView, query: &str) -> &'static str {
    if !query.trim().is_empty() {
        "No games match the search."
    } else if view.hidden && view.stores.is_empty() {
        "No hidden games."
    } else if is_filtered(view) {
        "No games match these filters."
    } else {
        "No games to show."
    }
}

/// A run of games in the lists, under one header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    /// Indices into the games, in the order shown.
    pub games: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionKind {
    /// Quick access: in the order they were pinned.
    Pinned,
    Store(Store),
    /// Every game that isn't pinned, with Group by store off.
    All,
}

/// The shown games (indices into `games`, as `shown` gives them) as the lists show them: the
/// pinned ones, then the others under their store or in one list, each in the view's order.
pub fn sections(games: &[Game], shown: &[usize], view: &LibraryView) -> Vec<Section> {
    let (pinned, rest) = split_pinned(games, shown);
    let mut sections = Vec::new();
    if !pinned.is_empty() {
        sections.push(Section {
            kind: SectionKind::Pinned,
            games: pinned,
        });
    }
    let mut add = |kind, mut section: Vec<usize>| {
        if !section.is_empty() {
            sort(games, &mut section, view.sort);
            sections.push(Section {
                kind,
                games: section,
            });
        }
    };
    if view.group_by_store {
        for store in Store::ALL {
            let of_store = rest
                .iter()
                .copied()
                .filter(|&index| games[index].store == store);
            add(SectionKind::Store(store), of_store.collect());
        }
    } else {
        add(SectionKind::All, rest);
    }
    sections
}

/// Orders games (indices in the library's own order, which is by name).
fn sort(games: &[Game], section: &mut [usize], order: Sort) {
    match order {
        Sort::Name => section.sort_unstable(),
        Sort::NameDescending => section.sort_unstable_by(|a, b| b.cmp(a)),
        // The newest first; a game without a date after those with one; then by name.
        Sort::RecentlyInstalled => {
            section
                .sort_unstable_by_key(|&index| (std::cmp::Reverse(games[index].installed), index));
        }
    }
}

/// Shown games (indices into `games`) as the pinned ones, in the order they were pinned, and the
/// others in their order.
pub fn split_pinned(games: &[Game], shown: &[usize]) -> (Vec<usize>, Vec<usize>) {
    let (mut pinned, rest): (Vec<usize>, Vec<usize>) = shown
        .iter()
        .partition(|&&index| games[index].pinned.is_some());
    pinned.sort_by_key(|&index| games[index].pinned);
    (pinned, rest)
}

/// Pins a game after the others, or unpins it and closes the gap, as `config::set_pinned` does
/// in the file. Pinning a pinned game, or unpinning one that isn't, changes nothing.
pub fn set_pin(games: &mut [Game], id: &str, pin: bool) {
    let next = games
        .iter()
        .filter_map(|game| game.pinned)
        .max()
        .map_or(0, |last| last + 1);
    let Some(game) = games.iter_mut().find(|game| game.id == id) else {
        return;
    };
    match (pin, game.pinned) {
        (true, None) => game.pinned = Some(next),
        (false, Some(place)) => {
            game.pinned = None;
            for game in games.iter_mut() {
                if let Some(later) = &mut game.pinned
                    && *later > place
                {
                    *later -= 1;
                }
            }
        }
        _ => {}
    }
}

/// Indices of the games the view and the search text show, in the library's order.
pub fn shown(games: &[Game], query: &str, view: &LibraryView, facts: Facts) -> Vec<usize> {
    let query = normalize(query);
    games
        .iter()
        .enumerate()
        .filter(|(_, game)| in_view(game, view, facts))
        .filter(|(_, game)| matches(game, &query))
        .map(|(index, _)| index)
        .collect()
}

/// The first game shown for the search and view, in the lists' order (`sections`).
pub fn first_shown(games: &[Game], query: &str, view: &LibraryView, facts: Facts) -> Option<usize> {
    let shown = shown(games, query, view, facts);
    let first = sections(games, &shown, view).into_iter().next()?;
    first.games.first().copied()
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

/// The stores to offer as filters, with how many games each has: those with games (hidden games
/// count only while Hidden is on), and any that is ticked.
pub fn store_counts(games: &[Game], view: &LibraryView) -> Vec<(Store, usize)> {
    Store::ALL
        .into_iter()
        .map(|store| {
            let count = games
                .iter()
                .filter(|game| game.store == store && game.hidden == view.hidden)
                .count();
            (store, count)
        })
        .filter(|(store, count)| *count > 0 || view.stores.contains(store))
        .collect()
}

/// How many games are hidden from the list.
pub fn hidden_count(games: &[Game]) -> usize {
    games.iter().filter(|game| game.hidden).count()
}

/// The collections to offer, with how many games each has (hidden games don't count): those
/// with games, and the one that is on.
pub fn collection_counts(
    games: &[Game],
    view: &LibraryView,
    facts: Facts,
) -> Vec<(Collection, usize)> {
    Collection::ALL
        .into_iter()
        .map(|collection| {
            let count = games
                .iter()
                .filter(|game| !game.hidden && in_collection(game, collection, facts))
                .count();
            (collection, count)
        })
        .filter(|(collection, count)| *count > 0 || view.collection == Some(*collection))
        .collect()
}

/// "1 game", "35 games".
pub fn games_label(count: usize) -> String {
    if count == 1 {
        "1 game".to_owned()
    } else {
        format!("{count} games")
    }
}

/// How many games the view's lists count against ("3 of 35 games"): the hidden ones with Hidden
/// on, else all the others.
pub fn total(games: &[Game], view: &LibraryView) -> usize {
    let hidden = hidden_count(games);
    if view.hidden {
        hidden
    } else {
        games.len() - hidden
    }
}

/// The view to keep when games are gone, hidden or changed: Hidden with no hidden game left, a
/// ticked store without games and a collection without games are dropped. The sort stays.
pub fn keep_view(games: &[Game], view: &LibraryView, facts: Facts) -> LibraryView {
    let mut kept = view.clone();
    if kept.hidden && hidden_count(games) == 0 {
        kept.hidden = false;
    }
    let listed = |store: Store| {
        games
            .iter()
            .any(|game| game.store == store && game.hidden == kept.hidden)
    };
    let stores: Vec<Store> = kept.stores.iter().copied().filter(|s| listed(*s)).collect();
    kept.stores = stores;
    if let Some(collection) = kept.collection
        && !games
            .iter()
            .any(|game| !game.hidden && in_collection(game, collection, facts))
    {
        kept.collection = None;
    }
    kept
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
            pinned: None,
            hidden: false,
            installed: None,
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

    const NOW: u64 = 1_800_000_000;
    const DAY: u64 = 24 * 60 * 60;

    fn facts(mods: &ModsFile) -> Facts<'_> {
        Facts { mods, now: NOW }
    }

    /// A view with these stores ticked.
    fn stores(stores: &[Store]) -> LibraryView {
        LibraryView {
            stores: stores.to_vec(),
            ..Default::default()
        }
    }

    /// The games a view and a search show, with no mods anywhere.
    fn found(games: &[Game], query: &str, view: &LibraryView) -> Vec<usize> {
        shown(games, query, view, facts(&ModsFile::default()))
    }

    #[test]
    fn filters_by_name_and_store() {
        let games = library();
        let all = LibraryView::default();
        let steam = stores(&[Store::Steam]);
        assert_eq!(found(&games, "", &all), [0, 1, 2, 3]);
        assert_eq!(found(&games, "  tomb RAIDER ", &all), [2, 3]);
        assert_eq!(found(&games, "theme hospital", &all), [1]);
        assert_eq!(found(&games, "", &steam), [0, 3]);
        assert_eq!(found(&games, "tomb", &steam), [3]);
        assert!(found(&games, "tomb", &stores(&[Store::Xbox])).is_empty());
        // Several stores at once.
        assert_eq!(found(&games, "", &stores(&[Store::Gog, Store::Ea])), [1, 2]);
        assert_eq!(
            found(&games, "tomb", &stores(&[Store::Gog, Store::Steam])),
            [2, 3]
        );
    }

    #[test]
    fn collections_fill_themselves() {
        let mut games = library();
        games[0].pinned = Some(0);
        games[1].arch = Some(gm_core::Arch::X86);
        games[2].arch = Some(gm_core::Arch::X64);
        games[2].installed = Some(NOW - 3 * DAY);
        games[3].installed = Some(NOW - 31 * DAY);
        let installed = || Some(gm_core::mods::Installed::default());
        let mut mods = ModsFile::default();
        let reshade = gm_core::mods::reshade::ReshadeState {
            installed: installed(),
            ..Default::default()
        };
        mods.games.entry(games[0].id.clone()).or_default().reshade = Some(reshade);
        mods.games.entry(games[1].id.clone()).or_default().laa = installed();
        // DXVK's settings alone are not an install.
        let mut dxvk = gm_core::mods::dxvk::DxvkState::default();
        mods.games.entry(games[2].id.clone()).or_default().dxvk = Some(dxvk.clone());
        dxvk.installed = installed();
        mods.games.entry(games[3].id.clone()).or_default().dxvk = Some(dxvk);

        let members = |collection, mods: &ModsFile| -> Vec<usize> {
            (0..games.len())
                .filter(|&index| in_collection(&games[index], collection, facts(mods)))
                .collect()
        };
        assert_eq!(members(Collection::Pinned, &mods), [0]);
        assert_eq!(members(Collection::Modded, &mods), [0, 1, 3]);
        assert_eq!(members(Collection::Reshade, &mods), [0]);
        assert_eq!(members(Collection::Dxvk, &mods), [3]);
        assert_eq!(members(Collection::Bits32, &mods), [1]);
        assert_eq!(members(Collection::RecentlyInstalled, &mods), [2]);
        let all = LibraryView::default();
        assert_eq!(
            collection_counts(&games, &all, facts(&mods)),
            [
                (Collection::Pinned, 1),
                (Collection::Modded, 3),
                (Collection::Reshade, 1),
                (Collection::Dxvk, 1),
                (Collection::Bits32, 1),
                (Collection::RecentlyInstalled, 1),
            ]
        );
        // NVIDIA's DLSS overrides count as a mod too.
        let mut more = mods.clone();
        more.games
            .entry(games[2].id.clone())
            .or_default()
            .dlss_override = Some(Default::default());
        assert_eq!(members(Collection::Modded, &more), [0, 1, 2, 3]);

        // A collection shows its games, and the search looks inside it.
        let modded = LibraryView {
            collection: Some(Collection::Modded),
            ..Default::default()
        };
        assert_eq!(shown(&games, "", &modded, facts(&mods)), [0, 1, 3]);
        assert_eq!(shown(&games, "tomb", &modded, facts(&mods)), [3]);
        // Hidden games are in none; one left empty is listed only while it is on, then dropped.
        games[3].hidden = true;
        assert_eq!(shown(&games, "", &modded, facts(&mods)), [0, 1]);
        let with_dxvk = LibraryView {
            collection: Some(Collection::Dxvk),
            ..Default::default()
        };
        let listed = |view: &LibraryView| -> Vec<Collection> {
            collection_counts(&games, view, facts(&mods))
                .into_iter()
                .map(|(collection, _)| collection)
                .collect()
        };
        assert!(!listed(&all).contains(&Collection::Dxvk));
        assert!(listed(&with_dxvk).contains(&Collection::Dxvk));
        assert_eq!(keep_view(&games, &with_dxvk, facts(&mods)), all);
        assert_eq!(keep_view(&games, &modded, facts(&mods)), modded);
    }

    #[test]
    fn a_collection_and_the_filters_take_turns() {
        let mut view = LibraryView {
            sort: Sort::NameDescending,
            ..Default::default()
        };
        assert!(!is_filtered(&view));
        assert_eq!(describe(&view), None);
        toggle_store(&mut view, Store::Gog);
        toggle_store(&mut view, Store::Steam);
        set_hidden(&mut view, true);
        assert!(is_filtered(&view));
        // Stores in the lists' order, whatever the order they were ticked in.
        assert_eq!(describe(&view).as_deref(), Some("Steam, GOG · Hidden"));
        toggle_store(&mut view, Store::Gog);
        assert_eq!(view.stores, [Store::Steam]);
        // A collection clears the filters; picking it again turns it off.
        pick_collection(&mut view, Collection::Modded);
        assert_eq!(
            (view.stores.len(), view.hidden, view.collection),
            (0, false, Some(Collection::Modded))
        );
        assert_eq!(describe(&view).as_deref(), Some("Modded"));
        pick_collection(&mut view, Collection::Bits32);
        assert_eq!(view.collection, Some(Collection::Bits32));
        pick_collection(&mut view, Collection::Bits32);
        assert_eq!(view.collection, None);
        // A filter turns the collection off.
        pick_collection(&mut view, Collection::Pinned);
        toggle_store(&mut view, Store::Ea);
        assert_eq!(
            (view.collection, view.stores.as_slice()),
            (None, &[Store::Ea][..])
        );
        pick_collection(&mut view, Collection::Pinned);
        set_hidden(&mut view, true);
        assert_eq!((view.collection, view.hidden), (None, true));
        // Clear all keeps the sort.
        toggle_store(&mut view, Store::Ea);
        clear(&mut view);
        assert!(!is_filtered(&view));
        assert_eq!(view.sort, Sort::NameDescending);

        let hidden = LibraryView {
            hidden: true,
            ..Default::default()
        };
        assert_eq!(empty_hint(&view, "x"), "No games match the search.");
        assert_eq!(empty_hint(&view, " "), "No games to show.");
        assert_eq!(empty_hint(&hidden, ""), "No hidden games.");
        assert_eq!(
            empty_hint(&stores(&[Store::Ea]), ""),
            "No games match these filters."
        );
    }

    #[test]
    fn sections_follow_the_sort_and_the_grouping() {
        let mut games = library();
        games.push(game("steam:22200", "Zeno Clash", Store::Steam));
        games[3].pinned = Some(0);
        games[3].installed = Some(99);
        games[0].installed = Some(10);
        games[1].installed = Some(30);
        games[4].installed = Some(50);
        let all = [0, 1, 2, 3, 4];
        let lists = |sort, group_by_store| -> Vec<(SectionKind, Vec<usize>)> {
            let view = LibraryView {
                sort,
                group_by_store,
                ..Default::default()
            };
            sections(&games, &all, &view)
                .into_iter()
                .map(|section| (section.kind, section.games))
                .collect()
        };
        let pinned = (SectionKind::Pinned, vec![3]);
        let steam = |games| (SectionKind::Store(Store::Steam), games);
        let others = [
            (SectionKind::Store(Store::Gog), vec![2]),
            (SectionKind::Store(Store::Ea), vec![1]),
        ];
        // Pinned first whatever the sort, then the stores in their order.
        for (sort, steam_games) in [
            (Sort::Name, vec![0, 4]),
            (Sort::NameDescending, vec![4, 0]),
            (Sort::RecentlyInstalled, vec![4, 0]),
        ] {
            let mut expected = vec![pinned.clone(), steam(steam_games)];
            expected.extend(others.clone());
            assert_eq!(lists(sort, true), expected, "{sort:?}");
        }
        // One list: by name either way, or the newest first with the undated game last.
        let flat = |games| vec![pinned.clone(), (SectionKind::All, games)];
        assert_eq!(lists(Sort::Name, false), flat(vec![0, 1, 2, 4]));
        assert_eq!(lists(Sort::NameDescending, false), flat(vec![4, 2, 1, 0]));
        assert_eq!(
            lists(Sort::RecentlyInstalled, false),
            flat(vec![4, 1, 0, 2])
        );
        // Only what is shown, and no empty sections.
        let view = LibraryView::default();
        assert_eq!(
            sections(&games, &[2], &view),
            [Section {
                kind: SectionKind::Store(Store::Gog),
                games: vec![2]
            }]
        );
        assert!(sections(&games, &[], &view).is_empty());
    }

    #[test]
    fn pinned_games_come_first_in_pin_order() {
        let mut games = library();
        set_pin(&mut games, "steam:8000", true);
        set_pin(&mut games, "ea:1025169", true);
        set_pin(&mut games, "steam:8000", true);
        let places: Vec<Option<usize>> = games.iter().map(|game| game.pinned).collect();
        assert_eq!(places, [None, Some(1), None, Some(0)]);
        let view = LibraryView::default();
        let mods = ModsFile::default();
        let all = found(&games, "", &view);
        assert_eq!(split_pinned(&games, &all), (vec![3, 1], vec![0, 2]));
        // The search and filters still apply; a pinned game's store filter shows it pinned.
        let tomb = found(&games, "tomb", &view);
        assert_eq!(split_pinned(&games, &tomb), (vec![3], vec![2]));
        assert_eq!(first_shown(&games, "tomb", &view, facts(&mods)), Some(3));
        let steam = found(&games, "", &stores(&[Store::Steam]));
        assert_eq!(split_pinned(&games, &steam), (vec![3], vec![0]));
        // Unpinning closes the gap; pinning again goes last.
        set_pin(&mut games, "steam:8000", false);
        set_pin(&mut games, "steam:8000", false);
        assert_eq!(games[1].pinned, Some(0));
        set_pin(&mut games, "steam:244210", true);
        set_pin(&mut games, "steam:8000", true);
        let places: Vec<Option<usize>> = games.iter().map(|game| game.pinned).collect();
        assert_eq!(places, [Some(1), Some(0), None, Some(2)]);
        // An id no game has changes nothing.
        set_pin(&mut games, "steam:1", true);
        assert_eq!(games.iter().filter(|game| game.pinned.is_some()).count(), 3);
    }

    #[test]
    fn the_filters_list_only_stores_with_games() {
        let games = library();
        let all = LibraryView::default();
        assert_eq!(
            store_counts(&games, &all),
            [(Store::Steam, 2), (Store::Gog, 1), (Store::Ea, 1)]
        );
        assert!(store_counts(&[], &all).is_empty());
        // A ticked store stays listed, so it can be unticked.
        assert_eq!(
            store_counts(&[], &stores(&[Store::Epic])),
            [(Store::Epic, 0)]
        );
    }

    #[test]
    fn hidden_games_show_only_with_hidden_on() {
        let mut games = library();
        games[0].hidden = true;
        games[3].hidden = true;
        games[3].pinned = Some(0);
        let mods = ModsFile::default();
        let facts = facts(&mods);
        let all = LibraryView::default();
        let steam = stores(&[Store::Steam]);
        let hidden = LibraryView {
            hidden: true,
            ..Default::default()
        };
        let hidden_of = |store| LibraryView {
            hidden: true,
            ..stores(&[store])
        };
        assert_eq!(found(&games, "", &all), [1, 2]);
        assert!(found(&games, "", &steam).is_empty());
        // The search leaves them out too, unless it searches the hidden ones.
        assert_eq!(found(&games, "tomb", &all), [2]);
        assert_eq!(found(&games, "", &hidden), [0, 3]);
        assert_eq!(found(&games, "tomb", &hidden), [3]);
        // Hidden and a store: that store's hidden games.
        assert_eq!(found(&games, "", &hidden_of(Store::Steam)), [0, 3]);
        assert!(found(&games, "", &hidden_of(Store::Gog)).is_empty());
        assert_eq!(first_shown(&games, "tomb", &all, facts), Some(2));
        assert_eq!(total(&games, &steam), 2);
        assert_eq!(total(&games, &hidden), 2);
        assert_eq!(games_label(1), "1 game");
        assert_eq!(games_label(2), "2 games");
        // Hidden games count only with Hidden on; a store with only hidden games isn't offered.
        assert_eq!(
            store_counts(&games, &all),
            [(Store::Gog, 1), (Store::Ea, 1)]
        );
        assert_eq!(store_counts(&games, &hidden), [(Store::Steam, 2)]);
        assert_eq!(hidden_count(&games), 2);
        // A store left without games to show is unticked; Hidden stays while some are hidden.
        assert_eq!(keep_view(&games, &steam, facts), all);
        assert_eq!(keep_view(&games, &hidden, facts), hidden);
        assert_eq!(
            keep_view(&games, &hidden_of(Store::Steam), facts),
            hidden_of(Store::Steam)
        );
        assert_eq!(keep_view(&games, &hidden_of(Store::Gog), facts), hidden);
        for game in &mut games {
            game.hidden = false;
        }
        // With none hidden, Hidden goes; the sort and a store with games stay.
        assert_eq!(hidden_count(&games), 0);
        assert_eq!(keep_view(&games, &hidden, facts), all);
        assert_eq!(keep_view(&games, &hidden_of(Store::Steam), facts), steam);
        assert_eq!(keep_view(&games, &steam, facts), steam);
        let sorted = LibraryView {
            sort: Sort::RecentlyInstalled,
            group_by_store: false,
            ..hidden.clone()
        };
        let kept = keep_view(&games, &sorted, facts);
        assert_eq!(
            (kept.sort, kept.group_by_store, kept.hidden),
            (Sort::RecentlyInstalled, false, false)
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
        let found = |query: &str| -> Vec<usize> { found(&games, query, &LibraryView::default()) };
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
        let mods = ModsFile::default();
        let first =
            |query: &str, view: &LibraryView| first_shown(&games, query, view, facts(&mods));
        let all = LibraryView::default();
        // Steam comes first in the sidebar, then its games in list order.
        assert_eq!(first("tomb", &all), Some(2));
        assert_eq!(first("tomb", &stores(&[Store::Gog])), Some(1));
        assert_eq!(first("nothing", &all), None);
        // The sort and the grouping decide which comes first.
        let view = |sort, group_by_store| LibraryView {
            sort,
            group_by_store,
            ..Default::default()
        };
        assert_eq!(first("tomb", &view(Sort::NameDescending, true)), Some(3));
        assert_eq!(first("tomb", &view(Sort::Name, false)), Some(0));
        assert_eq!(first("tomb", &view(Sort::NameDescending, false)), Some(3));
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
