//! The game panel's Upscalers expander: the DLSS, FSR, XeSS and Streamline DLLs in the selected
//! game's folder (`gm_core::mods::upscalers`), one row per place, each with its version, a
//! drop-down of the vendors' versions, Swap and Restore (✕). The top bar's library panel is in
//! `upscalers_panel.rs`; both share `UpscalerState`.
//!
//! - **Scanning** walks the game's folder, so it runs in the background, and only while the
//!   expander is open (`request_upscalers`, from `update`). Results are kept per game and
//!   cleared after a library scan, a mod job or `ModsLoaded`, as their state may have changed.
//! - **The library** (the kinds, GitHub's version lists and the cache) is read once, when the
//!   expander or the panel first needs it; Refresh asks GitHub again.
//! - **Swap and Restore** are mod jobs (`ModKind::Upscaler`, one at a time like the others);
//!   `UpscalerState::row` says which row shows "Working…". A swap downloads what the cache lacks,
//!   so the cache is read again after each job.
//! - **NVIDIA overrides**: with an NVIDIA driver, a last row opens the driver-level DLSS overrides
//!   dialog (`dlss_override_view.rs`).

use crate::dlss_override_view::OverrideLibrary;
use crate::mods_view::{ModDialog, ModKind, UNINSTALL_GLYPH, shown_path};
use crate::{GameManager, Message};
use gm_core::Game;
use gm_core::mods::dlss_override::OverrideState;
use gm_core::mods::upscalers::{self, Available, Catalog, Downloaded, Found, State};
use std::collections::{BTreeMap, HashMap, HashSet};
use windows_reactor::*;

/// What the Upscalers expander and panel show.
#[derive(Default)]
pub(crate) struct UpscalerState {
    /// The DLLs found in each game's folder, by id, once scanned.
    pub found: HashMap<String, Vec<Found>>,
    pub pending: HashSet<String>,
    /// The expander is open (kept while switching games).
    pub open: bool,
    /// The kinds, their versions and the cache, once read (`Err`: what failed).
    pub library: Option<Result<UpscalerLibrary, String>>,
    pub library_pending: bool,
    /// The version picked in each row's drop-down (by `row_key`); the newest otherwise.
    pub choice: HashMap<String, usize>,
    /// The row whose swap or restore runs; `mod_job` holds the game.
    pub row: Option<String>,
    /// The panel's download running: kind and version.
    pub download: Option<(String, String)>,
    /// Top bar ▸ Upscalers, while open.
    pub panel: bool,
    /// NVIDIA's override settings and the driver's facts, read once when the expander opens
    /// (`Ok(None)`: no NVIDIA driver).
    pub driver: Option<Result<Option<OverrideLibrary>, String>>,
    pub driver_pending: bool,
}

/// The kinds, every kind's versions (newest first) with what GitHub could not list, and the
/// cache.
#[derive(Clone)]
pub(crate) struct UpscalerLibrary {
    pub catalog: Catalog,
    pub versions: BTreeMap<String, Vec<Available>>,
    pub failed: Vec<String>,
    pub downloaded: Vec<Downloaded>,
}

impl UpscalerLibrary {
    /// The cache's copy of `kind` at `version`, if any.
    pub fn downloaded(&self, kind: &str, version: &str) -> Option<&Downloaded> {
        self.downloaded
            .iter()
            .find(|item| item.kind == kind && item.version == version)
    }

    /// A version as the drop-downs and the panel show it: the tag's, the DLL's own when it
    /// differs and is known, and the download's size for a release archive not cached yet.
    pub fn label(&self, kind: &str, available: &Available) -> String {
        let version = available.version.to_string();
        match self.downloaded(kind, &version) {
            Some(cached) => match &cached.dll_version {
                Some(dll) => format!("{version} (DLL {dll}), downloaded"),
                None => format!("{version}, downloaded"),
            },
            None => match available.asset {
                Some((_, size)) => format!("{version}, {} download", megabytes(size)),
                None => version,
            },
        }
    }
}

/// A size in megabytes, as shown.
pub(crate) fn megabytes(bytes: u64) -> String {
    format!("{} MB", bytes.div_ceil(1_000_000))
}

#[derive(Clone)]
pub(crate) enum UpscalerMessage {
    /// The expander was opened or closed.
    Expanded(bool),
    /// The kinds, their versions and the cache, or what failed.
    LibraryLoaded(Result<UpscalerLibrary, String>),
    /// A game's upscaler DLLs, by id.
    Scanned(String, Vec<Found>),
    /// A row's drop-down: its key and the version's index; `None` is ignored.
    VersionChosen(String, Option<usize>),
    /// Swaps the row's DLLs for the version picked.
    Swap(String),
    /// Puts the game's own DLLs of the row back.
    Restore(String),
    ShowInfo,
    OpenPanel,
    ClosePanel,
    /// Asks GitHub for the version lists again.
    Refresh,
    /// The panel's Download and Delete: kind and version.
    Download(String, String),
    Delete(String, String),
    /// The cache after a download, a delete or a mod job, or what failed.
    DownloadsChanged(Result<Vec<Downloaded>, String>),
    OpenCacheFolder,
    /// NVIDIA's override settings and the driver's facts, or what failed (`dlss_override_view.rs`).
    DriverLoaded(Result<Option<OverrideLibrary>, String>),
    ShowOverrides,
    ShowOverridesInfo,
    /// A game's driver profile for the overrides dialog, by id (`None`: no NVIDIA driver).
    OverridesLoaded(String, Result<Option<OverrideState>, String>),
    /// A setting's drop-down in the dialog: its id and the index picked (0 is "Original");
    /// `None` is ignored.
    OverrideChosen(u32, Option<usize>),
    ResetOverrides,
}

impl From<UpscalerMessage> for Message {
    fn from(message: UpscalerMessage) -> Self {
        Message::Upscalers(message)
    }
}

/// A row's key: its kind and folder, which also name its swap in `mods.json`.
pub(crate) fn row_key(found: &Found) -> String {
    format!("{}|{}", found.kind, found.folder.display())
}

/// Reads the kinds, the version lists (from GitHub when `refresh` or not kept) and the cache.
/// Blocking.
fn load_library(refresh: bool) -> Result<UpscalerLibrary, String> {
    let catalog = upscalers::catalog().map_err(|e| e.to_string())?;
    // Made here, off the UI thread, for Open cache folder.
    if let Ok(folder) = upscalers::cache_folder() {
        let _ = std::fs::create_dir_all(folder);
    }
    let (versions, failed) = upscalers::versions(&catalog, refresh);
    Ok(UpscalerLibrary {
        catalog,
        versions,
        failed,
        downloaded: upscalers::downloaded(),
    })
}

impl GameManager {
    pub(crate) fn handle_upscalers(
        &mut self,
        message: UpscalerMessage,
        context: &ComponentContext<Self>,
    ) {
        match message {
            UpscalerMessage::Expanded(open) => self.upscalers.open = open,
            UpscalerMessage::LibraryLoaded(library) => {
                self.upscalers.library_pending = false;
                self.upscalers.library = Some(library);
            }
            UpscalerMessage::Scanned(id, found) => {
                self.upscalers.pending.remove(&id);
                self.upscalers.found.insert(id, found);
            }
            UpscalerMessage::VersionChosen(key, Some(index)) => {
                self.upscalers.choice.insert(key, index);
            }
            UpscalerMessage::VersionChosen(_, None) => {}
            UpscalerMessage::Swap(key) => {
                let Some((game, found)) = self.upscaler_row(&key) else {
                    return;
                };
                let Some(available) = self.chosen_version(&key, &found).cloned() else {
                    return;
                };
                self.mod_job = Some((game.id.clone(), ModKind::Upscaler));
                self.upscalers.row = Some(key);
                context.spawn_background(move |_cancel| {
                    Message::ModJobDone(
                        upscalers::swap(&game, &found, &available).map_err(|e| e.to_string()),
                    )
                });
            }
            UpscalerMessage::Restore(key) => {
                let Some((game, found)) = self.upscaler_row(&key) else {
                    return;
                };
                self.mod_job = Some((game.id.clone(), ModKind::Upscaler));
                self.upscalers.row = Some(key);
                context.spawn_background(move |_cancel| {
                    Message::ModJobDone(
                        upscalers::restore(&game, &found.kind, &found.folder)
                            .map_err(|e| e.to_string()),
                    )
                });
            }
            UpscalerMessage::ShowInfo => self.mod_dialog = Some(ModDialog::UpscalerInfo),
            UpscalerMessage::OpenPanel => {
                self.upscalers.panel = true;
                if !matches!(self.upscalers.library, Some(Ok(_))) {
                    self.load_upscaler_library(false, context);
                }
            }
            UpscalerMessage::ClosePanel => self.upscalers.panel = false,
            UpscalerMessage::Refresh => self.load_upscaler_library(true, context),
            UpscalerMessage::Download(kind, version) => {
                let Some(Ok(library)) = &self.upscalers.library else {
                    return;
                };
                let (Some(found_kind), Some(available)) = (
                    library.catalog.kind(&kind).cloned(),
                    library.versions.get(&kind).and_then(|versions| {
                        versions
                            .iter()
                            .find(|available| available.version.to_string() == version)
                            .cloned()
                    }),
                ) else {
                    return;
                };
                self.upscalers.download = Some((kind, version));
                context.spawn_background(move |_cancel| {
                    let done = upscalers::download(&found_kind, &available)
                        .map(|()| upscalers::downloaded())
                        .map_err(|e| e.to_string());
                    UpscalerMessage::DownloadsChanged(done).into()
                });
            }
            UpscalerMessage::Delete(kind, version) => {
                context.spawn_background(move |_cancel| {
                    let done = upscalers::delete_download(&kind, &version)
                        .map(|()| upscalers::downloaded())
                        .map_err(|e| e.to_string());
                    UpscalerMessage::DownloadsChanged(done).into()
                });
            }
            UpscalerMessage::DownloadsChanged(downloaded) => {
                self.upscalers.download = None;
                match downloaded {
                    Ok(downloaded) => {
                        if let Some(Ok(library)) = &mut self.upscalers.library {
                            library.downloaded = downloaded;
                        }
                    }
                    Err(e) => self.error = Some(e),
                }
            }
            UpscalerMessage::OpenCacheFolder => {
                self.error = upscalers::cache_folder()
                    .and_then(|folder| gm_core::open_folder(&folder, None))
                    .err()
                    .map(|e| e.to_string());
            }
            message @ (UpscalerMessage::DriverLoaded(_)
            | UpscalerMessage::ShowOverrides
            | UpscalerMessage::ShowOverridesInfo
            | UpscalerMessage::OverridesLoaded(..)
            | UpscalerMessage::OverrideChosen(..)
            | UpscalerMessage::ResetOverrides) => self.handle_overrides(message, context),
        }
    }

    /// After a mod job: the rows' state may have changed, and a swap may have downloaded.
    pub(crate) fn upscaler_job_done(&mut self, context: &ComponentContext<Self>) {
        self.upscalers.found.clear();
        self.upscalers.row = None;
        if matches!(self.upscalers.library, Some(Ok(_))) {
            context.spawn_background(|_cancel| {
                UpscalerMessage::DownloadsChanged(Ok(upscalers::downloaded())).into()
            });
        }
    }

    fn load_upscaler_library(&mut self, refresh: bool, context: &ComponentContext<Self>) {
        if self.upscalers.library_pending {
            return;
        }
        self.upscalers.library_pending = true;
        context.spawn_background(move |_cancel| {
            UpscalerMessage::LibraryLoaded(load_library(refresh)).into()
        });
    }

    /// While the expander is open: the library once, and the selected game's DLLs.
    pub(crate) fn request_upscalers(&mut self, context: &ComponentContext<Self>) {
        if !self.upscalers.open {
            return;
        }
        if self.upscalers.library.is_none() {
            self.load_upscaler_library(false, context);
        }
        self.request_driver(context);
        let Some(game) = self.selected_game() else {
            return;
        };
        if self.upscalers.found.contains_key(&game.id) || self.upscalers.pending.contains(&game.id)
        {
            return;
        }
        let (game, mods) = (game.clone(), self.mods.clone());
        self.upscalers.pending.insert(game.id.clone());
        context.spawn_background(move |_cancel| {
            let found = upscalers::catalog()
                .map(|catalog| upscalers::scan(&game, &catalog, &mods))
                .unwrap_or_default();
            UpscalerMessage::Scanned(game.id, found).into()
        });
    }

    /// The selected game and its row with `key`.
    fn upscaler_row(&self, key: &str) -> Option<(Game, Found)> {
        let game = self.selected_game()?;
        let found = self
            .upscalers
            .found
            .get(&game.id)?
            .iter()
            .find(|found| row_key(found) == key)?;
        Some((game.clone(), found.clone()))
    }

    /// The versions that fit the row's DLLs, newest first (a Streamline 1 game gets no Streamline
    /// 2).
    fn offered_versions(&self, found: &Found) -> Vec<&Available> {
        let Some(Ok(library)) = &self.upscalers.library else {
            return Vec::new();
        };
        match (
            library.catalog.kind(&found.kind),
            library.versions.get(&found.kind),
        ) {
            (Some(kind), Some(versions)) => kind.offered(found, versions),
            _ => Vec::new(),
        }
    }

    /// The version picked in the row `key` (the newest that fits until one is picked).
    fn chosen_version(&self, key: &str, found: &Found) -> Option<&Available> {
        let offered = self.offered_versions(found);
        let index = self.upscalers.choice.get(key).copied().unwrap_or(0);
        offered.get(index).or_else(|| offered.first()).copied()
    }

    pub(crate) fn upscalers_expander(&self, game: &Game, context: &ViewContext<Self>) -> View {
        let intro = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto])
            .column_spacing(12.0)
            .children((
                TextBlock::new()
                    .text(
                        "The DLSS, FSR 3.1+, XeSS and Streamline DLLs in this game's folder. Swap \
                         one for another of its vendor's versions; ✕ puts the game's own back.",
                    )
                    .font_size(12.0)
                    .opacity(0.7)
                    .text_wrapping(TextWrapping::Wrap)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(0),
                Button::new()
                    .is_enabled(!self.dialog_open())
                    .on_click(context.message(UpscalerMessage::ShowInfo.into()))
                    .automation_name("Upscalers information")
                    .content("Info")
                    .grid_column(1),
            ));
        let mut rows: Vec<View> = vec![intro.into()];
        if let Some(Ok(library)) = &self.upscalers.library
            && !library.failed.is_empty()
        {
            rows.push(note(&format!(
                "Some versions could not be listed: {}",
                library.failed.join("; ")
            )));
        }
        if let Some(Err(error)) = &self.upscalers.library {
            rows.push(note(&format!("The versions could not be read: {error}")));
        }
        match self.upscalers.found.get(&game.id) {
            None => rows.push(working("Looking for upscaler DLLs…")),
            Some(found) if found.is_empty() => rows.push(note(
                "No DLSS, FSR 3.1+, XeSS or Streamline DLLs in this game's folder.",
            )),
            Some(found) => {
                rows.extend(
                    found
                        .iter()
                        .map(|found| self.upscaler_row_view(game, found, context)),
                );
            }
        }
        rows.extend(self.overrides_row(game, context));
        Expander::new()
            .is_expanded(self.upscalers.open)
            .on_is_expanded_changed(
                context.callback(|open: bool| UpscalerMessage::Expanded(open).into()),
            )
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name("Upscalers")
            .header("Upscalers")
            .content(StackPanel::new().spacing(12.0).children(rows))
            .into()
    }

    /// One place's DLLs: name and state, the version drop-down, Swap and ✕.
    fn upscaler_row_view(&self, game: &Game, found: &Found, context: &ViewContext<Self>) -> View {
        let key = row_key(found);
        let library = match &self.upscalers.library {
            Some(Ok(library)) => Some(library),
            _ => None,
        };
        let kind = library.and_then(|library| library.catalog.kind(&found.kind));
        let name = kind.map_or(found.kind.as_str(), |kind| kind.name.as_str());
        let version = found.version.as_deref().unwrap_or("unknown version");
        let mut state = match &found.state {
            State::Game => format!("{version}, the game's"),
            State::Swapped {
                version: sdk,
                original,
            } => {
                // The DLL's own version, and the SDK's when it differs (FidelityFX SDK 1.1.4 is
                // DLL 1.0.1).
                let swapped = match &found.version {
                    Some(dll) if dll != sdk => format!("{dll} (SDK {sdk})"),
                    _ => sdk.clone(),
                };
                match original {
                    Some(original) => format!("{swapped}, swapped from {original}"),
                    None => format!("{swapped}, swapped"),
                }
            }
            State::Replaced => format!("{version}, the game's again since an update"),
        };
        let listed = library
            .and_then(|library| library.versions.get(&found.kind))
            .is_some_and(|versions| !versions.is_empty());
        let versions = self.offered_versions(found);
        if !found.x64 {
            state.push_str(": 32-bit, which no version is offered for");
        } else if listed && versions.is_empty() {
            // Versions are listed, but none of the game's major version (Streamline 1).
            let major = found
                .game_version()
                .and_then(upscalers::Version::parse)
                .and_then(|version| version.major());
            match major {
                Some(major) => state.push_str(&format!(
                    ": only versions {major}.x work with it, and none is offered"
                )),
                None => state.push_str(": its version is unknown, so no version can be checked"),
            }
        }
        let place = format!(
            "{} in {}",
            found.files.join(", "),
            shown_path(game, &found.folder)
        );
        let label = StackPanel::new()
            .spacing(2.0)
            .vertical_alignment(VerticalAlignment::Center)
            .grid_column(0)
            .children((
                TextBlock::new()
                    .text(name)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .text_wrapping(TextWrapping::Wrap),
                TextBlock::new()
                    .text(state)
                    .font_size(12.0)
                    .opacity(0.7)
                    .text_wrapping(TextWrapping::Wrap),
            ))
            .tooltip(place);

        let chosen = self
            .upscalers
            .choice
            .get(&key)
            .copied()
            .filter(|index| *index < versions.len())
            .unwrap_or(0);
        let items: Vec<String> = versions
            .iter()
            .map(|available| {
                library.map_or_else(
                    || available.version.to_string(),
                    |library| library.label(&found.kind, available),
                )
            })
            .collect();
        let has_versions = !items.is_empty();
        let busy = self.mods_busy();
        let picker = {
            let key = key.clone();
            ComboBox::new()
                .items_source(if has_versions {
                    items
                } else if listed {
                    vec!["No version fits this one".to_owned()]
                } else {
                    vec!["No version listed".to_owned()]
                })
                .selected_index(Some(chosen))
                .is_enabled(has_versions && !busy && found.x64)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                // A ComboBox sits at the top of a taller row, unlike Swap and ✕.
                .vertical_alignment(VerticalAlignment::Center)
                .automation_name(format!("Version of {name}"))
                .on_selection_changed(context.callback(move |index: Option<usize>| {
                    UpscalerMessage::VersionChosen(key.clone(), index).into()
                }))
                .grid_column(1)
        };
        // The version swapped in already needs no swap.
        let already = matches!(
            &found.state,
            State::Swapped { version, .. }
                if versions.get(chosen).is_some_and(|available| available.version.to_string() == *version)
        );
        let working = self.upscalers.row.as_deref() == Some(key.as_str())
            && matches!(self.mod_job, Some((_, ModKind::Upscaler)));
        let swap_content: View = if working {
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    ProgressRing::new().is_active(true).width(16.0).height(16.0),
                    TextBlock::new().text("Working…"),
                ))
                .into()
        } else {
            TextBlock::new().text("Swap").into()
        };
        let swap = Button::new()
            .is_enabled(has_versions && !busy && found.x64 && !already)
            .on_click(context.message(UpscalerMessage::Swap(key.clone()).into()))
            .automation_name(format!("Swap {name}"))
            .content(swap_content)
            .grid_column(2)
            .tooltip(if already {
                "This version is the one in the game".to_owned()
            } else {
                "Download the version picked if needed, and put it in place of the game's DLL"
                    .to_owned()
            });
        let restorable = !matches!(found.state, State::Game);
        let restore = Button::new()
            .is_enabled(restorable && !busy)
            .on_click(context.message(UpscalerMessage::Restore(key).into()))
            .automation_name(format!("Restore the game's {name}"))
            .content(
                FontIcon::new()
                    .glyph(UNINSTALL_GLYPH)
                    .foreground(ThemeBrush::SystemCritical),
            )
            .grid_column(3)
            .tooltip("Put the game's own DLL back");
        Grid::new()
            .columns([
                GridLength::Pixel(240.0),
                GridLength::Star(1.0),
                GridLength::Auto,
                GridLength::Auto,
            ])
            .column_spacing(8.0)
            .children((label, picker, swap, restore))
            .into()
    }
}

/// A line of small, dimmed text.
pub(crate) fn note(text: &str) -> View {
    TextBlock::new()
        .text(text)
        .font_size(12.0)
        .opacity(0.7)
        .text_wrapping(TextWrapping::Wrap)
        .into()
}

/// A spinning ring and what it waits for.
pub(crate) fn working(text: &str) -> View {
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(8.0)
        .children((
            ProgressRing::new().is_active(true).width(16.0).height(16.0),
            TextBlock::new().text(text),
        ))
        .into()
}
