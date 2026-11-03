//! ReShade ▸ Shaders and Addons: the panels the top bar's ReShade button opens.
//! WinUI's ContentDialog is at most about 548 px wide, too narrow for the pack list, so a panel is
//! drawn by the app as the window's last layer, as the game menu is: a dimmed backdrop that takes
//! every click, and the panel centred on it.
//!
//! - **Shaders** edits a `ShadersDraft`, UI-free and unit-tested: whole packs, or single effects
//!   of a pack (▶ lists them, downloading the pack first). Every pack can be left out, Lilium's
//!   too. Escape and Cancel close it; Deploy saves the selection and brings it to the games
//!   (`reshade::deploy`).
//! - **Addons** lists every add-on with an On/Off switch. A switch takes effect when the panel
//!   closes (Close or Escape): the choice is saved and deployed like the shaders.
//!
//! **Apply to**: each panel shows and applies either the own choice of the game selected in the
//! library when it opened ("Only <game>", the first item and the default;
//! `ReshadeSettings::games`), or the shared choice ("All games with ReShade", leaving out the games
//! with their own; the only item when no game is selected). A game without its own choice shows
//! the shared one. "Use all games' selection" drops the game's own choice and deploys the shared
//! one to it.

use crate::mods_view::ModKind;
use crate::{GameManager, Message, icon_label};
use gm_core::mods::reshade::{self, DeployTo};
use gm_core::mods::reshade_addons::{self, Addon};
use gm_core::mods::reshade_settings::{self, Part, ReshadeSettings};
use gm_core::mods::shader_packs::{self, Catalog, Category, Pick, Selection};
use std::collections::{BTreeSet, HashMap};
use windows_pickers::OpenFilePicker;
use windows_reactor::*;

/// The user's own shaders, listed like a pack.
pub(crate) const CUSTOM: &str = "__custom__";
/// The panel's largest size, and its margin from the window's edges.
const PANEL_WIDTH: f64 = 1100.0;
const PANEL_HEIGHT: f64 = 860.0;
const PANEL_MARGIN: f64 = 32.0;

/// What the Shaders panel reads from the disk.
#[derive(Clone)]
pub(crate) struct ShaderLibrary {
    pub catalog: Catalog,
    pub settings: ReshadeSettings,
    /// Each cached pack's effects (`Shaders\Bloom.fx`), by id; `CUSTOM` holds the user's own.
    pub effects: HashMap<String, Vec<String>>,
}

/// The library, read in the background (the custom folder is made, for Open Custom Folder).
/// Blocking.
pub(crate) fn load_library() -> Result<ShaderLibrary, String> {
    if let Ok(custom) = shader_packs::custom_dir() {
        for top in [shader_packs::SHADERS, shader_packs::TEXTURES] {
            let _ = std::fs::create_dir_all(custom.join(top));
        }
    }
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

/// What the Addons panel reads: every add-on, the user's choices, and what is cached.
#[derive(Clone)]
pub(crate) struct AddonLibrary {
    pub addons: Vec<Addon>,
    pub settings: ReshadeSettings,
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
pub(crate) struct ReshadePanel {
    /// The game selected in the library when the panel opened (id, name), for "Only <game>". The
    /// panel's backdrop keeps the selection from changing while it is open.
    pub game: Option<(String, String)>,
    /// The panel shows and applies the game's own choice, not the shared one.
    pub only_game: bool,
    pub kind: PanelKind,
}

pub(crate) enum PanelKind {
    /// Shaders; `None` while the library loads.
    Shaders(Option<ShadersDraft>),
    /// Addons: the ids switched on; `None` while the list loads.
    Addons(Option<BTreeSet<String>>),
}

impl ReshadePanel {
    /// A panel for `game` when one is selected (its own choice), else for every game.
    fn new(game: Option<(String, String)>, kind: PanelKind) -> Self {
        Self {
            only_game: game.is_some(),
            game,
            kind,
        }
    }

    /// The id of the game whose own choice the panel shows; `None` for the shared one.
    fn target(&self) -> Option<&str> {
        self.game
            .as_ref()
            .filter(|_| self.only_game)
            .map(|(id, _)| id.as_str())
    }

    fn part(&self) -> Part {
        match self.kind {
            PanelKind::Shaders(_) => Part::Shaders,
            PanelKind::Addons(_) => Part::Addons,
        }
    }

    fn shaders_draft(&mut self) -> Option<&mut ShadersDraft> {
        match &mut self.kind {
            PanelKind::Shaders(Some(draft)) => Some(draft),
            _ => None,
        }
    }
}

/// The shader selection the panel shows for `target` (a game's id, else the shared one).
fn shown_shaders(library: &ShaderLibrary, target: Option<&str>) -> Selection {
    match target {
        Some(id) => library.settings.shaders_for(id, &library.catalog),
        None => library.settings.selection(&library.catalog),
    }
}

/// The add-ons the panel shows switched on for `target`.
fn shown_addons(settings: &ReshadeSettings, target: Option<&str>) -> BTreeSet<String> {
    target
        .map_or(settings.addons.as_slice(), |id| settings.addons_for(id))
        .iter()
        .cloned()
        .collect()
}

/// The shaders being chosen.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ShadersDraft {
    pub selection: Selection,
    pub search: String,
    /// The packs whose effects are listed (`CUSTOM` too).
    pub expanded: BTreeSet<String>,
    /// The profile being renamed, and the name typed so far.
    pub renaming: Option<(String, String)>,
    /// What Export or Import did, shown under the profiles.
    pub note: Option<String>,
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
    /// The top bar's ReShade button: Shaders, then Addons.
    pub(crate) fn reshade_button(&self, context: &ViewContext<Self>) -> View {
        // Messages cross threads, so the key travels as text.
        let menu = Menu::new(
            [
                MenuItem::item("shaders", "Shaders"),
                MenuItem::item("addons", "Addons"),
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

    /// Opens what the ReShade button's menu picked, for the game selected in the library.
    pub(crate) fn open_reshade_panel(&mut self, key: &str, context: &ComponentContext<Self>) {
        let game = self
            .selected_game()
            .map(|game| (game.id.clone(), game.name.clone()));
        match key {
            "shaders" => {
                self.reshade_panel = Some(ReshadePanel::new(game, PanelKind::Shaders(None)));
                context.spawn_background(|_cancel| Message::ShaderLibraryLoaded(load_library()));
            }
            "addons" => {
                self.reshade_panel = Some(ReshadePanel::new(game, PanelKind::Addons(None)));
                context.spawn_background(|_cancel| Message::AddonLibraryLoaded(load_addons()));
            }
            _ => {}
        }
    }

    /// "Apply to" and "Use all games' selection", in either panel.
    pub(crate) fn handle_target(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::ReshadeTargetChosen(Some(index)) => {
                let Some(panel) = &mut self.reshade_panel else {
                    return;
                };
                // With a game selected, its item is first.
                panel.only_game = index == 0 && panel.game.is_some();
                let target = panel.target().map(str::to_owned);
                match &mut panel.kind {
                    PanelKind::Shaders(Some(draft)) => {
                        if let Some(Ok(library)) = &self.shader_library {
                            draft.selection = shown_shaders(library, target.as_deref());
                        }
                    }
                    PanelKind::Addons(Some(enabled)) => {
                        if let Some(Ok(library)) = &self.addon_library {
                            *enabled = shown_addons(&library.settings, target.as_deref());
                        }
                    }
                    _ => {}
                }
            }
            Message::UseSharedChoice => {
                let Some(panel) = &self.reshade_panel else {
                    return;
                };
                let Some(id) = panel.target().map(str::to_owned) else {
                    return;
                };
                let settings = match panel.part() {
                    Part::Shaders => self
                        .shader_library
                        .as_ref()
                        .and_then(|library| Some(&library.as_ref().ok()?.settings)),
                    Part::Addons => self
                        .addon_library
                        .as_ref()
                        .and_then(|library| Some(&library.as_ref().ok()?.settings)),
                };
                let Some(mut settings) = settings.cloned() else {
                    return;
                };
                match panel.part() {
                    Part::Shaders => settings.set_own_shaders(&id, None),
                    Part::Addons => settings.set_own_addons(&id, None),
                }
                self.save_and_deploy(settings, DeployTo::Game(id), context);
            }
            _ => {}
        }
    }

    /// Closes the panel, then saves `settings` and deploys them to `to` in the background. Both
    /// panels' copies of the settings take them, so neither brings back an older choice.
    fn save_and_deploy(
        &mut self,
        settings: ReshadeSettings,
        to: DeployTo,
        context: &ComponentContext<Self>,
    ) {
        if let Some(Ok(library)) = &mut self.shader_library {
            library.settings = settings.clone();
        }
        if let Some(Ok(library)) = &mut self.addon_library {
            library.settings = settings.clone();
        }
        self.reshade_panel = None;
        let id = match &to {
            DeployTo::Game(id) => id.clone(),
            DeployTo::Sharing(_) => String::new(),
        };
        self.mod_job = Some((id, ModKind::Deploy));
        let games = self.games.clone();
        context.spawn_background(move |_cancel| {
            let done =
                reshade_settings::save(&settings).and_then(|()| reshade::deploy(&games, &to));
            Message::ModJobDone(done.map_err(|e| e.to_string()))
        });
    }

    /// The Addons panel's messages.
    pub(crate) fn handle_addons(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::AddonLibraryLoaded(loaded) => {
                match (&loaded, &mut self.reshade_panel) {
                    (Ok(library), Some(panel)) if matches!(panel.kind, PanelKind::Addons(None)) => {
                        panel.kind = PanelKind::Addons(Some(shown_addons(
                            &library.settings,
                            panel.target(),
                        )));
                    }
                    (Err(error), _) => {
                        self.error = Some(format!("ReShade add-ons: {error}"));
                        self.reshade_panel = None;
                    }
                    _ => {}
                }
                self.addon_library = Some(loaded);
            }
            Message::AddonSwitched(id, on) => {
                if let Some(ReshadePanel {
                    kind: PanelKind::Addons(Some(enabled)),
                    ..
                }) = &mut self.reshade_panel
                {
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

    /// Closes the Addons panel; a changed choice is saved and deployed to the games it applies to.
    fn apply_addons(&mut self, context: &ComponentContext<Self>) {
        let panel = self.reshade_panel.take();
        let (Some(panel), Some(Ok(library))) = (panel, &self.addon_library) else {
            return;
        };
        let target = panel.target().map(str::to_owned);
        let PanelKind::Addons(Some(enabled)) = panel.kind else {
            return;
        };
        if enabled == shown_addons(&library.settings, target.as_deref()) {
            return;
        }
        let mut settings = library.settings.clone();
        let chosen: Vec<String> = enabled.into_iter().collect();
        let to = match target {
            Some(id) => {
                settings.set_own_addons(&id, Some(chosen));
                DeployTo::Game(id)
            }
            None => {
                settings.addons = chosen;
                DeployTo::Sharing(Part::Addons)
            }
        };
        self.save_and_deploy(settings, to, context);
    }

    /// The Shaders panel's messages.
    pub(crate) fn handle_shaders(&mut self, message: Message, context: &ComponentContext<Self>) {
        let library = self
            .shader_library
            .as_ref()
            .and_then(|library| library.as_ref().ok());
        let draft = self
            .reshade_panel
            .as_mut()
            .and_then(ReshadePanel::shaders_draft);
        match message {
            Message::ShaderLibraryLoaded(loaded) => {
                if let (Ok(library), Some(panel)) = (&loaded, &mut self.reshade_panel)
                    && matches!(panel.kind, PanelKind::Shaders(None))
                {
                    panel.kind = PanelKind::Shaders(Some(ShadersDraft::new(shown_shaders(
                        library,
                        panel.target(),
                    ))));
                }
                if let Err(error) = &loaded {
                    self.error = Some(format!("ReShade shaders: {error}"));
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
            Message::ProfileLoad(name) => {
                let shaders = library.and_then(|library| {
                    library
                        .settings
                        .profiles
                        .iter()
                        .find(|profile| profile.name == name)
                        .map(|profile| profile.shaders.clone())
                });
                if let (Some(draft), Some(shaders)) = (draft, shaders) {
                    draft.selection = shaders;
                    self.change_profiles(context, |settings, _| settings.profile = Some(name));
                }
            }
            Message::ProfileRename(name) => {
                if let Some(draft) = draft {
                    draft.renaming = (!name.is_empty()).then(|| (name.clone(), name));
                }
            }
            Message::ProfileRenameText(text) => {
                if let Some(draft) = draft
                    && let Some((_, typed)) = &mut draft.renaming
                {
                    *typed = text;
                }
            }
            Message::ProfileRenamed => {
                let Some((old, new)) = draft.and_then(|draft| draft.renaming.take()) else {
                    return;
                };
                self.change_profiles(context, move |settings, draft| {
                    if !settings.rename_profile(&old, &new) {
                        draft.note = Some(format!(
                            "\u{201c}{}\u{201d} cannot be a profile's name: it is blank or taken.",
                            new.trim()
                        ));
                    }
                });
            }
            Message::ProfileDelete(name) => {
                self.change_profiles(context, |settings, _| settings.delete_profile(&name));
            }
            Message::ProfileSave => {
                self.change_profiles(context, |settings, draft| {
                    settings.save_profile(draft.selection.clone());
                });
            }
            Message::ProfileNew => {
                self.change_profiles(context, |settings, draft| {
                    settings.new_profile(draft.selection.clone());
                });
            }
            Message::ProfilesSaved(Err(error)) => self.error = Some(error),
            Message::ProfilesSaved(Ok(())) => {}
            Message::ExportShaders => {
                let (Some(draft), Some(library)) = (draft, library) else {
                    return;
                };
                draft.note = Some("Zipping the selected shaders…".to_owned());
                let (catalog, selection) = (library.catalog.clone(), draft.selection.clone());
                context.spawn_background(move |_cancel| {
                    let done =
                        shader_packs::export(&catalog, &selection).and_then(|(zip, failed)| {
                            gm_core::clipboard::put_file(&zip)?;
                            Ok((zip, failed))
                        });
                    Message::ShadersExported(done.map_err(|e| e.to_string()))
                });
            }
            Message::ShadersExported(done) => {
                if let Some(draft) = draft {
                    draft.note = Some(match done {
                        Ok((zip, failed)) if failed.is_empty() => format!(
                            "{} is on the clipboard: paste it into Discord or a folder to share.",
                            zip.file_name()
                                .map(|name| name.to_string_lossy())
                                .unwrap_or_default()
                        ),
                        Ok((_, failed)) => format!(
                            "The ZIP is on the clipboard, without what could not be downloaded:\n{}",
                            failed.join("\n")
                        ),
                        Err(error) => format!("Export failed: {error}"),
                    });
                }
            }
            Message::ImportShaders => {
                self.picking = OpenFilePicker::new()
                    .title("Choose a ZIP of ReShade shaders to import")
                    .filter_extensions("ZIP archives", ["zip"])
                    .request(context, |picked| {
                        Message::ImportPicked(
                            picked.map_err(|e| format!("could not choose a file: {}", e.message())),
                        )
                    });
            }
            Message::ImportPicked(picked) => {
                self.picking = false;
                match picked {
                    Ok(Some(zip)) => {
                        if let Some(draft) = draft {
                            draft.note = Some("Importing…".to_owned());
                        }
                        context.spawn_background(move |_cancel| {
                            let done = shader_packs::import(&zip)
                                .map(|count| (count, shader_packs::custom_effects()));
                            Message::ShadersImported(done.map_err(|e| e.to_string()))
                        });
                    }
                    Ok(None) => {}
                    Err(error) => self.error = Some(error),
                }
            }
            Message::ShadersImported(done) => {
                let note = match done {
                    Ok((count, effects)) => {
                        if let Some(Ok(library)) = &mut self.shader_library {
                            library.effects.insert(CUSTOM.to_owned(), effects);
                        }
                        format!("Imported {count} files into Custom Shaders.")
                    }
                    Err(error) => format!("Import failed: {error}"),
                };
                if let Some(draft) = self
                    .reshade_panel
                    .as_mut()
                    .and_then(ReshadePanel::shaders_draft)
                {
                    draft.note = Some(note);
                }
            }
            Message::OpenCustomFolder => {
                self.error = shader_packs::custom_dir()
                    .and_then(|folder| gm_core::open_folder(&folder, None))
                    .err()
                    .map(|e| e.to_string());
            }
            Message::DeployShaders => self.deploy_shaders(context),
            // Escape on the Addons panel closes it like its Close button: the switches apply.
            Message::CloseReshadePanel
                if self
                    .reshade_panel
                    .as_ref()
                    .is_some_and(|panel| panel.part() == Part::Addons) =>
            {
                self.apply_addons(context);
            }
            Message::CloseReshadePanel => self.reshade_panel = None,
            _ => {}
        }
    }

    /// Changes the profiles (in the library's settings, with the draft at hand) and saves them in
    /// the background. The shader selection saved stays the deployed one until Deploy.
    fn change_profiles(
        &mut self,
        context: &ComponentContext<Self>,
        change: impl FnOnce(&mut ReshadeSettings, &mut ShadersDraft),
    ) {
        let (Some(draft), Some(Ok(library))) = (
            self.reshade_panel
                .as_mut()
                .and_then(ReshadePanel::shaders_draft),
            &mut self.shader_library,
        ) else {
            return;
        };
        change(&mut library.settings, draft);
        let settings = library.settings.clone();
        context.spawn_background(move |_cancel| {
            Message::ProfilesSaved(reshade_settings::save(&settings).map_err(|e| e.to_string()))
        });
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

    /// Saves the selection (the shared one, or the game's own) and deploys it to the games it
    /// applies to.
    fn deploy_shaders(&mut self, context: &ComponentContext<Self>) {
        let (Some(panel), Some(Ok(library))) = (&self.reshade_panel, &self.shader_library) else {
            return;
        };
        let PanelKind::Shaders(Some(draft)) = &panel.kind else {
            return;
        };
        let mut settings = library.settings.clone();
        let selection = draft.selection.clone();
        let to = match panel.target() {
            Some(id) => {
                settings.set_own_shaders(id, Some(selection));
                DeployTo::Game(id.to_owned())
            }
            None => {
                settings.shaders = Some(selection);
                DeployTo::Sharing(Part::Shaders)
            }
        };
        self.save_and_deploy(settings, to, context);
    }

    /// The panel layer, while one is open: a dimmed backdrop and the panel centred on it.
    pub(crate) fn reshade_panel_view(&self, context: &ViewContext<Self>) -> Option<View> {
        let panel = self.reshade_panel.as_ref()?;
        let (width, height) = self.window_size;
        let width = (width - 2.0 * PANEL_MARGIN).clamp(480.0, PANEL_WIDTH);
        let height = (height - 2.0 * PANEL_MARGIN).clamp(360.0, PANEL_HEIGHT);
        let content = match &panel.kind {
            PanelKind::Shaders(draft) => self.shaders_panel(panel, draft.as_ref(), context),
            PanelKind::Addons(enabled) => self.addons_panel(panel, enabled.as_ref(), context),
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

    /// A panel's title with "Apply to" beside it, and under them what applying does: for every
    /// game with ReShade (but those with their own choice), or for the selected game alone (with
    /// "Use all games' …" when it has its own).
    fn panel_header(
        &self,
        panel: &ReshadePanel,
        title: &str,
        settings: Option<&ReshadeSettings>,
        context: &ViewContext<Self>,
    ) -> StackPanel {
        // "Only <game>" first, as the default.
        let mut items = Vec::new();
        if let Some((_, name)) = &panel.game {
            items.push(format!("Only {name}"));
        }
        items.push("All games with ReShade".to_owned());
        let selector = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(1)
            .children((
                TextBlock::new()
                    .text("Apply to")
                    .vertical_alignment(VerticalAlignment::Center),
                ComboBox::new()
                    .items_source(items)
                    .selected_index(Some(usize::from(panel.game.is_some() && !panel.only_game)))
                    .is_enabled(panel.game.is_some() && settings.is_some())
                    .min_width(280.0)
                    .automation_name("Apply to")
                    .on_selection_changed(context.callback(Message::ReshadeTargetChosen)),
            ));
        let top = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(16.0)
            .children((
                TextBlock::new()
                    .text(title)
                    .font_size(28.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .grid_column(0),
                selector,
            ));
        let mut rows: Vec<View> = vec![top.into()];
        if let Some(settings) = settings {
            rows.push(self.target_line(panel, settings, context));
        }
        StackPanel::new().spacing(6.0).children(rows)
    }

    /// What applying the panel's choice does, for its target.
    fn target_line(
        &self,
        panel: &ReshadePanel,
        settings: &ReshadeSettings,
        context: &ViewContext<Self>,
    ) -> View {
        let part = panel.part();
        let has_reshade = |id: &str| {
            self.mods
                .reshade(id)
                .is_some_and(|state| state.installed.is_some())
        };
        // What applying is called at the start of a sentence, and inside one.
        let (what, applying, applying_inside) = match part {
            Part::Shaders => ("selection", "Deploy", "Deploy"),
            Part::Addons => ("add-ons", "Closing this", "closing this"),
        };
        let (text, use_shared) = match (panel.target(), &panel.game) {
            (Some(id), Some((_, name))) => {
                let own = settings.has_own(id, part);
                let text = match (has_reshade(id), own) {
                    (false, _) => format!(
                        "ReShade is not installed in {name}: {applying_inside} saves its own \
                         {what} for its install."
                    ),
                    (true, true) => {
                        format!("{name} has its own {what}, which {applying_inside} updates.")
                    }
                    (true, false) => format!(
                        "{applying} gives {name} its own {what}, which deploying to all games then \
                         leaves alone."
                    ),
                };
                (text, own)
            }
            _ => {
                let own = settings
                    .games
                    .keys()
                    .filter(|id| has_reshade(id) && settings.has_own(id, part))
                    .count();
                let except = match own {
                    0 => String::new(),
                    1 => format!(", except the one with its own {what}"),
                    n => format!(", except the {n} with their own {what}"),
                };
                let text = match part {
                    Part::Shaders => {
                        format!("Deploy puts this selection into every game with ReShade{except}.")
                    }
                    Part::Addons => format!(
                        "Closing this puts the add-ons switched on next to the program of every \
                         game with ReShade{except}."
                    ),
                };
                (text, false)
            }
        };
        let text = match part {
            Part::Shaders => text,
            Part::Addons => format!("{text} Games with the No Addons build get none."),
        };
        let mut line: Vec<View> = vec![
            TextBlock::new()
                .text(text)
                .font_size(12.0)
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(0)
                .into(),
        ];
        if use_shared {
            let label = match part {
                Part::Shaders => "Use all games' selection",
                Part::Addons => "Use all games' add-ons",
            };
            line.push(
                Button::new()
                    .on_click(context.message(Message::UseSharedChoice))
                    .automation_name(label)
                    .content(label)
                    .grid_column(1)
                    .tooltip("Drop this game's own choice and deploy the one all games share"),
            );
        }
        Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(16.0)
            .children(line)
            .into()
    }

    /// ReShade Addon Manager: the custom folder's link, a card per add-on, and Close.
    fn addons_panel(
        &self,
        panel: &ReshadePanel,
        enabled: Option<&BTreeSet<String>>,
        context: &ViewContext<Self>,
    ) -> View {
        let library = self
            .addon_library
            .as_ref()
            .and_then(|library| library.as_ref().ok());
        let settings = library
            .filter(|_| enabled.is_some())
            .map(|library| &library.settings);
        let header = StackPanel::new().spacing(4.0).grid_row(0).children((
            self.panel_header(panel, "ReShade Addon Manager", settings, context),
            HyperlinkButton::new()
                .content("Place custom .addon64/.addon32 files here")
                .on_click(context.message(Message::OpenAddonFolder))
                .automation_name("Place custom add-on files here"),
        ));
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

    fn shaders_panel(
        &self,
        panel: &ReshadePanel,
        draft: Option<&ShadersDraft>,
        context: &ViewContext<Self>,
    ) -> View {
        let library = self
            .shader_library
            .as_ref()
            .and_then(|library| library.as_ref().ok());
        let settings = library
            .filter(|_| draft.is_some())
            .map(|library| &library.settings);
        let title = self
            .panel_header(panel, "Select Shader Packs", settings, context)
            .grid_row(0);
        let body: View = match (draft, library) {
            (Some(draft), Some(library)) => Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Pixel(300.0)])
                .column_spacing(32.0)
                .children((
                    Border::new()
                        .grid_column(0)
                        .content(self.shader_list(draft, library, context)),
                    Border::new()
                        .grid_column(1)
                        .content(profiles_column(draft, library, context)),
                ))
                .into(),
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
                    .tooltip(match &panel.game {
                        Some((_, name)) if panel.only_game => {
                            format!("Save the selection as {name}'s own and put it into {name}")
                        }
                        _ => "Save the selection and put it into every game with ReShade that \
                              has no selection of its own"
                            .to_owned(),
                    }),
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
        effects: &[String],
        cached: bool,
        draft: &ShadersDraft,
        context: &ViewContext<Self>,
    ) -> View {
        let state = draft.state(id, effects);
        let expanded = draft.expanded.contains(id)
            || (!draft.search.is_empty()
                && effects.iter().any(|file| matches(file, &draft.search)));
        let key = id.to_owned();
        let check = CheckBox::new()
            .is_checked(state)
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
                        .is_checked(draft.takes(id, file))
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

/// Profiles (each with ✎ and ✕; Save, New), Export and Import, and Open Custom Folder.
fn profiles_column(
    draft: &ShadersDraft,
    library: &ShaderLibrary,
    context: &ViewContext<GameManager>,
) -> View {
    let wide = |label: &str, message: Message| {
        Button::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .on_click(context.message(message))
            .automation_name(label)
            .content(label)
    };
    let mut rows: Vec<View> = vec![
        TextBlock::new()
            .text("Profiles")
            .font_size(20.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .into(),
    ];
    let current = library.settings.profile.as_deref();
    for profile in &library.settings.profiles {
        let name = profile.name.clone();
        let row: View = match &draft.renaming {
            Some((old, typed)) if *old == profile.name => Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                .column_spacing(4.0)
                .children((
                    TextBox::new()
                        .text(typed.as_str())
                        .automation_name(format!("New name for {name}"))
                        .on_text_changed(context.callback(|text: std::rc::Rc<str>| {
                            Message::ProfileRenameText(text.to_string())
                        }))
                        .grid_column(0),
                    Button::new()
                        .on_click(context.message(Message::ProfileRenamed))
                        .automation_name("Rename")
                        .content(SymbolIcon::new().symbol(Symbol::Accept))
                        .grid_column(1)
                        .tooltip("Rename"),
                    Button::new()
                        .on_click(context.message(Message::ProfileRename(String::new())))
                        .automation_name("Keep the name")
                        .content(SymbolIcon::new().symbol(Symbol::Cancel))
                        .grid_column(2)
                        .tooltip("Keep the name"),
                ))
                .into(),
            _ => {
                let weight = if current == Some(profile.name.as_str()) {
                    FontWeight::SEMI_BOLD
                } else {
                    FontWeight::NORMAL
                };
                Grid::new()
                    .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
                    .column_spacing(4.0)
                    .children((
                        Button::new()
                            .style(ButtonStyle::Subtle)
                            .horizontal_alignment(HorizontalAlignment::Stretch)
                            .horizontal_content_alignment(HorizontalAlignment::Left)
                            .on_click(context.message(Message::ProfileLoad(name.clone())))
                            .automation_name(format!("Load {name}"))
                            .content(TextBlock::new().text(name.as_str()).font_weight(weight))
                            .grid_column(0)
                            .tooltip("Use this profile's selection"),
                        Button::new()
                            .style(ButtonStyle::Subtle)
                            .on_click(context.message(Message::ProfileRename(name.clone())))
                            .automation_name(format!("Rename {name}"))
                            .content(SymbolIcon::new().symbol(Symbol::Edit))
                            .grid_column(1)
                            .tooltip("Rename"),
                        Button::new()
                            .style(ButtonStyle::Subtle)
                            .on_click(context.message(Message::ProfileDelete(name.clone())))
                            .automation_name(format!("Delete {name}"))
                            .content(
                                FontIcon::new()
                                    .glyph("\u{E711}")
                                    .foreground(ThemeBrush::SystemCritical),
                            )
                            .grid_column(2)
                            .tooltip("Delete"),
                    ))
                    .into()
            }
        };
        rows.push(row);
    }
    let separator = || -> View {
        Border::new()
            .height(1.0)
            .background(ThemeBrush::CardStroke)
            .margin(Thickness::xy(0.0, 6.0))
            .into()
    };
    rows.extend([
        wide("Save", Message::ProfileSave)
            .tooltip("Save the selection into the current profile (a new one when there is none)"),
        wide("New", Message::ProfileNew).tooltip("Save the selection as a new profile"),
        separator(),
        wide("Export", Message::ExportShaders).tooltip(
            "Zip the selected shader files and copy the archive to your clipboard. Paste it \
             into Discord or a folder to share.",
        ),
        wide("Import", Message::ImportShaders)
            .tooltip("Add the shaders of a ZIP to your own, under Custom Shaders"),
        separator(),
        wide("Open Custom Folder", Message::OpenCustomFolder)
            .tooltip("Your own shaders and textures go here; they show as Custom Shaders"),
    ]);
    if let Some(text) = &draft.note {
        rows.push(note(text));
    }
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(StackPanel::new().spacing(10.0).children(rows))
        .into()
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
