//! The Mods expander: a row per mod (DXVK so far) with its installed version, which links to the
//! page it was downloaded from, Info, Install or Reinstall, Settings and Uninstall, and the one
//! dialog these open (Info or settings). WinUI shows one dialog at a time, so the mod buttons wait
//! while any dialog is open.

use crate::{GameManager, Message, icon_label};
use gm_core::graphics::GraphicsApi;
use gm_core::mods::dxvk::{self, Blocked, DxvkConfig, DxvkVariant, Hud};
use gm_core::mods::laa::{LAA_INFO, LaaStatus};
use gm_core::nvidia::{self, NvidiaChoice, PRESENT_FLAG_VALUES, PRESENT_METHODS, SettingState};
use gm_core::{Arch, Game};
use std::path::Path;
use windows_reactor::*;

/// "Cancel" glyph from Segoe Fluent Icons, for Uninstall.
const UNINSTALL_GLYPH: &str = "\u{E711}";

/// The open mod dialog.
pub(crate) enum ModDialog {
    Info,
    Settings(DxvkSettings),
    LaaInfo,
}

/// Which mod a job works on, so only its row shows "Working…".
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModKind {
    Dxvk,
    Laa,
}

/// The columns of every mod row, fixed so the rows line up: name, version or status, Info, the
/// wide button, settings and uninstall.
fn mod_columns() -> [GridLength; 6] {
    [
        GridLength::Pixel(70.0),
        GridLength::Pixel(200.0),
        GridLength::Pixel(72.0),
        GridLength::Star(1.0),
        GridLength::Pixel(56.0),
        GridLength::Pixel(56.0),
    ]
}

/// The DXVK settings being edited in their dialog.
#[derive(Clone)]
pub(crate) struct DxvkSettings {
    pub variant: DxvkVariant,
    pub config: DxvkConfig,
    pub nvidia: NvidiaChoice,
}

/// A change in the DXVK settings dialog; `None` (a list losing its selection) changes nothing.
#[derive(Clone)]
pub(crate) enum DxvkSettingsEdit {
    Variant(Option<usize>),
    Hdr(bool),
    MaxFrameRate(Option<f64>),
    Hud(Option<usize>),
    /// The HDR-mod config picked; `Some(None)` is "None".
    HdrModConfig(Option<Option<String>>),
    /// NVIDIA's Present Method and its flags: item 0 is "Original", then their values.
    PresentMethod(Option<usize>),
    PresentFlags(Option<usize>),
    /// Both NVIDIA settings back to "Original".
    ResetNvidia,
}

/// The value at a drop-down's `index` among `values`, after "Original" (0, `None`).
fn choice(values: &[(u32, &str)], index: usize) -> Option<u32> {
    index
        .checked_sub(1)
        .and_then(|index| values.get(index))
        .map(|(value, _)| *value)
}

/// The drop-down's index of `value` among `values`, after "Original" (0).
fn choice_index(values: &[(u32, &str)], value: Option<u32>) -> usize {
    value
        .and_then(|value| values.iter().position(|(known, _)| *known == value))
        .map_or(0, |index| index + 1)
}

impl DxvkSettings {
    pub(crate) fn edit(&mut self, edit: DxvkSettingsEdit) {
        match edit {
            DxvkSettingsEdit::Variant(Some(index)) => {
                if let Some(&variant) = DxvkVariant::ALL.get(index) {
                    self.variant = variant;
                }
            }
            DxvkSettingsEdit::Hdr(on) => self.config.hdr = on,
            DxvkSettingsEdit::MaxFrameRate(Some(value)) if value.is_finite() => {
                self.config.max_frame_rate = value.clamp(0.0, 1000.0).round() as u32;
            }
            DxvkSettingsEdit::Hud(Some(index)) => {
                if let Some(&hud) = Hud::ALL.get(index) {
                    self.config.hud = hud;
                }
            }
            DxvkSettingsEdit::HdrModConfig(Some(config)) => self.config.hdr_mod_config = config,
            DxvkSettingsEdit::PresentMethod(Some(index)) => {
                self.nvidia.present_method = choice(&PRESENT_METHODS, index);
            }
            DxvkSettingsEdit::PresentFlags(Some(index)) => {
                self.nvidia.present_flags = choice(&PRESENT_FLAG_VALUES, index);
            }
            DxvkSettingsEdit::ResetNvidia => self.nvidia = NvidiaChoice::default(),
            _ => {}
        }
    }
}

impl GameManager {
    /// The API DXVK is installed for: the one chosen in Game settings, else the one detected when
    /// exactly one was found.
    pub(crate) fn dxvk_api(&self, game: &Game) -> Option<GraphicsApi> {
        game.edits
            .as_ref()
            .and_then(|edits| edits.graphics_api)
            .or_else(|| match self.graphics.get(&game.id) {
                Some(Some(info)) if info.apis.len() == 1 => info.apis.first().copied(),
                _ => None,
            })
    }

    /// Lists HDR-mod's configs in the background, once, while DXVK settings show HDR-mod.
    pub(crate) fn request_hdr_mod_configs(&mut self, context: &ComponentContext<Self>) {
        let shown = matches!(
            &self.mod_dialog,
            Some(ModDialog::Settings(settings)) if settings.variant == DxvkVariant::HdrMod
        );
        if !shown || self.hdr_mod_configs.is_some() || self.hdr_mod_configs_pending {
            return;
        }
        self.hdr_mod_configs_pending = true;
        context.spawn_background(|_cancel| {
            Message::HdrModConfigsLoaded(dxvk::hdr_mod_configs().map_err(|e| e.to_string()))
        });
    }

    /// Reads the selected 32-bit game's LAA status in the background, once (it is read again after
    /// a scan, a mod job or `mods.json` loading, which clear it).
    pub(crate) fn request_laa(&mut self, context: &ComponentContext<Self>) {
        let Some(game) = self.selected_game() else {
            return;
        };
        if game.arch != Some(Arch::X86)
            || self.laa.contains_key(&game.id)
            || self.laa_pending.contains(&game.id)
        {
            return;
        }
        let (game, mods) = (game.clone(), self.mods.clone());
        self.laa_pending.insert(game.id.clone());
        context.spawn_background(move |_cancel| {
            let status = gm_core::mods::laa::status(&game, &mods).map_err(|e| e.to_string());
            Message::LaaLoaded(game.id, status)
        });
    }

    /// Any dialog is open: WinUI shows one at a time.
    fn dialog_open(&self) -> bool {
        self.mod_dialog.is_some() || self.draft.is_some() || self.removing.is_some()
    }

    pub(crate) fn mods_expander(&self, game: &Game, context: &ViewContext<Self>) -> View {
        Expander::new()
            .is_expanded(self.mods_open)
            .on_is_expanded_changed(context.callback(Message::ModsExpanded))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name("Mods")
            .header("Mods")
            .content(
                StackPanel::new().spacing(16.0).children(
                    std::iter::once(self.dxvk_row(game, context))
                        .chain(self.laa_row(game, context))
                        .collect::<Vec<_>>(),
                ),
            )
            .into()
    }

    /// LAA's row for a 32-bit game, in DXVK's columns (no settings): its status, Info, Enable LAA
    /// and ✕, then what Enable does or what is done, and a warning for Steam's DRM wrapper.
    fn laa_row(&self, game: &Game, context: &ViewContext<Self>) -> Option<View> {
        if game.arch != Some(Arch::X86) {
            return None;
        }
        let working = self
            .mod_job
            .as_ref()
            .is_some_and(|(id, kind)| *id == game.id && *kind == ModKind::Laa);
        let busy = self.mod_job.is_some()
            || self.dialog_open()
            || self.scanning
            || self.saving
            || self.picking;
        let laa = self.laa.get(&game.id);
        let status = match laa {
            Some(Ok(laa)) => Some(laa.status),
            _ => None,
        };
        let (label, mut hint) = match laa {
            None => ("Checking…", "Reading the game's program…".to_owned()),
            Some(Err(error)) => ("Unknown", error.clone()),
            Some(Ok(laa)) => {
                let exe = &laa.exe;
                match laa.status {
                    LaaStatus::Off => (
                        "Off",
                        format!(
                            "Patches {exe} so the game can use 4 GB of memory instead of 2 GB; \
                             the original is kept as {exe}.gm-backup."
                        ),
                    ),
                    LaaStatus::Patched => (
                        "On (patched)",
                        format!("{exe} is patched; ✕ puts the original back."),
                    ),
                    LaaStatus::OnByDeveloper => (
                        "On (by its developer)",
                        format!("{exe} is already Large Address Aware."),
                    ),
                    LaaStatus::Updated => (
                        "Off (game updated)",
                        "The game's program changed since it was patched (an update): enable \
                         LAA again."
                            .to_owned(),
                    ),
                }
            }
        };
        if let Some(Ok(laa)) = laa
            && laa.steam_drm
            && laa.status != LaaStatus::OnByDeveloper
        {
            hint.push_str(&format!(
                " ⚠ {} has Steam's DRM wrapper: a patched copy may not start; ✕ puts the original \
                 back.",
                laa.exe
            ));
        }
        if working {
            hint = "Working… Windows asks for administrator rights when the game's folder needs \
                    them."
                .to_owned();
        }
        let can_enable = matches!(status, Some(LaaStatus::Off | LaaStatus::Updated));
        let can_restore = matches!(status, Some(LaaStatus::Patched | LaaStatus::Updated));
        let (symbol, button) = if can_enable || status.is_none() {
            (Symbol::Add, "Enable LAA")
        } else {
            (Symbol::Accept, "LAA enabled")
        };
        let content = if working {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    ProgressRing::new().is_active(true).width(16.0).height(16.0),
                    TextBlock::new().text("Working…"),
                ))
                .into()
        } else {
            icon_label(symbol, button)
        };
        let gap = Thickness::new(8.0, 0.0, 0.0, 0.0);
        let row = Grid::new().columns(mod_columns()).children((
            TextBlock::new()
                .text("LAA")
                .font_weight(FontWeight::SEMI_BOLD)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0)
                .tooltip("Large Address Aware"),
            TextBlock::new()
                .text(label)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1),
            Button::new()
                .is_enabled(!self.dialog_open())
                .on_click(context.message(Message::ShowLaaInfo))
                .automation_name("LAA information")
                .margin(gap)
                .grid_column(2)
                .content("Info"),
            Button::new()
                .is_enabled(!busy && can_enable)
                .on_click(context.message(Message::EnableLaa))
                .automation_name(button)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .margin(gap)
                .grid_column(3)
                .content(content),
            Button::new()
                .is_enabled(!busy && can_restore)
                .on_click(context.message(Message::RestoreLaa))
                .automation_name("Put the original program back")
                .margin(gap)
                .grid_column(5)
                .content(
                    FontIcon::new()
                        .glyph(UNINSTALL_GLYPH)
                        .foreground(ThemeBrush::SystemCritical),
                )
                .tooltip("Put the original program back"),
        ));
        Some(
            StackPanel::new()
                .spacing(8.0)
                .children((
                    row,
                    TextBlock::new()
                        .text(hint)
                        .font_size(12.0)
                        .opacity(0.7)
                        .text_wrapping(TextWrapping::Wrap),
                ))
                .into(),
        )
    }

    /// DXVK's row, and below it what Install does, what is installed, or why it can't be.
    fn dxvk_row(&self, game: &Game, context: &ViewContext<Self>) -> View {
        let state = self.mods.dxvk(&game.id);
        let installed = state.and_then(|state| state.installed.as_ref());
        let variant = state.map_or(DxvkVariant::default(), |state| state.variant);
        let working = self
            .mod_job
            .as_ref()
            .is_some_and(|(id, kind)| *id == game.id && *kind == ModKind::Dxvk);
        let idle = self.mod_job.is_none() && !self.dialog_open();
        let busy = !idle || self.scanning || self.saving || self.picking;
        let api = self.dxvk_api(game);
        let target = dxvk::target(game, api);

        let version: View = match installed {
            Some(installed) => HyperlinkButton::new()
                .content(installed.version.clone())
                .on_click(context.message(Message::OpenModPage(installed.page.clone())))
                .automation_name(format!("DXVK {}", installed.version))
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .tooltip(format!(
                    "Open the page DXVK {} was downloaded from",
                    installed.version
                )),
            None => TextBlock::new()
                .text("Not installed")
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .into(),
        };
        let (symbol, label) = if installed.is_some() {
            (Symbol::Refresh, "Reinstall DXVK")
        } else {
            (Symbol::Download, "Install DXVK")
        };
        let install_content = if working {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    ProgressRing::new().is_active(true).width(16.0).height(16.0),
                    TextBlock::new().text("Working…"),
                ))
                .into()
        } else {
            icon_label(symbol, label)
        };
        let gap = Thickness::new(8.0, 0.0, 0.0, 0.0);
        let row = Grid::new().columns(mod_columns()).children((
            TextBlock::new()
                .text("DXVK")
                .font_weight(FontWeight::SEMI_BOLD)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0),
            version,
            Button::new()
                .is_enabled(!self.dialog_open())
                .on_click(context.message(Message::ShowDxvkInfo))
                .automation_name("DXVK information")
                .margin(gap)
                .grid_column(2)
                .content("Info"),
            Button::new()
                .is_enabled(!busy && target.is_ok())
                .on_click(context.message(Message::InstallDxvk))
                .automation_name(label)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .margin(gap)
                .grid_column(3)
                .content(install_content),
            Button::new()
                .is_enabled(idle)
                .on_click(context.message(Message::ShowDxvkSettings))
                .automation_name("DXVK settings")
                .margin(gap)
                .grid_column(4)
                .content(SymbolIcon::new().symbol(Symbol::Setting))
                .tooltip("DXVK settings"),
            Button::new()
                .is_enabled(!busy && installed.is_some())
                .on_click(context.message(Message::UninstallDxvk))
                .automation_name("Uninstall DXVK")
                .margin(gap)
                .grid_column(5)
                .content(
                    FontIcon::new()
                        .glyph(UNINSTALL_GLYPH)
                        .foreground(ThemeBrush::SystemCritical),
                )
                .tooltip("Uninstall DXVK"),
        ));

        // The HDR-mod config an HDR-mod install brings.
        let with_config = state
            .and_then(|state| state.config.hdr_mod_config.as_deref())
            .filter(|_| variant == DxvkVariant::HdrMod)
            .map(|name| format!(" with config {}", dxvk::config_label(name)))
            .unwrap_or_default();
        let hint = if working {
            "Working… Windows asks for administrator rights when the game's folder needs them."
                .to_owned()
        } else {
            match (installed, &target) {
                (Some(installed), _) => format!(
                    "{} in {}. Reinstall gets the latest {} build{with_config}.",
                    installed
                        .files
                        .iter()
                        .map(|file| file.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                    shown_path(game, &installed.folder),
                    variant.name()
                ),
                (None, Ok(target)) => format!(
                    "Installs the latest {} build{with_config}: {}, into {}.",
                    variant.name(),
                    target.summary(),
                    shown_path(game, &target.folder)
                ),
                (None, Err(Blocked::NoApi)) if !self.graphics.contains_key(&game.id) => {
                    "Checking the game's graphics APIs…".to_owned()
                }
                (None, Err(blocked)) => blocked.to_string(),
            }
        };
        StackPanel::new()
            .spacing(8.0)
            .children((
                row,
                TextBlock::new()
                    .text(hint)
                    .font_size(12.0)
                    .opacity(0.7)
                    .text_wrapping(TextWrapping::Wrap),
            ))
            .into()
    }

    /// DXVK's Info or settings dialog, or LAA's Info, while one is open.
    pub(crate) fn mod_dialog_view(&self, context: &ViewContext<Self>) -> ContentDialog {
        let dialog = ContentDialog::new()
            .is_open(self.mod_dialog.is_some())
            .on_closed(context.callback(Message::ModDialogClosed));
        match &self.mod_dialog {
            Some(ModDialog::LaaInfo) => {
                let this_game = self
                    .selected_game()
                    .and_then(|game| self.laa.get(&game.id))
                    .map(|laa| match laa {
                        Ok(laa) => format!(
                            "For this game: {} {}.",
                            laa.exe,
                            match laa.status {
                                LaaStatus::Off => "is not Large Address Aware",
                                LaaStatus::Patched => "is patched by Game Manager",
                                LaaStatus::OnByDeveloper =>
                                    "is Large Address Aware by its developer",
                                LaaStatus::Updated =>
                                    "was replaced by an update since it was patched",
                            }
                        ),
                        Err(error) => format!("For this game: {error}"),
                    });
                dialog
                    .title("Large Address Aware (LAA)")
                    .close_button_text("OK")
                    .content(
                        StackPanel::new().spacing(12.0).children((
                            TextBlock::new()
                                .text(LAA_INFO)
                                .text_wrapping(TextWrapping::Wrap),
                            TextBlock::new()
                                .text(this_game.unwrap_or_default())
                                .opacity(0.7)
                                .text_wrapping(TextWrapping::Wrap),
                        )),
                    )
            }
            Some(ModDialog::Settings(settings)) => dialog
                .title("DXVK settings")
                .primary_button_text("Save")
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::Primary)
                .content(self.dxvk_settings_form(settings, context)),
            _ => {
                let this_game = self.selected_game().map(|game| {
                    match dxvk::target(game, self.dxvk_api(game)) {
                        Ok(target) => format!("For this game: {}.", target.summary()),
                        Err(blocked) => format!("For this game: {blocked}"),
                    }
                });
                dialog.title("DXVK").close_button_text("OK").content(
                    StackPanel::new().spacing(12.0).children((
                        TextBlock::new()
                            .text(dxvk::INFO)
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(this_game.unwrap_or_default())
                            .opacity(0.7)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
                )
            }
        }
    }

    fn dxvk_settings_form(&self, settings: &DxvkSettings, context: &ViewContext<Self>) -> View {
        let edited = |edit: DxvkSettingsEdit| Message::DxvkSettingsEdited(edit);
        let variant = DxvkVariant::ALL
            .iter()
            .position(|&variant| variant == settings.variant);
        let hud = Hud::ALL.iter().position(|&hud| hud == settings.config.hud);
        let mut rows: Vec<View> =
            vec![
                RadioButtons::new()
                    .header("Version")
                    .items_source(DxvkVariant::ALL.map(DxvkVariant::label))
                    .selected_index(variant)
                    .on_selection_changed(context.callback(move |index: Option<usize>| {
                        edited(DxvkSettingsEdit::Variant(index))
                    }))
                    .into(),
            ];
        if settings.variant == DxvkVariant::HdrMod {
            rows.push(self.hdr_mod_config_picker(settings, context));
        }
        rows.extend([
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(32.0)
                .children((
                    ToggleSwitch::new()
                        .header("HDR")
                        .is_on(settings.config.hdr)
                        .on_toggled(
                            context.callback(move |on: bool| edited(DxvkSettingsEdit::Hdr(on))),
                        ),
                    NumberBox::new()
                        .header("Frame-rate limit (0 = none)")
                        .minimum(0.0)
                        .maximum(1000.0)
                        .value(f64::from(settings.config.max_frame_rate))
                        .width(220.0)
                        .on_value_changed(context.callback(move |value: Option<f64>| {
                            edited(DxvkSettingsEdit::MaxFrameRate(value))
                        })),
                ))
                .into(),
            StackPanel::new()
                .spacing(6.0)
                .children((
                    TextBlock::new().text("HUD"),
                    ComboBox::new()
                        .items_source(Hud::ALL.map(Hud::label))
                        .selected_index(hud)
                        .min_width(240.0)
                        .automation_name("HUD")
                        .on_selection_changed(context.callback(move |index: Option<usize>| {
                            edited(DxvkSettingsEdit::Hud(index))
                        })),
                ))
                .into(),
            TextBlock::new()
                .text(
                    "The version is used by the next Install or Reinstall. The other settings \
                     are written to dxvk.conf next to the game's program.",
                )
                .font_size(12.0)
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        ]);
        rows.extend(self.nvidia_section(settings, context));
        // The dialog does not scroll its content by itself: without this, what does not fit the
        // window is cut off below the buttons. Its title and buttons take about 280 DIPs.
        let height = match self.window_size.1 {
            height if height > 0.0 => (height - 280.0).max(200.0),
            _ => 480.0,
        };
        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .max_height(height)
            .content(
                StackPanel::new()
                    .spacing(16.0)
                    .min_width(420.0)
                    // Room for the scroll bar, so it never covers the controls.
                    .margin(Thickness::new(0.0, 0.0, 16.0, 0.0))
                    .children(rows),
            )
            .into()
    }

    /// NVIDIA's Present Method and its flags for the game's driver profile, which HDR with DXVK
    /// needs on NVIDIA GPUs; nothing without an NVIDIA driver.
    fn nvidia_section(&self, settings: &DxvkSettings, context: &ViewContext<Self>) -> Option<View> {
        let game = self.selected_game()?;
        let exe = game
            .program
            .as_deref()
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned());
        let original = self
            .mods
            .dxvk(&game.id)
            .and_then(|state| state.nvidia_original.as_ref());
        let (profile, note) = match (&exe, &self.nvidia_profile) {
            (None, _) => (None, Blocked::NoProgram.to_string()),
            (Some(_), None) => (None, "Reading the NVIDIA driver settings…".to_owned()),
            (Some(_), Some(Ok(None))) => return None,
            (Some(_), Some(Err(error))) => (None, error.clone()),
            (Some(exe), Some(Ok(Some(profile)))) => {
                let name = profile
                    .profile
                    .clone()
                    .or_else(|| original.map(|original| original.profile.clone()))
                    .unwrap_or_else(|| format!("a new profile, {} (Game Manager)", game.name));
                (
                    Some(profile),
                    format!(
                        "For HDR with DXVK on NVIDIA: Prefer layered on DXGI Swapchain, and Allow \
                         promoting DXVK or Treat DXVK as Native. Saved in the driver profile of \
                         {exe} ({name})."
                    ),
                )
            }
        };
        // "Original" shows what was there before Game Manager changed it, else what is there now.
        let original_label =
            |recorded: Option<Option<u32>>, now: SettingState, label: fn(u32) -> String| {
                let text = match (recorded, now) {
                    (Some(Some(value)), _) => label(value),
                    (Some(None), _) => "driver default".to_owned(),
                    (
                        None,
                        SettingState {
                            user_value: Some(value),
                            ..
                        },
                    ) => label(value),
                    (
                        None,
                        SettingState {
                            value: Some(value), ..
                        },
                    ) => format!("{} (NVIDIA)", label(value)),
                    (None, _) => "driver default".to_owned(),
                };
                format!("Original: {text}")
            };
        let now = profile.cloned().unwrap_or_default();
        let enabled = profile.is_some();
        let picker =
            |values: &'static [(u32, &'static str)],
             first: String,
             chosen: Option<u32>,
             name: &str,
             edit: fn(Option<usize>) -> DxvkSettingsEdit| {
                let items: Vec<String> = std::iter::once(first)
                    .chain(values.iter().map(|(_, label)| (*label).to_owned()))
                    .collect();
                ComboBox::new()
                    .items_source(items)
                    .selected_index(choice_index(values, chosen))
                    .is_enabled(enabled)
                    .min_width(320.0)
                    .automation_name(name)
                    .on_selection_changed(context.callback(move |index: Option<usize>| {
                        Message::DxvkSettingsEdited(edit(index))
                    }))
            };
        Some(
            StackPanel::new()
                .spacing(6.0)
                .children((
                    TextBlock::new()
                        .text("NVIDIA (HDR with DXVK)")
                        .font_weight(FontWeight::SEMI_BOLD),
                    TextBlock::new().text("Vulkan/OpenGL present method"),
                    picker(
                        &PRESENT_METHODS,
                        original_label(
                            original.map(|o| o.method),
                            now.method,
                            nvidia::method_label,
                        ),
                        settings.nvidia.present_method,
                        "Vulkan/OpenGL present method",
                        DxvkSettingsEdit::PresentMethod,
                    ),
                    TextBlock::new().text("Present method flags"),
                    picker(
                        &PRESENT_FLAG_VALUES,
                        original_label(original.map(|o| o.flags), now.flags, nvidia::flags_label),
                        settings.nvidia.present_flags,
                        "Present method flags",
                        DxvkSettingsEdit::PresentFlags,
                    ),
                    Button::new()
                        .is_enabled(enabled && !settings.nvidia.is_original())
                        .on_click(
                            context.message(Message::DxvkSettingsEdited(
                                DxvkSettingsEdit::ResetNvidia,
                            )),
                        )
                        .automation_name("Reset NVIDIA settings to original")
                        .content(icon_label(Symbol::Undo, "Reset to original")),
                    TextBlock::new()
                        .text(note)
                        .font_size(12.0)
                        .opacity(0.7)
                        .text_wrapping(TextWrapping::Wrap),
                ))
                .into(),
        )
    }

    /// HDR-mod's own configs for the game's API: "None", then each in the latest HDR-mod build.
    fn hdr_mod_config_picker(&self, settings: &DxvkSettings, context: &ViewContext<Self>) -> View {
        let api = self.selected_game().and_then(|game| self.dxvk_api(game));
        let names: Vec<String> = match &self.hdr_mod_configs {
            Some(Ok(names)) => dxvk::configs_for(names, api)
                .into_iter()
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        let (first, note) = match (&self.hdr_mod_configs, api) {
            (_, None) => ("None", Blocked::NoApi.to_string()),
            (None, _) => ("Loading the HDR-mod configs…", String::new()),
            (Some(Err(error)), _) => ("None", error.clone()),
            (Some(Ok(_)), Some(api)) if names.is_empty() => (
                "None",
                format!("HDR-mod has no configs for {}.", api.label()),
            ),
            (Some(Ok(_)), Some(_)) => (
                "None",
                "0 is the safest; higher numbers upgrade more render targets for HDR and may \
                 cause visual issues or crashes."
                    .to_owned(),
            ),
        };
        let selected = settings
            .config
            .hdr_mod_config
            .as_ref()
            .and_then(|chosen| names.iter().position(|name| name == chosen))
            .map_or(0, |index| index + 1);
        let items: Vec<String> = std::iter::once(first.to_owned())
            .chain(names.iter().map(|name| dxvk::config_label(name)))
            .collect();
        let picker = ComboBox::new()
            .items_source(items)
            .selected_index(selected)
            .is_enabled(!names.is_empty())
            .min_width(240.0)
            .automation_name("HDR-mod config")
            .on_selection_changed(context.callback(move |index: Option<usize>| {
                let choice = index.map(|index| {
                    index
                        .checked_sub(1)
                        .and_then(|index| names.get(index).cloned())
                });
                Message::DxvkSettingsEdited(DxvkSettingsEdit::HdrModConfig(choice))
            }));
        StackPanel::new()
            .spacing(6.0)
            .children((
                TextBlock::new().text("HDR-mod config"),
                picker,
                TextBlock::new()
                    .text(note)
                    .font_size(12.0)
                    .opacity(0.7)
                    .text_wrapping(TextWrapping::Wrap),
            ))
            .into()
    }
}

/// `path` inside the install folder when it can be shown that way ("bin\x64").
fn shown_path(game: &Game, path: &Path) -> String {
    let shown = game
        .install_dir
        .as_deref()
        .and_then(|dir| path.strip_prefix(dir).ok())
        .filter(|relative| !relative.as_os_str().is_empty());
    match shown {
        Some(relative) => relative.display().to_string(),
        None if game.install_dir.as_deref() == Some(path) => "the install folder".to_owned(),
        None => path.display().to_string(),
    }
}
