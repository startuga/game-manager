//! Play tracking: when each game was last played and for how long, from the sessions Game Manager
//! sees while it is open, whatever started the game (Play, the store, a shortcut), and, for Steam
//! games, Steam's own history.
//!
//! - **Seeing a game run** (`processes`): every few seconds the app lists the running processes
//!   and matches them to the games' programs (`Watchlist`, `running`).
//! - **Sessions** (`Tracker::observe`): a game's processes seen together are one session. It
//!   starts when one is first seen and ends when none is; it counts only the time it was seen
//!   (each step of at most `MAX_GAP`, so the PC sleeping isn't play), and sessions shorter than
//!   `MIN_SESSION` don't count (a crash at start, a launcher handing over).
//! - **The history** (`%APPDATA%\Game Manager\play-history.json`, `load` and `save`): per game the
//!   last time it was played, its seconds and sessions, and the sessions still open with their
//!   processes. There is no hook when Game Manager closes, so the open sessions are saved as they
//!   go: the next start resumes one whose process (same id, same creation time) still runs,
//!   counting the time Game Manager was closed, and ends the others when it last saw them.
//! - **Steam** (`steam::play_times`): Steam's own last played and minutes, sessions before Game
//!   Manager included. Steam's count already holds the sessions Game Manager saw, so the two are
//!   merged by taking the larger of each (`merged`), never added.
//! - **Display settings while playing** (`display`): what Play changed on a display for a game,
//!   kept in the history (`History::displays`) from the change until it is put back.

pub mod display;
pub mod processes;

pub use display::DisplayRestore;
pub use processes::{Seen, Watchlist, running};

use crate::steam::SteamPlay;
use crate::{Result, json_file};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const HISTORY_FILE: &str = "play-history.json";
/// A session shorter than this, in seconds, doesn't count.
pub const MIN_SESSION: u64 = 10;
/// The most time between two sightings of a game, in seconds, that counts as play: a longer one
/// means the PC slept or the watcher stalled.
pub const MAX_GAP: u64 = 60;

/// A game's play so far.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GamePlay {
    /// When it was last played, in seconds since 1970.
    #[serde(default)]
    pub last_played: u64,
    #[serde(default)]
    pub seconds: u64,
    #[serde(default)]
    pub sessions: u32,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// A session still going on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenSession {
    /// When it started and when it was last seen, in seconds since 1970.
    pub started: u64,
    pub last_seen: u64,
    /// The seconds counted so far.
    #[serde(default)]
    pub played: u64,
    /// The game's program, so the session stays watched if a scan loses the game.
    pub program: PathBuf,
    /// Its processes when last seen: id and creation time (seconds since 1970; 0 when Windows
    /// wouldn't say).
    #[serde(default)]
    pub processes: Vec<(u32, u64)>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// `play-history.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub games: BTreeMap<String, GamePlay>,
    #[serde(default)]
    pub open: BTreeMap<String, OpenSession>,
    /// What Play changed on a display for each game, until it is put back (`display`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub displays: BTreeMap<String, DisplayRestore>,
    #[serde(flatten)]
    other: serde_json::Map<String, serde_json::Value>,
}

/// A session that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finished {
    pub game: String,
    pub started: u64,
    pub ended: u64,
    /// The seconds it counted.
    pub played: u64,
}

impl Finished {
    /// Whether it was long enough to count (`MIN_SESSION`).
    pub fn counts(&self) -> bool {
        self.played >= MIN_SESSION
    }
}

/// What changed in one look at the running processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Started(String),
    Ended(Finished),
}

/// What the window shows for a game: when it was last played, and for how long in all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Played {
    pub last_played: Option<u64>,
    pub seconds: u64,
}

/// Adds a finished session to `games`; one too short doesn't count. Whether it counted.
pub fn add(games: &mut BTreeMap<String, GamePlay>, finished: &Finished) -> bool {
    if !finished.counts() {
        return false;
    }
    let play = games.entry(finished.game.clone()).or_default();
    play.last_played = play.last_played.max(finished.ended);
    play.seconds += finished.played;
    play.sessions += 1;
    true
}

/// One reader or writer of the file at a time.
static FILE_LOCK: Mutex<()> = Mutex::new(());

/// The history; empty when there is none yet, an error when it can't be read (and it is then
/// never overwritten). Blocking.
pub fn load() -> Result<History> {
    load_in(&json_file::user_file(HISTORY_FILE)?)
}

fn load_in(file: &Path) -> Result<History> {
    let _lock = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    json_file::load(file)
}

/// What `save` writes: the open sessions, the finished ones to add, and the display changes to
/// put back.
pub struct Changes<'a> {
    pub open: &'a BTreeMap<String, OpenSession>,
    pub finished: &'a [Finished],
    pub displays: &'a BTreeMap<String, DisplayRestore>,
}

/// Saves the open sessions and the display changes and adds the finished sessions, the file read
/// first (so a file that can't be read is never overwritten, and fields of a newer version are
/// kept). Returns the history as saved. Blocking.
pub fn save(changes: &Changes) -> Result<History> {
    save_in(&json_file::user_file(HISTORY_FILE)?, changes)
}

fn save_in(file: &Path, changes: &Changes) -> Result<History> {
    let _lock = FILE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let mut history: History = json_file::load(file)?;
    for session in changes.finished {
        add(&mut history.games, session);
    }
    history.open.clone_from(changes.open);
    history.displays.clone_from(changes.displays);
    json_file::save(file, &history)?;
    Ok(history)
}

/// The sessions going on, followed from one look at the running processes to the next.
#[derive(Debug, Default)]
pub struct Tracker {
    open: BTreeMap<String, OpenSession>,
    /// Sessions an earlier run left open, not yet looked at in this one.
    resumed: HashSet<String>,
}

impl Tracker {
    /// Takes up the sessions an earlier run left open (`History::open`).
    pub fn resume(open: BTreeMap<String, OpenSession>) -> Self {
        let resumed = open.keys().cloned().collect();
        Self { open, resumed }
    }

    pub fn open(&self) -> &BTreeMap<String, OpenSession> {
        &self.open
    }

    /// Whether the game is running now.
    pub fn is_running(&self, game: &str) -> bool {
        self.open.contains_key(game)
    }

    /// Follows the sessions with what runs `now` (seconds since 1970).
    pub fn observe(&mut self, now: u64, seen: &[Seen]) -> Vec<Event> {
        let mut by_game: BTreeMap<&str, Vec<&Seen>> = BTreeMap::new();
        for process in seen {
            by_game
                .entry(process.game.as_str())
                .or_default()
                .push(process);
        }
        let mut events = Vec::new();
        let ids: Vec<String> = self.open.keys().cloned().collect();
        for id in ids {
            let resumed = self.resumed.remove(&id);
            match (by_game.get(id.as_str()), self.open.get_mut(&id)) {
                (Some(processes), Some(session)) => {
                    let gap = now.saturating_sub(session.last_seen);
                    // Game Manager was closed while the same process ran on: all of it counts.
                    let same = resumed
                        && processes.iter().any(|process| {
                            process.created != 0
                                && session.processes.contains(&(process.pid, process.created))
                        });
                    if gap <= MAX_GAP || same {
                        session.played += gap;
                    }
                    session.last_seen = now.max(session.last_seen);
                    session.processes = processes.iter().map(|p| (p.pid, p.created)).collect();
                }
                (None, Some(_)) => {
                    if let Some(session) = self.open.remove(&id) {
                        events.push(Event::Ended(Finished {
                            game: id,
                            started: session.started,
                            ended: session.last_seen,
                            played: session.played,
                        }));
                    }
                }
                _ => {}
            }
        }
        for (id, processes) in by_game {
            if self.open.contains_key(id) {
                continue;
            }
            self.open.insert(
                id.to_owned(),
                OpenSession {
                    started: now,
                    last_seen: now,
                    played: 0,
                    program: processes[0].program.clone(),
                    processes: processes.iter().map(|p| (p.pid, p.created)).collect(),
                    other: serde_json::Map::new(),
                },
            );
            events.push(Event::Started(id.to_owned()));
        }
        events
    }
}

/// Each game's play as shown: Game Manager's own history, and for `steam:<appid>` Steam's too,
/// merged by taking the later last played and the larger time (Steam's count already holds the
/// sessions Game Manager saw).
pub fn merged(
    games: &BTreeMap<String, GamePlay>,
    steam: &BTreeMap<u32, SteamPlay>,
) -> HashMap<String, Played> {
    let mut played: HashMap<String, Played> = games
        .iter()
        .map(|(id, play)| {
            let last_played = (play.last_played > 0).then_some(play.last_played);
            let seconds = play.seconds;
            (
                id.clone(),
                Played {
                    last_played,
                    seconds,
                },
            )
        })
        .collect();
    for (app, play) in steam {
        let entry = played.entry(format!("steam:{app}")).or_default();
        if play.last_played > 0 {
            entry.last_played = Some(
                entry
                    .last_played
                    .map_or(play.last_played, |ours| ours.max(play.last_played)),
            );
        }
        entry.seconds = entry.seconds.max(play.minutes * 60);
    }
    played
}

/// Steam's play history (`steam::play_times`), empty without Steam. Blocking.
pub fn steam_play() -> Result<BTreeMap<u32, SteamPlay>> {
    Ok(crate::steam::play_times()?.map_or_default(|(_, apps)| apps))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn seen(game: &str, pid: u32, created: u64) -> Seen {
        Seen {
            game: game.into(),
            pid,
            created,
            program: PathBuf::from(format!(r"C:\Games\{game}.exe")),
        }
    }

    fn ended(events: &[Event]) -> Vec<&Finished> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Ended(finished) => Some(finished),
                Event::Started(_) => None,
            })
            .collect()
    }

    #[test]
    fn sessions_start_count_and_end() {
        let mut tracker = Tracker::default();
        assert_eq!(
            tracker.observe(1000, &[seen("a", 1, 900)]),
            [Event::Started("a".into())]
        );
        assert!(tracker.is_running("a"));
        assert!(tracker.observe(1005, &[seen("a", 1, 900)]).is_empty());
        assert!(tracker.observe(1010, &[seen("a", 1, 900)]).is_empty());
        assert_eq!(tracker.open()["a"].played, 10);
        let events = tracker.observe(1015, &[]);
        assert_eq!(
            ended(&events),
            [&Finished {
                game: "a".into(),
                started: 1000,
                ended: 1010,
                played: 10
            }]
        );
        assert!(!tracker.is_running("a"));
    }

    #[test]
    fn a_gap_is_not_play() {
        let mut tracker = Tracker::default();
        tracker.observe(1000, &[seen("a", 1, 900)]);
        tracker.observe(1005, &[seen("a", 1, 900)]);
        // The PC slept for an hour: that hour isn't play.
        tracker.observe(4605, &[seen("a", 1, 900)]);
        tracker.observe(4610, &[seen("a", 1, 900)]);
        assert_eq!(tracker.open()["a"].played, 10);
    }

    #[test]
    fn a_resumed_session_continues_while_its_process_runs() {
        let mut first = Tracker::default();
        first.observe(1000, &[seen("a", 7, 990)]);
        first.observe(1005, &[seen("a", 7, 990)]);
        // Game Manager closed for ten minutes; the same process still runs.
        let mut next = Tracker::resume(first.open().clone());
        assert!(next.observe(1605, &[seen("a", 7, 990)]).is_empty());
        assert_eq!(next.open()["a"].played, 605);
        assert_eq!(next.open()["a"].started, 1000);
        // Not the same process (another start of the game): the gap isn't counted.
        let mut other = Tracker::resume(first.open().clone());
        other.observe(1605, &[seen("a", 8, 1500)]);
        assert_eq!(other.open()["a"].played, 5);
    }

    #[test]
    fn a_resumed_session_ends_when_its_process_is_gone() {
        let mut first = Tracker::default();
        first.observe(1000, &[seen("a", 7, 990)]);
        first.observe(1030, &[seen("a", 7, 990)]);
        let mut next = Tracker::resume(first.open().clone());
        let events = next.observe(5000, &[]);
        assert_eq!(ended(&events)[0].ended, 1030);
        assert_eq!(ended(&events)[0].played, 30);
    }

    #[test]
    fn two_processes_are_one_session() {
        let mut tracker = Tracker::default();
        let events = tracker.observe(1000, &[seen("a", 1, 900), seen("a", 2, 950)]);
        assert_eq!(events, [Event::Started("a".into())]);
        // One of them ends: the session goes on.
        assert!(tracker.observe(1005, &[seen("a", 2, 950)]).is_empty());
        assert_eq!(tracker.open()["a"].processes, [(2, 950)]);
    }

    #[test]
    fn short_sessions_are_not_counted() {
        let mut games = BTreeMap::new();
        let short = Finished {
            game: "a".into(),
            started: 1000,
            ended: 1005,
            played: 5,
        };
        assert!(!add(&mut games, &short));
        assert!(games.is_empty());
        let long = Finished {
            played: 600,
            ended: 1600,
            ..short
        };
        assert!(add(&mut games, &long));
        assert!(add(&mut games, &long));
        assert_eq!(games["a"].seconds, 1200);
        assert_eq!(games["a"].sessions, 2);
        assert_eq!(games["a"].last_played, 1600);
    }

    #[test]
    fn the_history_round_trips_and_keeps_unknown_fields() {
        let dir = std::env::temp_dir().join(format!("GM-Play-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(HISTORY_FILE);
        fs::write(
            &file,
            r#"{"games":{"steam:220":{"lastPlayed":10,"seconds":60,"sessions":1,"rating":5}},"version":3}"#,
        )
        .unwrap();
        let mut tracker = Tracker::default();
        tracker.observe(1000, &[seen("custom:1", 1, 990)]);
        let finished = Finished {
            game: "steam:220".into(),
            started: 1000,
            ended: 1700,
            played: 700,
        };
        let displays = BTreeMap::from([(
            "custom:1".to_owned(),
            DisplayRestore {
                display: "id".into(),
                name: "TV".into(),
                hdr_before: Some(false),
                mode: None,
            },
        )]);
        let changes = Changes {
            open: tracker.open(),
            finished: &[finished],
            displays: &displays,
        };
        let saved = save_in(&file, &changes).unwrap();
        assert_eq!(saved, load_in(&file).unwrap());
        assert_eq!(saved.games["steam:220"].seconds, 760);
        assert_eq!(saved.games["steam:220"].last_played, 1700);
        assert!(saved.open.contains_key("custom:1"));
        assert_eq!(saved.displays, displays);
        let text = fs::read_to_string(&file).unwrap();
        assert!(
            text.contains("\"rating\": 5") && text.contains("\"version\": 3"),
            "{text}"
        );
        // Once put back, the display changes leave the file.
        let none = Changes {
            open: tracker.open(),
            finished: &[],
            displays: &BTreeMap::new(),
        };
        assert!(save_in(&file, &none).unwrap().displays.is_empty());
        assert!(!fs::read_to_string(&file).unwrap().contains("displays"));
        // A file that can't be read is never overwritten.
        fs::write(&file, "{ not json").unwrap();
        assert!(save_in(&file, &none).is_err());
        assert_eq!(fs::read_to_string(&file).unwrap(), "{ not json");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn steam_and_ours_merge_without_double_counting() {
        let mut games = BTreeMap::new();
        add(
            &mut games,
            &Finished {
                game: "steam:220".into(),
                started: 0,
                ended: 2000,
                played: 1800,
            },
        );
        add(
            &mut games,
            &Finished {
                game: "custom:1".into(),
                started: 0,
                ended: 500,
                played: 60,
            },
        );
        let steam = BTreeMap::from([
            (
                220,
                SteamPlay {
                    last_played: 1000,
                    minutes: 600,
                },
            ),
            (
                570,
                SteamPlay {
                    last_played: 0,
                    minutes: 5,
                },
            ),
        ]);
        let played = merged(&games, &steam);
        assert_eq!(
            played["steam:220"],
            Played {
                last_played: Some(2000),
                seconds: 36_000
            }
        );
        assert_eq!(
            played["steam:570"],
            Played {
                last_played: None,
                seconds: 300
            }
        );
        assert_eq!(played["custom:1"].seconds, 60);
    }
}
