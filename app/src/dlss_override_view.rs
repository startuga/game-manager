//! NVIDIA's driver-level DLSS overrides (`gm_core::mods::dlss_override`): the last row of the
//! Upscalers expander, and the dialog it opens.
//!
//! - **The row** shows only with an NVIDIA driver: the driver's version and its own DLLs' versions
//!   are read once, in the background, when the expander first opens (`request_driver`, from
//!   `request_upscalers`). It says how many overrides are set, and when the game's folder has no
//!   NVIDIA DLL that only Smooth Motion works then.
//! - **The dialog** reads the game's driver profile in the background when it opens
//!   (`OverridesLoaded`, matched to the game); its drop-downs wait for it. Each group of the
//!   catalog has a heading, and each setting the driver knows a drop-down whose first item is
//!   "Original: …" (as in DXVK settings). Reset to original sets every one back to it; Save runs
//!   `dlss_override::save` as a mod job (`ModKind::Overrides`).
//! - **Changed elsewhere**: the NVIDIA app writes the same settings (resetting its DLSS override
//!   writes them all as Off or N/A), and may delete a profile. When the profile no longer holds
//!   what Game Manager set (`OverrideState::changed_elsewhere`), a warning names each setting with
//!   its value now, and Save sets them again even when the choice is unchanged.
//! - The dialog doesn't scroll by itself, so its content is in a `ScrollViewer` fitted to the
//!   window, as DXVK settings' is.

use crate::mods_view::{ModDialog, ModKind, original_label};
use crate::upscalers_view::{UpscalerMessage, note, working};
use crate::{GameManager, Message, icon_label};
use gm_core::Game;
use gm_core::mods::dlss_override::{self, Catalog, Driver, OverrideState, Setting};
use gm_core::mods::dxvk::Blocked;
use std::collections::BTreeMap;
use windows_reactor::*;

/// The override settings and what the driver offers.
#[derive(Clone)]
pub(crate) struct OverrideLibrary {
    pub catalog: Catalog,
    pub driver: Driver,
}

/// The overrides being edited in their dialog.
pub(crate) struct OverrideDraft {
    /// The game the dialog opened for.
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

/// The overrides the profile no longer holds as Game Manager set them: the NVIDIA app or
/// another tool changed them, or deleted the profile. Nothing when all are as set.
fn changed_warning(state: &OverrideState, catalog: &Catalog) -> Option<View> {
    let changed = state.changed_elsewhere();
    if changed.is_empty() {
        return None;
    }
    let text = if state.profile.name.is_none() {
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
    Some(
        TextBlock::new()
            .text(text)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    )
}

/// A setting's values as the drop-down shows them, after "Original".
fn shown_values(library: &OverrideLibrary, setting: &Setting) -> Vec<(u32, String)> {
    setting.values(library.driver.dll(setting))
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
    pub(crate) fn request_driver(&mut self, context: &ComponentContext<Self>) {
        if self.upscalers.driver.is_some() || self.upscalers.driver_pending {
            return;
        }
        self.upscalers.driver_pending = true;
        context.spawn_background(|_cancel| UpscalerMessage::DriverLoaded(load_driver()).into());
    }

    pub(crate) fn handle_overrides(
        &mut self,
        message: UpscalerMessage,
        context: &ComponentContext<Self>,
    ) {
        match message {
            UpscalerMessage::DriverLoaded(driver) => {
                self.upscalers.driver_pending = false;
                self.upscalers.driver = Some(driver);
            }
            UpscalerMessage::ShowOverrides => {
                let (Some(game), Some(library)) = (self.selected_game(), self.override_library())
                else {
                    return;
                };
                let (game, catalog) = (game.clone(), library.catalog.clone());
                let choice = self
                    .mods
                    .dlss_override(&game.id)
                    .map(|record| record.choice.clone())
                    .unwrap_or_default();
                self.mod_dialog = Some(ModDialog::Overrides(OverrideDraft {
                    game: game.id.clone(),
                    choice,
                    state: None,
                }));
                context.spawn_background(move |_cancel| {
                    let read = dlss_override::state(&game, &catalog).map_err(|e| e.to_string());
                    UpscalerMessage::OverridesLoaded(game.id, read).into()
                });
            }
            UpscalerMessage::ShowOverridesInfo => self.mod_dialog = Some(ModDialog::OverridesInfo),
            UpscalerMessage::OverridesLoaded(id, read) => {
                if let Some(ModDialog::Overrides(draft)) = &mut self.mod_dialog
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
                if let Some(ModDialog::Overrides(draft)) = &mut self.mod_dialog {
                    match value {
                        Some(value) => draft.choice.insert(id, value),
                        None => draft.choice.remove(&id),
                    };
                }
            }
            UpscalerMessage::ResetOverrides => {
                if let Some(ModDialog::Overrides(draft)) = &mut self.mod_dialog {
                    draft.choice.clear();
                }
            }
            _ => {}
        }
    }

    /// The dialog's Save: applies the choice to the game's driver profile, when it changed.
    pub(crate) fn save_overrides(
        &mut self,
        game: Game,
        draft: OverrideDraft,
        context: &ComponentContext<Self>,
    ) {
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
        let choice = draft.choice;
        context.spawn_background(move |_cancel| {
            let saved = dlss_override::save(&game, &catalog, choice);
            Message::ModJobDone(saved.map_err(|e| e.to_string()))
        });
    }

    /// The Upscalers expander's last row: how many overrides are set, Info and the dialog's
    /// button. Nothing without an NVIDIA driver.
    pub(crate) fn overrides_row(&self, game: &Game, context: &ViewContext<Self>) -> Option<View> {
        let library = match &self.upscalers.driver {
            Some(Ok(Some(library))) => library,
            Some(Err(error)) => {
                return Some(note(&format!(
                    "The NVIDIA driver's settings could not be read: {error}"
                )));
            }
            _ => return None,
        };
        let record = self.mods.dlss_override(&game.id);
        let set = record.map_or(0, |record| record.choice.len());
        let state = match set {
            0 => format!("Driver {}: none set", library.driver.label()),
            1 => format!("Driver {}: 1 set", library.driver.label()),
            set => format!("Driver {}: {set} set", library.driver.label()),
        };
        let has_exe = game.program.is_some() || record.is_some();
        let working = self
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
        let hint = if working {
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
        let label = StackPanel::new()
            .spacing(2.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(0)
            .children((
                TextBlock::new()
                    .text("NVIDIA overrides")
                    .font_weight(FontWeight::SEMI_BOLD),
                TextBlock::new()
                    .text(state)
                    .font_size(12.0)
                    .opacity(0.7)
                    .text_wrapping(TextWrapping::Wrap),
            ));
        let settings_content: View = if working {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    ProgressRing::new().is_active(true).width(16.0).height(16.0),
                    TextBlock::new().text("Working…"),
                ))
                .into()
        } else {
            SymbolIcon::new().symbol(Symbol::Setting).into()
        };
        Some(
            Grid::new()
                .columns([
                    GridLength::Pixel(240.0),
                    GridLength::Star(1.0),
                    GridLength::Auto,
                    GridLength::Auto,
                ])
                .column_spacing(8.0)
                .children((
                    label,
                    TextBlock::new()
                        .text(hint)
                        .font_size(12.0)
                        .opacity(0.7)
                        .text_wrapping(TextWrapping::Wrap)
                        .vertical_alignment(VerticalAlignment::Center)
                        .grid_column(1),
                    Button::new()
                        .is_enabled(!self.dialog_open())
                        .on_click(context.message(UpscalerMessage::ShowOverridesInfo.into()))
                        .automation_name("NVIDIA overrides information")
                        .content("Info")
                        .grid_column(2),
                    Button::new()
                        .is_enabled(has_exe && !self.mods_busy())
                        .on_click(context.message(UpscalerMessage::ShowOverrides.into()))
                        .automation_name("NVIDIA overrides settings")
                        .content(settings_content)
                        .grid_column(3)
                        .tooltip("NVIDIA overrides settings"),
                ))
                .into(),
        )
    }

    /// The overrides dialog: one drop-down per setting the driver knows, by group.
    pub(crate) fn overrides_dialog(
        &self,
        dialog: ContentDialog,
        draft: &OverrideDraft,
        context: &ViewContext<Self>,
    ) -> ContentDialog {
        dialog
            .title("NVIDIA overrides")
            .primary_button_text("Save")
            .close_button_text("Cancel")
            .default_button(ContentDialogButton::Primary)
            .is_primary_button_enabled(draft.loaded().is_some())
            .content(self.overrides_form(draft, context))
    }

    fn overrides_form(&self, draft: &OverrideDraft, context: &ViewContext<Self>) -> View {
        let state = draft.loaded();
        let game = self.games.iter().find(|game| game.id == draft.game);
        let mut rows: Vec<View> = Vec::new();
        rows.push(match &draft.state {
            None => working("Reading the NVIDIA driver settings…"),
            Some(Err(error)) => note(error),
            Some(Ok(None)) => note("No NVIDIA graphics driver was found."),
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
                note(&format!(
                    "Saved in the driver profile of {} ({name}). \"Original\" is what the profile \
                     held before Game Manager changed it.",
                    state.exe
                ))
            }
        });
        if let (Some(state), Some(library)) = (state, self.override_library()) {
            rows.extend(changed_warning(state, &library.catalog));
        }
        if let Some(library) = self.override_library() {
            let version = Some(library.driver.version);
            for group in &library.catalog.groups {
                let settings: Vec<View> = group
                    .settings
                    .iter()
                    .filter(|setting| setting.available(version))
                    .map(|setting| self.override_picker(library, setting, draft, context))
                    .collect();
                if settings.is_empty() {
                    continue;
                }
                rows.push(
                    StackPanel::new()
                        .spacing(6.0)
                        .children((
                            TextBlock::new()
                                .text(group.name.as_str())
                                .font_weight(FontWeight::SEMI_BOLD),
                            StackPanel::new().spacing(6.0).children(settings),
                        ))
                        .into(),
                );
            }
        }
        rows.push(
            Button::new()
                .is_enabled(state.is_some() && !draft.choice.is_empty())
                .on_click(context.message(UpscalerMessage::ResetOverrides.into()))
                .automation_name("Reset NVIDIA overrides to original")
                .content(icon_label(Symbol::Undo, "Reset to original"))
                .into(),
        );
        rows.push(note(
            "The game must use DLSS (Smooth Motion aside). A DLL override also replaces a DLL \
             swapped above. Settings this driver is too old for are not shown.",
        ));
        // As in DXVK settings: what does not fit the window would be cut off below the buttons.
        let height = match self.window_size.1 {
            height if height > 0.0 => (height - 280.0).max(200.0),
            _ => 480.0,
        };
        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .max_height(height)
            .content(
                StackPanel::new()
                    .spacing(12.0)
                    .min_width(460.0)
                    // Room for the scroll bar, so it never covers the controls.
                    .margin(Thickness::new(0.0, 0.0, 16.0, 0.0))
                    .children(rows),
            )
            .into()
    }

    /// One setting: its name and a drop-down of "Original: …" and its values.
    fn override_picker(
        &self,
        library: &OverrideLibrary,
        setting: &Setting,
        draft: &OverrideDraft,
        context: &ViewContext<Self>,
    ) -> View {
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
        let row = Grid::new()
            .columns([GridLength::Pixel(200.0), GridLength::Star(1.0)])
            .column_spacing(8.0)
            .children((
                TextBlock::new()
                    .text(setting.name.as_str())
                    .text_wrapping(TextWrapping::Wrap)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(0),
                ComboBox::new()
                    .items_source(items)
                    .selected_index(Some(selected))
                    .is_enabled(state.is_some())
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    // Beside a name that wraps to two lines.
                    .vertical_alignment(VerticalAlignment::Center)
                    .automation_name(setting.name.as_str())
                    .on_selection_changed(context.callback(move |index: Option<usize>| {
                        UpscalerMessage::OverrideChosen(id, index).into()
                    }))
                    .grid_column(1),
            ));
        if setting.hint.is_empty() {
            row.into()
        } else {
            row.tooltip(setting.hint.as_str())
        }
    }
}
