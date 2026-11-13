//! Game Manager's window: a native Win32 window drawn with Direct2D (`ui`, `shell`), Elm style:
//! `Message`s go through `update`, `view` builds the element tree from the state.
//!
//! - **Layout**: the title bar, the library sidebar (`sidebar.rs`) and the page (`game_page.rs`):
//!   the Library grid (`library_page.rs`, where the window opens), the selected game's header,
//!   Play and tabs, or My PC; the game menu (`game_menu.rs`) and dialogs as layers over them
//!   (`app_view.rs`).
//! - **Pages, tabs and views**: `Page` (Library, Game, My PC), `GameTab` and the ReShade tab's
//!   `ReshadeView`. Leaving the game page, the ReShade tab, its view or the game leaves Shaders or
//!   Add-ons (`leave_reshade_view`: Add-ons applies a change, Shaders drops its draft);
//!   `open_reshade_view` opens the view's panel again, after a Deploy has finished.
//! - **Logic**: `mods_view.rs` (ReShade, DXVK, LAA), `reshade_panel.rs` (shaders and add-ons),
//!   `upscalers_view.rs` and `dlss_override_view.rs` (upscaler DLLs, NVIDIA overrides), `my_pc.rs`
//!   (hardware, displays, Windows' game switches), `edit.rs` and `library.rs` (UI-free, tested).
//! - **Elevated copy**: started with `--file-ops`, `--reshade-ops`, `--nvidia-ops` or
//!   `--gpu-scheduling`, it is the copy that changes a protected game folder, ReShade's Vulkan
//!   layer, NVIDIA driver settings or GPU scheduling as administrator, and shows no window.

#![windows_subsystem = "windows"]

mod app_view;
mod covers;
mod dlss_override_view;
mod edit;
mod filter_menu;
mod game_menu;
mod game_page;
mod icons;
mod library;
mod library_page;
mod mods_view;
mod my_pc;
mod my_pc_page;
mod reshade_panel;
mod shell;
mod sidebar;
mod ui;
mod upscaler_versions;
mod upscalers_view;

use covers::{CoverMessage, CoverState};
use edit::{EditChoice, EditDraft, EditField};
use filter_menu::{FilterMessage, FilterState};
use game_menu::{MenuMessage, MenuState};
use gm_core::custom::NewGame;
use gm_core::graphics::{GraphicsApi, GraphicsInfo};
use gm_core::mods::laa::{self, Laa};
use gm_core::mods::reshade::{self, ReshadeStatus, ReshadeVariant};
use gm_core::mods::{ModsFile, dxvk};
use gm_core::nvidia::ProfileState;
use gm_core::{Game, Launch, Store};
use library::LibraryView;
use mods_view::{DxvkSettings, DxvkSettingsEdit, ModDialog, ModKind};
use my_pc::{MyPcMessage, MyPcState};
use reshade_panel::{AddonLibrary, ReshadePanel, ShaderLibrary};
use shell::ctx::{Ctx, FilePicker, WindowCommand};
use shell::{App, WindowInfo};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use ui::el::El;
use upscalers_view::{UpscalerMessage, UpscalerState};

/// The tooltip of a bitness or graphics API the user chose.
const CHOSEN: &str = "Chosen in Game settings";

/// How a dialog closed: its main button, or Cancel (the button, Escape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogResult {
    Primary,
    Cancel,
}

/// The page beside the sidebar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Page {
    /// Every game as a tile (`library_page.rs`); the window opens on it.
    #[default]
    Library,
    /// The selected game.
    Game,
    /// The PC's hardware, displays and Windows' game switches.
    MyPc,
}

/// The game page's tabs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum GameTab {
    /// Game settings, graphics and links.
    #[default]
    Overview,
    /// ReShade, and its Shaders and Add-ons (`ReshadeView`).
    Reshade,
    /// DXVK and Large Address Aware.
    Mods,
    /// Upscaler DLLs, and the versions library.
    Upscalers,
    /// NVIDIA's driver-level DLSS overrides; listed only with an NVIDIA driver.
    Nvidia,
}

/// The ReShade tab's views.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ReshadeView {
    /// Install, Reinstall, Uninstall and the settings.
    #[default]
    Install,
    Shaders,
    Addons,
}

#[derive(Clone)]
enum Message {
    Scan,
    /// The scanned games, and the message of each part of the scan that failed.
    Scanned(Vec<Game>, Vec<String>),
    /// The games saved by the last scan, shown until the first scan of this start finishes.
    SnapshotLoaded(Vec<Game>),
    /// Game icons read or decoded (`icons.rs`).
    Icon(icons::IconMessage),
    /// The window's display scaling changed: covers and icons are made again.
    ScaleChanged,
    Search(String),
    /// Enter in the search: opens the first game shown.
    SearchSubmit,
    /// The search's ✕: empties it.
    ClearSearch,
    /// The sidebar's Sort and filters button and its flyout (`filter_menu.rs`).
    Filter(FilterMessage),
    /// A store's group in the sidebar was folded or unfolded.
    ToggleStore(Store),
    /// The game menu and its Rename and Uninstall dialogs (`game_menu.rs`).
    Menu(MenuMessage),
    /// Cover art on the Library's tiles (`covers.rs`).
    Cover(CoverMessage),
    /// The title bar's minimize, maximize and close.
    Window(WindowCommand),
    /// A sidebar page.
    Page(Page),
    /// A tab of the game page.
    Tab(GameTab),
    /// A view of the ReShade tab.
    ReshadeView(ReshadeView),
    /// The graphics APIs of a game; `None` when none were found.
    GraphicsDetected(String, Option<GraphicsInfo>),
    /// A game picked in the sidebar. `None` keeps the current game.
    Select(Option<String>),
    /// Launches a game: its default action, or the alternative at `option` in `alt_launches`.
    Launch {
        id: String,
        option: Option<usize>,
    },
    /// A few seconds after a launch: Play shows Play again.
    LaunchSettled(String),
    /// The sidebar's resize grip, dragged to this x (window DIPs).
    SidebarWidth(f32),
    /// Opens the game's install folder in File Explorer.
    OpenFolder(String),
    /// Opens the game's PCGamingWiki page in the browser.
    OpenWiki(String),
    /// A Game settings field of the selected game changed.
    EditField(EditField, String),
    /// An item of a Game settings drop-down was picked; `None` keeps the choice.
    EditChoice(EditChoice, Option<usize>),
    /// Opens the file dialog for the launch program or the game program field.
    BrowseEdit(EditField),
    /// The program picked for a field; `None` when the dialog was cancelled.
    EditPathPicked(EditField, Result<Option<PathBuf>, String>),
    SaveEdits,
    CancelEdits,
    ResetEdits,
    /// The selected game's edits were saved or reset; a rescan applies them.
    EditsSaved(Result<(), String>),
    DismissError,
    /// Opens the file dialog to choose a custom game's program.
    AddGame,
    /// The chosen program; `None` when the dialog was cancelled.
    ProgramPicked(Result<Option<PathBuf>, String>),
    /// Files dropped on the window: a game's program or its shortcut adds it.
    Dropped(Vec<PathBuf>),
    /// The Add a game dialog's starting values, from a chosen program or a dropped file.
    DraftReady(Result<NewGame, String>),
    DraftName(String),
    DraftArgs(String),
    /// The Add game dialog closed: `Primary` is Add.
    DraftClosed(DialogResult),
    /// The custom game that was saved, completed like a scanned game.
    CustomGameAdded(Result<Box<Game>, String>),
    /// Asks to confirm removing the custom game with this id.
    RemoveGame(String),
    /// The remove confirmation closed: `Primary` is Remove.
    RemoveClosed(DialogResult),
    /// The id of the custom game that was removed.
    CustomGameRemoved(Result<String, String>),
    /// `mods.json`, read at start.
    ModsLoaded(Result<ModsFile, String>),
    /// Installs DXVK in the selected game, or reinstalls it.
    InstallDxvk,
    UninstallDxvk,
    /// A mod was installed, uninstalled or set up: `mods.json` as saved, or what failed.
    ModJobDone(Result<ModsFile, String>),
    /// Opens the web page a mod was downloaded from.
    OpenModPage(String),
    ShowDxvkInfo,
    ShowDxvkSettings,
    DxvkSettingsEdited(DxvkSettingsEdit),
    /// HDR-mod's own configs in its latest build, or what failed.
    HdrModConfigsLoaded(Result<Vec<String>, String>),
    /// A game's NVIDIA driver profile, read for DXVK settings (`None`: no NVIDIA driver).
    NvidiaLoaded(String, Result<Option<ProfileState>, String>),
    /// A 32-bit game's Large Address Aware status.
    LaaLoaded(String, Result<Laa, String>),
    /// Marks the selected game's program Large Address Aware.
    EnableLaa,
    /// Puts the selected game's original program back.
    RestoreLaa,
    ShowLaaInfo,
    /// What a game's ReShade row and graphics line read from the disk: DXVK in its folder, the
    /// Vulkan layer.
    ReshadeLoaded(String, ReshadeStatus),
    /// Installs ReShade in the selected game, or reinstalls it.
    InstallReshade,
    UninstallReshade,
    ShowReshadeInfo,
    ShowReshadeSettings,
    /// The variant picked in ReShade settings; `None` is ignored.
    ReshadeVariantChosen(Option<usize>),
    /// The shader pack list, the user's choices and the cached packs' effects, or what failed.
    ShaderLibraryLoaded(Result<ShaderLibrary, String>),
    /// A pack's box (or the user's own shaders'); `None` is ignored.
    ShaderPackChecked(String, Option<bool>),
    /// One effect of a pack: pack id, file, on.
    ShaderEffectChecked(String, String, Option<bool>),
    /// Shows or hides a pack's effects (downloading the pack to list them).
    ShaderPackExpanded(String),
    ShaderExpandAll,
    ShaderDeselectAll,
    ShaderSearch(String),
    /// A pack downloaded for its effects, or what failed.
    ShaderPackDownloaded(String, Result<Vec<String>, String>),
    /// Saves the shader selection and deploys it to every game with ReShade.
    DeployShaders,
    /// Shaders' Discard changes: drops the draft (the view opens the saved selection again).
    CloseReshadePanel,
    /// Shaders' profiles, by name: load, start or end renaming (an empty name ends it),
    /// the name typed, delete, and Save and New with the selection.
    ProfileLoad(String),
    ProfileRename(String),
    ProfileRenameText(String),
    ProfileRenamed,
    ProfileDelete(String),
    ProfileSave,
    ProfileNew,
    /// The profiles were saved, or what failed.
    ProfilesSaved(Result<(), String>),
    /// Zips the selected shaders onto the clipboard.
    ExportShaders,
    /// The ZIP on the clipboard, with what could not be downloaded; or what failed.
    ShadersExported(Result<(PathBuf, Vec<String>), String>),
    /// Chooses a ZIP of shaders to add to the user's own.
    ImportShaders,
    ImportPicked(Result<Option<PathBuf>, String>),
    /// How many files the import gave, and the user's own effects now; or what failed.
    ShadersImported(Result<(usize, Vec<String>), String>),
    /// Opens the folder for the user's own shaders.
    OpenCustomFolder,
    /// Every add-on, the user's choices and what is cached, or what failed.
    AddonLibraryLoaded(Result<AddonLibrary, String>),
    /// An add-on's switch in Add-ons: id, on.
    AddonSwitched(String, bool),
    /// Opens the folder for the user's own add-ons.
    OpenAddonFolder,
    /// Add-ons' Apply (and leaving Add-ons): saves and deploys a changed choice.
    CloseAddons,
    /// "Apply to" on Shaders or Add-ons: with a game selected, 0 is that game and 1 all games
    /// (else 0 is all games); `None` is ignored.
    ReshadeTargetChosen(Option<usize>),
    /// Drops the selected game's own choice on the open ReShade page and deploys the shared one
    /// to it.
    UseSharedChoice,
    /// The Info or settings dialog closed: `Primary` is the settings' Save.
    ModDialogClosed(DialogResult),
    /// The Upscalers tab and page (`upscalers_view.rs`).
    Upscalers(UpscalerMessage),
    /// The My PC page (`my_pc.rs`).
    MyPc(MyPcMessage),
}

struct GameManager {
    games: Vec<Game>,
    /// Game icons: their bytes and their pictures at the window's scaling (`icons.rs`).
    icons: icons::IconState,
    query: String,
    /// How the lists are sorted and filtered (Sort and filters), and its flyout.
    view: LibraryView,
    filters: FilterState,
    /// Store groups folded in the sidebar.
    folded: HashSet<Store>,
    selected: Option<String>,
    page: Page,
    tab: GameTab,
    /// The ReShade tab's view, kept across games.
    reshade_view: ReshadeView,
    /// The sidebar's width in DIPs (its resize grip).
    sidebar_width: f32,
    /// The game just launched: its Play button shows "Starting…" for a few seconds.
    launching: Option<String>,
    /// A game's menu while it is open, and its Rename and Uninstall dialogs.
    menu: MenuState,
    /// The covers on the Library's tiles, by game id.
    covers: CoverState,
    /// Graphics APIs by game id, once looked up (`None`: none found).
    graphics: HashMap<String, Option<GraphicsInfo>>,
    /// Games whose graphics APIs are being looked up.
    graphics_pending: HashSet<String>,
    scanning: bool,
    error: Option<String>,
    /// The file dialog for a custom game's program is open, or a chosen or dropped file is read.
    picking: bool,
    /// The custom game being set up in the Add a game dialog.
    draft: Option<NewGame>,
    /// The custom game whose removal waits for confirmation.
    removing: Option<String>,
    /// The Game settings being edited; see `selected_draft`.
    edit: Option<EditDraft>,
    /// Game settings are being saved or reset.
    saving: bool,
    /// `mods.json`: each game's mods, their settings and what is installed.
    mods: ModsFile,
    /// The game and mod being installed, uninstalled or set up; one job at a time.
    mod_job: Option<(String, ModKind)>,
    /// 32-bit games' Large Address Aware status by id, once read.
    laa: HashMap<String, Result<Laa, String>>,
    laa_pending: HashSet<String>,
    /// What each game's ReShade row read from the disk, by id.
    reshade: HashMap<String, ReshadeStatus>,
    reshade_pending: HashSet<String>,
    /// The ReShade tab's Shaders or Add-ons view, while it shows.
    reshade_panel: Option<ReshadePanel>,
    /// The upscaler DLLs found in games, their versions and cache (Versions), and the NVIDIA
    /// overrides tab's driver facts and draft.
    upscalers: UpscalerState,
    /// What the ReShade pages read: read again each time one opens.
    shader_library: Option<Result<ShaderLibrary, String>>,
    addon_library: Option<Result<AddonLibrary, String>>,
    /// Packs being downloaded to list their effects, and those that failed.
    packs_downloading: HashSet<String>,
    pack_errors: HashMap<String, String>,
    mod_dialog: Option<ModDialog>,
    /// HDR-mod's own configs, listed once the settings dialog shows HDR-mod (`Err`: what failed,
    /// tried again when the dialog opens again).
    hdr_mod_configs: Option<Result<Vec<String>, String>>,
    hdr_mod_configs_pending: bool,
    /// The selected game's NVIDIA driver profile, read when DXVK settings open.
    nvidia_profile: Option<Result<Option<ProfileState>, String>>,
    /// The My PC page: the last snapshot and the changes running.
    my_pc: MyPcState,
}

fn main() -> ExitCode {
    // Started as administrator to change a protected game folder, ReShade's Vulkan layer or
    // NVIDIA driver settings: do that, show no window, and exit with the result (0, or a code
    // saying what failed).
    let mut args = std::env::args().skip(1);
    let elevated: Option<fn(&str) -> u32> = match args.next().as_deref() {
        Some(gm_core::mods::FILE_OPS_ARG) => Some(gm_core::mods::run_encoded),
        Some(gm_core::mods::reshade::RESHADE_OPS_ARG) => Some(gm_core::mods::reshade::run_encoded),
        Some(gm_core::nvidia::NVIDIA_OPS_ARG) => Some(gm_core::nvidia::run_encoded),
        Some(gm_core::pc::GPU_SCHEDULING_ARG) => Some(gm_core::pc::run_encoded),
        _ => None,
    };
    if let Some(run) = elevated {
        std::process::exit(run(&args.next().unwrap_or_default()) as i32);
    }
    let options = shell::Options {
        title: "Game Manager".to_owned(),
        size: (1200.0, 760.0),
        min_size: (860.0, 520.0),
    };
    match shell::run(GameManager::create, options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            shell::dialogs::show_start_error(&error.message());
            ExitCode::FAILURE
        }
    }
}

impl App for GameManager {
    type Message = Message;

    fn update(&mut self, message: Message, ctx: &Ctx<Message>) {
        self.handle(message, ctx);
        // Whatever selected a game (a click, a scan, a game added), its APIs are looked up.
        self.request_graphics(ctx);
        // Whatever showed HDR-mod in DXVK settings, its configs are listed.
        self.request_hdr_mod_configs(ctx);
        // A 32-bit game's LAA status, whenever it is unknown.
        self.request_laa(ctx);
        // What the ReShade row needs from the disk, whenever it is unknown.
        self.request_reshade(ctx);
        // The Upscalers and NVIDIA overrides tabs' DLLs and the overrides' draft, while open.
        self.request_upscalers(ctx);
        self.request_overrides(ctx);
        // Shaders or Add-ons showing without its panel (just chosen, or after Deploy, Discard
        // changes or Apply) opens it.
        self.open_reshade_view(ctx);
    }

    fn view(&self, window: &WindowInfo) -> El<Message> {
        self.root_view(window)
    }

    /// Escape closes the topmost of the game menu, the Sort and filters flyout, a dialog, then
    /// leaves My PC, or Shaders and Add-ons for the ReShade view.
    fn escape(&self) -> Option<Message> {
        if let Some(message) = self.menu.escape().or_else(|| self.filters.escape()) {
            Some(message)
        } else if self.my_pc.trial.is_some() {
            Some(MyPcMessage::RevertMode.into())
        } else if self.mod_dialog.is_some() {
            Some(Message::ModDialogClosed(DialogResult::Cancel))
        } else if self.removing.is_some() {
            Some(Message::RemoveClosed(DialogResult::Cancel))
        } else if self.draft.is_some() {
            Some(Message::DraftClosed(DialogResult::Cancel))
        } else {
            match self.page {
                Page::MyPc => Some(Message::Page(Page::Game)),
                Page::Library => None,
                Page::Game
                    if self.tab == GameTab::Reshade
                        && self.reshade_view != ReshadeView::Install =>
                {
                    Some(Message::ReshadeView(ReshadeView::Install))
                }
                Page::Game => None,
            }
        }
    }

    /// A game's program or its shortcut dropped anywhere on the window adds it.
    fn drop_files(&self, paths: Vec<PathBuf>) -> Option<Message> {
        self.can_add_game().then_some(Message::Dropped(paths))
    }

    /// Ctrl+F goes to the sidebar's search.
    fn search_box(&self) -> Option<&'static str> {
        Some(sidebar::SEARCH_KEY)
    }

    /// Covers and icons are decoded at the window's pixel size: made again for the new scaling.
    fn scale_changed(&self, _scale: f32) -> Option<Message> {
        Some(Message::ScaleChanged)
    }
}

impl GameManager {
    fn create(ctx: &Ctx<Message>) -> Self {
        let mut this = Self {
            games: Vec::new(),
            icons: icons::IconState::default(),
            query: String::new(),
            view: LibraryView::default(),
            filters: FilterState::default(),
            folded: HashSet::new(),
            selected: None,
            page: Page::Library,
            tab: GameTab::Overview,
            reshade_view: ReshadeView::Install,
            sidebar_width: ui::theme::SIDEBAR_WIDTH,
            launching: None,
            menu: MenuState::default(),
            covers: CoverState::default(),
            graphics: HashMap::new(),
            graphics_pending: HashSet::new(),
            scanning: false,
            error: None,
            picking: false,
            draft: None,
            removing: None,
            edit: None,
            saving: false,
            mods: ModsFile::default(),
            mod_job: None,
            laa: HashMap::new(),
            laa_pending: HashSet::new(),
            reshade: HashMap::new(),
            reshade_pending: HashSet::new(),
            reshade_panel: None,
            upscalers: UpscalerState::default(),
            shader_library: None,
            addon_library: None,
            packs_downloading: HashSet::new(),
            pack_errors: HashMap::new(),
            mod_dialog: None,
            hdr_mod_configs: None,
            hdr_mod_configs_pending: false,
            nvidia_profile: None,
            my_pc: MyPcState::default(),
        };
        // The last scan's games show at once while the fresh scan runs.
        ctx.spawn_background(|_cancel| {
            Message::SnapshotLoaded(gm_core::snapshot::load().unwrap_or_default())
        });
        ctx.spawn_background(|_cancel| {
            Message::ModsLoaded(gm_core::mods::load().map_err(|e| format!("Mods: {e}")))
        });
        Self::load_view(ctx);
        // The NVIDIA overrides tab shows only with an NVIDIA driver: read it now.
        this.request_driver(ctx);
        this.start_scan(ctx);
        this
    }
}

impl GameManager {
    fn start_scan(&mut self, context: &Ctx<Message>) {
        self.scanning = true;
        context.spawn_background(|_cancel| {
            let report = gm_core::scan_all();
            // The next start shows these games at once. A cache only: failures are ignored.
            let _ = gm_core::snapshot::save(&report.games);
            let errors = report.errors.iter().map(ToString::to_string).collect();
            Message::Scanned(report.games, errors)
        });
    }

    /// Add game and dropping a game's file. Not while scanning: a scan that started before the
    /// save would drop the new game.
    fn can_add_game(&self) -> bool {
        !self.scanning && !self.picking && self.draft.is_none()
    }

    fn selected_game(&self) -> Option<&Game> {
        let id = self.selected.as_deref()?;
        self.games.iter().find(|game| game.id == id)
    }

    /// The selected game's settings draft: the one being edited while it still matches the game,
    /// else a fresh one from the game's current values.
    fn selected_draft(&self) -> Option<EditDraft> {
        Some(EditDraft::for_game(
            self.edit.as_ref(),
            self.selected_game()?,
        ))
    }

    /// Starts a game: its default action, or the alternative at `option` in `alt_launches`.
    fn launch(&mut self, id: &str, option: Option<usize>) {
        let Some(game) = self.games.iter().find(|game| game.id == id) else {
            return;
        };
        let launch = match option {
            None => Some(&game.launch),
            Some(i) => game.alt_launches.get(i).map(|alt| &alt.launch),
        };
        if let Some(launch) = launch {
            self.error = gm_core::launch(launch).err().map(|e| e.to_string());
        }
    }

    /// Shows another page. Leaving the game page leaves its Shaders or Add-ons view
    /// (`leave_reshade_view`).
    fn go_to(&mut self, page: Page, context: &Ctx<Message>) {
        if page == self.page {
            return;
        }
        if self.page == Page::Game {
            self.leave_reshade_view(context);
        }
        self.page = page;
        if page == Page::MyPc {
            // Read afresh at each visit, as Settings or a driver may have changed things.
            self.handle_my_pc(MyPcMessage::Refresh, context);
        }
    }

    /// Selects a game and shows its page. Another game first leaves the Shaders or Add-ons view
    /// of the one before (the view opens again for the new game).
    fn select(&mut self, id: String, context: &Ctx<Message>) {
        if self.selected.as_deref() != Some(id.as_str()) {
            self.leave_reshade_view(context);
        }
        self.selected = Some(id);
        self.go_to(Page::Game, context);
    }

    /// Opens the panel of the ReShade tab's Shaders or Add-ons view when it shows without one,
    /// once a Deploy that closed it has finished (so it reads what was saved).
    fn open_reshade_view(&mut self, context: &Ctx<Message>) {
        // A panel opened for another game than the one selected now (added, removed, rescanned)
        // is left like a click on another game leaves it.
        if let Some(panel) = &self.reshade_panel
            && panel.game.as_ref().map(|(id, _)| id.as_str()) != self.selected.as_deref()
        {
            self.leave_reshade_view(context);
        }
        let deploying = matches!(self.mod_job, Some((_, ModKind::Deploy)));
        if self.page != Page::Game
            || self.tab != GameTab::Reshade
            || self.reshade_panel.is_some()
            || self.selected_game().is_none()
            || deploying
        {
            return;
        }
        match self.reshade_view {
            ReshadeView::Shaders => self.open_reshade_panel("shaders", context),
            ReshadeView::Addons => self.open_reshade_panel("addons", context),
            ReshadeView::Install => {}
        }
    }

    /// Looks up the selected game's graphics APIs in the background, once per game.
    fn request_graphics(&mut self, context: &Ctx<Message>) {
        let Some(game) = self.selected_game() else {
            return;
        };
        if self.graphics.contains_key(&game.id) || self.graphics_pending.contains(&game.id) {
            return;
        }
        let game = game.clone();
        self.graphics_pending.insert(game.id.clone());
        context.spawn_background(move |_cancel| {
            let info = gm_core::graphics::detect(&game);
            Message::GraphicsDetected(game.id, info)
        });
    }
}

/// "DirectX 11, DirectX 12".
fn api_list(apis: &[GraphicsApi]) -> String {
    apis.iter()
        .map(|api| api.label())
        .collect::<Vec<_>>()
        .join(", ")
}

impl GameManager {
    fn handle(&mut self, message: Message, context: &Ctx<Message>) {
        match message {
            Message::GraphicsDetected(id, info) => {
                self.graphics_pending.remove(&id);
                self.graphics.insert(id, info);
            }
            Message::Scan => self.start_scan(context),
            Message::Scanned(games, errors) => {
                self.scanning = false;
                self.games = games;
                // A program may have changed (an update, or another one chosen).
                self.laa.clear();
                self.reshade.clear();
                self.upscalers.found.clear();
                self.error = (!errors.is_empty()).then(|| errors.join("\n"));
                self.selected = library::keep_selection(&self.games, self.selected.as_deref());
                self.tidy_view();
                Self::load_icons(self.games.clone(), true, context);
                self.drop_covers_of_missing_games();
                self.load_covers(true, true, context);
            }
            Message::SnapshotLoaded(games) => {
                // Only before the first scan has listed anything; the scan then replaces it.
                if self.scanning && self.games.is_empty() && !games.is_empty() {
                    self.games = games;
                    self.selected = library::keep_selection(&self.games, self.selected.as_deref());
                    Self::load_icons(self.games.clone(), false, context);
                    // What is on this PC already; the scan asks the network for the rest.
                    self.load_covers(false, false, context);
                }
            }
            Message::Icon(message) => self.handle_icon(message, context),
            Message::ScaleChanged => self.redecode_icons(context),
            Message::Search(query) => self.query = query,
            Message::ClearSearch => self.query.clear(),
            Message::SearchSubmit => {
                let first =
                    library::first_shown(&self.games, &self.query, &self.view, self.facts());
                if let Some(index) = first {
                    let id = self.games[index].id.clone();
                    self.select(id, context);
                }
            }
            Message::Filter(message) => self.handle_filter(message, context),
            Message::ToggleStore(store) => {
                if !self.folded.remove(&store) {
                    self.folded.insert(store);
                }
            }
            Message::Menu(message) => self.handle_menu(message, context),
            Message::Cover(message) => self.handle_cover(message, context),
            Message::Window(command) => context.window(command),
            Message::Page(page) => self.go_to(page, context),
            Message::Tab(tab) => {
                if tab != GameTab::Reshade {
                    self.leave_reshade_view(context);
                }
                self.tab = tab;
                // The game's upscaler DLLs are looked for only while these tabs show
                // (`request_upscalers`); the NVIDIA tab's hint needs them too.
                self.upscalers.open = matches!(tab, GameTab::Upscalers | GameTab::Nvidia);
            }
            Message::ReshadeView(view) => {
                if view != self.reshade_view {
                    self.leave_reshade_view(context);
                    self.reshade_view = view;
                }
            }
            Message::Select(Some(id)) => self.select(id, context),
            Message::Select(None) => {}
            Message::Launch { id, option } => {
                self.launch(&id, option);
                // Started: "Starting…" while the game's own window takes its time.
                if self.error.is_none() {
                    self.launching = Some(id.clone());
                    context.spawn_background(move |_cancel| {
                        std::thread::sleep(std::time::Duration::from_secs(4));
                        Message::LaunchSettled(id)
                    });
                }
            }
            Message::LaunchSettled(id) => {
                if self.launching.as_deref() == Some(id.as_str()) {
                    self.launching = None;
                }
            }
            Message::SidebarWidth(x) => {
                self.sidebar_width =
                    x.clamp(ui::theme::SIDEBAR_MIN_WIDTH, ui::theme::SIDEBAR_MAX_WIDTH);
            }
            Message::OpenFolder(id) => {
                let Some(game) = self.games.iter().find(|game| game.id == id) else {
                    return;
                };
                if let Some(dir) = &game.install_dir {
                    self.error = gm_core::open_folder(dir, game.program.as_deref())
                        .err()
                        .map(|e| e.to_string());
                }
            }
            Message::OpenWiki(id) => {
                let Some(game) = self.games.iter().find(|game| game.id == id) else {
                    return;
                };
                let page = Launch::Uri(gm_core::pcgamingwiki::page_url(game));
                self.error = gm_core::launch(&page).err().map(|e| e.to_string());
            }
            Message::ModsLoaded(Ok(mods)) => {
                self.mods = mods;
                self.filters.mods_ready = true;
                // The LAA status depends on what mods.json says was patched, ReShade's on
                // whether it lists DXVK, the upscalers' on what was swapped.
                self.laa.clear();
                self.reshade.clear();
                self.upscalers.found.clear();
            }
            Message::ModsLoaded(Err(e)) => {
                self.filters.mods_ready = true;
                self.error = Some(e);
            }
            Message::InstallDxvk => {
                let Some(game) = self.selected_game() else {
                    return;
                };
                let Some(api) = self.mod_api(game) else {
                    return;
                };
                let game = game.clone();
                self.mod_job = Some((game.id.clone(), ModKind::Dxvk));
                context.spawn_background(move |_cancel| {
                    Message::ModJobDone(dxvk::install(&game, api).map_err(|e| e.to_string()))
                });
            }
            Message::UninstallDxvk => {
                let Some(game) = self.selected_game().cloned() else {
                    return;
                };
                self.mod_job = Some((game.id.clone(), ModKind::Dxvk));
                context.spawn_background(move |_cancel| {
                    Message::ModJobDone(dxvk::uninstall(&game).map_err(|e| e.to_string()))
                });
            }
            Message::ModJobDone(done) => {
                self.mod_job = None;
                self.laa.clear();
                self.reshade.clear();
                self.upscaler_job_done(context);
                match done {
                    Ok(mods) => self.mods = mods,
                    Err(e) => {
                        self.error = Some(e);
                        // A job may have saved part of its work before it failed.
                        context.spawn_background(|_cancel| {
                            Message::ModsLoaded(
                                gm_core::mods::load().map_err(|e| format!("Mods: {e}")),
                            )
                        });
                    }
                }
            }
            Message::OpenModPage(page) => {
                self.error = gm_core::launch(&Launch::Uri(page))
                    .err()
                    .map(|e| e.to_string());
            }
            Message::ShowDxvkInfo => self.mod_dialog = Some(ModDialog::Info),
            Message::ShowLaaInfo => self.mod_dialog = Some(ModDialog::LaaInfo),
            Message::LaaLoaded(id, status) => {
                self.laa_pending.remove(&id);
                self.laa.insert(id, status);
            }
            Message::EnableLaa | Message::RestoreLaa => {
                let Some(game) = self.selected_game().cloned() else {
                    return;
                };
                let enable = matches!(message, Message::EnableLaa);
                self.mod_job = Some((game.id.clone(), ModKind::Laa));
                context.spawn_background(move |_cancel| {
                    let done = if enable {
                        laa::enable(&game)
                    } else {
                        laa::restore(&game)
                    };
                    Message::ModJobDone(done.map_err(|e| e.to_string()))
                });
            }
            Message::ShowDxvkSettings => {
                let Some(game) = self.selected_game() else {
                    return;
                };
                let state = self.mods.dxvk(&game.id).cloned().unwrap_or_default();
                let (id, program) = (game.id.clone(), game.program.clone());
                // The driver profile as it is now, for the NVIDIA section.
                self.nvidia_profile = None;
                if let Some(exe) = program.as_deref().and_then(Path::file_name) {
                    let exe = exe.to_string_lossy().into_owned();
                    context.spawn_background(move |_cancel| {
                        let read = gm_core::nvidia::read(&exe).map_err(|e| e.to_string());
                        Message::NvidiaLoaded(id, read)
                    });
                }
                self.mod_dialog = Some(ModDialog::Settings(DxvkSettings {
                    variant: state.variant,
                    config: state.config,
                    nvidia: state.nvidia,
                }));
                // A list that failed (offline) is asked for again.
                if matches!(self.hdr_mod_configs, Some(Err(_))) {
                    self.hdr_mod_configs = None;
                }
            }
            Message::NvidiaLoaded(id, read) => {
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.nvidia_profile = Some(read);
                }
            }
            Message::DxvkSettingsEdited(edit) => {
                if let Some(ModDialog::Settings(settings)) = &mut self.mod_dialog {
                    settings.edit(edit);
                }
            }
            Message::HdrModConfigsLoaded(configs) => {
                self.hdr_mod_configs_pending = false;
                self.hdr_mod_configs = Some(configs);
            }
            message @ (Message::ShaderLibraryLoaded(_)
            | Message::ShaderPackChecked(..)
            | Message::ShaderEffectChecked(..)
            | Message::ShaderPackExpanded(_)
            | Message::ShaderExpandAll
            | Message::ShaderDeselectAll
            | Message::ShaderSearch(_)
            | Message::ShaderPackDownloaded(..)
            | Message::DeployShaders
            | Message::CloseReshadePanel
            | Message::ProfileLoad(_)
            | Message::ProfileRename(_)
            | Message::ProfileRenameText(_)
            | Message::ProfileRenamed
            | Message::ProfileDelete(_)
            | Message::ProfileSave
            | Message::ProfileNew
            | Message::ProfilesSaved(_)
            | Message::ExportShaders
            | Message::ShadersExported(_)
            | Message::ImportShaders
            | Message::ImportPicked(_)
            | Message::ShadersImported(_)
            | Message::OpenCustomFolder) => self.handle_shaders(message, context),
            message @ (Message::AddonLibraryLoaded(_)
            | Message::AddonSwitched(..)
            | Message::OpenAddonFolder
            | Message::CloseAddons) => self.handle_addons(message, context),
            message @ (Message::ReshadeTargetChosen(_) | Message::UseSharedChoice) => {
                self.handle_target(message, context);
            }
            Message::Upscalers(message) => self.handle_upscalers(message, context),
            Message::MyPc(message) => self.handle_my_pc(message, context),
            Message::ReshadeLoaded(id, status) => {
                self.reshade_pending.remove(&id);
                self.reshade.insert(id, status);
            }
            Message::InstallReshade => {
                let Some(game) = self.selected_game() else {
                    return;
                };
                let Some(api) = self.mod_api(game) else {
                    return;
                };
                let game = game.clone();
                self.mod_job = Some((game.id.clone(), ModKind::Reshade));
                context.spawn_background(move |_cancel| {
                    Message::ModJobDone(reshade::install(&game, api).map_err(|e| e.to_string()))
                });
            }
            Message::UninstallReshade => {
                let Some(game) = self.selected_game().cloned() else {
                    return;
                };
                self.mod_job = Some((game.id.clone(), ModKind::Reshade));
                context.spawn_background(move |_cancel| {
                    Message::ModJobDone(reshade::uninstall(&game).map_err(|e| e.to_string()))
                });
            }
            Message::ShowReshadeInfo => self.mod_dialog = Some(ModDialog::ReshadeInfo),
            Message::ShowReshadeSettings => {
                let Some(game) = self.selected_game() else {
                    return;
                };
                let variant = self
                    .mods
                    .reshade(&game.id)
                    .map_or(ReshadeVariant::default(), |state| state.variant);
                self.mod_dialog = Some(ModDialog::ReshadeSettings(variant));
            }
            Message::ReshadeVariantChosen(index) => {
                if let Some(ModDialog::ReshadeSettings(variant)) = &mut self.mod_dialog
                    && let Some(&chosen) = index.and_then(|index| ReshadeVariant::ALL.get(index))
                {
                    *variant = chosen;
                }
            }
            Message::ModDialogClosed(result) => {
                let dialog = self.mod_dialog.take();
                if result != DialogResult::Primary {
                    return;
                }
                let Some(game) = self.selected_game().cloned() else {
                    return;
                };
                match dialog {
                    Some(ModDialog::Settings(settings)) => {
                        let api = self.mod_api(&game);
                        self.mod_job = Some((game.id.clone(), ModKind::Dxvk));
                        context.spawn_background(move |_cancel| {
                            let saved = dxvk::save_settings(
                                &game,
                                api,
                                settings.variant,
                                settings.config,
                                settings.nvidia,
                            );
                            Message::ModJobDone(saved.map_err(|e| e.to_string()))
                        });
                    }
                    Some(ModDialog::ReshadeSettings(variant)) => {
                        self.mod_job = Some((game.id.clone(), ModKind::Reshade));
                        context.spawn_background(move |_cancel| {
                            let saved = reshade::save_settings(&game, variant);
                            Message::ModJobDone(saved.map_err(|e| e.to_string()))
                        });
                    }
                    _ => {}
                }
            }
            Message::EditField(field, value) => {
                if let Some(mut draft) = self.selected_draft() {
                    draft.fields.set(field, value);
                    self.edit = Some(draft);
                }
            }
            Message::EditChoice(choice, Some(index)) => {
                if let Some(mut draft) = self.selected_draft() {
                    draft.fields.choose(choice, index);
                    self.edit = Some(draft);
                }
            }
            Message::EditChoice(_, None) => {}
            Message::BrowseEdit(field) => {
                let (Some(game), Some(draft)) = (self.selected_game(), self.selected_draft())
                else {
                    return;
                };
                // Opens where the field's program is, else in the install folder.
                let current = PathBuf::from(draft.fields.get(field).trim());
                let folder = current
                    .parent()
                    .filter(|dir| !dir.as_os_str().is_empty() && dir.is_dir())
                    .map(Path::to_path_buf)
                    .or_else(|| game.install_dir.clone());
                let mut picker = match field {
                    EditField::Launch => FilePicker::new()
                        .title(format!("Choose the program Play starts for {}", game.name))
                        .filter_extensions("Programs", ["exe", "bat", "cmd"]),
                    _ => FilePicker::new()
                        .title(format!("Choose the .exe of {}", game.name))
                        .filter_extensions("Programs", ["exe"]),
                };
                if let Some(folder) = folder {
                    picker = picker.folder(folder);
                }
                self.picking = picker.request(context, move |picked| {
                    let picked = picked.map_err(|e| format!("could not choose a program: {e}"));
                    Message::EditPathPicked(field, picked)
                });
                if !self.picking {
                    self.error = Some("The file dialog is not available right now.".to_owned());
                }
            }
            Message::EditPathPicked(field, Ok(Some(path))) => {
                self.picking = false;
                if let Some(mut draft) = self.selected_draft() {
                    draft.fields.set(field, path.display().to_string());
                    self.edit = Some(draft);
                }
            }
            Message::EditPathPicked(_, Ok(None)) => self.picking = false,
            Message::EditPathPicked(_, Err(e)) => {
                self.picking = false;
                self.error = Some(e);
            }
            Message::SaveEdits => {
                let (Some(game), Some(draft)) = (self.selected_game(), self.selected_draft())
                else {
                    return;
                };
                let id = game.id.clone();
                let edits = draft.edits(game.edits.clone().unwrap_or_default());
                self.saving = true;
                // Checking the paths and writing the file are disk work.
                context.spawn_background(move |_cancel| {
                    let saved = edit::check_paths(&edits).and_then(|()| {
                        gm_core::config::save_edits(&id, &edits).map_err(|e| e.to_string())
                    });
                    Message::EditsSaved(saved)
                });
            }
            Message::CancelEdits => self.edit = None,
            Message::ResetEdits => {
                let Some(id) = self.selected.clone() else {
                    return;
                };
                self.saving = true;
                context.spawn_background(move |_cancel| {
                    Message::EditsSaved(
                        gm_core::config::clear_edits(&id).map_err(|e| e.to_string()),
                    )
                });
            }
            Message::EditsSaved(result) => {
                self.saving = false;
                match result {
                    // The scan applies the edits everywhere: name and order, Play, bitness.
                    Ok(()) => {
                        self.edit = None;
                        self.start_scan(context);
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            Message::DismissError => self.error = None,
            Message::AddGame => {
                // Owned by the app's window, which is disabled while the dialog is open.
                self.picking = FilePicker::new()
                    .title("Choose the game's program")
                    .filter_extensions("Programs", ["exe", "bat", "cmd"])
                    .filter_all()
                    .request(context, |picked| {
                        Message::ProgramPicked(
                            picked.map_err(|e| format!("could not choose a program: {e}")),
                        )
                    });
            }
            Message::ProgramPicked(Ok(Some(program))) => {
                // The suggested name comes from the program's version information on disk.
                context.spawn_background(move |_cancel| {
                    Message::DraftReady(Ok(NewGame::from_program(program)))
                });
            }
            Message::ProgramPicked(Ok(None)) => self.picking = false,
            Message::ProgramPicked(Err(e)) => {
                self.picking = false;
                self.error = Some(e);
            }
            Message::Dropped(paths) => {
                // Drops are taken only while a game can be added.
                if !self.can_add_game() {
                    return;
                }
                self.picking = true;
                // Reading a shortcut uses COM and the disk.
                context.spawn_background(move |_cancel| {
                    Message::DraftReady(NewGame::from_dropped(&paths).map_err(|e| e.to_string()))
                });
            }
            Message::DraftReady(ready) => {
                self.picking = false;
                match ready {
                    Ok(game) => self.draft = Some(game),
                    Err(e) => self.error = Some(e),
                }
            }
            Message::DraftName(name) => {
                if let Some(draft) = &mut self.draft {
                    draft.name = name;
                }
            }
            Message::DraftArgs(args) => {
                if let Some(draft) = &mut self.draft {
                    draft.args = args;
                }
            }
            Message::DraftClosed(result) => {
                if let Some(draft) = self.draft.take()
                    && result == DialogResult::Primary
                {
                    context.spawn_background(move |_cancel| {
                        let added = gm_core::custom::add(&draft).map_err(|e| e.to_string());
                        let added = added.map(|custom| {
                            // As a scan would: program, icon and architecture.
                            let mut game = custom.to_game();
                            gm_core::complete_game(&mut game);
                            Box::new(game)
                        });
                        Message::CustomGameAdded(added)
                    });
                }
            }
            Message::CustomGameAdded(Ok(game)) => {
                let game = *game;
                // Show the new game even when the current search or filters would hide it.
                let one = std::slice::from_ref(&game);
                if library::shown(one, &self.query, &self.view, self.facts()).is_empty() {
                    self.query.clear();
                    library::clear(&mut self.view);
                }
                self.selected = Some(game.id.clone());
                Self::load_icons(vec![game.clone()], false, context);
                self.games.push(game);
                gm_core::sort_games(&mut self.games);
                self.load_covers(true, false, context);
            }
            Message::RemoveGame(id) => self.removing = Some(id),
            Message::RemoveClosed(result) => {
                if let Some(id) = self.removing.take()
                    && result == DialogResult::Primary
                {
                    // A pinned one leaves quick access too, and its cover goes.
                    let game = self.games.iter().find(|game| game.id == id).cloned();
                    let pinned = game.as_ref().is_some_and(|game| game.pinned.is_some());
                    context.spawn_background(move |_cancel| {
                        let removed = gm_core::custom::remove(&id).and_then(|()| {
                            if let Some(game) = &game {
                                gm_core::covers::clear_own(game)?;
                            }
                            if pinned {
                                gm_core::config::set_pinned(&id, false)
                            } else {
                                Ok(())
                            }
                        });
                        Message::CustomGameRemoved(removed.map(|()| id).map_err(|e| e.to_string()))
                    });
                }
            }
            Message::CustomGameRemoved(Ok(id)) => {
                self.games.retain(|game| game.id != id);
                self.icons.forget(&id);
                self.drop_covers_of_missing_games();
                self.selected = library::keep_selection(&self.games, self.selected.as_deref());
                self.tidy_view();
            }
            Message::CustomGameAdded(Err(e)) | Message::CustomGameRemoved(Err(e)) => {
                self.error = Some(e);
            }
        }
    }
}
