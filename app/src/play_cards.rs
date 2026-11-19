//! The game page's Overview cards about playing: **While playing**, the display settings Play
//! applies and puts back when the game ends (`play.rs`, `gm_core::play::display`).
//!
//! - **Display**: Don't change, or one of the connected displays (the main one marked; names that
//!   repeat numbered). A chosen display that isn't connected stays listed as "(not connected)".
//! - **HDR** (Leave as it is, On, Off), only for a display with HDR; **Resolution** (Leave as it
//!   is, or one of the display's) and, with one chosen, its **Refresh rate**, as My PC lists them
//!   (`my_pc::sizes`, `rates`, `mode_for_size`). A choice the display no longer offers stays
//!   listed as "(not available)".
//! - **Needs the game's program**: without it Game Manager can't see the game end, so the card
//!   only says to choose it.
//!
//! What each drop-down item picks is worked out here (`display_items`, `hdr_items`, `size_items`,
//! `rate_items`, tested), and the choice is sent whole (`PlayMessage::SetChoice`).
//!
//! And **Save backups** (`save_backups.rs`, `gm_core::save_backups`): Back up now and Open folder
//! in its header, the saves folder backed up (or why there is none on this PC), the After each
//! session switch, then a row per backup, newest first ("Today, 21:05", its size, "before
//! restore" for one a restore made), each with Restore, which asks first and waits while the game
//! runs.

use crate::my_pc::{mode_for_size, rates, sizes};
use crate::play::{PlayMessage, day_label};
use crate::save_backups::SaveBackupMessage;
use crate::ui::el::{El, col, dropdown, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    busy_width, button, card, card_header, card_header_with, folder_button, hint, setting_row,
    switch, working, working_button,
};
use crate::{GameManager, Message};
use gm_core::Game;
use gm_core::config::DisplayChoice;
use gm_core::game_data::Folder;
use gm_core::local_time::{self, LocalTime};
use gm_core::pc::{DisplayMode, format_bytes};
use gm_core::play::display::{find_display, pick};
use gm_core::save_backups::{Backup, KEEP};

/// A drop-down's labels, what each item picks, and the one selected.
#[derive(Debug, PartialEq)]
pub(crate) struct Items<T> {
    pub labels: Vec<String>,
    pub picks: Vec<T>,
    pub selected: usize,
}

/// A connected display, as the Display drop-down lists it.
pub(crate) struct Connected<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub primary: bool,
}

const LEAVE: &str = "Leave as it is";

/// The Display drop-down: Don't change, then each connected display; picking the chosen one keeps
/// its HDR and mode, another starts with both left as they are.
pub(crate) fn display_items(
    connected: &[Connected],
    choice: Option<&DisplayChoice>,
) -> Items<Option<DisplayChoice>> {
    let found = choice.and_then(|choice| {
        pick(
            connected.iter().map(|display| (display.id, display.name)),
            &choice.display,
            &choice.name,
        )
    });
    let mut labels = vec!["Don't change".to_owned()];
    let mut picks = vec![None];
    for (index, display) in connected.iter().enumerate() {
        let named = |d: &&Connected| d.name == display.name;
        let mut label = display.name.to_owned();
        if connected.iter().filter(named).count() > 1 {
            let nth = connected[..=index].iter().filter(named).count();
            label = format!("{label} {nth}");
        }
        if display.primary {
            label.push_str(" (main)");
        }
        labels.push(label);
        picks.push(Some(match choice {
            Some(choice) if found == Some(index) => choice.clone(),
            _ => DisplayChoice {
                display: display.id.to_owned(),
                name: display.name.to_owned(),
                hdr: None,
                mode: None,
            },
        }));
    }
    let selected = match (choice, found) {
        (None, _) => 0,
        (Some(_), Some(index)) => index + 1,
        (Some(choice), None) => {
            labels.push(format!("{} (not connected)", choice.name));
            picks.push(Some(choice.clone()));
            labels.len() - 1
        }
    };
    Items {
        labels,
        picks,
        selected,
    }
}

/// The HDR drop-down: Leave as it is, On, Off.
pub(crate) fn hdr_items(choice: &DisplayChoice) -> Items<DisplayChoice> {
    let values = [None, Some(true), Some(false)];
    Items {
        labels: vec![LEAVE.to_owned(), "On".to_owned(), "Off".to_owned()],
        picks: values
            .iter()
            .map(|&hdr| DisplayChoice {
                hdr,
                ..choice.clone()
            })
            .collect(),
        selected: values
            .iter()
            .position(|&hdr| hdr == choice.hdr)
            .unwrap_or_default(),
    }
}

/// The Resolution drop-down: Leave as it is, then the display's sizes, largest first. A new size
/// keeps the chosen refresh rate where it can (else the display's current one, else its
/// fastest).
pub(crate) fn size_items(
    modes: &[DisplayMode],
    current_hz: u32,
    choice: &DisplayChoice,
) -> Items<DisplayChoice> {
    let sizes = sizes(modes);
    let hz = choice.mode.map_or(current_hz, |mode| mode.hz);
    let with = |mode: Option<DisplayMode>| DisplayChoice {
        mode,
        ..choice.clone()
    };
    let mut labels = vec![LEAVE.to_owned()];
    let mut picks = vec![with(None)];
    for &(width, height) in &sizes {
        labels.push(format!("{width} × {height}"));
        picks.push(with(mode_for_size(modes, (width, height), hz)));
    }
    let selected = match choice.mode {
        None => 0,
        Some(mode) => match sizes
            .iter()
            .position(|&size| size == (mode.width, mode.height))
        {
            Some(index) => index + 1,
            None => {
                labels.push(format!("{} × {} (not available)", mode.width, mode.height));
                picks.push(choice.clone());
                labels.len() - 1
            }
        },
    };
    Items {
        labels,
        picks,
        selected,
    }
}

/// The Refresh rate drop-down for the chosen resolution, fastest first; `None` while the
/// resolution is left as it is.
pub(crate) fn rate_items(
    modes: &[DisplayMode],
    choice: &DisplayChoice,
) -> Option<Items<DisplayChoice>> {
    let mode = choice.mode?;
    let rates = rates(modes, (mode.width, mode.height));
    let mut labels: Vec<String> = rates.iter().map(|hz| format!("{hz} Hz")).collect();
    let mut picks: Vec<DisplayChoice> = rates
        .iter()
        .map(|&hz| DisplayChoice {
            mode: Some(DisplayMode { hz, ..mode }),
            ..choice.clone()
        })
        .collect();
    let selected = match rates.iter().position(|&hz| hz == mode.hz) {
        Some(index) => index,
        None => {
            labels.push(format!("{} Hz (not available)", mode.hz));
            picks.push(choice.clone());
            labels.len() - 1
        }
    };
    Some(Items {
        labels,
        picks,
        selected,
    })
}

impl GameManager {
    /// The While playing card: the display settings Play applies for `game`.
    pub(crate) fn while_playing_card(&self, game: &Game) -> El<Message> {
        let mut lines = vec![card_header(
            g::MONITOR,
            theme::cyan(),
            "While playing",
            "Display settings Play applies, put back when the game ends",
        )];
        if game.program.is_none() {
            lines.push(hint(
                "Choose the game's program in Game settings first: Game Manager watches it to know \
                 when the game ends.",
            ));
            return card(lines);
        }
        let displays = match &self.play.displays {
            None => {
                lines.push(working("Reading the displays…"));
                return card(lines);
            }
            Some(Err(error)) => {
                lines.push(hint(format!("The displays couldn't be read: {error}")));
                return card(lines);
            }
            Some(Ok(displays)) => displays,
        };
        let choice = self.play.display_choices.get(&game.id);
        let connected: Vec<Connected> = displays
            .iter()
            .map(|display| Connected {
                id: &display.id,
                name: &display.name,
                primary: display.primary,
            })
            .collect();
        let busy = self.play.is_applying(&game.id);
        let id = game.id.clone();
        let current = choice.cloned();
        // A drop-down whose items each pick a whole choice.
        let choose = |key: &str, name: &str, items: Items<Option<DisplayChoice>>| {
            let id = id.clone();
            let current = current.clone();
            let picks = items.picks;
            dropdown(
                format!("while-playing:{key}"),
                items.labels,
                Some(items.selected),
                move |index| {
                    let picked = picks.get(index).cloned().unwrap_or_else(|| current.clone());
                    PlayMessage::SetChoice(id.clone(), picked).into()
                },
            )
            .name(name)
            .enabled(!busy)
            .width(theme::SETTING_CONTROL_WIDTH)
        };
        let some = |items: Items<DisplayChoice>| Items {
            labels: items.labels,
            picks: items.picks.into_iter().map(Some).collect(),
            selected: items.selected,
        };
        lines.push(setting_row(
            "Display",
            "Changed when Play here starts the game, and put back when it ends. Starting it from \
             its store or a shortcut changes nothing.",
            Some(choose(
                "display",
                "Display while playing",
                display_items(&connected, choice),
            )),
        ));
        let Some(choice) = choice else {
            return card(lines);
        };
        let Some(display) = find_display(displays, &choice.display, &choice.name) else {
            lines.push(hint(format!(
                "{} isn't connected now: Play starts the game as the displays are.",
                choice.name
            )));
            return card(lines);
        };
        if display.hdr_supported {
            lines.push(setting_row(
                "HDR",
                "",
                Some(choose("hdr", "HDR while playing", some(hdr_items(choice)))),
            ));
        } else {
            lines.push(setting_row(
                "HDR",
                "This display does not support HDR.",
                None,
            ));
        }
        if !display.modes.is_empty() {
            let current_hz = display.current.map_or(0, |mode| mode.hz);
            lines.push(setting_row(
                "Resolution",
                "",
                Some(choose(
                    "resolution",
                    "Resolution while playing",
                    some(size_items(&display.modes, current_hz, choice)),
                )),
            ));
            if let Some(items) = rate_items(&display.modes, choice) {
                lines.push(setting_row(
                    "Refresh rate",
                    "",
                    Some(choose("refresh", "Refresh rate while playing", some(items))),
                ));
            }
        }
        card(lines)
    }
}

/// When a backup was taken, as its row says it: "Today, 21:05", "12 Nov, 08:30".
pub(crate) fn backup_label(taken: LocalTime, today: Option<LocalTime>) -> String {
    let time = format!("{:02}:{:02}", taken.hour, taken.minute);
    match today {
        Some(today) => format!("{}, {time}", day_label(taken, today)),
        None => format!("{}-{:02}-{:02} {time}", taken.year, taken.month, taken.day),
    }
}

/// "No backups yet", "1 backup", "3 backups".
fn backup_count(count: usize) -> String {
    match count {
        0 => "No backups yet".to_owned(),
        1 => "1 backup".to_owned(),
        _ => format!("{count} backups"),
    }
}

impl GameManager {
    /// The Save backups card: the game's backups, Back up now, and Restore.
    pub(crate) fn save_backups_card(&self, game: &Game) -> El<Message> {
        let id = &game.id;
        let listed = self.save_backups.by_game.get(id);
        let can = self.can_back_up(id);
        let back_up = if self.save_backups.working.as_deref() == Some(id.as_str()) {
            working_button()
        } else {
            busy_width(button("Back up now"))
                .enabled(can)
                .name(format!("Back up {}'s saves now", game.name))
                .on_click(SaveBackupMessage::BackUpNow(id.clone()).into())
        };
        let open = folder_button("Open folder")
            .enabled(matches!(listed, Some(Ok(backups)) if backups.folder.is_dir()))
            .name(format!("Open the save backups folder of {}", game.name))
            .on_click(SaveBackupMessage::OpenFolder(id.clone()).into());
        let status = match listed {
            Some(Ok(backups)) => backup_count(backups.list.len()),
            _ => String::new(),
        };
        let mut lines = vec![card_header_with(
            g::SAVE,
            theme::green(),
            "Save backups",
            (!status.is_empty()).then(|| hint(status)),
            vec![back_up, open],
        )];
        // Which folder is backed up, or why none can be.
        lines.push(hint(match self.game_folders.get(id) {
            None => "Looking for the game's saves folder…".to_owned(),
            Some(Err(error)) => format!("The saves folder couldn't be looked up: {error}"),
            Some(Ok(folders)) => match &folders.saves {
                Folder::Found(path) => format!("Backs up {}.", path.display()),
                Folder::Missing(path) => {
                    format!("The saves folder isn't on this PC yet ({path}): play the game first.")
                }
                Folder::Registry(key) => format!(
                    "This game keeps its saves in the registry ({key}), which Game Manager \
                     doesn't back up."
                ),
                Folder::Unlisted => "PCGamingWiki lists no saves folder for this game, so Game \
                                     Manager doesn't know what to back up."
                    .to_owned(),
            },
        }));
        let backups = match listed {
            None => {
                lines.push(working("Reading the backups…"));
                return card(lines);
            }
            Some(Err(error)) => {
                lines.push(hint(format!("The backups couldn't be read: {error}")));
                return card(lines);
            }
            Some(Ok(backups)) => backups,
        };
        lines.push(setting_row(
            "After each session",
            &format!(
                "When the game ends, its saves are backed up if they changed since the last \
                 backup. The newest {KEEP} are kept."
            ),
            Some(switch(
                format!("Back up {}'s saves after each session", game.name),
                backups.auto,
                SaveBackupMessage::SetAuto(id.clone(), !backups.auto).into(),
            )),
        ));
        let running = self.play.is_running(id);
        let today = local_time::local(local_time::now());
        for backup in &backups.list {
            lines.push(self.backup_row(game, backup, today, can, running));
        }
        card(lines)
    }

    /// A backup's row: when, its size, and Restore.
    fn backup_row(
        &self,
        game: &Game,
        backup: &Backup,
        today: Option<LocalTime>,
        can: bool,
        running: bool,
    ) -> El<Message> {
        let when = backup_label(backup.taken, today);
        let mut detail = format_bytes(backup.size);
        if backup.before_restore {
            detail.push_str("  ·  before restore");
        }
        let mut restore = button("Restore")
            .enabled(can && !running)
            .name(format!("Restore {}'s saves from {when}", game.name))
            .on_click(self.ask_restore(game, backup, &when));
        if running {
            restore = restore.tooltip(format!("{} is running", game.name));
        }
        row([
            col([text(when), hint(detail)])
                .gap(theme::space::XXS)
                .fill_width(),
            restore,
        ])
        .gap(theme::space::S)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_say_when_and_how_many() {
        let at = |day: u8| LocalTime {
            year: 2026,
            month: 11,
            day,
            hour: 8,
            minute: 5,
            second: 59,
        };
        assert_eq!(backup_label(at(19), Some(at(19))), "Today, 08:05");
        assert_eq!(backup_label(at(12), Some(at(19))), "12 Nov, 08:05");
        assert_eq!(backup_label(at(12), None), "2026-11-12 08:05");
        assert_eq!(backup_count(0), "No backups yet");
        assert_eq!(backup_count(1), "1 backup");
        assert_eq!(backup_count(10), "10 backups");
    }

    fn mode(width: u32, height: u32, hz: u32) -> DisplayMode {
        DisplayMode { width, height, hz }
    }

    fn tv() -> DisplayChoice {
        DisplayChoice {
            display: "tv".into(),
            name: "LG TV".into(),
            hdr: Some(true),
            mode: Some(mode(3840, 2160, 120)),
        }
    }

    fn connected() -> Vec<Connected<'static>> {
        vec![
            Connected {
                id: "main",
                name: "Dell",
                primary: true,
            },
            Connected {
                id: "tv",
                name: "LG TV",
                primary: false,
            },
            Connected {
                id: "side",
                name: "Dell",
                primary: false,
            },
        ]
    }

    #[test]
    fn the_display_list_keeps_the_chosen_display_and_its_settings() {
        let none = display_items(&connected(), None);
        assert_eq!(
            none.labels,
            ["Don't change", "Dell 1 (main)", "LG TV", "Dell 2"]
        );
        assert_eq!(none.selected, 0);
        assert_eq!(none.picks[0], None);
        // Another display starts with HDR and the mode left as they are.
        assert_eq!(
            none.picks[3],
            Some(DisplayChoice {
                display: "side".into(),
                name: "Dell".into(),
                hdr: None,
                mode: None,
            })
        );
        // The chosen one is selected, and picking it again keeps its settings.
        let chosen = display_items(&connected(), Some(&tv()));
        assert_eq!(chosen.selected, 2);
        assert_eq!(chosen.picks[2], Some(tv()));
        // Not connected: listed as such, and still selected.
        let away = display_items(&connected()[..1], Some(&tv()));
        assert_eq!(away.labels.last().unwrap(), "LG TV (not connected)");
        assert_eq!(away.selected, away.labels.len() - 1);
        assert_eq!(away.picks[away.selected], Some(tv()));
    }

    #[test]
    fn hdr_and_modes_pick_whole_choices() {
        let hdr = hdr_items(&tv());
        assert_eq!(hdr.selected, 1);
        assert_eq!(hdr.picks[0].hdr, None);
        assert_eq!(hdr.picks[2].hdr, Some(false));
        assert_eq!(hdr.picks[2].mode, tv().mode);

        let modes = vec![
            mode(3840, 2160, 120),
            mode(3840, 2160, 60),
            mode(1920, 1080, 120),
            mode(1920, 1080, 60),
            mode(1280, 720, 60),
        ];
        let sizes = size_items(&modes, 60, &tv());
        assert_eq!(
            sizes.labels,
            [LEAVE, "3840 × 2160", "1920 × 1080", "1280 × 720"]
        );
        assert_eq!(sizes.selected, 1);
        assert_eq!(sizes.picks[0].mode, None);
        // A new size keeps the chosen 120 Hz where it has it, else its fastest.
        assert_eq!(sizes.picks[2].mode, Some(mode(1920, 1080, 120)));
        assert_eq!(sizes.picks[3].mode, Some(mode(1280, 720, 60)));
        // With the size left alone, a pick starts from the display's current rate.
        let leave = DisplayChoice { mode: None, ..tv() };
        assert_eq!(
            size_items(&modes, 60, &leave).picks[2].mode,
            Some(mode(1920, 1080, 60))
        );
        assert!(rate_items(&modes, &leave).is_none());

        let rates = rate_items(&modes, &tv()).unwrap();
        assert_eq!(rates.labels, ["120 Hz", "60 Hz"]);
        assert_eq!(rates.selected, 0);
        assert_eq!(rates.picks[1].mode, Some(mode(3840, 2160, 60)));

        // A mode the display no longer offers stays listed.
        let gone = DisplayChoice {
            mode: Some(mode(2560, 1440, 144)),
            ..tv()
        };
        let sizes = size_items(&modes, 60, &gone);
        assert_eq!(sizes.labels[sizes.selected], "2560 × 1440 (not available)");
        let rates = rate_items(&modes, &gone).unwrap();
        assert_eq!(rates.labels[rates.selected], "144 Hz (not available)");
    }
}
