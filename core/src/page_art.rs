//! A game page's art: the wide picture across the top of the page and the game's logo drawn over
//! it, as the stores' original files (the app draws them: `app/src/page_art.rs`).
//!
//! **Where each comes from**, the first that exists:
//! 1. The store's original, downloaded once into `%LOCALAPPDATA%\Game Manager\cache\page-art`
//!    (`picture_cache`):
//!    - by the game's own id: Steam's store API and the store's files beside it, GOG's API, the
//!      Epic launcher's catalog cache, Microsoft's store catalog for Xbox games;
//!    - else, for a game of another store (EA and Ubisoft games and games added by hand have no
//!      source by id, and an Epic game may have no wide art), from the Steam store by name: only a
//!      result with the same name counts.
//! 2. A copy the store keeps on this PC, shown until the original has arrived (`Art::stand_in`):
//!    Steam's client keeps `library_hero.jpg` (1920 × 620) and `logo.png` beside its cover, GOG
//!    Galaxy its background (`_glx_bg_top_padding_7.webp`).
//!
//! The background and the logo are looked up, cached and missed apart: a game may have only one.
//! What the stores had on 2026-11-14:
//! - Steam: `library_hero_2x`, 3840 × 1240 (about 1 MB), with no text in it, and `logo_2x.png` up
//!   to 1280 wide, which `GetItems` doesn't list but the store serves beside the hero.
//! - GOG: `galaxyBackgroundImage`, 2560 × 670, and a logo (a 1600 × 740 PNG).
//! - Epic: `DieselGameBox`, 2560 × 1440, often with the title in it; `DieselGameBoxLogo` for few
//!   games (18 of the 858 items of one catalog).
//! - Xbox: `SuperHeroArt`, 3840 × 2160 (a 2.4 MB JPEG for Forza Horizon 5); a `Logo` for few games.
//!
//! `local` needs no network and says whether the stores may still help; `fetch` asks them. Both
//! block: call them off the UI thread.

use crate::covers::{download, is_absent, store_number};
use crate::picture_cache::{PictureCache, describe_file, read_file};
use crate::text::letters;
use crate::{Game, Result, Store, epic, gog, picture, steam, xbox};
use std::path::PathBuf;

/// Bump when what is cached changes, so art cached by an older version is not reused.
const CACHE_FORMAT: u32 = 1;
/// The page art's folder in `%LOCALAPPDATA%\Game Manager\cache`.
const CACHE: &str = "page-art";

/// One picture of a game's page art: a file as the store has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Art {
    pub file: PathBuf,
    /// A copy the store keeps on this PC, shown until the original has arrived.
    pub stand_in: bool,
}

/// A game page's art; either part may be missing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageArt {
    /// The wide picture across the top of the page.
    pub background: Option<Art>,
    /// The game's logo, drawn over it.
    pub logo: Option<Art>,
}

impl PageArt {
    fn set(&mut self, part: Part, art: Option<Art>) {
        match part {
            Part::Background => self.background = art,
            Part::Logo => self.logo = art,
        }
    }
}

/// Where a store serves a game's page art, sharpest first.
#[derive(Clone, Debug, Default)]
pub(crate) struct ArtUrls {
    pub background: Vec<String>,
    pub logo: Vec<String>,
}

impl ArtUrls {
    fn get(&self, part: Part) -> &[String] {
        match part {
            Part::Background => &self.background,
            Part::Logo => &self.logo,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    Background,
    Logo,
}

impl Part {
    const ALL: [Part; 2] = [Part::Background, Part::Logo];

    fn name(self) -> &'static str {
        match self {
            Part::Background => "background",
            Part::Logo => "logo",
        }
    }
}

/// Where page art is looked for: the game's own store, by its id, or the Steam store, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Store,
    SteamByName,
}

/// The sources a store's games are looked up in, in order. An Epic game asks the Steam store
/// first: Epic's wide box art often has the title drawn in it, Steam's heroes never do (the logo
/// is drawn over them). EA and Ubisoft games and games added by hand have no source by id.
fn sources(store: Store) -> &'static [Source] {
    match store {
        Store::Steam => &[Source::Store],
        Store::Epic => &[Source::SteamByName, Source::Store],
        Store::Gog | Store::Xbox => &[Source::Store, Source::SteamByName],
        Store::Ea | Store::Ubisoft | Store::Custom => &[Source::SteamByName],
    }
}

/// What a part from a source is cached as: by the game's id, or by its id and name (a renamed
/// game is looked up again under its new name).
fn identity(source: Source, part: Part, game: &Game) -> String {
    match source {
        Source::Store => format!("{}|net|{}|{CACHE_FORMAT}", part.name(), game.id),
        Source::SteamByName => format!(
            "{}|name|{}|{}|{CACHE_FORMAT}",
            part.name(),
            game.id,
            letters(&game.name)
        ),
    }
}

/// The page art that needs no network: the originals in the cache, else the stores' copies on
/// this PC; and whether the stores' servers may still have an original. Blocking.
pub fn local(game: &Game) -> (PageArt, bool) {
    let Some(cache) = PictureCache::open(CACHE) else {
        return (PageArt::default(), false);
    };
    local_in(game, &cache, &Copies::find())
}

/// A game's page art from its store's servers (else from the Steam store, by name), cached, then
/// as `local` finds it. An error is a request that failed: it is tried again next time. Blocking.
pub fn fetch(game: &Game) -> Result<PageArt> {
    let Some(cache) = PictureCache::open(CACHE) else {
        return Ok(PageArt::default());
    };
    fetch_into(game, &cache)?;
    Ok(local_in(game, &cache, &Copies::find()).0)
}

/// Deletes cached page art no game uses any more. For a full library only.
pub fn prune(games: &[Game]) {
    let Some(cache) = PictureCache::open(CACHE) else {
        return;
    };
    let used = cache.used(games.iter().flat_map(|game| {
        Part::ALL.into_iter().flat_map(|part| {
            [Source::Store, Source::SteamByName].map(|source| identity(source, part, game))
        })
    }));
    cache.prune(&used);
}

/// Where a game's background and logo come from, or would be looked for, in a line: for
/// `scan --covers`. Reads only; nothing is downloaded or cached.
pub fn describe(game: &Game) -> String {
    let cache = PictureCache::existing(CACHE);
    let copies = Copies::find();
    let missed = |identity: &str| cache.as_ref().is_some_and(|cache| cache.missed(identity));
    let found = |identity: &str| cache.as_ref().and_then(|cache| cache.find(identity));
    Part::ALL
        .map(|part| {
            let from = |source: Source| match source {
                Source::Store => format!("{}'s original", game.store.name()),
                Source::SteamByName => "the Steam store's, found by name".to_owned(),
            };
            let cached = sources(game.store).iter().find_map(|&source| {
                found(&identity(source, part, game)).map(|file| (source, file))
            });
            let text = if let Some((source, file)) = cached {
                format!(
                    "{} ({}): {}",
                    from(source),
                    describe_file(&file),
                    file.display()
                )
            } else {
                let mut steps: Vec<String> = sources(game.store)
                    .iter()
                    .map(
                        |&source| match (source, missed(&identity(source, part, game))) {
                            (Source::Store, true) => format!("{} has none", game.store.name()),
                            (Source::Store, false) => format!("to ask {}", game.store.name()),
                            (Source::SteamByName, true) => {
                                "none on the Steam store by name".to_owned()
                            }
                            (Source::SteamByName, false) => {
                                "to look up on the Steam store by name".to_owned()
                            }
                        },
                    )
                    .collect();
                if let Some(file) = copies.file(part, game) {
                    steps.push(format!(
                        "meanwhile the copy on this PC ({}): {}",
                        describe_file(&file),
                        file.display()
                    ));
                }
                steps.join("; ")
            };
            format!("{}: {text}", part.name())
        })
        .join(" | ")
}

/// For each part: the original in the cache (from the first source that has it), else the
/// store's copy on this PC; and whether a source may still have an original of either.
fn local_in(game: &Game, cache: &PictureCache, copies: &Copies) -> (PageArt, bool) {
    let mut art = PageArt::default();
    let mut ask = false;
    for part in Part::ALL {
        let identities: Vec<String> = sources(game.store)
            .iter()
            .map(|&source| identity(source, part, game))
            .collect();
        let found = identities
            .iter()
            .enumerate()
            .find_map(|(index, identity)| cache.find(identity).map(|file| (index, file)));
        // A source before the one that gave it may still have a better one.
        let before = found.as_ref().map_or(identities.len(), |(index, _)| *index);
        ask |= identities[..before]
            .iter()
            .any(|identity| !cache.missed(identity));
        if let Some((_, file)) = found {
            art.set(
                part,
                Some(Art {
                    file,
                    stand_in: false,
                }),
            );
            continue;
        }
        let copy = copies
            .file(part, game)
            .filter(|file| read_file(file).is_some_and(|bytes| picture::format(&bytes).is_some()))
            .map(|file| Art {
                file,
                stand_in: true,
            });
        art.set(part, copy);
    }
    (art, ask)
}

/// Asks each source in turn for the parts no source before it has given and it hasn't given or
/// said it has none of: one request a source, for both parts.
fn fetch_into(game: &Game, cache: &PictureCache) -> Result<()> {
    let order = sources(game.store);
    for (index, &source) in order.iter().enumerate() {
        let open: Vec<Part> = Part::ALL
            .into_iter()
            .filter(|&part| {
                let mine = identity(source, part, game);
                order[..index]
                    .iter()
                    .all(|&earlier| cache.find(&identity(earlier, part, game)).is_none())
                    && cache.find(&mine).is_none()
                    && !cache.missed(&mine)
            })
            .collect();
        if open.is_empty() {
            continue;
        }
        let urls = match source {
            Source::Store => store_urls(game),
            Source::SteamByName => match steam::find_app(&game.name)? {
                Some(app_id) => steam::page_art_urls(app_id),
                None => Ok(ArtUrls::default()),
            },
        };
        let urls = match urls {
            Err(error) if is_absent(&error) => ArtUrls::default(),
            urls => urls?,
        };
        for part in open {
            save(cache, &identity(source, part, game), urls.get(part))?;
        }
    }
    Ok(())
}

/// The first of `urls` that is a whole picture, cached; a miss when none is.
fn save(cache: &PictureCache, identity: &str, urls: &[String]) -> Result<()> {
    for url in urls {
        if let Some(bytes) = download(url)? {
            cache.write(identity, &bytes);
            return Ok(());
        }
    }
    cache.mark_miss(identity);
    Ok(())
}

/// Where the game's store serves its page art.
fn store_urls(game: &Game) -> Result<ArtUrls> {
    match game.store {
        Store::Steam => match store_number(game).and_then(|id| id.parse().ok()) {
            Some(app_id) => steam::page_art_urls(app_id),
            None => Ok(ArtUrls::default()),
        },
        Store::Epic => Ok(epic::page_art_urls(game)),
        Store::Gog => match store_number(game) {
            Some(id) => gog::page_art_urls(id),
            None => Ok(ArtUrls::default()),
        },
        Store::Xbox => match game.install_dir.as_deref().and_then(xbox::store_id) {
            Some(id) => xbox::page_art_urls(&id),
            None => Ok(ArtUrls::default()),
        },
        Store::Ea | Store::Ubisoft | Store::Custom => Ok(ArtUrls::default()),
    }
}

/// Where the stores keep page art on this PC, looked up once.
struct Copies {
    steam: Option<PathBuf>,
    galaxy: Option<PathBuf>,
}

impl Copies {
    fn find() -> Self {
        Self {
            steam: steam::install_dir(),
            galaxy: gog::galaxy_webcache(),
        }
    }

    /// The store's own copy of a game's background or logo on this PC.
    fn file(&self, part: Part, game: &Game) -> Option<PathBuf> {
        match (game.store, part) {
            (Store::Steam, _) => {
                let app_id = store_number(game)?.parse().ok()?;
                let (background, logo) = steam::cached_page_art(self.steam.as_deref()?, app_id);
                match part {
                    Part::Background => background,
                    Part::Logo => logo,
                }
            }
            (Store::Gog, Part::Background) => {
                gog::cached_background(self.galaxy.as_deref()?, store_number(game)?)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Launch;
    use crate::picture_cache::tests::{bmp, png, temp_dir};
    use std::fs;

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

    /// Each part's file and whether it is a stand-in, and whether to ask the network.
    type Found = (Option<(PathBuf, bool)>, Option<(PathBuf, bool)>, bool);

    fn found(art: (PageArt, bool)) -> Found {
        let pair = |art: Option<Art>| art.map(|art| (art.file, art.stand_in));
        (pair(art.0.background), pair(art.0.logo), art.1)
    }

    fn net_identity(part: Part, game: &Game) -> String {
        identity(Source::Store, part, game)
    }

    fn name_identity(part: Part, game: &Game) -> String {
        identity(Source::SteamByName, part, game)
    }

    #[test]
    fn identities_follow_the_part_the_game_and_its_name() {
        let dead_space = game("ea:1", "Dead Space™", Store::Ea);
        assert_eq!(
            net_identity(Part::Background, &dead_space),
            "background|net|ea:1|1"
        );
        assert_eq!(
            name_identity(Part::Logo, &dead_space),
            "logo|name|ea:1|deadspace|1"
        );
        let renamed = game("ea:1", "Dead Space (2023)", Store::Ea);
        assert_ne!(
            name_identity(Part::Logo, &renamed),
            name_identity(Part::Logo, &dead_space)
        );
        // An Epic game asks the Steam store first; a Steam game only Steam.
        assert_eq!(sources(Store::Epic), [Source::SteamByName, Source::Store]);
        assert_eq!(sources(Store::Steam), [Source::Store]);
    }

    #[test]
    fn the_original_wins_and_a_copy_on_this_pc_shows_meanwhile() {
        let dir = temp_dir("page-art");
        let cache = PictureCache::at(dir.join("cache")).unwrap();
        // What Steam's client and GOG Galaxy keep.
        let steam = dir.join("steam");
        let hero = steam.join(r"appcache\librarycache\220\5925343a\library_hero.jpg");
        let logo = steam.join(r"appcache\librarycache\220\logo.png");
        let galaxy = dir.join("galaxy");
        let background = galaxy.join(r"4744\gog\1495134320\58a4_glx_bg_top_padding_7.webp");
        for (file, bytes) in [
            (&hero, bmp(192, 62)),
            (&logo, png(64, 36)),
            (&background, bmp(256, 67)),
        ] {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(file, bytes).unwrap();
        }
        let copies = Copies {
            steam: Some(steam),
            galaxy: Some(galaxy),
        };
        let find = |game: &Game| found(local_in(game, &cache, &copies));

        // Steam's copies show while the originals are asked for.
        let half_life = game("steam:220", "Half-Life 2", Store::Steam);
        assert_eq!(
            find(&half_life),
            (Some((hero.clone(), true)), Some((logo.clone(), true)), true)
        );
        // The original background once it has arrived; the logo's stand-in still asks.
        let original = cache
            .write(&net_identity(Part::Background, &half_life), &png(384, 124))
            .unwrap();
        assert_eq!(
            find(&half_life),
            (
                Some((original.clone(), false)),
                Some((logo.clone(), true)),
                true
            )
        );
        // Steam has no larger logo: the copy stays, and nothing is asked any more.
        cache.mark_miss(&net_identity(Part::Logo, &half_life));
        assert_eq!(
            find(&half_life),
            (Some((original, false)), Some((logo, true)), false)
        );

        // GOG Galaxy keeps a background only; the logo is the API's.
        let witcher = game("gog:1495134320", "The Witcher 3", Store::Gog);
        assert_eq!(find(&witcher), (Some((background, true)), None, true));
        // Neither by id: the Steam store by name, until it has none either.
        for part in Part::ALL {
            cache.mark_miss(&net_identity(part, &witcher));
        }
        assert!(find(&witcher).2);
        for part in Part::ALL {
            cache.mark_miss(&name_identity(part, &witcher));
        }
        assert!(!find(&witcher).2);

        // An EA game has only the lookup by name, whose originals then show.
        let dead_space = game("ea:1", "Dead Space", Store::Ea);
        assert_eq!(find(&dead_space), (None, None, true));
        let by_name = cache
            .write(&name_identity(Part::Logo, &dead_space), &png(64, 20))
            .unwrap();
        cache.mark_miss(&name_identity(Part::Background, &dead_space));
        assert_eq!(find(&dead_space), (None, Some((by_name, false)), false));
        // An Epic game: the Steam store's art first, else Epic's own. Epic's, found first, shows
        // while the Steam store may still have one.
        let cyberpunk = game("epic:Ginger", "Cyberpunk 2077", Store::Epic);
        let epic_wide = cache
            .write(&net_identity(Part::Background, &cyberpunk), &png(256, 144))
            .unwrap();
        cache.mark_miss(&name_identity(Part::Logo, &cyberpunk));
        cache.mark_miss(&net_identity(Part::Logo, &cyberpunk));
        assert_eq!(
            find(&cyberpunk),
            (Some((epic_wide.clone(), false)), None, true)
        );
        cache.mark_miss(&name_identity(Part::Background, &cyberpunk));
        assert_eq!(find(&cyberpunk), (Some((epic_wide, false)), None, false));
        let steam_hero = cache
            .write(&name_identity(Part::Background, &cyberpunk), &png(384, 124))
            .unwrap();
        assert_eq!(find(&cyberpunk), (Some((steam_hero, false)), None, false));
        // A Steam app with nothing on this PC: the store, until it says it has none.
        let tool = game("steam:7", "A tool", Store::Steam);
        assert_eq!(find(&tool), (None, None, true));
        for part in Part::ALL {
            cache.mark_miss(&net_identity(part, &tool));
        }
        assert_eq!(find(&tool), (None, None, false));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_address_is_a_miss() {
        let dir = temp_dir("page-art-miss");
        let cache = PictureCache::at(dir.clone()).unwrap();
        save(&cache, "logo|net|steam:7|1", &[]).unwrap();
        assert!(cache.missed("logo|net|steam:7|1"));
        fs::remove_dir_all(&dir).unwrap();
    }

    /// One real page art of each network source. Run with
    /// `cargo test -p gm-core live_page_art -- --ignored --nocapture`.
    #[test]
    #[ignore = "asks Steam's, GOG's and Microsoft's servers"]
    fn live_page_art() {
        let check = |source: &str, urls: &[String], wide: bool| {
            let url = urls
                .first()
                .unwrap_or_else(|| panic!("{source}: no address"));
            let bytes = download(url)
                .unwrap()
                .unwrap_or_else(|| panic!("{source}: no picture at {url}"));
            let format = picture::format(&bytes).unwrap();
            let (width, height) = picture::size(&bytes).unwrap();
            println!(
                "{source}: {format} {width} × {height}, {} bytes, {url}",
                bytes.len()
            );
            if wide {
                // At least 16:9, and sharp across a wide window.
                assert!(width >= 1920 && width * 9 >= height * 16, "{source}");
            }
        };
        let steam = steam::page_art_urls(220).unwrap();
        check("Steam background (Half-Life 2)", &steam.background, true);
        check("Steam logo (Half-Life 2)", &steam.logo, false);
        let gog = gog::page_art_urls("1495134320").unwrap();
        check("GOG background (The Witcher 3)", &gog.background, true);
        check("GOG logo (The Witcher 3)", &gog.logo, false);
        let xbox = xbox::page_art_urls("9NKX70BBCDRN").unwrap();
        check("Xbox background (Forza Horizon 5)", &xbox.background, true);
    }
}
