//! NVIDIA's driver-level DLSS overrides (`gm_core::mods::dlss_override`): the game page's NVIDIA
//! overrides tab, right of Upscalers.
//!
//! - **The tab** is listed only with an NVIDIA driver: the driver's version and its own DLLs'
//!   versions are read once, in the background, at start (`request_driver`). A driver that
//!   failed to read shows the tab with the error.
//! - **Its header card** says how many overrides are set, and when the game's folder has no
//!   NVIDIA DLL that only Smooth Motion works then (from the game's DLL scan, which runs for this
//!   tab too).
//! - **The draft** (`UpscalerState::overrides`) is made when the tab shows a game it wasn't made
//!   for (`request_overrides`, from `update`), from what `mods.json` holds; the game's driver
//!   profile is read in the background (`OverridesLoaded`, matched to the game) and the
//!   drop-downs wait for it. Selecting another game drops an unsaved draft, as Game settings
//!   does.
//! - **The form**: each group of the catalog has a heading, and each setting the driver knows a
//!   drop-down whose first item is "Original: …" (as in DXVK settings). Reset to original sets
//!   every one back to it; Save runs `dlss_override::save` as a mod job (`ModKind::Overrides`),
//!   after which the draft is made again from the profile.
//! - **Changed elsewhere**: the NVIDIA app writes the same settings (resetting its DLSS override
//!   writes them all as Off or N/A), and may delete a profile. When the profile no longer holds
//!   what Game Manager set (`OverrideState::changed_elsewhere`), a warning names each setting with
//!   its value now, and Save sets them again even when the choice is unchanged.

use crate::mods_view::{ModCard, ModDialog, ModKind, mod_card, original_label};
use crate::shell::ctx::Ctx;
use crate::ui::el::{El, col, dropdown, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    accent_button, button, card, hint, icon_text_button, working, working_button,
};
use crate::upscalers_view::UpscalerMessage;
use crate::{GameManager, GameTab, Message, Page};
use gm_core::Game;
use gm_core::mods::dlss_override::{self, Catalog, Driver, OverrideState, Setting};
use gm_core::mods::dxvk::Blocked;
use std::collections::BTreeMap;

/// The override settings and what the driver offers.
#[derive(Clone)]
pub(crate) struct OverrideLibrary {
    pub catalog: Catalog,
    pub driver: Driver,
}

/// The overrides being edited in the tab.
pub(crate) struct OverrideDraft {
    /// The game the draft was made for.
    pub game: String,
    /// The values chosen, by setting id; a setting left out is "Original".
    pub choice: BTreeMap<u32, u32>,
    /// The game's driver profile once read (`Ok(None)`: no NVIDIA driver).
    pub state: Option<Result<Option<OverrideState>, String>>,
}

impl OverrideDraft {
    fn loaded(&self) -> Option<&OverrideState> {
        match &self.state {
            Some(Ok(Some(state))) => Some(state),
            _ => None,
        }
    }
}

/// Reads the settings and the driver's facts. Blocking: NVAPI and the NGX folder.
fn load_driver() -> Result<Option<OverrideLibrary>, String> {
    let catalog = dlss_override::catalog().map_err(|e| e.to_string())?;
    let driver = dlss_override::driver(&catalog).map_err(|e| e.to_string())?;
    Ok(driver.map(|driver| OverrideLibrary { catalog, driver }))
}

/// A setting's values as the drop-down shows them, after "Original".
fn shown_values(library: &OverrideLibrary, setting: &Setting) -> Vec<(u32, String)> {
    setting.values(library.driver.dll(setting))
}

/// The overrides the profile no longer holds as Game Manager set them: the NVIDIA app or
/// another tool changed them, or deleted the profile. Nothing when all are as set.
fn changed_warning(state: &OverrideState, catalog: &Catalog) -> Option<El<Message>> {
    let changed = state.changed_elsewhere();
    if changed.is_empty() {
        return None;
    }
    let message = if state.profile.name.is_none() {
        "⚠ The driver profile Game Manager set these in is gone: the NVIDIA app or another tool \
         deleted it. Save creates it again with them; Reset to original and Save forget them."
            .to_owned()
    } else {
        let settings: Vec<String> = changed
            .iter()
            .map(|&(id, now)| {
                let (name, now) = match catalog.setting(id) {
                    Some(setting) => (
                        setting.name.clone(),
                        now.map_or_else(|| "driver default".to_owned(), |v| setting.label(v)),
                    ),
                    None => (
                        format!("0x{id:08X}"),
                        now.map_or_else(|| "driver default".to_owned(), |v| format!("0x{v:08X}")),
                    ),
                };
                format!("{name} (now {now})")
            })
            .collect();
        format!(
            "⚠ Changed since Game Manager set them, by the NVIDIA app or another tool: {}. Save \
             sets them again; Reset to original and Save put back what the profile held first.",
            settings.join(", ")
        )
    };
    Some(text(message).wrap().color(theme::AMBER))
}

impl GameManager {
    /// The settings and the driver, once read and with an NVIDIA driver.
    fn override_library(&self) -> Option<&OverrideLibrary> {
        match &self.upscalers.driver {
            Some(Ok(Some(library))) => Some(library),
            _ => None,
        }
    }

    /// Reads the settings and the driver's facts in the background, once.
    pub(crate) fn request_driver(&mut self, context: &Ctx<Message>) {
        if self.upscalers.driver.is_some() || self.upscalers.driver_pending {
            return;
        }
        self.upscalers.driver_pending = true;
        context.spawn_background(|_cancel| UpscalerMessage::DriverLoaded(load_driver()).into());
    }

    /// The NVIDIA overrides tab is listed: an NVIDIA driver was found, or reading it failed.
    pub(crate) fn nvidia_tab_shown(&self) -> bool {
        matches!(self.upscalers.driver, Some(Ok(Some(_)) | Err(_)))
    }

    /// While the NVIDIA overrides tab shows a game its draft wasn't made for: a new draft from
    /// `mods.json`, and the game's driver profile read in the background. Not while a save runs:
    /// its end makes the draft again.
    pub(crate) fn request_overrides(&mut self, context: &Ctx<Message>) {
        if self.page != Page::Game || self.tab != GameTab::Nvidia || self.mod_job.is_some() {
            return;
        }
        let (Some(game), Some(library)) = (self.selected_game(), self.override_library()) else {
            return;
        };
        if game.program.is_none() && self.mods.dlss_override(&game.id).is_none() {
            return;
        }
        if self
            .upscalers
            .overrides
            .as_ref()
            .is_some_and(|draft| draft.game == game.id)
        {
            return;
        }
        let (game, catalog) = (game.clone(), library.catalog.clone());
        let choice = self
            .mods
            .dlss_override(&game.id)
            .map(|record| record.choice.clone())
            .unwrap_or_default();
        self.upscalers.overrides = Some(OverrideDraft {
            game: game.id.clone(),
            choice,
            state: None,
        });
        context.spawn_background(move |_cancel| {
            let read = dlss_override::state(&game, &catalog).map_err(|e| e.to_string());
            UpscalerMessage::OverridesLoaded(game.id, read).into()
        });
    }

    pub(crate) fn handle_overrides(&mut self, message: UpscalerMessage, context: &Ctx<Message>) {
        match message {
            UpscalerMessage::DriverLoaded(driver) => {
                self.upscalers.driver_pending = false;
                self.upscalers.driver = Some(driver);
            }
            UpscalerMessage::ShowOverridesInfo => self.mod_dialog = Some(ModDialog::OverridesInfo),
            UpscalerMessage::SaveOverrides => {
                let (Some(game), Some(draft)) = (
                    self.selected_game().cloned(),
                    self.upscalers.overrides.take(),
                ) else {
                    return;
                };
                self.save_overrides(game, &draft, context);
                // Shown while the save runs; its end makes the draft again from the profile.
                self.upscalers.overrides = Some(draft);
            }
            UpscalerMessage::OverridesLoaded(id, read) => {
                if let Some(draft) = &mut self.upscalers.overrides
                    && draft.game == id
                {
                    // What mods.json holds now is what was saved.
                    if let Ok(Some(state)) = &read {
                        draft.choice = state.choice();
                    }
                    draft.state = Some(read);
                }
            }
            UpscalerMessage::OverrideChosen(id, Some(index)) => {
                let Some(library) = self.override_library() else {
                    return;
                };
                let Some(setting) = library.catalog.setting(id) else {
                    return;
                };
                // 0 is "Original".
                let value = index
                    .checked_sub(1)
                    .and_then(|index| shown_values(library, setting).get(index).map(|(v, _)| *v));
                if let Some(draft) = &mut self.upscalers.overrides {
                    match value {
                        Some(value) => draft.choice.insert(id, value),
                        None => draft.choice.remove(&id),
                    };
                }
            }
            UpscalerMessage::ResetOverrides => {
                if let Some(draft) = &mut self.upscalers.overrides {
                    draft.choice.clear();
                }
            }
            _ => {}
        }
    }

    /// The tab's Save: applies the choice to the game's driver profile, when it changed.
    fn save_overrides(&mut self, game: Game, draft: &OverrideDraft, context: &Ctx<Message>) {
        let Some(state) = draft.loaded().filter(|_| draft.game == game.id) else {
            return;
        };
        // Saving the same choice again sets back what another tool changed.
        let changed_elsewhere = !state.changed_elsewhere().is_empty();
        let saved = self
            .mods
            .dlss_override(&game.id)
            .map(|record| record.choice.clone())
            .unwrap_or_default();
        let Some(catalog) = self
            .override_library()
            .map(|library| library.catalog.clone())
        else {
            return;
        };
        if draft.choice == saved && !changed_elsewhere {
            return;
        }
        self.mod_job = Some((game.id.clone(), ModKind::Overrides));
        let choice = draft.choice.clone();
        context.spawn_background(move |_cancel| {
            let saved = dlss_override::save(&game, &catalog, choice);
            Message::ModJobDone(saved.map_err(|e| e.to_string()))
        });
    }

    /// The NVIDIA overrides tab: its header card, then the settings with Save and Reset to
    /// original.
    pub(crate) fn nvidia_tab(&self, game: &Game) -> El<Message> {
        let library = match &self.upscalers.driver {
            Some(Ok(Some(library))) => library,
            Some(Err(error)) => {
                return card([hint(format!(
                    "The NVIDIA driver's settings could not be read: {error}"
                ))]);
            }
            _ => return working("Reading the NVIDIA driver…"),
        };
        let record = self.mods.dlss_override(&game.id);
        let set = record.map_or(0, |record| record.choice.len());
        let status = match set {
            0 => format!("Driver {}: none set", library.driver.label()),
            1 => format!("Driver {}: 1 set", library.driver.label()),
            set => format!("Driver {}: {set} set", library.driver.label()),
        };
        let has_exe = game.program.is_some() || record.is_some();
        let saving = self
            .mod_job
            .as_ref()
            .is_some_and(|(id, kind)| *id == game.id && *kind == ModKind::Overrides);
        // An NVIDIA DLL in the game's folder: DLSS or Streamline.
        let nvidia_dll = match (self.upscalers.found.get(&game.id), &self.upscalers.library) {
            (Some(found), Some(Ok(upscalers))) => Some(found.iter().any(|found| {
                upscalers
                    .catalog
                    .kind(&found.kind)
                    .is_some_and(|kind| kind.vendor == "NVIDIA")
            })),
            _ => None,
        };
        let hint_text = if saving {
            "Working… Windows asks for administrator rights when the driver needs them.".to_owned()
        } else if !has_exe {
            Blocked::NoProgram.to_string()
        } else if nvidia_dll == Some(false) {
            "No DLSS DLL is in this game's folder: of these, only Smooth Motion works without one."
                .to_owned()
        } else {
            "The driver's latest DLSS, presets, resolution and frame generation, without changing \
             the game's files."
                .to_owned()
        };
        let header = mod_card(ModCard {
            icon: g::NVIDIA,
            tint: theme::GREEN,
            title: "NVIDIA overrides",
            status: text(status).color(theme::TEXT_SECONDARY),
            actions: vec![
                button("Info")
                    .enabled(!self.dialog_open())
                    .name("NVIDIA overrides information")
                    .on_click(UpscalerMessage::ShowOverridesInfo.into()),
            ],
            hint: hint_text,
        });
        if !has_exe {
            return header;
        }
        let Some(draft) = self
            .upscalers
            .overrides
            .as_ref()
            .filter(|draft| draft.game == game.id)
        else {
            return col([header, working("Reading the game's driver profile…")]).gap(16.0);
        };
        let save = if saving {
            working_button()
        } else {
            accent_button("Save")
                .min_width(120.0)
                .enabled(draft.loaded().is_some() && !self.mods_busy())
                .name("Save NVIDIA overrides")
                .tooltip("Set these in the game's driver profile")
                .on_click(UpscalerMessage::SaveOverrides.into())
        };
        let mut form = self.overrides_form(draft);
        form.push(
            row([
                save,
                icon_text_button(g::REFRESH, "Reset to original")
                    .enabled(draft.loaded().is_some() && !draft.choice.is_empty() && !saving)
                    .name("Reset NVIDIA overrides to original")
                    .on_click(UpscalerMessage::ResetOverrides.into()),
            ])
            .gap(8.0),
        );
        col([header, card(form)]).gap(16.0)
    }

    fn overrides_form(&self, draft: &OverrideDraft) -> Vec<El<Message>> {
        let state = draft.loaded();
        let game = self.games.iter().find(|game| game.id == draft.game);
        let mut rows: Vec<El<Message>> = vec![match &draft.state {
            None => working("Reading the NVIDIA driver settings…"),
            Some(Err(error)) => hint(error.clone()),
            Some(Ok(None)) => hint("No NVIDIA graphics driver was found."),
            Some(Ok(Some(state))) => {
                let name = state
                    .profile
                    .name
                    .clone()
                    .or_else(|| {
                        state
                            .record
                            .as_ref()
                            .map(|record| record.original.profile.clone())
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "a new profile, {}",
                            gm_core::nvidia::new_profile_name(
                                game.map_or("the game", |game| game.name.as_str())
                            )
                        )
                    });
                hint(format!(
                    "Saved in the driver profile of {} ({name}). \"Original\" is what the profile \
                     held before Game Manager changed it.",
                    state.exe
                ))
            }
        }];
        if let (Some(state), Some(library)) = (state, self.override_library()) {
            rows.extend(changed_warning(state, &library.catalog));
        }
        if let Some(library) = self.override_library() {
            let version = Some(library.driver.version);
            for group in &library.catalog.groups {
                let settings: Vec<El<Message>> = group
                    .settings
                    .iter()
                    .filter(|setting| setting.available(version))
                    .map(|setting| self.override_picker(library, setting, draft))
                    .collect();
                if settings.is_empty() {
                    continue;
                }
                rows.push(
                    col([text(group.name.as_str()).bold(), col(settings).gap(6.0)])
                        .gap(6.0)
                        .padding(crate::ui::el::Edges::new(0.0, 8.0, 0.0, 0.0)),
                );
            }
        }
        rows.push(hint(
            "The game must use DLSS (Smooth Motion aside). A DLL override also replaces a DLL \
             swapped on the Upscalers tab. Settings this driver is too old for are not shown.",
        ));
        rows
    }

    /// One setting: its name and a drop-down of "Original: …" and its values.
    fn override_picker(
        &self,
        library: &OverrideLibrary,
        setting: &Setting,
        draft: &OverrideDraft,
    ) -> El<Message> {
        let state = draft.loaded();
        let values = shown_values(library, setting);
        let label = |value: u32| {
            values
                .iter()
                .find(|(known, _)| *known == value)
                .map_or_else(|| format!("0x{value:08X}"), |(_, name)| name.clone())
        };
        let original = original_label(
            state.and_then(|state| state.original(setting.id)),
            state.map_or_else(Default::default, |state| state.profile.setting(setting.id)),
            &label,
        );
        let selected = draft
            .choice
            .get(&setting.id)
            .and_then(|chosen| values.iter().position(|(value, _)| value == chosen))
            .map_or(0, |index| index + 1);
        let items: Vec<String> = std::iter::once(original)
            .chain(values.iter().map(|(_, name)| name.clone()))
            .collect();
        let id = setting.id;
        row([
            text(setting.name.as_str()).wrap().width(190.0),
            dropdown(
                format!("override:{id}"),
                items,
                Some(selected),
                move |index| UpscalerMessage::OverrideChosen(id, Some(index)).into(),
            )
            .enabled(state.is_some())
            .name(setting.name.as_str())
            .tooltip(setting.hint.as_str())
            .fill_width(),
        ])
        .gap(8.0)
    }
}
