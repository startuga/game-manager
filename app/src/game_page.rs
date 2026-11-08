//! The page beside the sidebar: the selected game, or the Shaders, Add-ons and Upscalers pages.
//!
//! The game page: a header with the icon, the name in the title
//! font, the install folder and coloured chips (store, bitness, graphics APIs; DXVK's Vulkan), a
//! launch bar with the Play button, the alternative launches, Folder and PCGamingWiki, then the
//! tabs (Overview, ReShade, DXVK & LAA, Upscalers) and the open tab's cards.
//!
//! Overview holds Game settings (the draft in `edit.rs`) beside the game's details. The other tabs
//! come from `mods_view.rs` (ReShade, DXVK & LAA) and `upscalers_view.rs` (Upscalers, with
//! NVIDIA's overrides from `dlss_override_view.rs`); the Shaders and Add-ons pages from
//! `reshade_panel.rs`, the Upscalers page from `upscalers_page.rs`, My PC from `my_pc_page.rs`.

use gm_core::graphics::GraphicsSource;
use gm_core::mods::dxvk;
use gm_core::{Game, Launch, Store};
use windows_canvas::ColorF;

use crate::app_view::field_label;
use crate::edit::{EditChoice, EditDraft, EditField};
use crate::shell::WindowInfo;
use crate::ui::el::{Align, El, col, dropdown, row, scroll, spacer, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    PlayState, Tab, accent_button, button, card, card_header, chip, heading, hint,
    icon_text_button, play_button, subtle_button, tabs, text_box, working,
};
use crate::{CHOSEN, GameManager, GameTab, Message, Page, api_list};

/// The widest the page's content grows.
pub(crate) const PAGE_MAX_WIDTH: f32 = 1180.0;

impl GameManager {
    pub(crate) fn page_view(&self, window: &WindowInfo) -> El<Message> {
        let content = match self.page {
            Page::Game => return self.game_page(),
            // The panel opens with the page; `update` leaves the page once it closes.
            Page::Shaders | Page::Addons => {
                self.reshade_page().unwrap_or_else(|| working("Opening…"))
            }
            Page::Upscalers => self.upscalers_page(),
            Page::MyPc => self.my_pc_page(window),
        };
        page(content)
    }

    fn game_page(&self) -> El<Message> {
        let Some(game) = self.selected_game() else {
            let message = if self.scanning {
                "Looking for installed games…"
            } else if self.games.is_empty() {
                "No installed games were found. Use Add a game, or drop a game's .exe or its shortcut here."
            } else {
                "Select a game in the library."
            };
            return col([hint(message)]).center().grow();
        };
        let tab_strip = tabs(vec![
            tab(
                g::OVERVIEW,
                theme::BLUE,
                "Overview",
                self.tab == GameTab::Overview,
                GameTab::Overview,
            ),
            tab(
                g::BRUSH,
                theme::PINK,
                "ReShade",
                self.tab == GameTab::Reshade,
                GameTab::Reshade,
            ),
            tab(
                g::WRENCH,
                theme::ORANGE,
                "DXVK & LAA",
                self.tab == GameTab::Mods,
                GameTab::Mods,
            ),
            tab(
                g::CHIP,
                theme::GREEN,
                "Upscalers",
                self.tab == GameTab::Upscalers,
                GameTab::Upscalers,
            ),
        ]);
        let body = match self.tab {
            GameTab::Overview => self.overview(game),
            GameTab::Reshade => self.reshade_tab(game),
            GameTab::Mods => self.mods_tab(game),
            GameTab::Upscalers => self.upscalers_tab(game),
        };
        page(col([self.header(game), self.launch_bar(game), tab_strip, body]).gap(18.0))
    }

    fn header(&self, game: &Game) -> El<Message> {
        let folder = game.install_dir.as_ref().map_or_else(
            || "Install folder unknown".to_owned(),
            |dir| dir.display().to_string(),
        );
        row([
            self.icon(game, 64.0),
            col([
                heading(game.name.clone()).wrap(),
                text(folder)
                    .text_size(theme::TEXT_SMALL)
                    .color(theme::TEXT_TERTIARY)
                    .wrap(),
                row(self.chips(game)).gap(6.0),
            ])
            .gap(6.0)
            .fill_width(),
        ])
        .gap(16.0)
    }

    /// The store, the bitness, the graphics APIs and DXVK's Vulkan, as chips.
    fn chips(&self, game: &Game) -> Vec<El<Message>> {
        let mut chips = vec![chip(
            game.store.name().to_uppercase(),
            store_tint(game.store),
        )];
        if let Some(arch) = game.arch {
            let chosen = game
                .edits
                .as_ref()
                .is_some_and(|edits| edits.arch.is_some());
            let arch = chip(arch.label(), theme::BLUE);
            chips.push(if chosen { arch.tooltip(CHOSEN) } else { arch });
        }
        if let Some((apis, tooltip)) = self.graphics_text(game) {
            for api in apis {
                chips.push(chip(api, theme::VIOLET).tooltip(tooltip.clone()));
            }
        }
        if self.runs_on_dxvk(game) {
            chips.push(chip("VULKAN (DXVK)", theme::ORANGE).tooltip(
                "DXVK is in the game's folder: it runs the game's Direct3D on Vulkan, so \
                     overlays and ReShade see a Vulkan game.",
            ));
        }
        chips
    }

    /// The graphics APIs to show and where they come from; "Checking…" while they are looked up,
    /// nothing when none was found.
    fn graphics_text(&self, game: &Game) -> Option<(Vec<String>, String)> {
        if let Some(api) = game.edits.as_ref().and_then(|edits| edits.graphics_api) {
            return Some((vec![api.label().to_owned()], CHOSEN.to_owned()));
        }
        match self.graphics.get(&game.id) {
            Some(Some(info)) => {
                let source = match &info.source {
                    GraphicsSource::PcGamingWiki { page } => {
                        format!("Graphics APIs from PCGamingWiki ({page})")
                    }
                    GraphicsSource::GameFiles => "Graphics APIs read from the game's files; they \
                                                  may include APIs the game does not use"
                        .to_owned(),
                };
                let tooltip = if info.apis.len() > 1 {
                    format!("{source}. Choose the one the game runs on in Game settings.")
                } else {
                    source
                };
                Some((
                    info.apis.iter().map(|api| api.label().to_owned()).collect(),
                    tooltip,
                ))
            }
            Some(None) => None,
            None => Some((
                vec!["CHECKING…".to_owned()],
                "Looking on PCGamingWiki, then in the game's files".to_owned(),
            )),
        }
    }

    /// DXVK in the game's folder translates one of the APIs it runs on.
    fn runs_on_dxvk(&self, game: &Game) -> bool {
        let dxvk_here = self.reshade.get(&game.id).is_some_and(|status| status.dxvk);
        let apis = match game.edits.as_ref().and_then(|edits| edits.graphics_api) {
            Some(api) => vec![api],
            None => match self.graphics.get(&game.id) {
                Some(Some(info)) => info.apis.clone(),
                _ => Vec::new(),
            },
        };
        dxvk_here && apis.iter().any(|&api| dxvk::translates(api))
    }

    fn launch_bar(&self, game: &Game) -> El<Message> {
        let state = if self.launching.as_deref() == Some(game.id.as_str()) {
            PlayState::Starting
        } else {
            PlayState::Play
        };
        let mut items = vec![
            play_button(
                state,
                Message::Launch {
                    id: game.id.clone(),
                    option: None,
                },
            )
            .name(format!("Play {}", game.name)),
        ];
        items.extend(game.alt_launches.iter().enumerate().map(|(i, alt)| {
            button(alt.label.clone()).on_click(Message::Launch {
                id: game.id.clone(),
                option: Some(i),
            })
        }));
        if game.install_dir.is_some() {
            items.push(
                icon_text_button(g::FOLDER, "Folder")
                    .name(format!("Open the folder of {}", game.name))
                    .on_click(Message::OpenFolder(game.id.clone())),
            );
        }
        items.push(
            icon_text_button(g::LINK, "PCGamingWiki")
                .name(format!("Open {} on PCGamingWiki", game.name))
                .tooltip("Fixes and settings for this game on PCGamingWiki")
                .on_click(Message::OpenWiki(game.id.clone())),
        );
        items.push(spacer());
        if game.store == Store::Custom {
            items.push(
                subtle_button(g::DELETE, "Remove")
                    .name(format!("Remove {}", game.name))
                    .on_click(Message::RemoveGame(game.id.clone())),
            );
        }
        row(items).gap(8.0)
    }

    fn overview(&self, game: &Game) -> El<Message> {
        row([
            self.game_settings(game).fill_width().valign(Align::Start),
            self.details(game).width(320.0).valign(Align::Start),
        ])
        .gap(16.0)
    }

    /// Game settings: name, launch program, arguments, game program, bitness and graphics API.
    fn game_settings(&self, game: &Game) -> El<Message> {
        let draft = EditDraft::for_game(self.edit.as_ref(), game);
        let busy = self.saving || self.scanning || self.picking;
        let store = game.store.name();
        let field = |field: EditField, key: &str, name: &str, placeholder: String| {
            text_box(
                format!("edit:{key}"),
                draft.fields.get(field),
                placeholder,
                name,
                move |value| Message::EditField(field, value),
            )
            .fill_width()
        };
        let browse = |field: EditField, name: &str| {
            button("Browse…")
                .enabled(!busy)
                .name(name)
                .on_click(Message::BrowseEdit(field))
        };
        let launch_set = !draft.fields.launch.trim().is_empty();
        let args_enabled = launch_set || gm_core::accepts_args(&game.launch);
        let args_hint = if !args_enabled {
            format!(
                "{store} starts this game itself, so it takes no arguments. Set a launch .exe to pass some."
            )
        } else if !launch_set && matches!(game.launch, Launch::Uri(_)) {
            "Passed to Steam as the game's launch options.".to_owned()
        } else {
            "Passed to the program Play starts.".to_owned()
        };
        // "As detected" names what was detected; a chosen bitness has replaced the detected one.
        let detected_arch = match (draft.start.arch, game.arch) {
            (Some(_), _) => "As detected".to_owned(),
            (None, Some(arch)) => format!("As detected: {}", arch.label()),
            (None, None) => "As detected: unknown".to_owned(),
        };
        let detected_api = match self.graphics.get(&game.id) {
            Some(Some(info)) => format!("As detected: {}", api_list(&info.apis)),
            Some(None) => "As detected: none found".to_owned(),
            None => "As detected: checking…".to_owned(),
        };
        let choice = |choice: EditChoice, key: &str, title: &str, detected: String| {
            let items = std::iter::once(detected)
                .chain(choice.labels().into_iter().map(str::to_owned))
                .collect();
            col([
                field_label(title),
                dropdown(
                    format!("choice:{key}"),
                    items,
                    Some(draft.fields.choice_index(choice)),
                    move |i| Message::EditChoice(choice, Some(i)),
                )
                .enabled(!busy)
                .name(title),
            ])
            .gap(4.0)
            .fill_width()
        };
        let title = if game.edits.is_some() {
            "Game settings (edited)"
        } else {
            "Game settings"
        };
        card([
            card_header(
                g::SETTINGS,
                theme::BLUE,
                title,
                "What Play starts, and what mods need to know",
            ),
            field_label("Name"),
            field(EditField::Name, "name", "Game name", String::new()),
            field_label("Launch .exe"),
            row([
                field(
                    EditField::Launch,
                    "launch",
                    "Launch .exe",
                    format!("Empty: the game starts through {store}"),
                ),
                browse(EditField::Launch, "Browse for the launch .exe"),
            ])
            .gap(8.0),
            hint(format!(
                "The program Play starts. Leave it empty to start the game through {store}."
            )),
            field_label("Arguments"),
            field(EditField::Args, "args", "Arguments", String::new()).enabled(args_enabled),
            hint(args_hint),
            field_label("Game program"),
            row([
                field(EditField::Program, "program", "Game program", String::new()),
                browse(EditField::Program, "Browse for the game program"),
            ])
            .gap(8.0),
            hint("The game's own .exe: used for its bitness, Folder and mods."),
            row([
                choice(EditChoice::Bitness, "bitness", "Bitness", detected_arch),
                choice(EditChoice::GraphicsApi, "api", "Graphics API", detected_api),
            ])
            .gap(16.0),
            hint(
                "Mods such as ReShade and DXVK need the game's bitness and the one graphics API it runs on.",
            ),
            row([
                accent_button("Save")
                    .enabled(draft.is_dirty() && !busy)
                    .name("Save game settings")
                    .on_click(Message::SaveEdits),
                button("Cancel")
                    .enabled(draft.is_dirty() && !busy)
                    .name("Cancel game settings changes")
                    .on_click(Message::CancelEdits),
                icon_text_button(g::REFRESH, "Reset to detected")
                    .enabled(game.edits.is_some() && !busy)
                    .name("Reset game settings")
                    .on_click(Message::ResetEdits),
            ])
            .gap(8.0)
            .padding(crate::ui::el::Edges::new(0.0, 6.0, 0.0, 0.0)),
        ])
    }

    /// The game's store, folder, program and launch, as found.
    fn details(&self, game: &Game) -> El<Message> {
        let program = game.program.as_deref().map_or_else(
            || "Unknown".to_owned(),
            |program| {
                let shown = game
                    .install_dir
                    .as_deref()
                    .and_then(|dir| program.strip_prefix(dir).ok())
                    .unwrap_or(program);
                shown.display().to_string()
            },
        );
        let line = |label: &str, value: String| {
            col([
                text(label)
                    .text_size(theme::TEXT_CAPTION)
                    .weight(700)
                    .color(theme::TEXT_TERTIARY),
                text(value).wrap(),
            ])
            .gap(2.0)
        };
        card([
            card_header(g::INFO, theme::SLATE, "Details", ""),
            line("STORE", game.store.name().to_owned()),
            line("GAME PROGRAM", program),
            line(
                "BITNESS",
                game.arch
                    .map_or_else(|| "Unknown".to_owned(), |arch| arch.label().to_owned()),
            ),
        ])
    }
}

fn tab(
    icon: &'static str,
    tint: ColorF,
    label: &str,
    selected: bool,
    tab: GameTab,
) -> Tab<Message> {
    Tab {
        icon,
        tint,
        label: label.to_owned(),
        selected,
        message: Message::Tab(tab),
    }
}

/// A store's chip colour.
fn store_tint(store: Store) -> ColorF {
    match store {
        Store::Steam => theme::BLUE,
        Store::Epic => theme::SLATE,
        Store::Gog => theme::VIOLET,
        Store::Ea => theme::ORANGE,
        Store::Xbox => theme::GREEN,
        Store::Custom => theme::AMBER,
    }
}

/// A page's content, scrolled and no wider than `PAGE_MAX_WIDTH`.
fn page(content: El<Message>) -> El<Message> {
    scroll(
        "page",
        col([content
            .fill_width()
            .max_width(PAGE_MAX_WIDTH)
            .halign(Align::Center)])
        .pad(24.0),
    )
    .grow()
}
