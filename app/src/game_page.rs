//! The page beside the sidebar: the Library grid (`library_page.rs`), the selected game, or My PC
//! (`my_pc_page.rs`).
//!
//! The game page: its header is a band across the page from edge to edge, on every game
//! (`page_art.rs`): the game's wide picture, the user's own or the store's (else its cover, else
//! its icon, blurred and calmed with a shade; the store's colour until one is ready), cut to fill
//! it, as high as a share of
//! its width (`theme::ART_BAND_*`), darker on the left and towards the text's bottom (white text
//! must read on light art: Half-Life 2's fog), and fading out from
//! `theme::ART_FADE_FROM` down (`El::fade_out`, an opacity mask, so it melts into Mica as into the
//! base colour). Over it, above the fade, the logo (else the icon and the name in the hero size),
//! the folder and the chips, in the page's column, in the dark palette whatever the theme: art is
//! mostly dark, and logos are drawn for it. The band's top right has the Artwork button
//! (`widgets::picture_icon_button`: the Artwork dialog on its Banner tab, `artwork.rs`). Then a
//! launch bar with the Play button, the alternative launches, Folder, PCGamingWiki and Nexus Mods
//! (its page found in the background, `gm_core::nexus_mods`, the button waiting meanwhile), Saves
//! and Config (the folders PCGamingWiki names, `gm_core::game_data`, looked up in the background
//! each time the game is selected; disabled, the tooltip saying where the folder would be, that the
//! game keeps it in the registry, or that the wiki lists none), then
//! the tabs and the open tab's cards. A game added by hand is removed from its menu (`game_menu.rs`,
//! Remove from Game Manager), not from here.
//!
//! The tabs: Overview (Game settings, the draft in `edit.rs`: its text values `field`s, Bitness
//! and Graphics API `setting_row`s, then Reset to detected, Save and Cancel in the form's row;
//! beside it the game's details as `facts`), ReShade
//! with its Shaders and Add-ons views (`mods_view.rs`, `reshade_panel.rs`), DXVK & LAA
//! (`mods_view.rs`), Upscalers with its Versions section (`upscalers_view.rs`,
//! `upscaler_versions.rs`), and NVIDIA overrides (`dlss_override_view.rs`), listed only once an
//! NVIDIA driver was found.

use gm_core::appearance::AnimatedCovers;
use gm_core::game_data::{Folder, FolderKind};
use gm_core::graphics::GraphicsSource;
use gm_core::mods::dxvk;
use gm_core::{Game, Launch, Store};
use windows_canvas::ColorF;

use crate::edit::{EditChoice, EditDraft, EditField};
use crate::shell::WindowInfo;
use std::sync::Arc;

use crate::artwork::ArtworkMessage;
use crate::ui::animation::Animation;
use crate::ui::el::{
    Align, Edges, El, Font, Play, Role, col, dropdown, image, layers, paint, row, scroll, spacer,
    text,
};
use crate::ui::image::Picture;
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    PlayState, Tab, accent_button, busy_width, button, card, card_header, chip, facts, field,
    folder_button, hint, icon_text_button, picture_icon_button, play_button, setting_row, tabs,
    text_box, working, working_button,
};
use crate::{CHOSEN, GameManager, GameTab, Message, Page, api_list, folder_tooltip};

/// The widest the page's content grows.
pub(crate) const PAGE_MAX_WIDTH: f32 = 1180.0;

impl GameManager {
    pub(crate) fn page_view(&self, window: &WindowInfo) -> El<Message> {
        match self.page {
            Page::Library => page("library", self.library_page(window)),
            Page::Game => self.game_page(window),
            Page::MyPc => page("my-pc", self.my_pc_page(window)),
            Page::Settings => page("settings", self.settings_page()),
        }
    }

    /// The page's content width: the window less the sidebar, its grip and the page's padding,
    /// at most `PAGE_MAX_WIDTH`.
    pub(crate) fn page_width(&self, window: &WindowInfo) -> f32 {
        (window.size.0 - self.sidebar_width - 52.0).min(PAGE_MAX_WIDTH)
    }

    fn game_page(&self, window: &WindowInfo) -> El<Message> {
        let Some(game) = self.selected_game() else {
            // A lookup still running shows as every one does: a spinner and what it is doing.
            let status = if self.scanning {
                working("Looking for installed games…")
            } else if self.games.is_empty() {
                hint(
                    "No installed games were found. Use Add a game, or drop a game's .exe or its \
                     shortcut here.",
                )
            } else {
                hint("Select a game in the library.")
            };
            return col([status]).center().grow();
        };
        let mut strip = vec![
            tab(
                g::OVERVIEW,
                theme::blue(),
                "Overview",
                self.tab == GameTab::Overview,
                GameTab::Overview,
            ),
            tab(
                g::BRUSH,
                theme::pink(),
                "ReShade",
                self.tab == GameTab::Reshade,
                GameTab::Reshade,
            ),
            tab(
                g::WRENCH,
                theme::orange(),
                "DXVK & LAA",
                self.tab == GameTab::Mods,
                GameTab::Mods,
            ),
            tab(
                g::CHIP,
                theme::cyan(),
                "Upscalers",
                self.tab == GameTab::Upscalers,
                GameTab::Upscalers,
            ),
        ];
        let nvidia = self.nvidia_tab_shown();
        if nvidia {
            strip.push(tab(
                g::NVIDIA,
                theme::green(),
                "NVIDIA overrides",
                self.tab == GameTab::Nvidia,
                GameTab::Nvidia,
            ));
        }
        let body = match self.tab {
            GameTab::Overview => self.overview(game),
            GameTab::Reshade => self.reshade_tab(game),
            GameTab::Mods => self.mods_tab(game),
            GameTab::Upscalers => self.upscalers_tab(game),
            GameTab::Nvidia if nvidia => self.nvidia_tab(game),
            GameTab::Nvidia => self.overview(game),
        };
        let tab_strip = tabs(strip);
        let rest = [self.launch_bar(game), tab_strip, body];
        // The band spans the page from edge to edge, the rest in the page's column below it.
        scroll(
            "page:game",
            col([
                self.art_band(game, window),
                page_column(col(rest).gap(theme::space::L)).padding(Edges::new(
                    theme::space::XL,
                    0.0,
                    theme::space::XL,
                    theme::space::XL,
                )),
            ]),
        )
        .grow()
    }

    /// The band across the top of every game page: the game's wide art (the user's own or the
    /// store's), else its cover or icon blurred, over the store's colour (all that shows until a
    /// picture is ready); fading out at the bottom into whatever is behind (Mica, or the base
    /// colour), with its logo (else its icon and name), folder and chips over it, above the fade,
    /// and the Artwork button at its top right. Art is dark more often than not and logos are
    /// drawn for it, so the band keeps the dark palette in either theme.
    fn art_band(&self, game: &Game, window: &WindowInfo) -> El<Message> {
        let shown = self.page_art.shown(&game.id);
        // Animated art plays unless Settings say covers never do.
        let art_plays = self.appearance.animated_covers != AnimatedCovers::Never;
        let dark = theme::DARK;
        let width = (window.size.0 - self.sidebar_width).max(1.0);
        let height = (width / theme::ART_BAND_RATIO)
            .clamp(theme::ART_BAND_MIN_HEIGHT, theme::ART_BAND_MAX_HEIGHT);
        // Under everything: the base, and the store's colour while there is no picture (an
        // icon's transparent parts show it too).
        let tint = store_tint(game.store, &dark);
        let bare = shown.background.is_none();
        let ground = paint(move |painter, rect, _| {
            painter.fill_rect(rect, dark.base);
            if bare {
                painter.fill_linear(
                    rect,
                    (0.0, 0.0),
                    (1.0, 1.0),
                    &[
                        (0.0, theme::with_alpha(tint, 0x59)),
                        (1.0, theme::with_alpha(tint, 0x0D)),
                    ],
                );
            }
        })
        .fill_width()
        .height(height);
        let mut picture = vec![ground];
        if let Some(background) = shown.background {
            picture.push(
                image(format!("art:{}", game.id), Arc::clone(background))
                    .cover()
                    .animate(
                        shown.background_animation.filter(|_| art_plays),
                        Play::Always,
                    )
                    .fill_width()
                    .height(height),
            );
        }
        let blurred = shown.blurred;
        let scrim = paint(move |painter, rect, _| {
            let shade = |alpha| theme::with_alpha(dark.base, alpha);
            if blurred {
                // A blurred cover is all colour: calmer behind the text.
                painter.fill_rect(rect, shade(0x70));
            }
            // Darker on the left, behind the text.
            painter.fill_linear(
                rect,
                (0.0, 0.0),
                (1.0, 0.0),
                &[(0.0, shade(0xC8)), (0.45, shade(0x60)), (0.8, shade(0))],
            );
            // And darker down to where the text ends, so white text reads on light art too
            // (Half-Life 2's fog); the band's fade then takes it all out.
            painter.fill_linear(
                rect,
                (0.0, 0.0),
                (0.0, 1.0),
                &[
                    (0.3, shade(0)),
                    (theme::ART_FADE_FROM, shade(0xA0)),
                    (1.0, shade(0xA0)),
                ],
            );
        })
        .fill_width()
        .height(height);
        picture.push(scrim);
        let picture = layers(picture)
            .fill_width()
            .height(height)
            .fade_out(theme::ART_FADE_FROM);
        let title = match shown.logo {
            Some(logo) => art_logo(game, logo, shown.logo_animation.filter(|_| art_plays)),
            None => row([
                self.icon(game, crate::icons::LARGE),
                text(game.name.clone())
                    .font(Font::Display)
                    .text_size(theme::TEXT_HERO)
                    .weight(600)
                    .color(dark.text)
                    .wrap()
                    .fill_width(),
            ])
            .gap(theme::space::L),
        };
        let info = col([
            title,
            self.folder_line(game, &dark),
            row(self.chips(game, &dark)).gap(theme::space::S),
        ])
        .gap(theme::space::S)
        .fill_width()
        .max_width(PAGE_MAX_WIDTH)
        .halign(Align::Center);
        // Above the fade, over the picture at its fullest.
        let over = col([info])
            .fill_width()
            .padding(Edges::new(
                theme::space::XL,
                0.0,
                theme::space::XL,
                height * (1.0 - theme::ART_FADE_FROM),
            ))
            .valign(Align::End);
        // The Artwork button, at the band's top right, in the page's column.
        let edit = picture_icon_button(g::PICTURE, format!("Change the artwork of {}", game.name))
            .tooltip("Artwork: the cover, banner, logo and icon")
            .on_click(
                ArtworkMessage::Open(game.id.clone(), gm_core::steamgriddb::Kind::Banner).into(),
            )
            .halign(Align::End);
        let corner = col([edit])
            .fill_width()
            .max_width(PAGE_MAX_WIDTH)
            .halign(Align::Center)
            .valign(Align::Start);
        let corner = col([corner])
            .fill_width()
            .padding(Edges::new(
                theme::space::XL,
                theme::space::M,
                theme::space::XL,
                0.0,
            ))
            .valign(Align::Start);
        layers([picture, over, corner]).fill_width().height(height)
    }

    /// Where the game is installed, as the header's second line.
    fn folder_line(&self, game: &Game, palette: &theme::Palette) -> El<Message> {
        let folder = game.install_dir.as_ref().map_or_else(
            || "Install folder unknown".to_owned(),
            |dir| dir.display().to_string(),
        );
        text(folder)
            .text_size(theme::TEXT_SMALL)
            .color(palette.text_tertiary)
            .wrap()
    }

    /// The store, the bitness, the graphics APIs and DXVK's Vulkan, as chips in `palette`'s tints.
    fn chips(&self, game: &Game, palette: &theme::Palette) -> Vec<El<Message>> {
        let mut chips = vec![chip(game.store.name(), store_tint(game.store, palette))];
        if let Some(arch) = game.arch {
            let chosen = game
                .edits
                .as_ref()
                .is_some_and(|edits| edits.arch.is_some());
            let arch = chip(arch.label(), palette.blue);
            chips.push(if chosen { arch.tooltip(CHOSEN) } else { arch });
        }
        if let Some((apis, tooltip)) = self.graphics_text(game) {
            for api in apis {
                chips.push(chip(api, palette.violet).tooltip(tooltip.clone()));
            }
        }
        if self.runs_on_dxvk(game) {
            chips.push(chip("Vulkan (DXVK)", palette.orange).tooltip(
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
                vec!["Checking…".to_owned()],
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
        items.push(
            icon_text_button(g::LINK, "Nexus Mods")
                .name(format!("Open {} on Nexus Mods", game.name))
                .tooltip("Mods for this game on Nexus Mods")
                .enabled(!self.finding_nexus_mods)
                .on_click(Message::OpenNexusMods(game.id.clone())),
        );
        // Where PCGamingWiki says the game keeps its saves and settings; disabled, with the path
        // or the reason in the tooltip, when it isn't there.
        for (kind, glyph) in [
            (FolderKind::Saves, g::SAVE),
            (FolderKind::Config, g::SETTINGS),
        ] {
            let state = self.game_folders.get(&game.id).map(|folders| {
                folders
                    .as_ref()
                    .map(|folders| folders.get(kind).clone())
                    .map_err(Clone::clone)
            });
            let found = matches!(state, Some(Ok(Folder::Found(_))));
            items.push(
                icon_text_button(glyph, kind.label())
                    .name(format!("Open the {} of {}", kind.noun(), game.name))
                    .tooltip(folder_tooltip(kind, state.as_ref()))
                    .enabled(found)
                    .on_click(Message::OpenGameFolder(game.id.clone(), kind)),
            );
        }
        row(items).gap(theme::space::S)
    }

    fn overview(&self, game: &Game) -> El<Message> {
        row([
            self.game_settings(game).fill_width().valign(Align::Start),
            self.details(game).width(320.0).valign(Align::Start),
        ])
        .gap(theme::space::L)
    }

    /// Game settings: name, launch program, arguments, game program, bitness and graphics API.
    fn game_settings(&self, game: &Game) -> El<Message> {
        let draft = EditDraft::for_game(self.edit.as_ref(), game);
        let busy = self.saving || self.scanning || self.picking;
        let store = game.store.name();
        let edit_box = |field: EditField, key: &str, name: &str, placeholder: String| {
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
            folder_button("Browse…")
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
            dropdown(
                format!("choice:{key}"),
                items,
                Some(draft.fields.choice_index(choice)),
                move |i| Message::EditChoice(choice, Some(i)),
            )
            .enabled(!busy)
            .name(title)
            .width(theme::SETTING_CONTROL_WIDTH)
        };
        let title = if game.edits.is_some() {
            "Game settings (edited)"
        } else {
            "Game settings"
        };
        card([
            card_header(
                g::SETTINGS,
                theme::blue(),
                title,
                "What Play starts, and what mods need to know",
            ),
            field(
                "Name",
                edit_box(EditField::Name, "name", "Game name", String::new()),
                "",
            ),
            field(
                "Launch .exe",
                row([
                    edit_box(
                        EditField::Launch,
                        "launch",
                        "Launch .exe",
                        format!("Empty: the game starts through {store}"),
                    ),
                    browse(EditField::Launch, "Browse for the launch .exe"),
                ])
                .gap(theme::space::S),
                &format!(
                    "The program Play starts. Leave it empty to start the game through {store}."
                ),
            ),
            field(
                "Arguments",
                edit_box(EditField::Args, "args", "Arguments", String::new()).enabled(args_enabled),
                &args_hint,
            ),
            field(
                "Game program",
                row([
                    edit_box(EditField::Program, "program", "Game program", String::new()),
                    browse(EditField::Program, "Browse for the game program"),
                ])
                .gap(theme::space::S),
                "The game's own .exe: used for its bitness, Folder and mods.",
            ),
            setting_row(
                "Bitness",
                "Mods such as ReShade and DXVK need the game's bitness.",
                Some(choice(
                    EditChoice::Bitness,
                    "bitness",
                    "Bitness",
                    detected_arch,
                )),
            ),
            setting_row(
                "Graphics API",
                "And the one graphics API it runs on.",
                Some(choice(
                    EditChoice::GraphicsApi,
                    "api",
                    "Graphics API",
                    detected_api,
                )),
            ),
            // The form's buttons: Reset apart, then Save and Cancel at the right.
            row([
                icon_text_button(g::RESET, "Reset to detected")
                    .enabled(game.edits.is_some() && !busy)
                    .name("Reset game settings")
                    .on_click(Message::ResetEdits),
                spacer(),
                if self.saving {
                    working_button()
                } else {
                    busy_width(accent_button("Save"))
                        .enabled(draft.is_dirty() && !busy)
                        .name("Save game settings")
                        .on_click(Message::SaveEdits)
                },
                busy_width(button("Cancel"))
                    .enabled(draft.is_dirty() && !busy)
                    .name("Cancel game settings changes")
                    .on_click(Message::CancelEdits),
            ])
            .gap(theme::space::S)
            .padding(crate::ui::el::Edges::new(0.0, theme::space::S, 0.0, 0.0)),
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
        card([
            card_header(g::INFO, theme::slate(), "Details", ""),
            facts(vec![
                ("Store", game.store.name().to_owned()),
                ("Game program", program),
                (
                    "Bitness",
                    game.arch
                        .map_or_else(|| "Unknown".to_owned(), |arch| arch.label().to_owned()),
                ),
            ]),
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
fn store_tint(store: Store, palette: &theme::Palette) -> ColorF {
    match store {
        Store::Steam => palette.blue,
        Store::Epic => palette.slate,
        Store::Gog => palette.violet,
        Store::Ea => palette.orange,
        Store::Ubisoft => palette.cyan,
        Store::Xbox => palette.green,
        Store::Custom => palette.amber,
    }
}

/// A page's content, scrolled and no wider than `PAGE_MAX_WIDTH`. Each page keeps its own scroll
/// position (`key`), so the Library doesn't open where a game's page was scrolled to.
fn page(key: &str, content: El<Message>) -> El<Message> {
    scroll(
        format!("page:{key}"),
        page_column(content).pad(theme::space::XL),
    )
    .grow()
}

/// The page's column: as wide as the page, at most `PAGE_MAX_WIDTH`, centred.
fn page_column(content: El<Message>) -> El<Message> {
    col([content
        .fill_width()
        .max_width(PAGE_MAX_WIDTH)
        .halign(Align::Center)])
}

/// A game's logo over its art, as large as fits `ART_LOGO_MAX`, its shape kept, playing when it is
/// animated. UI Automation
/// reads the game's name.
fn art_logo(game: &Game, logo: &Arc<Picture>, animation: Option<&Arc<Animation>>) -> El<Message> {
    let (most_w, most_h) = theme::ART_LOGO_MAX;
    let (w, h) = (logo.width.max(1) as f32, logo.height.max(1) as f32);
    let fit = (most_w / w).min(most_h / h);
    image(format!("art-logo:{}", game.id), Arc::clone(logo))
        .size(w * fit, h * fit)
        .animate(animation, Play::Always)
        .role(Role::Text)
        .name(game.name.clone())
}
