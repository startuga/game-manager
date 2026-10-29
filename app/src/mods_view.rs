//! The Mods expander: a row per mod (DXVK so far) with its installed version, which links to the
//! page it was downloaded from, Info, Install or Reinstall, Settings and Uninstall, and the one
//! dialog these open (Info or settings). WinUI shows one dialog at a time, so the mod buttons wait
//! while any dialog is open.

use crate::{GameManager, Message, icon_label};
use gm_core::Game;
use gm_core::graphics::GraphicsApi;
use gm_core::mods::dxvk::{self, Blocked, DxvkConfig, DxvkVariant, Hud};
use std::path::Path;
use windows_reactor::*;

/// "Cancel" glyph from Segoe Fluent Icons, for Uninstall.
const UNINSTALL_GLYPH: &str = "\u{E711}";

/// The open mod dialog.
pub(crate) enum ModDialog {
    Info,
    Settings(DxvkSettings),
}

/// The DXVK settings being edited in their dialog.
#[derive(Clone, Copy)]
pub(crate) struct DxvkSettings {
    pub variant: DxvkVariant,
    pub config: DxvkConfig,
}

/// A change in the DXVK settings dialog; `None` (a list losing its selection) changes nothing.
#[derive(Clone, Copy)]
pub(crate) enum DxvkSettingsEdit {
    Variant(Option<usize>),
    Hdr(bool),
    MaxFrameRate(Option<f64>),
    Hud(Option<usize>),
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
            .content(self.dxvk_row(game, context))
            .into()
    }

    /// DXVK's row, and below it what Install does, what is installed, or why it can't be.
    fn dxvk_row(&self, game: &Game, context: &ViewContext<Self>) -> View {
        let state = self.mods.dxvk(&game.id);
        let installed = state.and_then(|state| state.installed.as_ref());
        let variant = state.map_or(DxvkVariant::default(), |state| state.variant);
        let working = self.mod_job.as_deref() == Some(game.id.as_str());
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
        let row = Grid::new()
            .columns([
                GridLength::Pixel(70.0),
                // HDR-mod versions are long (3.1-HDR-mod-v0.3.4).
                GridLength::Auto.min(150.0),
                GridLength::Auto,
                GridLength::Star(1.0),
                GridLength::Auto,
                GridLength::Auto,
            ])
            .children((
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

        let hint = if working {
            "Working… Windows asks for administrator rights when the game's folder needs them."
                .to_owned()
        } else {
            match (installed, &target) {
                (Some(installed), _) => format!(
                    "{} in {}. Reinstall gets the latest {} build.",
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
                    "Installs the latest {} build: {}, into {}.",
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

    /// DXVK's Info or settings dialog, while one is open.
    pub(crate) fn mod_dialog_view(&self, context: &ViewContext<Self>) -> ContentDialog {
        let dialog = ContentDialog::new()
            .is_open(self.mod_dialog.is_some())
            .on_closed(context.callback(Message::ModDialogClosed));
        match &self.mod_dialog {
            Some(ModDialog::Settings(settings)) => dialog
                .title("DXVK settings")
                .primary_button_text("Save")
                .close_button_text("Cancel")
                .default_button(ContentDialogButton::Primary)
                .content(dxvk_settings_form(settings, context)),
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
}

fn dxvk_settings_form(settings: &DxvkSettings, context: &ViewContext<GameManager>) -> View {
    let edited = |edit: DxvkSettingsEdit| Message::DxvkSettingsEdited(edit);
    let variant = DxvkVariant::ALL
        .iter()
        .position(|&variant| variant == settings.variant);
    let hud = Hud::ALL.iter().position(|&hud| hud == settings.config.hud);
    StackPanel::new()
        .spacing(16.0)
        .min_width(420.0)
        .children((
            RadioButtons::new()
                .header("Version")
                .items_source(DxvkVariant::ALL.map(DxvkVariant::label))
                .selected_index(variant)
                .on_selection_changed(context.callback(move |index: Option<usize>| {
                    edited(DxvkSettingsEdit::Variant(index))
                })),
            ToggleSwitch::new()
                .header("HDR")
                .is_on(settings.config.hdr)
                .on_toggled(context.callback(move |on: bool| edited(DxvkSettingsEdit::Hdr(on)))),
            NumberBox::new()
                .header("Frame-rate limit (0 = none)")
                .minimum(0.0)
                .maximum(1000.0)
                .value(f64::from(settings.config.max_frame_rate))
                .on_value_changed(context.callback(move |value: Option<f64>| {
                    edited(DxvkSettingsEdit::MaxFrameRate(value))
                })),
            StackPanel::new().spacing(6.0).children((
                TextBlock::new().text("HUD"),
                ComboBox::new()
                    .items_source(Hud::ALL.map(Hud::label))
                    .selected_index(hud)
                    .min_width(240.0)
                    .automation_name("HUD")
                    .on_selection_changed(context.callback(move |index: Option<usize>| {
                        edited(DxvkSettingsEdit::Hud(index))
                    })),
            )),
            TextBlock::new()
                .text(
                    "The version is used by the next Install or Reinstall. The other settings \
                     are written to dxvk.conf next to the game's program.",
                )
                .font_size(12.0)
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap),
        ))
        .into()
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
