//! ReShade ▸ Global Shaders and Global Addons: the panels the top bar's ReShade button opens.
//! WinUI's ContentDialog is at most about 548 px wide, too narrow for the pack list, so a panel is
//! drawn by the app as the window's last layer, as the game menu is: a dimmed backdrop that takes
//! every click, and the panel centred on it.
//!
//! - **Global Shaders** edits a `ShadersDraft`, UI-free and unit-tested: whole packs, or single
//!   effects of a pack (▶ lists them, downloading the pack first). Essential packs are always
//!   deployed, so their box is checked and cannot be cleared. Escape and Cancel close it; Deploy
//!   saves the selection and brings it to every game with ReShade (`reshade::deploy`).
//! - **Global Addons** lists every add-on with an On/Off switch. A switch takes effect when the
//!   panel closes (Close or Escape): the choice is saved and deployed like the shaders.

use crate::mods_view::ModKind;
use crate::{GameManager, Message, icon_label};
use gm_core::mods::reshade;
use gm_core::mods::reshade_addons::{self, Addon};
use gm_core::mods::reshade_settings::{self, GlobalSettings};
use gm_core::mods::shader_packs::{self, Catalog, Category, Pick, Selection};
use std::collections::{BTreeSet, HashMap};
use windows_reactor::*;

/// The user's own shaders, listed like a pack.
pub(crate) const CUSTOM: &str = "__custom__";
/// The panel's largest size, and its margin from the window's edges.
const PANEL_WIDTH: f64 = 1100.0;
const PANEL_HEIGHT: f64 = 860.0;
const PANEL_MARGIN: f64 = 32.0;

/// What the Global Shaders panel reads from the disk.
#[derive(Clone)]
pub(crate) struct ShaderLibrary {
    pub catalog: Catalog,
    pub settings: GlobalSettings,
    /// Each cached pack's effects (`Shaders\Bloom.fx`), by id; `CUSTOM` holds the user's own.
    pub effects: HashMap<String, Vec<String>>,
}

/// The library, read in the background. Blocking.
pub(crate) fn load_library() -> Result<ShaderLibrary, String> {
    let catalog = shader_packs::catalog().map_err(|e| e.to_string())?;
    let settings = reshade_settings::load().map_err(|e| e.to_string())?;
    let mut effects: HashMap<String, Vec<String>> = catalog
        .packs
        .iter()
        .filter(|pack| shader_packs::is_cached(&pack.id))
        .map(|pack| (pack.id.clone(), shader_packs::effects(&pack.id)))
        .collect();
    effects.insert(CUSTOM.to_owned(), shader_packs::custom_effects());
    Ok(ShaderLibrary {
        catalog,
        settings,
        effects,
    })
}

/// What the Global Addons panel reads: every add-on, the user's choices, and what is cached.
#[derive(Clone)]
pub(crate) struct AddonLibrary {
    pub addons: Vec<Addon>,
    pub settings: GlobalSettings,
    /// The cached add-ons, by id, with their version when they have one.
    pub cached: HashMap<String, Option<String>>,
}

/// The add-on list, read in the background (the custom folder is made, for its link). Blocking:
/// one web request.
pub(crate) fn load_addons() -> Result<AddonLibrary, String> {
    if let Ok(folder) = reshade_addons::custom_dir() {
        let _ = std::fs::create_dir_all(folder);
    }
    let addons = reshade_addons::list().map_err(|e| e.to_string())?;
    let settings = reshade_settings::load().map_err(|e| e.to_string())?;
    let cached = addons
        .iter()
        .filter(|addon| reshade_addons::is_cached(addon))
        .map(|addon| (addon.id.clone(), reshade_addons::version(addon)))
        .collect();
    Ok(AddonLibrary {
        addons,
        settings,
        cached,
    })
}

/// The open panel.
pub(crate) enum ReshadePanel {
    /// Global Shaders; `None` while the library loads.
    Shaders(Option<ShadersDraft>),
    /// Global Addons: the ids switched on; `None` while the list loads.
    Addons(Option<BTreeSet<String>>),
}

/// The Global Shaders being chosen.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ShadersDraft {
    pub selection: Selection,
    pub search: String,
    /// The packs whose effects are listed (`CUSTOM` too).
    pub expanded: BTreeSet<String>,
}

impl ShadersDraft {
    pub(crate) fn new(selection: Selection) -> Self {
        Self {
            selection,
            ..Self::default()
        }
    }

    fn pick(&self, id: &str) -> Option<&Pick> {
        if id == CUSTOM {
            self.selection.custom.as_ref()
        } else {
            self.selection.packs.get(id)
        }
    }

    fn set(&mut self, id: &str, pick: Option<Pick>) {
        if id == CUSTOM {
            self.selection.custom = pick;
        } else if let Some(pick) = pick {
            self.selection.packs.insert(id.to_owned(), pick);
        } else {
            self.selection.packs.remove(id);
        }
    }

    /// A pack's box: on, off, or `None` when some of its `effects` are picked.
    pub(crate) fn state(&self, id: &str, effects: &[String]) -> Option<bool> {
        match self.pick(id) {
            None => Some(false),
            Some(Pick { files: None }) => Some(true),
            Some(Pick { files: Some(files) }) => {
                let all = !effects.is_empty()
                    && effects
                        .iter()
                        .all(|effect| files.iter().any(|file| file.eq_ignore_ascii_case(effect)));
                all.then_some(true)
            }
        }
    }

    pub(crate) fn check_pack(&mut self, id: &str, on: bool) {
        self.set(id, on.then(Pick::default));
    }

    /// Whether the effect `file` of the pack is picked.
    pub(crate) fn takes(&self, id: &str, file: &str) -> bool {
        self.pick(id).is_some_and(|pick| pick.takes(file))
    }

    /// Picks or leaves out one of a pack's `effects`. All of them picked is the whole pack; none
    /// leaves the pack out.
    pub(crate) fn check_effect(&mut self, id: &str, file: &str, on: bool, effects: &[String]) {
        let mut files: Vec<String> = match self.pick(id) {
            None => Vec::new(),
            Some(Pick { files: None }) => effects.to_vec(),
            Some(Pick { files: Some(files) }) => files.clone(),
        };
        files.retain(|picked| !picked.eq_ignore_ascii_case(file));
        if on {
            files.push(file.to_owned());
        }
        let all = !effects.is_empty()
            && effects.iter().all(|effect| {
                files
                    .iter()
                    .any(|picked| picked.eq_ignore_ascii_case(effect))
            });
        let pick = if files.is_empty() {
            None
        } else if all {
            Some(Pick::default())
        } else {
            files.sort();
            Some(Pick { files: Some(files) })
        };
        self.set(id, pick);
    }

    pub(crate) fn deselect_all(&mut self) {
        self.selection.packs.clear();
        self.selection.custom = None;
    }
}

/// Whether `text` contains the search, ignoring case.
fn matches(text: &str, search: &str) -> bool {
    text.to_lowercase().contains(&search.to_lowercase())
}

/// An effect's name as listed (`Bloom.fx` for `Shaders\Bloom.fx`).
fn effect_name(file: &str) -> &str {
    file.strip_prefix(r"Shaders\").unwrap_or(file)
}

impl GameManager {
    /// The top bar's ReShade button: Global Shaders, then Global Addons.
    pub(crate) fn reshade_button(&self, context: &ViewContext<Self>) -> View {
        // Messages cross threads, so the key travels as text.
        let menu = Menu::new(
            [
                MenuItem::item("shaders", "Global Shaders"),
                MenuItem::item("addons", "Global Addons"),
            ],
            context.callback(|key: Key| {
                Message::ReshadeMenu(key.as_str().unwrap_or_default().to_owned())
            }),
        );
        DropDownButton::new()
            .is_enabled(self.mod_job.is_none() && !self.dialog_open())
            .automation_name("ReShade")
            .content(icon_label(Symbol::Pictures, "ReShade"))
            .menu(menu)
    }

    /// Opens what the ReShade button's menu picked.
    pub(crate) fn open_reshade_panel(&mut self, key: &str, context: &ComponentContext<Self>) {
        match key {
            "shaders" => {
                self.reshade_panel = Some(ReshadePanel::Shaders(None));
                context.spawn_background(|_cancel| Message::ShaderLibraryLoaded(load_library()));
            }
            "addons" => {
                self.reshade_panel = Some(ReshadePanel::Addons(None));
                context.spawn_background(|_cancel| Message::AddonLibraryLoaded(load_addons()));
            }
            _ => {}
        }
    }

    /// The Global Addons panel's messages.
    pub(crate) fn handle_addons(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::AddonLibraryLoaded(loaded) => {
                match (&loaded, &mut self.reshade_panel) {
                    (Ok(library), Some(ReshadePanel::Addons(draft @ None))) => {
                        *draft = Some(library.settings.addons.iter().cloned().collect());
                    }
                    (Err(error), _) => {
                        self.error = Some(format!("Global Addons: {error}"));
                        self.reshade_panel = None;
                    }
                    _ => {}
                }
                self.addon_library = Some(loaded);
            }
            Message::AddonSwitched(id, on) => {
                if let Some(ReshadePanel::Addons(Some(enabled))) = &mut self.reshade_panel {
                    if on {
                        enabled.insert(id);
                    } else {
                        enabled.remove(&id);
                    }
                }
            }
            Message::OpenAddonFolder => {
                self.error = reshade_addons::custom_dir()
                    .and_then(|folder| gm_core::open_folder(&folder, None))
                    .err()
                    .map(|e| e.to_string());
            }
            Message::CloseAddons => self.apply_addons(context),
            _ => {}
        }
    }

    /// Closes Global Addons; a changed choice is saved and deployed to every game with ReShade.
    fn apply_addons(&mut self, context: &ComponentContext<Self>) {
        let panel = self.reshade_panel.take();
        let (Some(ReshadePanel::Addons(Some(enabled))), Some(Ok(library))) =
            (panel, &self.addon_library)
        else {
            return;
        };
        let mut settings = library.settings.clone();
        let chosen: Vec<String> = enabled.into_iter().collect();
        let mut before = settings.addons.clone();
        before.sort();
        if chosen == before {
            return;
        }
        settings.addons = chosen;
        // The shaders' copy of the settings must not bring back the old add-ons.
        if let Some(Ok(shaders)) = &mut self.shader_library {
            shaders.settings.addons = settings.addons.clone();
        }
        let games = self.games.clone();
        self.mod_job = Some((String::new(), ModKind::Deploy));
        context.spawn_background(move |_cancel| {
            let done = reshade_settings::save(&settings).and_then(|()| reshade::deploy(&games));
            Message::ModJobDone(done.map_err(|e| e.to_string()))
        });
    }

    /// The Global Shaders panel's messages.
    pub(crate) fn handle_shaders(&mut self, message: Message, context: &ComponentContext<Self>) {
        let library = self
            .shader_library
            .as_ref()
            .and_then(|library| library.as_ref().ok());
        let draft = match &mut self.reshade_panel {
            Some(ReshadePanel::Shaders(Some(draft))) => Some(draft),
            _ => None,
        };
        match message {
            Message::ShaderLibraryLoaded(loaded) => {
                if let (Ok(library), Some(ReshadePanel::Shaders(draft @ None))) =
                    (&loaded, &mut self.reshade_panel)
                {
                    *draft = Some(ShadersDraft::new(
                        library.settings.selection(&library.catalog),
                    ));
                }
                if let Err(error) = &loaded {
                    self.error = Some(format!("Global Shaders: {error}"));
                    self.reshade_panel = None;
                }
                self.shader_library = Some(loaded);
            }
            Message::ShaderPackChecked(id, Some(on)) => {
                if let Some(draft) = draft {
                    draft.check_pack(&id, on);
                }
            }
            Message::ShaderEffectChecked(id, file, Some(on)) => {
                let effects = library
                    .and_then(|library| library.effects.get(&id))
                    .cloned()
                    .unwrap_or_default();
                if let Some(draft) = draft {
                    draft.check_effect(&id, &file, on, &effects);
                }
            }
            Message::ShaderPackExpanded(id) => {
                let Some(draft) = draft else {
                    return;
                };
                if !draft.expanded.remove(&id) {
                    draft.expanded.insert(id.clone());
                    self.download_pack(&id, context);
                }
            }
            Message::ShaderExpandAll => {
                let Some(library) = library else {
                    return;
                };
                let mut ids: Vec<String> = library
                    .catalog
                    .packs
                    .iter()
                    .map(|pack| pack.id.clone())
                    .collect();
                ids.push(CUSTOM.to_owned());
                let Some(draft) = draft else {
                    return;
                };
                if ids.iter().all(|id| draft.expanded.contains(id)) {
                    draft.expanded.clear();
                } else {
                    draft.expanded.extend(ids.iter().cloned());
                    for id in &ids {
                        self.download_pack(id, context);
                    }
                }
            }
            Message::ShaderDeselectAll => {
                if let Some(draft) = draft {
                    draft.deselect_all();
                }
            }
            Message::ShaderSearch(text) => {
                if let Some(draft) = draft {
                    draft.search = text;
                }
            }
            Message::ShaderPackDownloaded(id, effects) => {
                self.packs_downloading.remove(&id);
                match effects {
                    Ok(effects) => {
                        self.pack_errors.remove(&id);
                        if let Some(Ok(library)) = &mut self.shader_library {
                            library.effects.insert(id, effects);
                        }
                    }
                    Err(error) => {
                        self.pack_errors.insert(id, error);
                    }
                }
            }
            Message::DeployShaders => self.deploy_shaders(context),
            // Escape on Global Addons closes it like its Close button: the switches apply.
            Message::CloseReshadePanel
                if matches!(self.reshade_panel, Some(ReshadePanel::Addons(_))) =>
            {
                self.apply_addons(context);
            }
            Message::CloseReshadePanel => self.reshade_panel = None,
            _ => {}
        }
    }

    /// Downloads a pack that is not cached, to list its effects.
    fn download_pack(&mut self, id: &str, context: &ComponentContext<Self>) {
        let Some(Ok(library)) = &self.shader_library else {
            return;
        };
        if id == CUSTOM || library.effects.contains_key(id) || self.packs_downloading.contains(id) {
            return;
        }
        let catalog = library.catalog.clone();
        let id = id.to_owned();
        self.packs_downloading.insert(id.clone());
        context.spawn_background(move |_cancel| {
            let effects = catalog
                .pack(&id)
                .ok_or_else(|| format!("{id} is not in the pack list"))
                .and_then(|pack| {
                    shader_packs::ensure(&catalog, pack, false).map_err(|e| e.to_string())
                })
                .map(|_| shader_packs::effects(&id));
            Message::ShaderPackDownloaded(id, effects)
        });
    }

    /// Saves the selection and deploys it to every game with ReShade.
    fn deploy_shaders(&mut self, context: &ComponentContext<Self>) {
        let (Some(ReshadePanel::Shaders(Some(draft))), Some(Ok(library))) =
            (&self.reshade_panel, &self.shader_library)
        else {
            return;
        };
        let mut settings = library.settings.clone();
        settings.shaders = Some(draft.selection.clone());
        let games = self.games.clone();
        if let Some(Ok(library)) = &mut self.shader_library {
            library.settings = settings.clone();
        }
        self.reshade_panel = None;
        self.mod_job = Some((String::new(), ModKind::Deploy));
        context.spawn_background(move |_cancel| {
            let done = reshade_settings::save(&settings).and_then(|()| reshade::deploy(&games));
            Message::ModJobDone(done.map_err(|e| e.to_string()))
        });
    }

    /// The panel layer, while one is open: a dimmed backdrop and the panel centred on it.
    pub(crate) fn reshade_panel_view(&self, context: &ViewContext<Self>) -> Option<View> {
        let panel = self.reshade_panel.as_ref()?;
        let (width, height) = self.window_size;
        let width = (width - 2.0 * PANEL_MARGIN).clamp(480.0, PANEL_WIDTH);
        let height = (height - 2.0 * PANEL_MARGIN).clamp(360.0, PANEL_HEIGHT);
        let content = match panel {
            ReshadePanel::Shaders(draft) => self.shaders_panel(draft.as_ref(), context),
            ReshadePanel::Addons(enabled) => self.addons_panel(enabled.as_ref(), context),
        };
        let card = Border::new()
            .width(width)
            .height(height)
            .padding(Thickness::uniform(28.0))
            .corner_radius(8.0)
            .background(ThemeBrush::SolidBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::uniform(1.0))
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .content(content);
        // The backdrop has a background, so it takes the clicks meant for what is under it.
        let backdrop = Border::new().background(Color::argb(0x99, 0, 0, 0));
        Some(
            Grid::new()
                .grid_row(0)
                .grid_row_span(3)
                .children((backdrop, card))
                .into(),
        )
    }

    /// ReShade Addon Manager: the custom folder's link, a card per add-on, and Close.
    fn addons_panel(
        &self,
        enabled: Option<&BTreeSet<String>>,
        context: &ViewContext<Self>,
    ) -> View {
        let title = TextBlock::new()
            .text("ReShade Addon Manager")
            .font_size(28.0)
            .font_weight(FontWeight::SEMI_BOLD);
        let header = StackPanel::new().spacing(4.0).grid_row(0).children((
            title,
            HyperlinkButton::new()
                .content("Place custom .addon64/.addon32 files here")
                .on_click(context.message(Message::OpenAddonFolder))
                .automation_name("Place custom add-on files here"),
            note(
                "A switch takes effect when this closes: the add-ons switched on go into every \
                     game with ReShade, next to its program. Games with the No Addons build get \
                     none.",
            ),
        ));
        let library = self
            .addon_library
            .as_ref()
            .and_then(|library| library.as_ref().ok());
        let body: View = match (enabled, library) {
            (Some(enabled), Some(library)) => {
                let cards: Vec<View> = library
                    .addons
                    .iter()
                    .map(|addon| addon_card(addon, enabled.contains(&addon.id), library, context))
                    .collect();
                ScrollViewer::new()
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(
                        StackPanel::new()
                            .spacing(12.0)
                            .margin(Thickness::new(0.0, 0.0, 16.0, 0.0))
                            .children(cards),
                    )
                    .into()
            }
            _ => StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .children((
                    ProgressRing::new().is_active(true).width(20.0).height(20.0),
                    TextBlock::new().text("Reading the add-on list…"),
                ))
                .into(),
        };
        let close = Button::new()
            .horizontal_alignment(HorizontalAlignment::Right)
            .min_width(280.0)
            .on_click(context.message(Message::CloseAddons))
            .automation_name("Close")
            .content("Close")
            .grid_row(2);
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
            .row_spacing(20.0)
            .children((header, Border::new().grid_row(1).content(body), close))
            .into()
    }

    fn shaders_panel(&self, draft: Option<&ShadersDraft>, context: &ViewContext<Self>) -> View {
        let title = TextBlock::new()
            .text("Select Shader Packs")
            .font_size(28.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .grid_row(0);
        let library = self
            .shader_library
            .as_ref()
            .and_then(|library| library.as_ref().ok());
        let body: View = match (draft, library) {
            (Some(draft), Some(library)) => self.shader_list(draft, library, context),
            _ => StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .children((
                    ProgressRing::new().is_active(true).width(20.0).height(20.0),
                    TextBlock::new().text("Reading the shader packs…"),
                ))
                .into(),
        };
        let buttons = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Star(1.0)])
            .column_spacing(16.0)
            .grid_row(2)
            .children((
                Button::new()
                    .is_enabled(draft.is_some())
                    .style(ButtonStyle::Accent)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_click(context.message(Message::DeployShaders))
                    .automation_name("Deploy")
                    .content("Deploy")
                    .grid_column(0)
                    .tooltip("Save the selection and put it into every game with ReShade"),
                Button::new()
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .on_click(context.message(Message::CloseReshadePanel))
                    .automation_name("Cancel")
                    .content("Cancel")
                    .grid_column(1),
            ));
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
            .row_spacing(20.0)
            .children((title, Border::new().grid_row(1).content(body), buttons))
            .into()
    }

    /// The toolbar and the packs by category.
    fn shader_list(
        &self,
        draft: &ShadersDraft,
        library: &ShaderLibrary,
        context: &ViewContext<Self>,
    ) -> View {
        let all_ids = library.catalog.packs.len() + 1;
        let expand_label = if draft.expanded.len() >= all_ids {
            "Collapse All"
        } else {
            "Expand All"
        };
        let toolbar = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .grid_row(0)
            .children((
                Button::new()
                    .on_click(context.message(Message::ShaderExpandAll))
                    .automation_name(expand_label)
                    .content(expand_label),
                Button::new()
                    .on_click(context.message(Message::ShaderDeselectAll))
                    .automation_name("Deselect All")
                    .content("Deselect All"),
                TextBox::new()
                    .text(draft.search.as_str())
                    .placeholder_text("Search packs or shaders…")
                    .width(320.0)
                    .automation_name("Search packs or shaders")
                    .on_text_changed(context.callback(|text: std::rc::Rc<str>| {
                        Message::ShaderSearch(text.to_string())
                    })),
                Button::new()
                    .is_enabled(!draft.search.is_empty())
                    .on_click(context.message(Message::ShaderSearch(String::new())))
                    .automation_name("Clear the search")
                    .content(SymbolIcon::new().symbol(Symbol::Cancel)),
            ));

        let mut rows: Vec<View> = Vec::new();
        for category in [Category::Essential, Category::Recommended] {
            self.category_rows(&mut rows, category, draft, library, context);
        }
        // The user's own shaders sit between Recommended and Extra.
        let custom = library.effects.get(CUSTOM).cloned().unwrap_or_default();
        let custom_shown = draft.search.is_empty()
            || matches("General", &draft.search)
            || custom.iter().any(|file| matches(file, &draft.search));
        if custom_shown {
            rows.push(heading("Custom Shaders"));
            rows.push(self.pack_row(
                CUSTOM,
                "General",
                "Your own shaders, in Game Manager's custom folder",
                false,
                &custom,
                true,
                draft,
                context,
            ));
        }
        self.category_rows(&mut rows, Category::Extra, draft, library, context);
        if rows.is_empty() {
            rows.push(
                TextBlock::new()
                    .text("No pack or shader matches the search.")
                    .opacity(0.7)
                    .into(),
            );
        }
        let list = ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .grid_row(1)
            .content(
                StackPanel::new()
                    .spacing(14.0)
                    // Room for the scroll bar, so it never covers the rows.
                    .margin(Thickness::new(0.0, 0.0, 16.0, 0.0))
                    .children(rows),
            );
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(16.0)
            .children((toolbar, list))
            .into()
    }

    /// A category's heading and the packs in it that match the search, by name.
    fn category_rows(
        &self,
        rows: &mut Vec<View>,
        category: Category,
        draft: &ShadersDraft,
        library: &ShaderLibrary,
        context: &ViewContext<Self>,
    ) {
        let mut packs: Vec<_> = library
            .catalog
            .packs
            .iter()
            .filter(|pack| pack.category == category)
            .filter(|pack| {
                let effects = library
                    .effects
                    .get(&pack.id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                draft.search.is_empty()
                    || matches(&pack.name, &draft.search)
                    || matches(&pack.description, &draft.search)
                    || effects.iter().any(|file| matches(file, &draft.search))
            })
            .collect();
        if packs.is_empty() {
            return;
        }
        packs.sort_by_key(|pack| pack.name.to_lowercase());
        rows.push(heading(category.label()));
        for pack in packs {
            let effects = library.effects.get(&pack.id).cloned();
            let cached = effects.is_some();
            rows.push(self.pack_row(
                &pack.id,
                &pack.name,
                &pack.description,
                category == Category::Essential,
                &effects.unwrap_or_default(),
                cached,
                draft,
                context,
            ));
        }
    }

    /// One pack: its box, name, ✓ when cached, ▶ for its effects, and its description; when
    /// expanded, its effects with their own boxes.
    #[allow(
        clippy::too_many_arguments,
        reason = "one row's facts, from two places"
    )]
    fn pack_row(
        &self,
        id: &str,
        name: &str,
        description: &str,
        essential: bool,
        effects: &[String],
        cached: bool,
        draft: &ShadersDraft,
        context: &ViewContext<Self>,
    ) -> View {
        let state = if essential {
            Some(true)
        } else {
            draft.state(id, effects)
        };
        let expanded = draft.expanded.contains(id)
            || (!draft.search.is_empty()
                && effects.iter().any(|file| matches(file, &draft.search)));
        let key = id.to_owned();
        let check = CheckBox::new()
            .is_checked(state)
            .is_enabled(!essential)
            .automation_name(name)
            .vertical_alignment(VerticalAlignment::Top)
            .margin(Thickness::new(0.0, 2.0, 0.0, 0.0))
            .on_is_checked_changed(
                context
                    .callback(move |on: Option<bool>| Message::ShaderPackChecked(key.clone(), on)),
            )
            .grid_column(0);
        let downloading = self.packs_downloading.contains(id);
        let mut title: Vec<View> = vec![
            TextBlock::new()
                .text(name)
                .font_size(16.0)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        ];
        if cached && id != CUSTOM {
            title.push(
                FontIcon::new()
                    .glyph("\u{E73E}")
                    .foreground(ThemeBrush::Accent)
                    .vertical_alignment(VerticalAlignment::Center)
                    .tooltip("Downloaded"),
            );
        }
        let expand_glyph = if expanded { "\u{E70D}" } else { "\u{E76C}" };
        title.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .on_click(context.message(Message::ShaderPackExpanded(id.to_owned())))
                .automation_name(format!("Effects of {name}"))
                .content(FontIcon::new().glyph(expand_glyph).font_size(12.0))
                .into(),
        );
        if downloading {
            title.push(
                ProgressRing::new()
                    .is_active(true)
                    .width(16.0)
                    .height(16.0)
                    .into(),
            );
        }
        let mut lines: Vec<View> = vec![
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(10.0)
                .children(title)
                .into(),
            TextBlock::new()
                .text(description)
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        ];
        if expanded {
            let shown: Vec<&String> = effects
                .iter()
                .filter(|file| {
                    draft.search.is_empty()
                        || matches(file, &draft.search)
                        || matches(name, &draft.search)
                })
                .collect();
            if let Some(error) = self.pack_errors.get(id) {
                lines.push(note(&format!("Could not download it: {error}")));
            } else if downloading || (!cached && id != CUSTOM) {
                lines.push(note("Downloading the pack to list its shaders…"));
            } else if shown.is_empty() {
                lines.push(note(if id == CUSTOM {
                    "No shaders in the custom folder yet."
                } else {
                    "This pack has no effects."
                }));
            }
            let mut boxes: Vec<View> = Vec::new();
            for file in shown {
                let (pack, effect) = (id.to_owned(), file.clone());
                boxes.push(
                    CheckBox::new()
                        .is_checked(essential || draft.takes(id, file))
                        .is_enabled(!essential)
                        .content(effect_name(file))
                        .automation_name(effect_name(file))
                        .on_is_checked_changed(context.callback(move |on: Option<bool>| {
                            Message::ShaderEffectChecked(pack.clone(), effect.clone(), on)
                        }))
                        .into(),
                );
            }
            if !boxes.is_empty() {
                lines.push(
                    StackPanel::new()
                        .spacing(0.0)
                        .margin(Thickness::new(4.0, 4.0, 0.0, 0.0))
                        .children(boxes)
                        .into(),
                );
            }
        }
        Grid::new()
            .columns([GridLength::Auto, GridLength::Star(1.0)])
            .column_spacing(12.0)
            .children((
                check,
                StackPanel::new()
                    .spacing(4.0)
                    .grid_column(1)
                    .children(lines),
            ))
            .into()
    }
}

/// One add-on: its name, version ✓ when downloaded, description and How to use, and its switch.
fn addon_card(
    addon: &Addon,
    on: bool,
    library: &AddonLibrary,
    context: &ViewContext<GameManager>,
) -> View {
    let mut title: Vec<View> = vec![
        TextBlock::new()
            .text(addon.name.as_str())
            .font_size(16.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    ];
    if let Some(version) = library.cached.get(&addon.id) {
        if let Some(version) = version {
            title.push(
                TextBlock::new()
                    .text(version.as_str())
                    .opacity(0.7)
                    .vertical_alignment(VerticalAlignment::Center)
                    .into(),
            );
        }
        title.push(
            FontIcon::new()
                .glyph("\u{E73E}")
                .foreground(ThemeBrush::Accent)
                .vertical_alignment(VerticalAlignment::Center)
                .tooltip("Downloaded"),
        );
    }
    let mut lines: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(10.0)
            .children(title)
            .into(),
        TextBlock::new()
            .text(addon.description.as_str())
            .opacity(0.7)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    ];
    if let Some(page) = &addon.repository {
        lines.push(
            HyperlinkButton::new()
                .content("How to use")
                .on_click(context.message(Message::OpenModPage(page.clone())))
                .automation_name(format!("How to use {}", addon.name))
                .into(),
        );
    }
    let id = addon.id.clone();
    // Named after its add-on, for screen readers.
    let switch = ToggleSwitch::new()
        .is_on(on)
        .on_content("On")
        .off_content("Off")
        .automation_name(addon.name.as_str())
        .vertical_alignment(VerticalAlignment::Center)
        .on_toggled(context.callback(move |on: bool| Message::AddonSwitched(id.clone(), on)));
    Border::new()
        .padding(Thickness::uniform(16.0))
        .corner_radius(8.0)
        .background(ThemeBrush::CardBackground)
        .border_brush(ThemeBrush::CardStroke)
        .border_thickness(Thickness::uniform(1.0))
        .content(
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .column_spacing(24.0)
                .children((
                    StackPanel::new()
                        .spacing(4.0)
                        .grid_column(0)
                        .children(lines),
                    Border::new().grid_column(1).content(switch),
                )),
        )
        .into()
}

fn heading(text: &str) -> View {
    TextBlock::new()
        .text(text)
        .font_size(20.0)
        .font_weight(FontWeight::SEMI_BOLD)
        .margin(Thickness::new(0.0, 10.0, 0.0, 0.0))
        .into()
}

fn note(text: &str) -> View {
    TextBlock::new()
        .text(text)
        .font_size(12.0)
        .opacity(0.7)
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effects() -> Vec<String> {
        [r"Shaders\A.fx", r"Shaders\B.fx", r"Shaders\C.fx"]
            .map(String::from)
            .to_vec()
    }

    #[test]
    fn packs_and_single_effects_are_picked() {
        let effects = effects();
        let mut draft = ShadersDraft::default();
        assert_eq!(draft.state("P", &effects), Some(false));
        draft.check_pack("P", true);
        assert_eq!(draft.state("P", &effects), Some(true));
        assert!(draft.takes("P", r"Shaders\B.fx"));

        // Leaving one out of a whole pack keeps the others.
        draft.check_effect("P", r"Shaders\B.fx", false, &effects);
        assert_eq!(draft.state("P", &effects), None);
        assert!(!draft.takes("P", r"Shaders\B.fx") && draft.takes("P", r"Shaders\A.fx"));
        // Picking it again makes the whole pack again.
        draft.check_effect("P", r"Shaders\B.fx", true, &effects);
        assert_eq!(draft.selection.packs["P"], Pick::default());
        // Leaving every one out leaves the pack out.
        for file in &effects {
            draft.check_effect("P", file, false, &effects);
        }
        assert!(!draft.selection.packs.contains_key("P"));
        // One effect of a pack that was not picked.
        draft.check_effect("Q", r"Shaders\C.fx", true, &effects);
        assert_eq!(
            draft.selection.packs["Q"].files.as_deref(),
            Some(&[r"Shaders\C.fx".to_owned()][..])
        );

        // The user's own shaders work alike.
        draft.check_pack(CUSTOM, true);
        assert_eq!(draft.selection.custom, Some(Pick::default()));
        draft.deselect_all();
        assert_eq!(draft.selection, Selection::default());
        assert_eq!(effect_name(r"Shaders\Sub\X.fx"), r"Sub\X.fx");
        assert!(matches("SweetFX by CeeJay.dk", "sweet"));
    }
}
