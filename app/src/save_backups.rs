//! Save backups in the window (`gm_core::save_backups`; the card is `play_cards.rs`): each game's
//! backups, Back up now, Restore, and the backup after each session.
//!
//! - **Listing**: the selected game's backups are read in the background while its Overview
//!   shows (`request_save_backups`), and again after each change and each time it is selected.
//! - **The saves folder** is the one the Saves button opens (`game_folders`, from PCGamingWiki):
//!   without it nothing can be backed up, and the card says why.
//! - **One job at a time** (`working`): Back up now or a restore, in the background; the error
//!   bar says what failed. Restore asks first (`Message::AskFirst`) and waits while the game runs.
//! - **After each session**: when a counted session ends (`play.rs`), the game's saves folder is
//!   looked up and backed up if it changed since the newest backup, unless the game's switch is
//!   off. A game without a saves folder on this PC is left alone, quietly.

use crate::mods_view::Confirm;
use crate::shell::ctx::Ctx;
use crate::{GameManager, GameTab, Message, Page};
use gm_core::game_data::{self, Folder};
use gm_core::save_backups::{self, Backup, Backups};
use gm_core::{Error, Game};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Default)]
pub(crate) struct SaveBackupsState {
    /// Each game's backups once listed, or why they couldn't be.
    pub by_game: HashMap<String, Result<Backups, String>>,
    pending: HashSet<String>,
    /// The game whose Back up now or restore runs.
    pub working: Option<String>,
}

#[derive(Clone)]
pub(crate) enum SaveBackupMessage {
    Listed(String, Result<Backups, String>),
    BackUpNow(String),
    /// Restore said yes to: the game and the backup.
    Restore(String, PathBuf),
    /// Back up now or a restore ended.
    Done(String, Result<(), String>),
    SetAuto(String, bool),
    AutoSet(String, Result<(), String>),
    OpenFolder(String),
    /// The backup after a session: made, not needed, or what failed.
    AfterSession(String, Result<Option<Backup>, String>),
}

impl From<SaveBackupMessage> for Message {
    fn from(message: SaveBackupMessage) -> Self {
        Message::SaveBackup(message)
    }
}

impl GameManager {
    /// The game's saves folder on this PC, once looked up and found.
    pub(crate) fn saves_folder(&self, id: &str) -> Option<&Path> {
        match self.game_folders.get(id)? {
            Ok(folders) => match &folders.saves {
                Folder::Found(path) => Some(path),
                _ => None,
            },
            Err(_) => None,
        }
    }

    /// The selected game's backups, read in the background while its Overview shows.
    pub(crate) fn request_save_backups(&mut self, context: &Ctx<Message>) {
        if self.page != Page::Game || self.tab != GameTab::Overview {
            return;
        }
        let Some(game) = self.selected_game().cloned() else {
            return;
        };
        let state = &mut self.save_backups;
        if state.by_game.contains_key(&game.id) || state.pending.contains(&game.id) {
            return;
        }
        state.pending.insert(game.id.clone());
        context.spawn_background(move || {
            let listed = save_backups::list(&game).map_err(|e| e.to_string());
            SaveBackupMessage::Listed(game.id, listed).into()
        });
    }

    /// The game's backups are read again when next shown.
    pub(crate) fn forget_save_backups(&mut self, id: &str) {
        if !self.save_backups.pending.contains(id) {
            self.save_backups.by_game.remove(id);
        }
    }

    pub(crate) fn handle_save_backups(
        &mut self,
        message: SaveBackupMessage,
        context: &Ctx<Message>,
    ) {
        match message {
            SaveBackupMessage::Listed(id, listed) => {
                self.save_backups.pending.remove(&id);
                self.save_backups.by_game.insert(id, listed);
            }
            SaveBackupMessage::BackUpNow(id) => {
                let Some((game, saves)) = self.backup_target(&id) else {
                    return;
                };
                self.save_backups.working = Some(id.clone());
                context.spawn_background(move || {
                    let done = save_backups::back_up(&game, &saves)
                        .map(|_| ())
                        .map_err(|e| e.to_string());
                    SaveBackupMessage::Done(id, done).into()
                });
            }
            SaveBackupMessage::Restore(id, backup) => {
                if self.play.is_running(&id) {
                    return;
                }
                let Some((game, saves)) = self.backup_target(&id) else {
                    return;
                };
                self.save_backups.working = Some(id.clone());
                context.spawn_background(move || {
                    let done =
                        save_backups::restore(&game, &saves, &backup).map_err(|e| e.to_string());
                    SaveBackupMessage::Done(id, done).into()
                });
            }
            SaveBackupMessage::Done(id, done) => {
                self.save_backups.working = None;
                self.forget_save_backups(&id);
                if let Err(error) = done {
                    self.error = Some(format!("Save backups of {}: {error}", self.game_name(&id)));
                }
            }
            SaveBackupMessage::SetAuto(id, auto) => {
                let Some(game) = self.games.iter().find(|game| game.id == id).cloned() else {
                    return;
                };
                if let Some(Ok(backups)) = self.save_backups.by_game.get_mut(&id) {
                    backups.auto = auto;
                }
                context.spawn_background(move || {
                    let set = save_backups::set_auto(&game, auto).map_err(|e| e.to_string());
                    SaveBackupMessage::AutoSet(id, set).into()
                });
            }
            SaveBackupMessage::AutoSet(_, Ok(())) => context.quiet(),
            SaveBackupMessage::AutoSet(id, Err(error)) => {
                self.forget_save_backups(&id);
                self.error = Some(format!("Save backups of {}: {error}", self.game_name(&id)));
            }
            SaveBackupMessage::OpenFolder(id) => {
                if let Some(Ok(backups)) = self.save_backups.by_game.get(&id) {
                    self.error = gm_core::open_folder(&backups.folder, None)
                        .err()
                        .map(|e| e.to_string());
                }
            }
            SaveBackupMessage::AfterSession(id, Ok(made)) => {
                if made.is_some() {
                    self.forget_save_backups(&id);
                } else {
                    context.quiet();
                }
            }
            SaveBackupMessage::AfterSession(id, Err(error)) => {
                self.forget_save_backups(&id);
                self.error = Some(format!(
                    "{}'s saves weren't backed up after playing: {error}",
                    self.game_name(&id)
                ));
            }
        }
    }

    /// The game and its saves folder, when nothing else is being backed up or restored.
    fn backup_target(&self, id: &str) -> Option<(Game, PathBuf)> {
        if self.save_backups.working.is_some() {
            return None;
        }
        let game = self.games.iter().find(|game| game.id == id)?.clone();
        let saves = self.saves_folder(id)?.to_path_buf();
        Some((game, saves))
    }

    /// After a counted session: the game's saves backed up in the background if they changed,
    /// unless its backups after each session are off or it has no saves folder on this PC.
    pub(crate) fn back_up_after_session(&mut self, id: &str, context: &Ctx<Message>) {
        let Some(game) = self.games.iter().find(|game| game.id == id).cloned() else {
            return;
        };
        context.spawn_background(move || {
            let made = after_session(&game).map_err(|e| e.to_string());
            SaveBackupMessage::AfterSession(game.id, made).into()
        });
    }

    /// Whether Back up now and Restore can start for the game.
    pub(crate) fn can_back_up(&self, id: &str) -> bool {
        self.save_backups.working.is_none() && self.saves_folder(id).is_some()
    }

    /// The question Restore asks first.
    pub(crate) fn ask_restore(&self, game: &Game, backup: &Backup, when: &str) -> Message {
        let saves = self
            .saves_folder(&game.id)
            .map_or_default(|path| path.display().to_string());
        Message::AskFirst(Confirm {
            title: format!("Restore {}'s saves from {when}?", game.name),
            text: format!(
                "The saves folder ({saves}) is replaced by this backup. The saves as they are now \
                 are backed up first, as \"before restore\". A store that keeps the game's saves \
                 in the cloud (Steam Cloud and others) may bring its own copy back when the game \
                 next starts."
            ),
            action: "Restore",
            then: Box::new(SaveBackupMessage::Restore(game.id.clone(), backup.file.clone()).into()),
        })
    }
}

/// The backup after a session, off the UI thread: the switch first (so a game with it off isn't
/// looked up), then the saves folder, then the backup if they changed.
fn after_session(game: &Game) -> gm_core::Result<Option<Backup>> {
    if !save_backups::backs_up_after_sessions(game)? {
        return Ok(None);
    }
    // No saves folder known on this PC (or the wiki unreachable): nothing to do, quietly.
    let Ok(folders) = game_data::folders(game) else {
        return Ok(None);
    };
    let Folder::Found(saves) = folders.saves else {
        return Ok(None);
    };
    match save_backups::back_up_after_session(game, &saves) {
        Err(Error::EmptyFolder(_) | Error::NotFound(_)) => Ok(None),
        other => other,
    }
}
