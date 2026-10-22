#![windows_subsystem = "windows"]

mod edit;
mod library;

use edit::{EditDraft, EditField};
use gm_core::custom::NewGame;
use gm_core::{Game, Launch, ScanReport, Store};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use windows_pickers::OpenFilePicker;
use windows_reactor::*;

/// Icons are extracted at 128 px so they stay sharp up to 96 DIPs at 133% or 40 DIPs at 320%.
const ICON_PIXELS: u32 = 128;
const LIST_ICON_SIZE: f64 = 32.0;
const HEADER_ICON_SIZE: f64 = 96.0;
const SIDEBAR_WIDTH: f64 = 340.0;
/// "Game" glyph from Segoe Fluent Icons, shown while an icon loads or when a game has none.
const GAME_GLYPH: &str = "\u{E7FC}";

#[derive(Clone)]
enum Message {
    Scan,
    Scanned(ScanReport),
    /// The games saved by the last scan, shown until the first scan of this start finishes.
    SnapshotLoaded(Vec<Game>),
    /// PNG icons keyed by game id.
    IconsLoaded(Vec<(String, Arc<[u8]>)>),
    Search(String),
    FilterStore(Option<Store>),
    /// A game picked in the sidebar. `None` (e.g. the list losing its selection while filtered)
    /// keeps the current game in the main panel.
    Select(Option<String>),
    /// Launches a game: its default action, or the alternative at `option` in `alt_launches`.
    Launch {
        id: String,
        option: Option<usize>,
    },
    /// Opens the game's install folder in File Explorer.
    OpenFolder(String),
    /// Opens the game's PCGamingWiki page in the browser.
    OpenWiki(String),
    /// The Game settings expander was opened or closed.
    SettingsExpanded(bool),
    /// A Game settings field of the selected game changed.
    EditField(EditField, String),
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
    Dropped(DroppedData),
    /// The Add a game dialog's starting values, from a chosen program or a dropped file.
    DraftReady(Result<NewGame, String>),
    DraftName(String),
    DraftArgs(String),
    /// The Add game dialog closed: `Primary` is Add.
    DraftClosed(ContentDialogResult),
    /// The custom game that was saved, completed like a scanned game.
    CustomGameAdded(Result<Box<Game>, String>),
    /// Asks to confirm removing the custom game with this id.
    RemoveGame(String),
    /// The remove confirmation closed: `Primary` is Remove.
    RemoveClosed(ContentDialogResult),
    /// The id of the custom game that was removed.
    CustomGameRemoved(Result<String, String>),
}

struct GameManager {
    games: Vec<Game>,
    icons: HashMap<String, EncodedImage>,
    query: String,
    store_filter: Option<Store>,
    selected: Option<String>,
    scanning: bool,
    error: Option<String>,
    /// The file dialog for a custom game's program is open, or a chosen or dropped file is read.
    picking: bool,
    /// The custom game being set up in the Add a game dialog.
    draft: Option<NewGame>,
    /// The custom game whose removal waits for confirmation.
    removing: Option<String>,
    /// The Game settings expander is open (kept while switching games).
    settings_open: bool,
    /// The Game settings being edited; see `selected_draft`.
    edit: Option<EditDraft>,
    /// Game settings are being saved or reset.
    saving: bool,
}

impl GameManager {
    fn start_scan(&mut self, context: &ComponentContext<Self>) {
        self.scanning = true;
        context.spawn_background(|_cancel| {
            let report = gm_core::scan_all();
            // The next start shows these games at once. A cache only: failures are ignored.
            let _ = gm_core::snapshot::save(&report.games);
            Message::Scanned(report)
        });
    }

    /// Add game and dropping a game's file. Not while scanning: a scan that started before the
    /// save would drop the new game.
    fn can_add_game(&self) -> bool {
        !self.scanning && !self.picking && self.draft.is_none()
    }

    /// Extracts icons after the list is shown, so a large library appears without waiting.
    /// `prune` (full library only) also drops cached icons no game uses any more.
    fn load_icons(games: Vec<Game>, prune: bool, context: &ComponentContext<Self>) {
        context.spawn_background(move |_cancel| {
            let icons = if prune {
                gm_core::icon::refresh_all(&games, ICON_PIXELS)
            } else {
                gm_core::icon::extract_all(&games, ICON_PIXELS)
            };
            Message::IconsLoaded(
                icons
                    .into_iter()
                    .map(|(id, png)| (id, png.into()))
                    .collect(),
            )
        });
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

    /// After games are removed: a store filter with no games left goes back to all stores.
    fn drop_empty_store_filter(&mut self) {
        if self
            .store_filter
            .is_some_and(|store| !self.games.iter().any(|game| game.store == store))
        {
            self.store_filter = None;
        }
    }

    fn icon_view(&self, game: &Game, size: f64) -> View {
        match self.icons.get(&game.id) {
            Some(png) => Image::new()
                .source_data(png.clone())
                .stretch(Stretch::Uniform)
                .width(size)
                .height(size)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
            None => FontIcon::new()
                .glyph(GAME_GLYPH)
                .width(size)
                .height(size)
                .opacity(0.5)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        }
    }

    /// Global controls: app title, scan status, Add game and Rescan.
    fn top_bar(&self, context: &ViewContext<Self>) -> View {
        let status = if self.scanning {
            "Scanning libraries…".to_owned()
        } else {
            format!("{} games", self.games.len())
        };

        let title = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .grid_column(0)
            .children((
                FontIcon::new()
                    .glyph(GAME_GLYPH)
                    .vertical_alignment(VerticalAlignment::Center),
                TextBlock::new()
                    .text("Game Manager")
                    .font_size(20.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .vertical_alignment(VerticalAlignment::Center),
            ));
        let controls = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .grid_column(2)
            .children((
                ProgressRing::new()
                    .is_active(self.scanning)
                    .width(20.0)
                    .height(20.0),
                TextBlock::new()
                    .text(status)
                    .opacity(0.7)
                    .vertical_alignment(VerticalAlignment::Center),
                Button::new()
                    .is_enabled(self.can_add_game())
                    .on_click(context.message(Message::AddGame))
                    .automation_name("Add game")
                    .content(icon_label(Symbol::Add, "Add game"))
                    .tooltip("Or drop a game's .exe or its shortcut on the window"),
                Button::new()
                    .is_enabled(!self.scanning)
                    .on_click(context.message(Message::Scan))
                    .automation_name("Rescan")
                    .content(icon_label(Symbol::Refresh, "Rescan")),
            ));

        Border::new()
            .padding(Thickness::xy(20.0, 10.0))
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::new(0.0, 0.0, 0.0, 1.0))
            .grid_row(0)
            .content(
                Grid::new()
                    .columns([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
                    .children((title, controls)),
            )
            .into()
    }

    /// Game library: search, store filter and the list of detected games.
    fn sidebar(&self, context: &ViewContext<Self>) -> View {
        let search = TextBox::new()
            .placeholder_text("Search games")
            .automation_name("Search games")
            .text(self.query.clone())
            .on_text_changed(context.callback(|text: Rc<str>| Message::Search(text.to_string())))
            .grid_row(0);

        let options = library::store_options(&self.games);
        let stores: Vec<Option<Store>> = options.iter().map(|(store, _)| *store).collect();
        let store_index = stores.iter().position(|store| *store == self.store_filter);
        let store_filter = ComboBox::new()
            .items_source(options.into_iter().map(|(_, label)| label))
            .selected_index(store_index)
            .on_selection_changed(context.callback(move |index: Option<usize>| {
                Message::FilterStore(index.and_then(|i| stores.get(i).copied()).flatten())
            }))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .automation_name("Store")
            .grid_row(1);

        let visible = library::filter(&self.games, &self.query, self.store_filter);
        let ids: Vec<String> = visible.iter().map(|&i| self.games[i].id.clone()).collect();
        let selected_index = self
            .selected
            .as_ref()
            .and_then(|selected| ids.iter().position(|id| id == selected));

        let items = visible.iter().map(|&index| {
            let game = &self.games[index];
            let row = StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .margin(Thickness::xy(0.0, 4.0))
                .children((
                    self.icon_view(game, LIST_ICON_SIZE),
                    StackPanel::new()
                        .vertical_alignment(VerticalAlignment::Center)
                        .children((
                            TextBlock::new()
                                .text(game.name.clone())
                                .text_trimming(TextTrimming::CharacterEllipsis)
                                .max_width(SIDEBAR_WIDTH - 100.0),
                            TextBlock::new()
                                .text(game.store.name())
                                .font_size(12.0)
                                .opacity(0.6),
                        )),
                ));
            DataItem::new(game.id.clone(), &game.name).content(row)
        });
        // windows-reactor applies `selected_index` while it is still changing a live list's rows,
        // which can select a row that is not there yet (E_INVALIDARG, which stops the app) or
        // leave rows showing another game's name. So every different set of visible games gets a
        // new list, keyed by those games, instead of changing the rows in place.
        let list_key = library::list_key(&ids);
        let list: View = if visible.is_empty() && !self.games.is_empty() {
            TextBlock::new()
                .text("No games match the search.")
                .opacity(0.7)
                .margin(Thickness::uniform(8.0))
                .into()
        } else {
            ListView::new()
                .selection_mode(ListViewSelectionMode::Single)
                .selected_index(selected_index)
                .on_selection_changed(context.callback(move |index: Option<usize>| {
                    Message::Select(index.and_then(|i| ids.get(i).cloned()))
                }))
                .items(items)
                .into()
        };
        let list = Grid::new()
            .grid_row(2)
            .keyed_children(std::iter::once(KeyedView::new(list_key, list)));

        Border::new()
            .width(SIDEBAR_WIDTH)
            .padding(Thickness::uniform(12.0))
            .background(ThemeBrush::CardBackground)
            .border_brush(ThemeBrush::CardStroke)
            .border_thickness(Thickness::new(0.0, 0.0, 1.0, 0.0))
            .grid_column(0)
            .content(
                Grid::new()
                    .rows([GridLength::Auto, GridLength::Auto, GridLength::Star(1.0)])
                    .row_spacing(8.0)
                    .children((search, store_filter, list)),
            )
            .into()
    }

    /// The selected game: header with title and install path, then the action and metadata bar.
    fn main_panel(&self, context: &ViewContext<Self>) -> View {
        let Some(game) = self.selected_game() else {
            let message = if self.scanning {
                "Looking for installed games…"
            } else if self.games.is_empty() {
                "No installed games were found. Use Add game, or drop a game's .exe or its \
                 shortcut here."
            } else {
                "Select a game in the library."
            };
            return TextBlock::new()
                .text(message)
                .opacity(0.7)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .grid_column(1)
                .into();
        };

        let install_path = game.install_dir.as_ref().map_or_else(
            || "Install folder unknown".to_owned(),
            |dir| dir.display().to_string(),
        );
        // The game's own program, inside the install folder when it can be shown that way.
        let program = game.program.as_deref().map_or_else(
            || "Game program unknown".to_owned(),
            |program| {
                let shown = game
                    .install_dir
                    .as_deref()
                    .and_then(|dir| program.strip_prefix(dir).ok())
                    .unwrap_or(program);
                format!("Game program: {}", shown.display())
            },
        );
        let header = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(20.0)
            .children((
                self.icon_view(game, HEADER_ICON_SIZE),
                StackPanel::new()
                    .spacing(6.0)
                    .vertical_alignment(VerticalAlignment::Center)
                    .children((
                        TextBlock::new()
                            .text(game.name.clone())
                            .font_size(32.0)
                            .font_weight(FontWeight::SEMI_BOLD)
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(install_path)
                            .opacity(0.7)
                            .is_text_selection_enabled(true)
                            .text_wrapping(TextWrapping::Wrap),
                        TextBlock::new()
                            .text(program)
                            .opacity(0.7)
                            .is_text_selection_enabled(true)
                            .text_wrapping(TextWrapping::Wrap),
                    )),
            ));

        let play = Button::new()
            .style(ButtonStyle::Accent)
            .on_click(context.message(Message::Launch {
                id: game.id.clone(),
                option: None,
            }))
            .automation_name(format!("Play {}", game.name))
            .content(icon_label(Symbol::Play, "Play"));
        let alternatives = game.alt_launches.iter().enumerate().map(|(i, alt)| {
            let button = Button::new()
                .on_click(context.message(Message::Launch {
                    id: game.id.clone(),
                    option: Some(i),
                }))
                .content(alt.label.clone());
            KeyedView::new(format!("alt-{i}"), button)
        });
        let open_folder = game.install_dir.is_some().then(|| {
            let button = Button::new()
                .on_click(context.message(Message::OpenFolder(game.id.clone())))
                .automation_name(format!("Open the folder of {}", game.name))
                .content(icon_label(Symbol::Folder, "Open folder"));
            KeyedView::new("open-folder", button)
        });
        let wiki = Button::new()
            .on_click(context.message(Message::OpenWiki(game.id.clone())))
            .automation_name(format!("Open {} on PCGamingWiki", game.name))
            .content(icon_label(Symbol::Globe, "PCGamingWiki"))
            .tooltip("Fixes and settings for this game on PCGamingWiki");
        let remove = (game.store == Store::Custom).then(|| {
            let button = Button::new()
                .on_click(context.message(Message::RemoveGame(game.id.clone())))
                .automation_name(format!("Remove {}", game.name))
                .content(icon_label(Symbol::Delete, "Remove"));
            KeyedView::new("remove", button)
        });
        let store = TextBlock::new()
            .text(format!("Store: {}", game.store.name()))
            .opacity(0.7)
            .vertical_alignment(VerticalAlignment::Center)
            .margin(Thickness::new(12.0, 0.0, 0.0, 0.0));
        let arch = game.arch.map(|arch| {
            let text = TextBlock::new()
                .text(arch.label())
                .opacity(0.7)
                .vertical_alignment(VerticalAlignment::Center);
            KeyedView::new("arch", text)
        });
        let actions = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .keyed_children(
                std::iter::once(KeyedView::new("play", play))
                    .chain(alternatives)
                    .chain(open_folder)
                    .chain(std::iter::once(KeyedView::new("wiki", wiki)))
                    .chain(remove)
                    .chain(std::iter::once(KeyedView::new("store", store)))
                    .chain(arch),
            );

        ScrollViewer::new()
            .grid_column(1)
            .content(
                StackPanel::new()
                    .spacing(28.0)
                    .margin(Thickness::uniform(32.0))
                    .children((header, actions, self.game_settings(game, context))),
            )
            .into()
    }

    /// The Game settings expander: name, launch program, arguments and game program.
    fn game_settings(&self, game: &Game, context: &ViewContext<Self>) -> View {
        let draft = EditDraft::for_game(self.edit.as_ref(), game);
        let busy = self.saving || self.scanning || self.picking;
        let store = game.store.name();
        let text_box = |field: EditField, name: &str| {
            TextBox::new()
                .text(draft.fields.get(field))
                .automation_name(name)
                .on_text_changed(
                    context
                        .callback(move |text: Rc<str>| Message::EditField(field, text.to_string())),
                )
        };
        let label = |text: &str| {
            TextBlock::new()
                .text(text)
                .font_weight(FontWeight::SEMI_BOLD)
        };
        let hint = |text: String| {
            TextBlock::new()
                .text(text)
                .font_size(12.0)
                .opacity(0.7)
                .text_wrapping(TextWrapping::Wrap)
        };
        let with_browse = |text_box: TextBox, field: EditField, name: &str| -> View {
            Grid::new()
                .columns([GridLength::Star(1.0), GridLength::Auto])
                .children((
                    text_box.grid_column(0),
                    Button::new()
                        .is_enabled(!busy)
                        .on_click(context.message(Message::BrowseEdit(field)))
                        .automation_name(name)
                        .margin(Thickness::new(8.0, 0.0, 0.0, 0.0))
                        .grid_column(1)
                        .content("Browse…"),
                ))
                .into()
        };

        // Arguments reach a program, or a Steam game as launch options; other store links
        // cannot take any.
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

        let form = StackPanel::new().spacing(6.0).children((
            label("Name"),
            text_box(EditField::Name, "Game name"),
            label("Launch .exe").margin(Thickness::new(0.0, 10.0, 0.0, 0.0)),
            with_browse(
                text_box(EditField::Launch, "Launch .exe")
                    .placeholder_text(format!("Empty: the game starts through {store}")),
                EditField::Launch,
                "Browse for the launch .exe",
            ),
            hint(format!(
                "The program Play starts. Leave it empty to start the game through {store}."
            )),
            label("Arguments").margin(Thickness::new(0.0, 10.0, 0.0, 0.0)),
            text_box(EditField::Args, "Arguments").is_enabled(args_enabled),
            hint(args_hint),
            label("Game program").margin(Thickness::new(0.0, 10.0, 0.0, 0.0)),
            with_browse(
                text_box(EditField::Program, "Game program"),
                EditField::Program,
                "Browse for the game program",
            ),
            hint("The game's own .exe: used for its bitness, Open folder and mods.".to_owned()),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(12.0)
                .margin(Thickness::new(0.0, 12.0, 0.0, 0.0))
                .children((
                    Button::new()
                        .style(ButtonStyle::Accent)
                        .is_enabled(draft.is_dirty() && !busy)
                        .on_click(context.message(Message::SaveEdits))
                        .automation_name("Save game settings")
                        .content("Save"),
                    Button::new()
                        .is_enabled(draft.is_dirty() && !busy)
                        .on_click(context.message(Message::CancelEdits))
                        .automation_name("Cancel game settings changes")
                        .content("Cancel"),
                    Button::new()
                        .is_enabled(game.edits.is_some() && !busy)
                        .on_click(context.message(Message::ResetEdits))
                        .automation_name("Reset game settings")
                        .content(icon_label(Symbol::Undo, "Reset to detected")),
                )),
        ));

        let header = if game.edits.is_some() {
            "Game settings (edited)"
        } else {
            "Game settings"
        };
        Expander::new()
            .is_expanded(self.settings_open)
            .on_is_expanded_changed(context.callback(Message::SettingsExpanded))
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name("Game settings")
            .header(header)
            .content(form)
            .into()
    }

    /// Name and arguments for a custom game whose program was just chosen.
    fn add_dialog(&self, context: &ViewContext<Self>) -> ContentDialog {
        let (program, name, args) = self.draft.as_ref().map_or_else(Default::default, |draft| {
            (
                draft.program.display().to_string(),
                draft.name.clone(),
                draft.args.clone(),
            )
        });
        let has_name = !name.trim().is_empty();
        let label = |text: &str| {
            TextBlock::new()
                .text(text)
                .margin(Thickness::new(0.0, 8.0, 0.0, 0.0))
        };
        ContentDialog::new()
            .title("Add a game")
            .primary_button_text("Add")
            .close_button_text("Cancel")
            .default_button(ContentDialogButton::Primary)
            .is_primary_button_enabled(has_name)
            .is_open(self.draft.is_some())
            .on_closed(context.callback(Message::DraftClosed))
            .content(
                StackPanel::new().spacing(6.0).children((
                    TextBlock::new()
                        .text(program)
                        .opacity(0.7)
                        .is_text_selection_enabled(true)
                        .text_wrapping(TextWrapping::Wrap),
                    label("Name"),
                    TextBox::new()
                        .text(name)
                        .automation_name("Name")
                        .on_text_changed(
                            context.callback(|text: Rc<str>| Message::DraftName(text.to_string())),
                        ),
                    label("Command-line arguments (optional)"),
                    TextBox::new()
                        .text(args)
                        .placeholder_text("For example: -windowed")
                        .automation_name("Command-line arguments")
                        .on_text_changed(
                            context.callback(|text: Rc<str>| Message::DraftArgs(text.to_string())),
                        ),
                )),
            )
    }

    /// Confirms removing a custom game from the library.
    fn remove_dialog(&self, context: &ViewContext<Self>) -> ContentDialog {
        let name = self
            .removing
            .as_deref()
            .and_then(|id| self.games.iter().find(|game| game.id == id))
            .map(|game| game.name.clone())
            .unwrap_or_default();
        ContentDialog::new()
            .title(format!("Remove {name}?"))
            .primary_button_text("Remove")
            .close_button_text("Cancel")
            .is_open(self.removing.is_some())
            .on_closed(context.callback(Message::RemoveClosed))
            .content(
                TextBlock::new()
                    .text("It is removed from the library only. The game's files are not deleted.")
                    .text_wrapping(TextWrapping::Wrap),
            )
    }
}

/// Button content: a symbol followed by a label.
fn icon_label(symbol: Symbol, label: &str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            SymbolIcon::new().symbol(symbol),
            TextBlock::new().text(label),
        ))
        .into()
}

impl Component for GameManager {
    type Input = ();
    type Message = Message;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let mut this = Self {
            games: Vec::new(),
            icons: HashMap::new(),
            query: String::new(),
            store_filter: None,
            selected: None,
            scanning: false,
            error: None,
            picking: false,
            draft: None,
            removing: None,
            settings_open: false,
            edit: None,
            saving: false,
        };
        // The last scan's games show at once while the fresh scan runs.
        context.spawn_background(|_cancel| {
            Message::SnapshotLoaded(gm_core::snapshot::load().unwrap_or_default())
        });
        this.start_scan(context);
        this
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::Scan => self.start_scan(context),
            Message::Scanned(report) => {
                self.scanning = false;
                self.games = report.games;
                self.error = (!report.errors.is_empty()).then(|| report.errors.join("\n"));
                self.selected = library::keep_selection(&self.games, self.selected.as_deref());
                self.drop_empty_store_filter();
                Self::load_icons(self.games.clone(), true, context);
            }
            Message::SnapshotLoaded(games) => {
                // Only before the first scan has listed anything; the scan then replaces it.
                if self.scanning && self.games.is_empty() && !games.is_empty() {
                    self.games = games;
                    self.selected = library::keep_selection(&self.games, self.selected.as_deref());
                    Self::load_icons(self.games.clone(), false, context);
                }
            }
            Message::IconsLoaded(icons) => {
                self.icons.extend(
                    icons
                        .into_iter()
                        .map(|(id, png)| (id, EncodedImage::new(png))),
                );
            }
            Message::Search(query) => self.query = query,
            Message::FilterStore(store) => self.store_filter = store,
            Message::Select(Some(id)) => self.selected = Some(id),
            Message::Select(None) => {}
            Message::Launch { id, option } => {
                let Some(game) = self.games.iter().find(|game| game.id == id) else {
                    return;
                };
                let launch = match option {
                    None => Some(&game.launch),
                    Some(i) => game.alt_launches.get(i).map(|alt| &alt.launch),
                };
                if let Some(launch) = launch {
                    self.error = gm_core::launch(launch).err();
                }
            }
            Message::OpenFolder(id) => {
                let Some(game) = self.games.iter().find(|game| game.id == id) else {
                    return;
                };
                if let Some(dir) = &game.install_dir {
                    self.error = gm_core::open_folder(dir, game.program.as_deref()).err();
                }
            }
            Message::OpenWiki(id) => {
                let Some(game) = self.games.iter().find(|game| game.id == id) else {
                    return;
                };
                let page = Launch::Uri(gm_core::pcgamingwiki::page_url(game));
                self.error = gm_core::launch(&page).err();
            }
            Message::SettingsExpanded(open) => self.settings_open = open,
            Message::EditField(field, value) => {
                if let Some(mut draft) = self.selected_draft() {
                    draft.fields.set(field, value);
                    self.edit = Some(draft);
                }
            }
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
                    EditField::Launch => OpenFilePicker::new()
                        .title(format!("Choose the program Play starts for {}", game.name))
                        .filter_extensions("Programs", ["exe", "bat", "cmd"]),
                    _ => OpenFilePicker::new()
                        .title(format!("Choose the .exe of {}", game.name))
                        .filter_extensions("Programs", ["exe"]),
                };
                if let Some(folder) = folder {
                    picker = picker.folder(folder);
                }
                self.picking = picker.request(context, move |picked| {
                    let picked =
                        picked.map_err(|e| format!("could not choose a program: {}", e.message()));
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
                    let saved = edit::check_paths(&edits)
                        .and_then(|()| gm_core::config::save_edits(&id, &edits));
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
                    Message::EditsSaved(gm_core::config::clear_edits(&id))
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
                self.picking =
                    OpenFilePicker::new()
                        .title("Choose the game's program")
                        .filter_extensions("Programs", ["exe", "bat", "cmd"])
                        .filter_all()
                        .request(context, |picked| {
                            Message::ProgramPicked(picked.map_err(|e| {
                                format!("could not choose a program: {}", e.message())
                            }))
                        });
                if !self.picking {
                    self.error = Some("The file dialog is not available right now.".to_owned());
                }
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
            Message::Dropped(DroppedData::StorageItems(items)) => {
                // The drop policy only accepts files while a game can be added.
                if !self.can_add_game() {
                    return;
                }
                self.picking = true;
                let paths: Vec<PathBuf> = items.into_iter().map(|item| item.path.into()).collect();
                // Reading a shortcut uses COM and the disk.
                context.spawn_background(move |_cancel| {
                    Message::DraftReady(NewGame::from_dropped(&paths))
                });
            }
            Message::Dropped(_) => {}
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
                    && result == ContentDialogResult::Primary
                {
                    context.spawn_background(move |_cancel| {
                        let added = gm_core::custom::add(&draft).map(|custom| {
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
                // Show the new game even when the current search or store filter would hide it.
                if library::filter(std::slice::from_ref(&game), &self.query, self.store_filter)
                    .is_empty()
                {
                    self.query.clear();
                    self.store_filter = None;
                }
                self.selected = Some(game.id.clone());
                Self::load_icons(vec![game.clone()], false, context);
                self.games.push(game);
                gm_core::sort_games(&mut self.games);
            }
            Message::RemoveGame(id) => self.removing = Some(id),
            Message::RemoveClosed(result) => {
                if let Some(id) = self.removing.take()
                    && result == ContentDialogResult::Primary
                {
                    context.spawn_background(move |_cancel| {
                        Message::CustomGameRemoved(gm_core::custom::remove(&id).map(|()| id))
                    });
                }
            }
            Message::CustomGameRemoved(Ok(id)) => {
                self.games.retain(|game| game.id != id);
                self.icons.remove(&id);
                self.selected = library::keep_selection(&self.games, self.selected.as_deref());
                self.drop_empty_store_filter();
            }
            Message::CustomGameAdded(Err(e)) | Message::CustomGameRemoved(Err(e)) => {
                self.error = Some(e);
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("Game Manager");
        context.window_visuals(
            WindowVisuals::new()
                .backdrop(WindowBackdrop::Mica)
                .client_size(1200.0, 760.0)
                .constraints(WindowConstraints {
                    min_width: Some(860.0),
                    min_height: Some(520.0),
                    ..WindowConstraints::default()
                }),
        );

        let error = InfoBar::new()
            .title("Something went wrong")
            .message(self.error.clone().unwrap_or_default())
            .severity(InfoBarSeverity::Error)
            .is_open(self.error.is_some())
            .on_closed(context.message(Message::DismissError))
            .margin(Thickness::xy(20.0, 8.0))
            .grid_row(1);

        let body = Grid::new()
            .columns([GridLength::Auto, GridLength::Star(1.0)])
            .grid_row(2)
            .children((self.sidebar(context), self.main_panel(context)));

        let window = Grid::new()
            .rows([GridLength::Auto, GridLength::Auto, GridLength::Star(1.0)])
            .children((
                // Each view can carry one dialog.
                self.top_bar(context)
                    .content_dialog(self.add_dialog(context)),
                error,
                body.content_dialog(self.remove_dialog(context)),
            ));

        // A game's program or shortcut dropped anywhere on the window adds it. The transparent
        // background makes empty areas accept the drop too; without a policy nothing is accepted.
        let drop_policy = if self.can_add_game() {
            DragDropPolicy::new().storage_items(
                DragDropAction::new(DragDropOperation::Link).caption("Add as a game"),
            )
        } else {
            DragDropPolicy::new()
        };
        Border::new()
            .background(Color::transparent())
            .drop_policy(drop_policy)
            .on_drop(context.callback(Message::Dropped))
            .content(window)
            .into()
    }
}

fn main() {
    App::run_component::<GameManager>(()).unwrap();
}
