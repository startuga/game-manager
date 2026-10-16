#![windows_subsystem = "windows"]

use gm_core::{Game, ScanReport};
use windows_reactor::*;

#[derive(Clone)]
enum Message {
    Scan,
    Scanned(ScanReport),
    Launch(usize),
    DismissError,
}

struct GameManager {
    games: Vec<Game>,
    scanning: bool,
    error: Option<String>,
}

impl GameManager {
    fn start_scan(&mut self, context: &ComponentContext<Self>) {
        self.scanning = true;
        context.spawn_background(|_cancel| Message::Scanned(gm_core::scan_all()));
    }
}

impl Component for GameManager {
    type Input = ();
    type Message = Message;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        let mut this = Self {
            games: Vec::new(),
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
            }
            Message::Launch(index) => {
                if let Some(game) = self.games.get(index) {
                    self.error = gm_core::launch(&game.launch).err();
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
            let row = StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(16.0)
                .margin(Thickness::xy(0.0, 6.0))
                .children((
                    Button::new()
                        .style(ButtonStyle::Accent)
                        .on_click(context.message(Message::Launch(index)))
                        .content("Play"),
                    StackPanel::new().children((
                        TextBlock::new().text(game.name.clone()).font_size(16.0),
                        TextBlock::new().text(game.store.name()).opacity(0.6),
                    )),
                ));
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
