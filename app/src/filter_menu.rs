//! Sort and filters: the drop-down button beside the sidebar's search, and its flyout.
//!
//! The flyout changes the library's view (`library::LibraryView`), which the sidebar and the
//! Library page both follow:
//! - **Sort**: by name either way or the most recently installed first; and Group by store.
//! - **Smart filters**: a check box per store that has games (none ticked is every store), and
//!   Hidden games while some are hidden.
//! - **Smart Collections**: the sets `library::in_collection` fills, each with its count, listed
//!   while they have games. One at most is on; it and the filters take turns (`library.rs`).
//! - **Clear all** takes the filters and the collection off; the sort stays.
//!
//! It stays open while its lines are clicked; a click outside or Escape closes it. The button
//! shows a dot while a filter or a collection is on.
//!
//! **Where it opens**: its layer lies over the sidebar and the page only (`app_view.rs`), so the
//! panel sits right under the button whatever is above them (the title bar, an error).
//!
//! **Saved** in `game-settings.json` (`gm_core::config::save_library_view`) when the flyout
//! closes, if anything changed: one write per visit. The saved view is read once at start. A
//! filter or collection that has no games left is taken off (`tidy_view`: after a scan, hiding
//! or removing a game), which is saved with the next change.

use gm_core::Store;

use crate::library::{self, Collection, LibraryView, Sort};
use crate::shell::WindowInfo;
use crate::shell::ctx::Ctx;
use crate::ui::el::{Align, Edges, El, Role, col, glyph, layers, row, scroll, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{check_square, dropdown_button};
use crate::{GameManager, Message};

/// The flyout's width, and its lines' heights.
const WIDTH: f32 = 280.0;
const ITEM_HEIGHT: f32 = 30.0;
const CAPTION_HEIGHT: f32 = 26.0;
const SEPARATOR_HEIGHT: f32 = 9.0;
const GAP: f32 = 2.0;
const PADDING: f32 = 4.0;
/// The sidebar's padding: the flyout's left edge is the search box's.
const SIDE: f32 = 10.0;
/// From the top of the sidebar to under the button, and the space kept under the flyout.
const TOP: f32 = SIDE + theme::CONTROL_HEIGHT + 4.0;
const MARGIN: f32 = 8.0;

/// The button's tooltip and UI Automation name.
const NAME: &str = "Sort and filters";

#[derive(Clone)]
pub(crate) enum FilterMessage {
    /// The button: opens the flyout, or closes it.
    Toggle,
    /// A click outside the flyout, or Escape.
    Close,
    Sort(Sort),
    GroupByStore(bool),
    /// Ticks or unticks a store.
    Store(Store),
    Hidden(bool),
    /// Picks a collection, or turns off the one that is on.
    Collection(Collection),
    Clear,
    /// The view saved last time, read at start.
    Loaded(Result<LibraryView, String>),
    /// The view was saved (or not).
    Saved(Result<(), String>),
}

impl From<FilterMessage> for Message {
    fn from(message: FilterMessage) -> Self {
        Message::Filter(message)
    }
}

/// The flyout, and what the settings file holds.
#[derive(Default)]
pub(crate) struct FilterState {
    pub open: bool,
    /// The view as it was last read or saved: closing the flyout saves only a change.
    saved: LibraryView,
    /// `mods.json` was read: only then does an empty mods collection mean it has no games.
    pub mods_ready: bool,
}

impl FilterState {
    /// What Escape closes: the flyout.
    pub fn escape(&self) -> Option<Message> {
        self.open.then(|| FilterMessage::Close.into())
    }
}

/// The flyout's lines, with the height they take.
struct Lines {
    lines: Vec<El<Message>>,
    height: f32,
}

impl Lines {
    fn push(&mut self, line: El<Message>, height: f32) {
        if !self.lines.is_empty() {
            self.height += GAP;
        }
        self.lines.push(line);
        self.height += height;
    }

    /// A part's title, in small capitals.
    fn caption(&mut self, label: &str) {
        let title = text(label.to_uppercase())
            .text_size(theme::TEXT_CAPTION)
            .weight(700)
            .color(theme::TEXT_TERTIARY);
        self.push(
            row([title])
                .padding(Edges::new(10.0, 4.0, 10.0, 0.0))
                .height(CAPTION_HEIGHT),
            CAPTION_HEIGHT,
        );
    }

    fn separator(&mut self) {
        let line = col([]).height(1.0).bg(theme::DIVIDER);
        self.push(col([line]).pad_xy(6.0, 4.0), SEPARATOR_HEIGHT);
    }

    /// One choice of several: a tick while it is on.
    fn choice(&mut self, label: &str, count: Option<usize>, on: bool, message: FilterMessage) {
        // White on the line's selection fill.
        let tick = glyph(if on { g::CHECK } else { "" }, 12.0).color(theme::TEXT);
        let name = match count {
            Some(count) => format!("{label}, {}", library::games_label(count)),
            None => label.to_owned(),
        };
        self.push(
            line(tick, label, count, name, Role::ListItem, on, message),
            ITEM_HEIGHT,
        );
    }

    /// A check box.
    fn check(&mut self, label: &str, count: Option<usize>, on: bool, message: FilterMessage) {
        let name = match count {
            Some(count) => format!("{label}, {}", library::games_label(count)),
            None => label.to_owned(),
        };
        self.push(
            line(
                check_square(Some(on)),
                label,
                count,
                name,
                Role::CheckBox,
                on,
                message,
            ),
            ITEM_HEIGHT,
        );
    }
}

/// A line of the flyout: its mark, its label and, at the right, how many games it has.
fn line(
    mark: El<Message>,
    label: &str,
    count: Option<usize>,
    name: String,
    role: Role,
    on: bool,
    message: FilterMessage,
) -> El<Message> {
    let mut parts = vec![mark.width(18.0), text(label).fill_width()];
    if let Some(count) = count {
        parts.push(
            text(count.to_string())
                .text_size(theme::TEXT_SMALL)
                .color(theme::TEXT_TERTIARY),
        );
    }
    row(parts)
        .gap(10.0)
        .pad_xy(10.0, 0.0)
        .height(ITEM_HEIGHT)
        .hover_bg(theme::SUBTLE_HOVER)
        .pressed_bg(theme::SUBTLE_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(role)
        .selected(on)
        .name(name)
        .on_click(message.into())
}

impl GameManager {
    /// What the view's collections need to know: the mods and the time.
    pub(crate) fn facts(&self) -> library::Facts<'_> {
        library::Facts {
            mods: &self.mods,
            now: library::now(),
        }
    }

    /// The games the view and the search show (indices into `games`).
    pub(crate) fn shown_games(&self) -> Vec<usize> {
        library::shown(&self.games, &self.query, &self.view, self.facts())
    }

    /// The drop-down button beside the search.
    pub(crate) fn filter_button(&self) -> El<Message> {
        dropdown_button(g::FILTER, NAME, library::is_filtered(&self.view))
            .on_click(FilterMessage::Toggle.into())
    }

    /// The open flyout over a backdrop that closes it, for the layer over the sidebar and the
    /// page.
    pub(crate) fn filter_layer(&self, window: &WindowInfo) -> Option<El<Message>> {
        if !self.filters.open {
            return None;
        }
        let view = &self.view;
        let mut lines = Lines {
            lines: Vec::new(),
            height: 0.0,
        };

        lines.caption("Sort");
        for sort in library::SORTS {
            lines.choice(
                library::sort_label(sort),
                None,
                view.sort == sort,
                FilterMessage::Sort(sort),
            );
        }
        lines.check(
            "Group by store",
            None,
            view.group_by_store,
            FilterMessage::GroupByStore(!view.group_by_store),
        );

        let stores = library::store_counts(&self.games, view);
        let hidden = library::hidden_count(&self.games);
        if !stores.is_empty() || hidden > 0 || view.hidden {
            lines.separator();
            lines.caption("Smart filters");
            for (store, count) in stores {
                lines.check(
                    store.name(),
                    Some(count),
                    view.stores.contains(&store),
                    FilterMessage::Store(store),
                );
            }
            if hidden > 0 || view.hidden {
                lines.check(
                    "Hidden games",
                    Some(hidden),
                    view.hidden,
                    FilterMessage::Hidden(!view.hidden),
                );
            }
        }

        let collections = library::collection_counts(&self.games, view, self.facts());
        if !collections.is_empty() {
            lines.separator();
            lines.caption("Smart Collections");
            for (collection, count) in collections {
                lines.choice(
                    library::collection_label(collection),
                    Some(count),
                    view.collection == Some(collection),
                    FilterMessage::Collection(collection),
                );
            }
        }

        lines.separator();
        let clear = row([
            glyph(g::CLOSE, 10.0).width(18.0),
            text("Clear all").fill_width(),
        ])
        .gap(10.0)
        .pad_xy(10.0, 0.0)
        .height(ITEM_HEIGHT)
        .hover_bg(theme::SUBTLE_HOVER)
        .pressed_bg(theme::SUBTLE_PRESSED)
        .radius(theme::RADIUS_CONTROL)
        .role(Role::Button)
        .name("Clear all filters")
        .enabled(library::is_filtered(view))
        .on_click(FilterMessage::Clear.into());
        lines.push(clear, ITEM_HEIGHT);

        // As tall as its lines, or as the window leaves room for: then it scrolls.
        let room = window.size.1 - theme::TITLE_BAR_HEIGHT - TOP - MARGIN;
        let height = (lines.height + 2.0 * PADDING).min(room.max(ITEM_HEIGHT * 4.0));
        let panel = col([scroll("filters", col(lines.lines).gap(GAP)).fill_height()])
            .pad(PADDING)
            .size(WIDTH, height)
            .bg(theme::FLYOUT)
            .stroke(theme::FLYOUT_STROKE, 1.0)
            .radius(theme::RADIUS_FLYOUT)
            .role(Role::Group)
            .name(NAME)
            // A click on its captions or between its lines leaves it open.
            .opaque();
        let backdrop = col([]).grow().modal().on_click(FilterMessage::Close.into());
        let placed = col([panel])
            .padding(Edges::new(SIDE, TOP, 0.0, 0.0))
            .halign(Align::Start)
            .valign(Align::Start);
        Some(layers([backdrop, placed]).grow())
    }

    pub(crate) fn handle_filter(&mut self, message: FilterMessage, context: &Ctx<Message>) {
        match message {
            FilterMessage::Toggle if self.filters.open => self.close_filters(context),
            FilterMessage::Toggle => self.filters.open = true,
            FilterMessage::Close => self.close_filters(context),
            FilterMessage::Sort(sort) => self.view.sort = sort,
            FilterMessage::GroupByStore(on) => self.view.group_by_store = on,
            FilterMessage::Store(store) => library::toggle_store(&mut self.view, store),
            FilterMessage::Hidden(on) => library::set_hidden(&mut self.view, on),
            FilterMessage::Collection(collection) => {
                library::pick_collection(&mut self.view, collection);
            }
            FilterMessage::Clear => library::clear(&mut self.view),
            FilterMessage::Loaded(Ok(view)) => {
                self.filters.saved = view.clone();
                self.view = view;
                // Before the first games are listed nothing can be judged empty.
                if !self.games.is_empty() {
                    self.tidy_view();
                }
            }
            // The scan reports a settings file that can't be read.
            FilterMessage::Loaded(Err(_)) | FilterMessage::Saved(Ok(())) => {}
            FilterMessage::Saved(Err(e)) => self.error = Some(e),
        }
    }

    /// Closes the flyout, and saves the view if it changed since it was read or saved.
    fn close_filters(&mut self, context: &Ctx<Message>) {
        self.filters.open = false;
        if self.view == self.filters.saved {
            return;
        }
        self.filters.saved = self.view.clone();
        let view = self.view.clone();
        context.spawn_background(move |_cancel| {
            let saved = gm_core::config::save_library_view(&view)
                .map_err(|e| format!("Sort and filters: {e}"));
            FilterMessage::Saved(saved).into()
        });
    }

    /// After games are gone, hidden or shown again: a ticked store, Hidden or a collection with
    /// no games left is taken off (`library::keep_view`), so the lists never sit empty behind a
    /// filter the flyout no longer offers.
    pub(crate) fn tidy_view(&mut self) {
        let mut kept = library::keep_view(&self.games, &self.view, self.facts());
        if !self.filters.mods_ready {
            kept.collection = self.view.collection;
        }
        self.view = kept;
    }

    /// Reads the view saved last time, in the background.
    pub(crate) fn load_view(context: &Ctx<Message>) {
        context.spawn_background(|_cancel| {
            let view = gm_core::config::load_library_view().map_err(|e| e.to_string());
            FilterMessage::Loaded(view).into()
        });
    }
}
