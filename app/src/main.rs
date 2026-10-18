#![windows_subsystem = "windows"]

mod library;
mod picker;

use gm_core::custom::CustomGame;
use gm_core::{Game, ScanReport, Store};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
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
    DismissError,
    /// Opens the file dialog to choose a custom game's program.
    AddGame,
    /// The chosen program and a suggested name; `None` when the dialog was cancelled.
    ProgramPicked(Result<Option<(PathBuf, String)>, String>),
    DraftName(String),
    DraftArgs(String),
    /// The Add game dialog closed: `Primary` is Add.
    DraftClosed(ContentDialogResult),
    CustomGameAdded(Result<CustomGame, String>),
    /// Asks to confirm removing the custom game with this id.
    RemoveGame(String),
    /// The remove confirmation closed: `Primary` is Remove.
    RemoveClosed(ContentDialogResult),
    /// The id of the custom game that was removed.
    CustomGameRemoved(Result<String, String>),
}

/// A custom game being set up in the Add game dialog.
struct Draft {
    program: PathBuf,
    name: String,
    args: String,
}

struct GameManager {
    games: Vec<Game>,
    icons: HashMap<String, EncodedImage>,
    query: String,
    store_filter: Option<Store>,
    selected: Option<String>,
    scanning: bool,
    error: Option<String>,
    /// The file dialog for a custom game's program is open.
    picking: bool,
    draft: Option<Draft>,
    /// The custom game whose removal waits for confirmation.
    removing: Option<String>,
}

impl GameManager {
    fn start_scan(&mut self, context: &ComponentContext<Self>) {
        self.scanning = true;
        context.spawn_background(|_cancel| Message::Scanned(gm_core::scan_all()));
    }

    /// Extracts icons after the list is shown, so a large library appears without waiting.
    fn load_icons(games: Vec<Game>, context: &ComponentContext<Self>) {
        context.spawn_background(move |_cancel| {
            let icons = gm_core::icon::extract_all(&games, ICON_PIXELS);
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
                // Not while scanning: a scan that started before the save would drop the new game.
                Button::new()
                    .is_enabled(!self.scanning && !self.picking)
                    .on_click(context.message(Message::AddGame))
                    .automation_name("Add game")
                    .content(icon_label(Symbol::Add, "Add game")),
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
    }

    /// Game library: search, store filter and the list of detected games.
    fn sidebar(&self, context: &ViewContext<Self>) -> View {
        let search = TextBox::new()
            .placeholder_text("Search games")
            .automation_name("Search games")
            .text(self.query.clone())
            .on_text_changed(context.callback(Message::Search))
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
            KeyedView::new(
                game.id.clone(),
                ListViewItem::new().tag(game.id.clone()).content(row),
            )
        });
        let list: View = if visible.is_empty() && !self.games.is_empty() {
            TextBlock::new()
                .text("No games match the search.")
                .opacity(0.7)
                .margin(Thickness::uniform(8.0))
                .grid_row(2)
                .into()
        } else {
            ListView::new()
                .selection_mode(ListViewSelectionMode::Single)
                .selected_index(selected_index)
                .on_selection_changed(context.callback(move |index: Option<usize>| {
                    Message::Select(index.and_then(|i| ids.get(i).cloned()))
                }))
                .grid_row(2)
                .collection_slot(ListViewSlot::Items, items)
        };

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
    }

    /// The selected game: header with title and install path, then the action and metadata bar.
    fn main_panel(&self, context: &ViewContext<Self>) -> View {
        let Some(game) = self.selected_game() else {
            let message = if self.scanning {
                "Looking for installed games…"
            } else if self.games.is_empty() {
                "No installed games were found. Use Add game to add one by hand."
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
        let actions = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(12.0)
            .keyed_children(
                std::iter::once(KeyedView::new("play", play))
                    .chain(alternatives)
                    .chain(remove)
                    .chain(std::iter::once(KeyedView::new("store", store))),
            );

        ScrollViewer::new().grid_column(1).content(
            StackPanel::new()
                .spacing(28.0)
                .margin(Thickness::uniform(32.0))
                .children((header, actions)),
        )
    }

    /// Name and arguments for a custom game whose program was just chosen.
    fn add_dialog(&self, context: &ViewContext<Self>) -> View {
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
                        .on_text_changed(context.callback(Message::DraftName)),
                    label("Command-line arguments (optional)"),
                    TextBox::new()
                        .text(args)
                        .placeholder_text("For example: -windowed")
                        .automation_name("Command-line arguments")
                        .on_text_changed(context.callback(Message::DraftArgs)),
                )),
            )
    }

    /// Confirms removing a custom game from the library.
    fn remove_dialog(&self, context: &ViewContext<Self>) -> View {
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
        };
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
                Self::load_icons(self.games.clone(), context);
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
            Message::DismissError => self.error = None,
            Message::AddGame => {
                self.picking = true;
                let owner = picker::Owner::active();
                context.spawn_background(move |_cancel| {
                    let picked = picker::pick_program(owner).map(|program| {
                        program.map(|program| {
                            let name = gm_core::custom::suggested_name(&program);
                            (program, name)
                        })
                    });
                    Message::ProgramPicked(picked)
                });
            }
            Message::ProgramPicked(picked) => {
                self.picking = false;
                match picked {
                    Ok(Some((program, name))) => {
                        self.draft = Some(Draft {
                            program,
                            name,
                            args: String::new(),
                        });
                    }
                    Ok(None) => {}
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
                        Message::CustomGameAdded(gm_core::custom::add(
                            &draft.name,
                            &draft.program,
                            &draft.args,
                        ))
                    });
                }
            }
            Message::CustomGameAdded(Ok(custom)) => {
                let game = custom.to_game();
                // Show the new game even when the current search or store filter would hide it.
                if library::filter(std::slice::from_ref(&game), &self.query, self.store_filter)
                    .is_empty()
                {
                    self.query.clear();
                    self.store_filter = None;
                }
                self.selected = Some(game.id.clone());
                Self::load_icons(vec![game.clone()], context);
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

        Grid::new()
            .rows([GridLength::Auto, GridLength::Auto, GridLength::Star(1.0)])
            .children((
                self.top_bar(context),
                error,
                body,
                self.add_dialog(context),
                self.remove_dialog(context),
            ))
    }
}

fn main() {
    App::run_component::<GameManager>(()).unwrap();
}
