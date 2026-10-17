#![windows_subsystem = "windows"]

use gm_core::{Game, ScanReport};
use std::collections::HashMap;
use std::sync::Arc;
use windows_reactor::*;

/// Icons are shown at 40×40 (80 px at 200% scaling); extracting at 128 px keeps them sharp up to
/// 320% scaling.
const ICON_SIZE: f64 = 40.0;
const ICON_PIXELS: u32 = 128;
/// "Game" glyph from Segoe Fluent Icons, shown while an icon loads or when a game has none.
const GAME_GLYPH: &str = "\u{E7FC}";

#[derive(Clone)]
enum Message {
    Scan,
    Scanned(ScanReport),
    /// PNG icons keyed by game id.
    IconsLoaded(Vec<(String, Arc<[u8]>)>),
    /// Launches a game: its default action, or the alternative at `option` in `alt_launches`.
    Launch {
        game: usize,
        option: Option<usize>,
    },
    DismissError,
}

struct GameManager {
    games: Vec<Game>,
    icons: HashMap<String, EncodedImage>,
    scanning: bool,
    error: Option<String>,
}

impl GameManager {
    fn start_scan(&mut self, context: &ComponentContext<Self>) {
        self.scanning = true;
        context.spawn_background(|_cancel| Message::Scanned(gm_core::scan_all()));
    }

    /// Extracts icons after the list is shown, so a large library appears without waiting.
    fn load_icons(&self, context: &ComponentContext<Self>) {
        let games = self.games.clone();
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

    fn icon_view(&self, game: &Game) -> View {
        match self.icons.get(&game.id) {
            Some(png) => Image::new()
                .source_data(png.clone())
                .stretch(Stretch::Uniform)
                .width(ICON_SIZE)
                .height(ICON_SIZE)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
            None => FontIcon::new()
                .glyph(GAME_GLYPH)
                .width(ICON_SIZE)
                .height(ICON_SIZE)
                .opacity(0.5)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        }
    }
}

impl Component for GameManager {
    type Input = ();
    type Message = Message;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let mut this = Self {
            games: Vec::new(),
            icons: HashMap::new(),
            scanning: false,
            error: None,
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
                self.load_icons(context);
            }
            Message::IconsLoaded(icons) => {
                self.icons = icons
                    .into_iter()
                    .map(|(id, png)| (id, EncodedImage::new(png)))
                    .collect();
            }
            Message::Launch { game, option } => {
                let Some(game) = self.games.get(game) else {
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
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("Game Manager");
        context.window_visuals(WindowVisuals::new().backdrop(WindowBackdrop::Mica));

        let status = if self.scanning {
            "Scanning libraries…".to_owned()
        } else {
            format!("{} games", self.games.len())
        };

        let header = StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(16.0)
            .grid_row(0)
            .children((
                TextBlock::new()
                    .text("Game Manager")
                    .font_size(28.0)
                    .font_weight(FontWeight::SEMI_BOLD),
                Button::new()
                    .is_enabled(!self.scanning)
                    .on_click(context.message(Message::Scan))
                    .content("Rescan"),
                ProgressRing::new()
                    .is_active(self.scanning)
                    .width(20.0)
                    .height(20.0),
                TextBlock::new()
                    .text(status)
                    .opacity(0.7)
                    .vertical_alignment(VerticalAlignment::Center),
            ));

        let error = InfoBar::new()
            .title("Something went wrong")
            .message(self.error.clone().unwrap_or_default())
            .severity(InfoBarSeverity::Error)
            .is_open(self.error.is_some())
            .on_closed(context.message(Message::DismissError))
            .grid_row(1);

        let items = self.games.iter().enumerate().map(|(index, game)| {
            let play = Button::new()
                .style(ButtonStyle::Accent)
                .on_click(context.message(Message::Launch {
                    game: index,
                    option: None,
                }))
                .content("Play");
            let alternatives = game.alt_launches.iter().enumerate().map(|(i, alt)| {
                let button = Button::new()
                    .on_click(context.message(Message::Launch {
                        game: index,
                        option: Some(i),
                    }))
                    .content(alt.label.clone());
                KeyedView::new(format!("alt-{i}"), button)
            });
            let details = StackPanel::new().children((
                TextBlock::new().text(game.name.clone()).font_size(16.0),
                TextBlock::new().text(game.store.name()).opacity(0.6),
            ));

            let row = StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(16.0)
                .margin(Thickness::xy(0.0, 6.0))
                .keyed_children(
                    [
                        KeyedView::new("icon", self.icon_view(game)),
                        KeyedView::new("play", play),
                    ]
                    .into_iter()
                    .chain(alternatives)
                    .chain(std::iter::once(KeyedView::new("details", details))),
                );
            KeyedView::new(
                game.id.clone(),
                ListViewItem::new().tag(game.id.clone()).content(row),
            )
        });

        let list = ListView::new()
            .selection_mode(ListViewSelectionMode::None)
            .grid_row(2)
            .collection_slot(ListViewSlot::Items, items);

        Grid::new()
            .rows([GridLength::Auto, GridLength::Auto, GridLength::Star(1.0)])
            .row_spacing(12.0)
            .margin(Thickness::uniform(24.0))
            .children((header, error, list))
    }
}

fn main() {
    App::run_component::<GameManager>(()).unwrap();
}
