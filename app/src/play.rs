//! Play tracking in the window (`gm_core::play`): which games are running, when each was last
//! played and for how long, for the game page (the Play button reads Running, Details shows Last
//! played and Play time) and the lists (the Recently played sort and Smart Collection).
//!
//! - **At start** the history (`play-history.json`) and Steam's own are read in the background
//!   (`load_play`), the sessions left open are taken up (`Tracker::resume`), and the watcher
//!   starts: every `LOOK_EVERY` a token-carrying timer (`Ctx::after`) has a background job list
//!   the running processes (`play::running`) and the tracker follows the sessions with them. A
//!   tick that sees nothing new is quiet (`Ctx::quiet`): the view isn't built again.
//! - **The watchlist** (the games' programs, `Watchlist`) is made again in the background whenever
//!   the games change (a scan, a game added or removed: `rewatch`).
//! - **Saving**: when a session starts or ends, and every `SAVE_EVERY` while one is open, the open
//!   sessions and the finished ones go to the file, one save at a time; a failed save keeps them
//!   for the next and shows its error once.
//! - **Steam** writes its own history a little after a game ends: it is read again then
//!   (`STEAM_AFTER`).
//! - **Display settings while playing** (`gm_core::play::display`; the card is `play_cards.rs`):
//!   each game's choice is read at start (`config::display_choices`) and saved as it changes.
//!   Play with a choice applies it in the background first ("Starting…" meanwhile), then starts
//!   the game; a choice that fails still starts it, and the error bar says why. What changed is
//!   kept (`restores`, saved with the open sessions) and put back when the game's session ends,
//!   or after `DISPLAY_WAIT` if the game never shows (another program runs, it failed to start).
//!   Only one game's changes at a time, and none during My PC's mode trial. A change an earlier
//!   run left is put back at start, unless its game still runs.
//! - **The displays** the card lists are read when the Overview shows them, and again after a
//!   change (`request_displays`).

use crate::shell::ctx::Ctx;
use crate::{GameManager, GameTab, Message, Page};
use gm_core::config::{self, DisplayChoice};
use gm_core::local_time::{self, LocalTime};
use gm_core::pc::{self, Display};
use gm_core::play::{
    self, DisplayRestore, Event, Finished, GamePlay, History, Played, Seen, Tracker, Watchlist,
    display,
};
use gm_core::steam::SteamPlay;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// How often the running processes are looked at.
const LOOK_EVERY: Duration = Duration::from_secs(5);
/// How often, in seconds, the open sessions are saved while nothing else changes.
const SAVE_EVERY: u64 = 60;
/// How long after a Steam game ends Steam's history is read again (Steam writes it a little
/// after).
const STEAM_AFTER: Duration = Duration::from_secs(20);
/// How long "Starting…" waits for a watched game to be seen running (a store's launcher first).
pub(crate) const LAUNCH_WAIT: Duration = Duration::from_secs(30);
/// How long display settings wait for their game to be seen running before they are put back.
const DISPLAY_WAIT: Duration = Duration::from_secs(180);

#[derive(Default)]
pub(crate) struct PlayTracking {
    /// Each game's play as shown: Game Manager's history and Steam's merged, running games as
    /// played now.
    pub played: HashMap<String, Played>,
    /// The history was read (or couldn't be): until then the Recently played collection isn't
    /// dropped for having no games.
    pub ready: bool,
    tracker: Tracker,
    totals: BTreeMap<String, GamePlay>,
    steam: BTreeMap<u32, SteamPlay>,
    watch: Arc<Watchlist>,
    /// The watcher's token: a timer of an older chain stops.
    token: u64,
    /// A look at the processes is under way.
    looking: bool,
    /// A save is under way: finished sessions wait in `unsaved` meanwhile, and it runs again
    /// after when `save_again`.
    saving: bool,
    save_again: bool,
    unsaved: Vec<Finished>,
    in_flight: Vec<Finished>,
    last_saved: u64,
    /// The last save failed and said so: the next failure stays quiet.
    save_failed: bool,
    /// Each game's display settings while it plays, by game id.
    pub display_choices: BTreeMap<String, DisplayChoice>,
    /// They were read (or couldn't be): a desktop shortcut's game waits for them.
    pub choices_ready: bool,
    /// The displays the While playing card lists: `None` until read (again).
    pub displays: Option<Result<Vec<Display>, String>>,
    displays_pending: bool,
    /// Display settings being applied before their game starts.
    applying: HashSet<String>,
    /// What Play changed on a display, by game, until it is put back; saved with the sessions.
    restores: BTreeMap<String, DisplayRestore>,
    /// Each change's token, so the wait of an earlier one does nothing.
    restore_tokens: HashMap<String, u64>,
    changes: u64,
}

impl PlayTracking {
    /// Whether the game is running now.
    pub(crate) fn is_running(&self, game: &str) -> bool {
        self.tracker.is_running(game)
    }

    /// Whether the game's program is watched (so "Starting…" can wait for it to be seen).
    pub(crate) fn watches(&self, game: &str) -> bool {
        self.watch.watches(game)
    }

    /// Whether the game's display settings are being applied before it starts.
    pub(crate) fn is_applying(&self, game: &str) -> bool {
        self.applying.contains(game)
    }

    /// The displays are read again when next shown (a change, another page).
    pub(crate) fn forget_displays(&mut self) {
        if !self.displays_pending {
            self.displays = None;
        }
    }

    /// The seconds a game was played in all, the session going on included.
    pub(crate) fn seconds(&self, game: &str) -> u64 {
        let before = self.played.get(game).map_or(0, |play| play.seconds);
        before
            + self
                .tracker
                .open()
                .get(game)
                .map_or(0, |session| session.played)
    }

    /// `played` from the totals, Steam's history and the sessions going on.
    fn remerge(&mut self) {
        self.played = play::merged(&self.totals, &self.steam);
        for (id, session) in self.tracker.open() {
            let entry = self.played.entry(id.clone()).or_default();
            entry.last_played = Some(
                entry
                    .last_played
                    .map_or(session.last_seen, |at| at.max(session.last_seen)),
            );
        }
    }
}

#[derive(Clone)]
pub(crate) enum PlayMessage {
    /// The history and Steam's, read at start.
    Loaded(Result<(History, BTreeMap<u32, SteamPlay>), String>),
    Watching(Arc<Watchlist>),
    Tick(u64),
    /// What a look at the processes saw, for the watcher with that token.
    Seen(u64, Result<Vec<Seen>, String>),
    Saved(Result<History, String>),
    ReadSteam,
    SteamRead(BTreeMap<u32, SteamPlay>),
    /// The games' display choices, read at start.
    ChoicesLoaded(Result<BTreeMap<String, DisplayChoice>, String>),
    DisplaysRead(Result<Vec<Display>, String>),
    /// A game's display choice changed on its card (`None`: don't change the display).
    SetChoice(String, Option<DisplayChoice>),
    ChoiceSaved(Result<(), String>),
    /// A game's display settings were applied (what changed), or what failed: now it starts.
    DisplayApplied {
        id: String,
        option: Option<usize>,
        result: Result<Option<DisplayRestore>, String>,
    },
    /// The wait of the change with this token is over: put it back unless its game runs.
    DisplayWaited(String, u64),
    /// A game's display settings were put back, or what failed; the game's name.
    DisplayRestored(String, Result<(), String>),
}

impl From<PlayMessage> for Message {
    fn from(message: PlayMessage) -> Self {
        Message::Play(message)
    }
}

/// The watcher's next look.
fn tick(token: u64, context: &Ctx<Message>) {
    context.after(LOOK_EVERY, PlayMessage::Tick(token).into());
}

impl GameManager {
    /// Reads the play history and Steam's, in the background, at start.
    pub(crate) fn load_play(context: &Ctx<Message>) {
        context.spawn_background(|| {
            let loaded = play::load()
                .map(|history| (history, play::steam_play().unwrap_or_default()))
                .map_err(|e| format!("Play history: {e}"));
            PlayMessage::Loaded(loaded).into()
        });
        context.spawn_background(|| {
            let choices = config::display_choices().map_err(|e| format!("Display settings: {e}"));
            PlayMessage::ChoicesLoaded(choices).into()
        });
    }

    /// Makes the watchlist again from the games, in the background (the games changed).
    pub(crate) fn rewatch(&self, context: &Ctx<Message>) {
        if !self.play.ready {
            return;
        }
        let games = self.games.clone();
        let open = self.play.tracker.open().clone();
        context.spawn_background(move || {
            PlayMessage::Watching(Arc::new(Watchlist::new(&games, &open))).into()
        });
    }

    pub(crate) fn handle_play(&mut self, message: PlayMessage, context: &Ctx<Message>) {
        match message {
            PlayMessage::Loaded(Ok((history, steam))) => {
                let state = &mut self.play;
                state.tracker = Tracker::resume(history.open);
                state.totals = history.games;
                state.steam = steam;
                state.ready = true;
                state.remerge();
                // Display changes an earlier run left: kept while their game still runs (put back
                // when it ends), else put back now.
                let (running, done): (BTreeMap<_, _>, BTreeMap<_, _>) = history
                    .displays
                    .into_iter()
                    .partition(|(id, _)| state.tracker.is_running(id));
                state.restores = running;
                if !done.is_empty() {
                    for (id, restore) in done {
                        self.put_back(&id, restore, context);
                    }
                    self.save_play(context);
                }
                self.tidy_view();
                self.rewatch(context);
                self.play.token += 1;
                tick(self.play.token, context);
            }
            PlayMessage::Loaded(Err(error)) => {
                // A history that can't be read is never overwritten: no tracking this time.
                self.play.ready = true;
                self.error = Some(error);
                self.tidy_view();
            }
            PlayMessage::Watching(watch) => {
                self.play.watch = watch;
                context.quiet();
            }
            PlayMessage::Tick(token) => {
                context.quiet();
                let state = &mut self.play;
                if token != state.token || state.looking {
                    return;
                }
                if state.watch.is_empty() && state.tracker.open().is_empty() {
                    tick(token, context);
                    return;
                }
                state.looking = true;
                let watch = Arc::clone(&state.watch);
                context.spawn_background(move || {
                    PlayMessage::Seen(token, play::running(&watch).map_err(|e| e.to_string()))
                        .into()
                });
            }
            PlayMessage::Seen(token, seen) => self.seen(token, seen, context),
            PlayMessage::Saved(result) => {
                let state = &mut self.play;
                state.saving = false;
                match result {
                    Ok(history) => {
                        state.in_flight.clear();
                        state.save_failed = false;
                        // The file's totals, with the sessions that ended since this save began.
                        state.totals = history.games;
                        for finished in &state.unsaved {
                            play::add(&mut state.totals, finished);
                        }
                        state.remerge();
                        context.quiet();
                    }
                    Err(error) => {
                        let failed = std::mem::take(&mut state.in_flight);
                        state.unsaved.splice(0..0, failed);
                        if !state.save_failed {
                            state.save_failed = true;
                            self.error = Some(format!("Play history: {error}"));
                        }
                    }
                }
                if std::mem::take(&mut self.play.save_again) {
                    self.save_play(context);
                }
            }
            PlayMessage::ReadSteam => {
                context.quiet();
                context.spawn_background(|| {
                    PlayMessage::SteamRead(play::steam_play().unwrap_or_default()).into()
                });
            }
            PlayMessage::SteamRead(steam) => {
                self.play.steam = steam;
                self.play.remerge();
            }
            PlayMessage::ChoicesLoaded(Ok(choices)) => {
                self.play.display_choices = choices;
                self.play.choices_ready = true;
            }
            PlayMessage::ChoicesLoaded(Err(error)) => {
                self.play.choices_ready = true;
                self.error = Some(error);
            }
            PlayMessage::ChoiceSaved(Err(error)) => self.error = Some(error),
            PlayMessage::ChoiceSaved(Ok(())) => context.quiet(),
            PlayMessage::DisplaysRead(displays) => {
                self.play.displays_pending = false;
                self.play.displays = Some(displays);
            }
            PlayMessage::SetChoice(id, choice) => {
                let choices = &mut self.play.display_choices;
                if choices.get(&id) == choice.as_ref() {
                    context.quiet();
                    return;
                }
                match &choice {
                    Some(choice) => choices.insert(id.clone(), choice.clone()),
                    None => choices.remove(&id),
                };
                context.spawn_background(move || {
                    let saved = config::set_display_choice(&id, choice.as_ref())
                        .map_err(|e| format!("Display settings: {e}"));
                    PlayMessage::ChoiceSaved(saved).into()
                });
            }
            PlayMessage::DisplayApplied { id, option, result } => {
                self.display_applied(&id, option, result, context);
            }
            PlayMessage::DisplayWaited(id, token) => {
                let state = &mut self.play;
                if state.restore_tokens.get(&id) != Some(&token) || state.tracker.is_running(&id) {
                    context.quiet();
                    return;
                }
                if let Some(restore) = state.restores.remove(&id) {
                    self.put_back(&id, restore, context);
                    self.save_play(context);
                }
            }
            PlayMessage::DisplayRestored(name, result) => {
                self.displays_changed(context);
                if let Err(error) = result {
                    self.error = Some(format!(
                        "{name}'s display settings weren't put back: {error}"
                    ));
                }
            }
        }
    }

    /// The selected game's displays for its While playing card, read in the background while
    /// the Overview shows and they aren't known.
    pub(crate) fn request_displays(&mut self, context: &Ctx<Message>) {
        let state = &self.play;
        if self.page != Page::Game
            || self.tab != GameTab::Overview
            || state.displays.is_some()
            || state.displays_pending
            || self
                .selected_game()
                .is_none_or(|game| game.program.is_none())
        {
            return;
        }
        self.play.displays_pending = true;
        context.spawn_background(|| {
            let displays = pc::displays().map_err(|e| e.to_string());
            PlayMessage::DisplaysRead(displays).into()
        });
    }

    /// Play for a game with display settings: they are applied in the background first, and the
    /// game starts after (`DisplayApplied`). `Ok(false)` when there is nothing to apply, and
    /// `Err` with why when they can't be applied now: the game starts at once either way.
    pub(crate) fn apply_display_first(
        &mut self,
        id: &str,
        option: Option<usize>,
        context: &Ctx<Message>,
    ) -> Result<bool, String> {
        let state = &self.play;
        let Some(choice) = state.display_choices.get(id).cloned() else {
            return Ok(false);
        };
        let Some(game) = self.games.iter().find(|game| game.id == id) else {
            return Ok(false);
        };
        // Without its program the game's end can't be seen; already changed for this game (it
        // was started and hasn't shown yet): as it is.
        if game.program.is_none() || !state.ready || state.restores.contains_key(id) {
            return Ok(false);
        }
        let name = &game.name;
        if let Some(other) = state.restores.keys().chain(&state.applying).next() {
            return Err(format!(
                "{name} started without its display settings: {}'s are still on.",
                self.game_name(other)
            ));
        }
        if self.my_pc.trial.is_some() {
            return Err(format!(
                "{name} started without its display settings: My PC is trying a display mode."
            ));
        }
        self.play.applying.insert(id.to_owned());
        self.launching = Some(id.to_owned());
        let id = id.to_owned();
        context.spawn_background(move || {
            let result = display::apply(&choice).map_err(|e| e.to_string());
            PlayMessage::DisplayApplied { id, option, result }.into()
        });
        Ok(true)
    }

    /// A game's display settings are on (or failed): keep what changed, start the game, and put
    /// them back if it didn't start.
    fn display_applied(
        &mut self,
        id: &str,
        option: Option<usize>,
        result: Result<Option<DisplayRestore>, String>,
        context: &Ctx<Message>,
    ) {
        self.play.applying.remove(id);
        if self.launching.as_deref() == Some(id) {
            self.launching = None;
        }
        let failed = match result {
            Ok(Some(restore)) => {
                let state = &mut self.play;
                state.changes += 1;
                state.restore_tokens.insert(id.to_owned(), state.changes);
                state.restores.insert(id.to_owned(), restore);
                context.after(
                    DISPLAY_WAIT,
                    PlayMessage::DisplayWaited(id.to_owned(), state.changes).into(),
                );
                self.save_play(context);
                self.displays_changed(context);
                None
            }
            Ok(None) => None,
            Err(error) => Some(format!(
                "{} started without its display settings: {error}",
                self.game_name(id)
            )),
        };
        if self.start_game(id, option, context) {
            if failed.is_some() {
                self.error = failed;
            }
        } else if let Some(restore) = self.play.restores.remove(id) {
            self.put_back(id, restore, context);
            self.save_play(context);
        }
    }

    /// Puts back what Play changed on a display for a game, in the background.
    fn put_back(&mut self, id: &str, restore: DisplayRestore, context: &Ctx<Message>) {
        self.play.restore_tokens.remove(id);
        let name = self.game_name(id);
        context.spawn_background(move || {
            let result = display::restore(&restore).map_err(|e| e.to_string());
            PlayMessage::DisplayRestored(name, result).into()
        });
    }

    /// A display changed: the card and My PC read them again.
    fn displays_changed(&mut self, context: &Ctx<Message>) {
        self.play.forget_displays();
        if self.page == Page::MyPc {
            self.handle_my_pc(crate::my_pc::MyPcMessage::Refresh, context);
        }
    }

    /// A game's name, or its id when it is gone.
    pub(crate) fn game_name(&self, id: &str) -> String {
        self.games
            .iter()
            .find(|game| game.id == id)
            .map_or_else(|| id.to_owned(), |game| game.name.clone())
    }

    /// Follows the sessions with what a look saw, and schedules the next look.
    fn seen(&mut self, token: u64, seen: Result<Vec<Seen>, String>, context: &Ctx<Message>) {
        self.play.looking = false;
        if token != self.play.token {
            context.quiet();
            return;
        }
        tick(token, context);
        // A failed look ends no session.
        let Ok(seen) = seen else {
            context.quiet();
            return;
        };
        let now = local_time::now();
        let events = self.play.tracker.observe(now, &seen);
        let mut steam_ended = false;
        for event in &events {
            match event {
                Event::Started(id) => {
                    if self.launching.as_deref() == Some(id.as_str()) {
                        self.launching = None;
                    }
                }
                Event::Ended(finished) => {
                    play::add(&mut self.play.totals, finished);
                    self.play.unsaved.push(finished.clone());
                    steam_ended |= finished.game.starts_with("steam:");
                    // Its display settings go back now (saved below, with the session).
                    if let Some(restore) = self.play.restores.remove(&finished.game) {
                        self.put_back(&finished.game, restore, context);
                    }
                    // And its saves are backed up, if they changed (`save_backups.rs`).
                    if finished.counts() {
                        self.back_up_after_session(&finished.game, context);
                    }
                }
            }
        }
        if !events.is_empty() {
            self.play.remerge();
        }
        if steam_ended {
            context.after(STEAM_AFTER, PlayMessage::ReadSteam.into());
        }
        let due = !events.is_empty()
            || (!self.play.tracker.open().is_empty()
                && now.saturating_sub(self.play.last_saved) >= SAVE_EVERY);
        if due {
            self.save_play(context);
        }
        // The page shows the selected game's time going up at each save while it runs.
        let selected_running = self
            .selected
            .as_deref()
            .is_some_and(|id| self.play.is_running(id));
        if events.is_empty() && !(due && selected_running) {
            context.quiet();
        }
    }

    /// Saves the open sessions and the finished ones in the background, one save at a time.
    fn save_play(&mut self, context: &Ctx<Message>) {
        let state = &mut self.play;
        state.last_saved = local_time::now();
        if state.saving {
            state.save_again = true;
            return;
        }
        state.saving = true;
        state.in_flight = std::mem::take(&mut state.unsaved);
        let open = state.tracker.open().clone();
        let finished = state.in_flight.clone();
        let displays = state.restores.clone();
        context.spawn_background(move || {
            let changes = play::Changes {
                open: &open,
                finished: &finished,
                displays: &displays,
            };
            PlayMessage::Saved(play::save(&changes).map_err(|e| e.to_string())).into()
        });
    }
}

/// A play time: "Under a minute", "45 min", "12 h 30 min", "120 h" (from 100 hours, the hours
/// alone).
pub(crate) fn play_time_label(seconds: u64) -> String {
    let minutes = seconds / 60;
    let (hours, rest) = (minutes / 60, minutes % 60);
    match minutes {
        0 => "Under a minute".to_owned(),
        1..=59 => format!("{minutes} min"),
        _ if hours >= 100 || rest == 0 => format!("{hours} h"),
        _ => format!("{hours} h {rest} min"),
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A day as a list says it, seen from `today`: "Today", "Yesterday", "3 days ago" (this week),
/// "19 Nov" (this year), "19 Nov 2025".
pub(crate) fn day_label(at: LocalTime, today: LocalTime) -> String {
    let days = today.day_number() - at.day_number();
    let month = MONTHS[usize::from(at.month.clamp(1, 12)) - 1];
    match days {
        ..=0 => "Today".to_owned(),
        1 => "Yesterday".to_owned(),
        2..=6 => format!("{days} days ago"),
        _ if at.year == today.year => format!("{} {month}", at.day),
        _ => format!("{} {month} {}", at.day, at.year),
    }
}

/// When a game was last played (seconds since 1970), on this PC's clock; empty when Windows
/// can't say.
pub(crate) fn last_played_label(at: u64, now: u64) -> String {
    match (local_time::local(at), local_time::local(now)) {
        (Some(at), Some(today)) => day_label(at, today),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(year: u16, month: u8, day: u8) -> LocalTime {
        LocalTime {
            year,
            month,
            day,
            hour: 12,
            minute: 0,
            second: 0,
        }
    }

    #[test]
    fn play_time_labels() {
        assert_eq!(play_time_label(0), "Under a minute");
        assert_eq!(play_time_label(59), "Under a minute");
        assert_eq!(play_time_label(45 * 60 + 30), "45 min");
        assert_eq!(play_time_label(60 * 60), "1 h");
        assert_eq!(play_time_label(12 * 3600 + 30 * 60), "12 h 30 min");
        assert_eq!(play_time_label(120 * 3600 + 59 * 60), "120 h");
    }

    #[test]
    fn day_labels() {
        let today = day(2026, 11, 19);
        assert_eq!(day_label(day(2026, 11, 19), today), "Today");
        assert_eq!(day_label(day(2026, 11, 18), today), "Yesterday");
        assert_eq!(day_label(day(2026, 11, 16), today), "3 days ago");
        assert_eq!(day_label(day(2026, 11, 12), today), "12 Nov");
        assert_eq!(day_label(day(2026, 1, 1), today), "1 Jan");
        assert_eq!(day_label(day(2025, 12, 31), today), "31 Dec 2025");
        // Across a year's end, within a week: still days.
        assert_eq!(day_label(day(2025, 12, 31), day(2026, 1, 2)), "2 days ago");
        // A clock set back: today.
        assert_eq!(day_label(day(2026, 11, 20), today), "Today");
    }
}
