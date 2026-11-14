//! The game page's mod tabs: ReShade, and DXVK & LAA (LAA for 32-bit games only). Each mod is a
//! card: its installed version (a link to the page it was downloaded from), Info, Install or
//! Reinstall, Settings and the red ✕, then a line saying what Install does, what is installed, or
//! why it can't be. Then the dialogs these open (Info or settings), as modal layers: one at a
//! time, and the mod buttons wait while one is open.
//!
//! The ReShade tab has three views (`ReshadeView`), switched by pills under the tab strip:
//! ReShade (its card), Shaders and Add-ons (`reshade_panel.rs`; `main.rs` opens and leaves them).

use crate::app_view::{dialog, field_label};
use crate::shell::WindowInfo;
use crate::shell::ctx::Ctx;
use crate::ui::el::{El, col, dropdown, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    accent_button, button, card, hint, icon_button, icon_square, icon_text_button, link,
    radio_group, remove_button, segmented, text_box, toggle, working, working_button,
};
use crate::{DialogResult, GameManager, Message, ReshadeView};
use gm_core::graphics::GraphicsApi;
use gm_core::mods::dxvk::{self, Blocked, DxvkConfig, DxvkVariant, Hud};
use gm_core::mods::laa::{LAA_INFO, LaaStatus};
use gm_core::mods::reshade::{self, Mode, ReshadeVariant};
use gm_core::nvidia::{self, NvidiaChoice, PRESENT_FLAG_VALUES, PRESENT_METHODS, SettingState};
use gm_core::{Arch, Game};
use std::path::Path;
use windows_canvas::ColorF;

/// The open mod dialog.
pub(crate) enum ModDialog {
    Info,
    Settings(DxvkSettings),
    LaaInfo,
    ReshadeInfo,
    /// ReShade settings: the variant being chosen.
    ReshadeSettings(ReshadeVariant),
    UpscalerInfo,
    OverridesInfo,
}

/// Which mod a job works on, so only its row shows "Working…".
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModKind {
    Dxvk,
    Laa,
    Reshade,
    /// ReShade's shaders and add-ons going to games with ReShade: to every game when the job's
    /// game id is empty.
    Deploy,
    /// An upscaler DLL swapped or restored; `UpscalerState::row` says which.
    Upscaler,
    /// NVIDIA's DLSS overrides saved in the game's driver profile.
    Overrides,
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

/// A driver setting's "Original" item: what was there before Game Manager changed it (`recorded`:
/// the user's value then, `Some(None)` for none), else what is there now.
pub(crate) fn original_label(
    recorded: Option<Option<u32>>,
    now: SettingState,
    label: &dyn Fn(u32) -> String,
) -> String {
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
    /// The API mods (DXVK, ReShade) are installed for: the one chosen in Game settings, else the
    /// one detected when exactly one was found.
    pub(crate) fn mod_api(&self, game: &Game) -> Option<GraphicsApi> {
        game.edits
            .as_ref()
            .and_then(|edits| edits.graphics_api)
            .or_else(|| match self.graphics.get(&game.id) {
                Some(Some(info)) if info.apis.len() == 1 => info.apis.first().copied(),
                _ => None,
            })
    }

    /// Lists HDR-mod's configs in the background, once, while DXVK settings show HDR-mod.
    pub(crate) fn request_hdr_mod_configs(&mut self, context: &Ctx<Message>) {
        let shown = matches!(
            &self.mod_dialog,
            Some(ModDialog::Settings(settings)) if settings.variant == DxvkVariant::HdrMod
        );
        if !shown || self.hdr_mod_configs.is_some() || self.hdr_mod_configs_pending {
            return;
        }
        self.hdr_mod_configs_pending = true;
        context.spawn_background(|| {
            Message::HdrModConfigsLoaded(dxvk::hdr_mod_configs().map_err(|e| e.to_string()))
        });
    }

    /// Reads the selected 32-bit game's LAA status in the background, once (it is read again after
    /// a scan, a mod job or `mods.json` loading, which clear it).
    pub(crate) fn request_laa(&mut self, context: &Ctx<Message>) {
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
        context.spawn_background(move || {
            let status = gm_core::mods::laa::status(&game, &mods).map_err(|e| e.to_string());
            Message::LaaLoaded(game.id, status)
        });
    }

    /// Reads what the selected game's ReShade row and graphics line need from the disk (DXVK in
    /// its folder, the Vulkan layer) in the background, once (it is read again after a scan, a mod
    /// job or `mods.json` loading, which clear it).
    pub(crate) fn request_reshade(&mut self, context: &Ctx<Message>) {
        let Some(game) = self.selected_game() else {
            return;
        };
        if self.reshade.contains_key(&game.id) || self.reshade_pending.contains(&game.id) {
            return;
        }
        let (game, mods) = (game.clone(), self.mods.clone());
        self.reshade_pending.insert(game.id.clone());
        context.spawn_background(move || {
            let status = reshade::status(&game, &mods);
            Message::ReshadeLoaded(game.id, status)
        });
    }

    /// Any dialog is open: one at a time.
    pub(crate) fn dialog_open(&self) -> bool {
        self.mod_dialog.is_some() || self.draft.is_some() || self.removing.is_some()
    }

    /// Any work that the mod buttons wait for.
    pub(crate) fn mods_busy(&self) -> bool {
        self.mod_job.is_some() || self.dialog_open() || self.scanning || self.saving || self.picking
    }
}

/// `path` inside the install folder when it can be shown that way ("bin\x64").
pub(crate) fn shown_path(game: &Game, path: &Path) -> String {
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

/// What a mod's card shows.
pub(crate) struct ModCard {
    pub icon: &'static str,
    pub tint: ColorF,
    pub title: &'static str,
    /// The installed version (a link) or a status.
    pub status: El<Message>,
    pub actions: Vec<El<Message>>,
    pub hint: String,
}

/// A mod's card: icon, name and status, its actions on the right, and the hint below.
pub(crate) fn mod_card(card_spec: ModCard) -> El<Message> {
    card([
        row([
            icon_square(card_spec.icon, card_spec.tint, 36.0),
            col([
                text(card_spec.title).text_size(theme::TEXT_SUBTITLE).bold(),
                card_spec.status,
            ])
            .gap(2.0)
            .fill_width(),
            row(card_spec.actions).gap(8.0),
        ])
        .gap(12.0),
        hint(card_spec.hint),
    ])
}

/// The installed version as a link to its download page, or "Not installed".
fn version_link(mod_name: &str, installed: Option<(&str, &str)>) -> El<Message> {
    match installed {
        Some((version, page)) => link(version.to_owned(), Message::OpenModPage(page.to_owned()))
            .name(format!("{mod_name} {version}"))
            .tooltip(format!(
                "Open the page {mod_name} {version} was downloaded from"
            )),
        None => text("Not installed").color(theme::TEXT_TERTIARY),
    }
}

impl GameManager {
    /// Whether a job on this game's `kind` of mod is running.
    fn working_on(&self, game: &Game, kind: ModKind) -> bool {
        self.mod_job
            .as_ref()
            .is_some_and(|(id, job)| *id == game.id && *job == kind)
    }

    /// The ReShade tab: its views (ReShade, Shaders, Add-ons), then the one chosen.
    pub(crate) fn reshade_tab(&self, game: &Game) -> El<Message> {
        let view = self.reshade_view;
        let views = segmented(vec![
            (
                "ReShade",
                view == ReshadeView::Install,
                Message::ReshadeView(ReshadeView::Install),
            ),
            (
                "Shaders",
                view == ReshadeView::Shaders,
                Message::ReshadeView(ReshadeView::Shaders),
            ),
            (
                "Add-ons",
                view == ReshadeView::Addons,
                Message::ReshadeView(ReshadeView::Addons),
            ),
        ]);
        let body = match view {
            ReshadeView::Install => {
                let choose = card([
                    text("Shaders and add-ons").bold(),
                    hint(
                        "Chosen for every game with ReShade, or for this game alone, in Shaders \
                         and Add-ons; Install and Reinstall bring them.",
                    ),
                    row([
                        icon_text_button(g::BRUSH, "Choose shaders")
                            .on_click(Message::ReshadeView(ReshadeView::Shaders)),
                        icon_text_button(g::PUZZLE, "Choose add-ons")
                            .on_click(Message::ReshadeView(ReshadeView::Addons)),
                    ])
                    .gap(8.0),
                ]);
                col([self.reshade_card(game), choose]).gap(16.0)
            }
            // The panel opens in `update` (`open_reshade_view`); a Deploy closes it until done.
            ReshadeView::Shaders | ReshadeView::Addons => {
                self.reshade_page().unwrap_or_else(|| {
                    working(if matches!(self.mod_job, Some((_, ModKind::Deploy))) {
                        "Deploying…"
                    } else {
                        "Opening…"
                    })
                })
            }
        };
        col([views, body]).gap(16.0)
    }

    /// ReShade's card, and below it what Install does, what is installed, or why it can't be.
    fn reshade_card(&self, game: &Game) -> El<Message> {
        let state = self.mods.reshade(&game.id);
        let installed = state.and_then(|state| state.installed.as_ref());
        let on_layer = state.is_some_and(|state| state.layer.is_some());
        let variant = state.map_or(ReshadeVariant::default(), |state| state.variant);
        let working = self.working_on(game, ModKind::Reshade);
        let busy = self.mods_busy();
        let status = self.reshade.get(&game.id);
        let api = self.mod_api(game);
        let target = status.map(|status| reshade::target(game, api, status.dxvk));

        let label = if installed.is_some() {
            "Reinstall"
        } else {
            "Install"
        };
        let install = if working {
            working_button()
        } else {
            accent_button(label)
                .enabled(!busy && matches!(target, Some(Ok(_))))
                .name(format!("{label} ReShade"))
                .on_click(Message::InstallReshade)
        };
        let actions = vec![
            button("Info")
                .enabled(!self.dialog_open())
                .name("ReShade information")
                .on_click(Message::ShowReshadeInfo),
            install,
            icon_button(g::SETTINGS, "ReShade settings")
                .enabled(self.mod_job.is_none() && !self.dialog_open())
                .on_click(Message::ShowReshadeSettings),
            remove_button("Uninstall ReShade")
                .enabled(!busy && (installed.is_some() || on_layer))
                .on_click(Message::UninstallReshade),
        ];

        let layer_note = |apps: usize| {
            format!(
                " The layer is shared by every program on its list ({apps} now): installing \
                 updates it for all of them."
            )
        };
        let hint = if working {
            "Working… Windows asks for administrator rights when the game's folder or ReShade's \
             Vulkan layer needs them."
                .to_owned()
        } else {
            match (installed, &target) {
                (_, None) => "Checking the game's folder…".to_owned(),
                (Some(installed), Some(target)) => {
                    let how = match state.and_then(|state| state.dll()) {
                        Some(dll) => format!("ReShade as {dll}"),
                        None if on_layer => "ReShade's Vulkan layer".to_owned(),
                        None => "ReShade".to_owned(),
                    };
                    let mut hint = format!(
                        "{how} with its effects in reshade-shaders, in {}. Reinstall gets the \
                         latest {} build.",
                        shown_path(game, &installed.folder),
                        variant.name()
                    );
                    // The game changed since: DXVK added or removed.
                    match target {
                        Ok(target) if on_layer && target.mode != Mode::Layer => hint.push_str(
                            " DXVK is no longer installed: Reinstall ReShade to use it without \
                             the Vulkan layer.",
                        ),
                        Ok(target) if !on_layer && target.mode == Mode::Layer => hint.push_str(
                            " DXVK is installed now: Reinstall ReShade to use its Vulkan layer.",
                        ),
                        _ => {}
                    }
                    hint
                }
                (None, Some(Ok(target))) => match target.mode {
                    Mode::Dll(_) => format!(
                        "Installs the latest {} build: {}, with the shaders chosen on the Shaders \
                         page, into {}.",
                        variant.name(),
                        target.summary(),
                        shown_path(game, &target.folder)
                    ),
                    Mode::Layer => {
                        let apps = status.map_or(0, |status| status.layer.programs());
                        let why = if status.is_some_and(|status| status.dxvk) {
                            ", as DXVK is installed"
                        } else {
                            ""
                        };
                        format!(
                            "Installs the latest {} build through ReShade's Vulkan layer{why}, with \
                             the shaders chosen in Shaders.{}",
                            variant.name(),
                            layer_note(apps)
                        )
                    }
                },
                (None, Some(Err(reshade::Blocked::NoApi)))
                    if !self.graphics.contains_key(&game.id) =>
                {
                    "Checking the game's graphics APIs…".to_owned()
                }
                (None, Some(Err(blocked))) => blocked.to_string(),
            }
        };
        mod_card(ModCard {
            icon: g::BRUSH,
            tint: theme::PINK,
            title: "ReShade",
            status: version_link(
                "ReShade",
                installed.map(|installed| (installed.version.as_str(), installed.page.as_str())),
            ),
            actions,
            hint,
        })
    }

    /// The DXVK & LAA tab: DXVK's card, and LAA's for a 32-bit game.
    pub(crate) fn mods_tab(&self, game: &Game) -> El<Message> {
        let mut cards = vec![self.dxvk_card(game)];
        cards.extend(self.laa_card(game));
        col(cards).gap(16.0)
    }

    /// LAA's card for a 32-bit game: its status, Info, Enable and ✕, then what Enable does or
    /// what is done, and a warning for Steam's DRM wrapper.
    fn laa_card(&self, game: &Game) -> Option<El<Message>> {
        if game.arch != Some(Arch::X86) {
            return None;
        }
        let working = self.working_on(game, ModKind::Laa);
        let busy = self.mods_busy();
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
        let enable = if working {
            working_button()
        } else {
            let button_label = if can_enable || status.is_none() {
                "Enable LAA"
            } else {
                "LAA enabled"
            };
            accent_button(button_label)
                .enabled(!busy && can_enable)
                .on_click(Message::EnableLaa)
        };
        let actions = vec![
            button("Info")
                .enabled(!self.dialog_open())
                .name("LAA information")
                .on_click(Message::ShowLaaInfo),
            enable,
            remove_button("Put the original program back")
                .enabled(!busy && can_restore)
                .on_click(Message::RestoreLaa),
        ];
        Some(mod_card(ModCard {
            icon: g::CHIP,
            tint: theme::CYAN,
            title: "Large Address Aware",
            status: text(label).color(theme::TEXT_SECONDARY),
            actions,
            hint,
        }))
    }

    /// DXVK's card, and below it what Install does, what is installed, or why it can't be.
    fn dxvk_card(&self, game: &Game) -> El<Message> {
        let state = self.mods.dxvk(&game.id);
        let installed = state.and_then(|state| state.installed.as_ref());
        let variant = state.map_or(DxvkVariant::default(), |state| state.variant);
        let working = self.working_on(game, ModKind::Dxvk);
        let idle = self.mod_job.is_none() && !self.dialog_open();
        let busy = self.mods_busy();
        let api = self.mod_api(game);
        let target = dxvk::target(game, api);
        // ReShade installed as one of DXVK's DLLs is in the way.
        let reshade_dll = target
            .as_ref()
            .ok()
            .and_then(|target| dxvk::reshade_dll(&self.mods, &game.id, target));

        let label = if installed.is_some() {
            "Reinstall"
        } else {
            "Install"
        };
        let install = if working {
            working_button()
        } else {
            accent_button(label)
                .enabled(!busy && target.is_ok() && reshade_dll.is_none())
                .name(format!("{label} DXVK"))
                .on_click(Message::InstallDxvk)
        };
        let actions = vec![
            button("Info")
                .enabled(!self.dialog_open())
                .name("DXVK information")
                .on_click(Message::ShowDxvkInfo),
            install,
            icon_button(g::SETTINGS, "DXVK settings")
                .enabled(idle)
                .on_click(Message::ShowDxvkSettings),
            remove_button("Uninstall DXVK")
                .enabled(!busy && installed.is_some())
                .on_click(Message::UninstallDxvk),
        ];

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
                (None, Ok(_)) if let Some(dll) = &reshade_dll => reshade::blocks_dxvk(dll),
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
        mod_card(ModCard {
            icon: g::WRENCH,
            tint: theme::ORANGE,
            title: "DXVK",
            status: version_link(
                "DXVK",
                installed.map(|installed| (installed.version.as_str(), installed.page.as_str())),
            ),
            actions,
            hint,
        })
    }

    /// The open mod dialog: an Info, or ReShade's or DXVK's settings.
    pub(crate) fn mod_dialog_view(&self, window: &WindowInfo) -> Option<El<Message>> {
        let close = Message::ModDialogClosed(DialogResult::Cancel);
        let save = Some((
            "Save",
            true,
            Message::ModDialogClosed(DialogResult::Primary),
        ));
        let info = |title: &str, text_body: &str, this_game: Option<String>| {
            let mut body = vec![text(text_body).wrap()];
            if let Some(line) = this_game {
                body.push(hint(line));
            }
            dialog(window, title, body, None, close.clone())
        };
        Some(match self.mod_dialog.as_ref()? {
            ModDialog::OverridesInfo => {
                info("NVIDIA overrides", gm_core::mods::dlss_override::INFO, None)
            }
            ModDialog::UpscalerInfo => info("Upscalers", gm_core::mods::upscalers::INFO, None),
            ModDialog::LaaInfo => {
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
                info("Large Address Aware (LAA)", LAA_INFO, this_game)
            }
            ModDialog::Settings(settings) => dialog(
                window,
                "DXVK settings",
                self.dxvk_settings_form(settings),
                save,
                close,
            ),
            ModDialog::ReshadeInfo => {
                let this_game = self.selected_game().map(|game| {
                    let dxvk = self.reshade.get(&game.id).is_some_and(|status| status.dxvk);
                    match reshade::target(game, self.mod_api(game), dxvk) {
                        Ok(target) => format!("For this game: {}.", target.summary()),
                        Err(blocked) => format!("For this game: {blocked}"),
                    }
                });
                info("ReShade", reshade::INFO, this_game)
            }
            ModDialog::ReshadeSettings(variant) => {
                let index = ReshadeVariant::ALL
                    .iter()
                    .position(|known| known == variant);
                let items = ReshadeVariant::ALL
                    .map(|variant| variant.label().to_owned())
                    .to_vec();
                let body = vec![
                    radio_group("Version", items, index, |i| {
                        Message::ReshadeVariantChosen(Some(i))
                    }),
                    hint("The version is used by the next Install or Reinstall."),
                ];
                dialog(window, "ReShade settings", body, save, close)
            }
            ModDialog::Info => {
                let this_game =
                    self.selected_game()
                        .map(|game| match dxvk::target(game, self.mod_api(game)) {
                            Ok(target) => format!("For this game: {}.", target.summary()),
                            Err(blocked) => format!("For this game: {blocked}"),
                        });
                info("DXVK", dxvk::INFO, this_game)
            }
        })
    }

    fn dxvk_settings_form(&self, settings: &DxvkSettings) -> Vec<El<Message>> {
        let edited = Message::DxvkSettingsEdited;
        let variant = DxvkVariant::ALL
            .iter()
            .position(|&variant| variant == settings.variant);
        let hud = Hud::ALL.iter().position(|&hud| hud == settings.config.hud);
        let variants = DxvkVariant::ALL
            .map(|variant| variant.label().to_owned())
            .to_vec();
        let mut rows = vec![radio_group("Version", variants, variant, |i| {
            edited(DxvkSettingsEdit::Variant(Some(i)))
        })];
        if settings.variant == DxvkVariant::HdrMod {
            rows.push(self.hdr_mod_config_picker(settings));
        }
        rows.push(
            row([
                toggle(
                    "HDR",
                    settings.config.hdr,
                    edited(DxvkSettingsEdit::Hdr(!settings.config.hdr)),
                )
                .fill_width(),
                col([
                    field_label("Frame-rate limit (0 = none)"),
                    text_box(
                        "dxvk:fps",
                        settings.config.max_frame_rate.to_string(),
                        "0",
                        "Frame-rate limit",
                        move |value| {
                            edited(DxvkSettingsEdit::MaxFrameRate(value.trim().parse().ok()))
                        },
                    ),
                ])
                .gap(4.0)
                .width(220.0),
            ])
            .gap(24.0),
        );
        let huds = Hud::ALL.map(|hud| hud.label().to_owned()).to_vec();
        rows.push(
            col([
                field_label("HUD"),
                dropdown("dxvk:hud", huds, hud, move |i| {
                    edited(DxvkSettingsEdit::Hud(Some(i)))
                })
                .name("HUD"),
            ])
            .gap(4.0),
        );
        rows.push(hint(
            "The version is used by the next Install or Reinstall. The other settings are \
             written to dxvk.conf next to the game's program.",
        ));
        rows.extend(self.nvidia_section(settings));
        rows
    }

    /// NVIDIA's Present Method and its flags for the game's driver profile, which HDR with DXVK
    /// needs on NVIDIA GPUs; nothing without an NVIDIA driver.
    fn nvidia_section(&self, settings: &DxvkSettings) -> Option<El<Message>> {
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
        let now = profile.cloned().unwrap_or_default();
        let enabled = profile.is_some();
        let picker = |key: &str,
                      values: &'static [(u32, &'static str)],
                      first: String,
                      chosen: Option<u32>,
                      name: &str,
                      edit: fn(Option<usize>) -> DxvkSettingsEdit| {
            let items: Vec<String> = std::iter::once(first)
                .chain(values.iter().map(|(_, label)| (*label).to_owned()))
                .collect();
            dropdown(
                key,
                items,
                Some(choice_index(values, chosen)),
                move |index| Message::DxvkSettingsEdited(edit(Some(index))),
            )
            .enabled(enabled)
            .name(name)
        };
        Some(
            col([
                text("NVIDIA (HDR with DXVK)").bold(),
                field_label("Vulkan/OpenGL present method"),
                picker(
                    "nvidia:method",
                    &PRESENT_METHODS,
                    original_label(
                        original.map(|o| o.method),
                        now.method,
                        &nvidia::method_label,
                    ),
                    settings.nvidia.present_method,
                    "Vulkan/OpenGL present method",
                    DxvkSettingsEdit::PresentMethod,
                ),
                field_label("Present method flags"),
                picker(
                    "nvidia:flags",
                    &PRESENT_FLAG_VALUES,
                    original_label(original.map(|o| o.flags), now.flags, &nvidia::flags_label),
                    settings.nvidia.present_flags,
                    "Present method flags",
                    DxvkSettingsEdit::PresentFlags,
                ),
                row([icon_text_button(g::REFRESH, "Reset to original")
                    .enabled(enabled && !settings.nvidia.is_original())
                    .name("Reset NVIDIA settings to original")
                    .on_click(Message::DxvkSettingsEdited(DxvkSettingsEdit::ResetNvidia))]),
                hint(note),
            ])
            .gap(6.0)
            .padding(crate::ui::el::Edges::new(0.0, 8.0, 0.0, 0.0)),
        )
    }

    /// HDR-mod's own configs for the game's API: "None", then each in the latest HDR-mod build.
    fn hdr_mod_config_picker(&self, settings: &DxvkSettings) -> El<Message> {
        let api = self.selected_game().and_then(|game| self.mod_api(game));
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
        let enabled = !names.is_empty();
        let picker = dropdown("dxvk:hdr-config", items, Some(selected), move |index| {
            let choice = index
                .checked_sub(1)
                .and_then(|index| names.get(index).cloned());
            Message::DxvkSettingsEdited(DxvkSettingsEdit::HdrModConfig(Some(choice)))
        })
        .enabled(enabled)
        .name("HDR-mod config");
        col([field_label("HDR-mod config"), picker, hint(note)]).gap(4.0)
    }
}
