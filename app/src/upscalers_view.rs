//! The game page's Upscalers tab: the DLSS, FSR, XeSS and Streamline DLLs in the selected game's
//! folder (`gm_core::mods::upscalers`), one card per place, each with its version, a drop-down of
//! the vendors' versions, Swap and Restore (✕); then the folded Versions section (the versions
//! downloaded and available, `upscaler_versions.rs`). `UpscalerState` also holds the NVIDIA
//! overrides tab's driver facts and draft (`dlss_override_view.rs`).
//!
//! - **Scanning** walks the game's folder, so it runs in the background, and only while this tab
//!   or the NVIDIA overrides tab shows (`request_upscalers`, from `update`). Results are kept per
//!   game and cleared after a library scan, a mod job or `ModsLoaded`, as their state may have
//!   changed.
//! - **The library** (the kinds, GitHub's version lists and the cache) is read once, when the tab
//!   first needs it; Refresh asks GitHub again.
//! - **Swap and Restore** are mod jobs (`ModKind::Upscaler`, one at a time like the others);
//!   `UpscalerState::row` says which card shows "Working…". A swap downloads what the cache lacks,
//!   so the cache is read again after each job.

use crate::dlss_override_view::{OverrideDraft, OverrideLibrary};
use crate::mods_view::{ModDialog, ModKind, shown_path};
use crate::shell::ctx::Ctx;
use crate::ui::el::{El, col, dropdown, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{button, card, hint, icon_square, remove_button, working, working_button};
use crate::{GameManager, Message};
use gm_core::Game;
use gm_core::mods::dlss_override::OverrideState;
use gm_core::mods::upscalers::{self, Available, Catalog, Downloaded, Found, State};
use std::collections::{BTreeMap, HashMap, HashSet};

/// What the Upscalers tab and page show.
#[derive(Default)]
pub(crate) struct UpscalerState {
    /// The DLLs found in each game's folder, by id, once scanned.
    pub found: HashMap<String, Vec<Found>>,
    pub pending: HashSet<String>,
    /// The tab shows (kept while switching games).
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
    /// The Versions section is unfolded.
    pub versions_open: bool,
    /// The Versions section's kinds shown unfolded, by id.
    pub unfolded: HashSet<String>,
    /// NVIDIA's override settings and the driver's facts, read once at start (`Ok(None)`: no
    /// NVIDIA driver, and no NVIDIA overrides tab).
    pub driver: Option<Result<Option<OverrideLibrary>, String>>,
    pub driver_pending: bool,
    /// The NVIDIA overrides tab's draft, for the game it was made for.
    pub overrides: Option<OverrideDraft>,
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
    /// The Versions section was folded or unfolded.
    VersionsToggled,
    /// A kind's section in Versions was folded or unfolded.
    KindToggled(String),
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
    /// Asks GitHub for the version lists again.
    Refresh,
    /// Versions' Download and Delete: kind and version.
    Download(String, String),
    Delete(String, String),
    /// The cache after a download, a delete or a mod job, or what failed.
    DownloadsChanged(Result<Vec<Downloaded>, String>),
    OpenCacheFolder,
    /// NVIDIA's override settings and the driver's facts, or what failed (`dlss_override_view.rs`).
    DriverLoaded(Result<Option<OverrideLibrary>, String>),
    ShowOverridesInfo,
    /// A game's driver profile for the NVIDIA overrides tab, by id (`None`: no NVIDIA driver).
    OverridesLoaded(String, Result<Option<OverrideState>, String>),
    /// A setting's drop-down in the tab: its id and the index picked (0 is "Original"); `None`
    /// is ignored.
    OverrideChosen(u32, Option<usize>),
    ResetOverrides,
    /// The tab's Save.
    SaveOverrides,
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
    pub(crate) fn handle_upscalers(&mut self, message: UpscalerMessage, context: &Ctx<Message>) {
        match message {
            UpscalerMessage::KindToggled(kind) => {
                if !self.upscalers.unfolded.remove(&kind) {
                    self.upscalers.unfolded.insert(kind);
                }
            }
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
            UpscalerMessage::VersionsToggled => {
                self.upscalers.versions_open = !self.upscalers.versions_open;
            }
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
            | UpscalerMessage::ShowOverridesInfo
            | UpscalerMessage::OverridesLoaded(..)
            | UpscalerMessage::OverrideChosen(..)
            | UpscalerMessage::ResetOverrides
            | UpscalerMessage::SaveOverrides) => self.handle_overrides(message, context),
        }
    }

    /// After a mod job: the rows' state may have changed, and a swap may have downloaded; saved
    /// overrides are read back from the profile (a new draft).
    pub(crate) fn upscaler_job_done(&mut self, context: &Ctx<Message>) {
        self.upscalers.found.clear();
        self.upscalers.row = None;
        self.upscalers.overrides = None;
        if matches!(self.upscalers.library, Some(Ok(_))) {
            context.spawn_background(|_cancel| {
                UpscalerMessage::DownloadsChanged(Ok(upscalers::downloaded())).into()
            });
        }
    }

    fn load_upscaler_library(&mut self, refresh: bool, context: &Ctx<Message>) {
        if self.upscalers.library_pending {
            return;
        }
        self.upscalers.library_pending = true;
        context.spawn_background(move |_cancel| {
            UpscalerMessage::LibraryLoaded(load_library(refresh)).into()
        });
    }

    /// While the Upscalers or NVIDIA overrides tab is open: the library once, and the selected
    /// game's DLLs.
    pub(crate) fn request_upscalers(&mut self, context: &Ctx<Message>) {
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

    /// The Upscalers tab: what it does, a card per place with upscaler DLLs, and Versions.
    pub(crate) fn upscalers_tab(&self, game: &Game) -> El<Message> {
        let mut cards = vec![card([row([
            hint(
                "The DLSS, FSR 3.1+, XeSS and Streamline DLLs in this game's folder. Swap one for \
                 another of its vendor's versions; ✕ puts the game's own back.",
            )
            .fill_width(),
            button("Info")
                .enabled(!self.dialog_open())
                .name("Upscalers information")
                .on_click(UpscalerMessage::ShowInfo.into()),
        ])
        .gap(12.0)])];
        if let Some(Ok(library)) = &self.upscalers.library
            && !library.failed.is_empty()
        {
            cards.push(hint(format!(
                "Some versions could not be listed: {}",
                library.failed.join("; ")
            )));
        }
        if let Some(Err(error)) = &self.upscalers.library {
            cards.push(hint(format!("The versions could not be read: {error}")));
        }
        match self.upscalers.found.get(&game.id) {
            None => cards.push(working("Looking for upscaler DLLs…")),
            Some(found) if found.is_empty() => cards.push(hint(
                "No DLSS, FSR 3.1+, XeSS or Streamline DLLs in this game's folder.",
            )),
            Some(found) => {
                cards.extend(found.iter().map(|found| self.upscaler_card(game, found)));
            }
        }
        cards.push(self.versions_section());
        col(cards).gap(16.0)
    }

    /// One place's DLLs: name and state, the version drop-down, Swap and ✕.
    fn upscaler_card(&self, game: &Game, found: &Found) -> El<Message> {
        let key = row_key(found);
        let library = match &self.upscalers.library {
            Some(Ok(library)) => Some(library),
            _ => None,
        };
        let kind = library.and_then(|library| library.catalog.kind(&found.kind));
        let name = kind.map_or(found.kind.as_str(), |kind| kind.name.as_str());
        let vendor = kind.map_or("", |kind| kind.vendor.as_str());
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
            let items = if has_versions {
                items
            } else if listed {
                vec!["No version fits this one".to_owned()]
            } else {
                vec!["No version listed".to_owned()]
            };
            dropdown(
                format!("upscaler:{key}"),
                items,
                Some(chosen),
                move |index| UpscalerMessage::VersionChosen(key.clone(), Some(index)).into(),
            )
            .enabled(has_versions && !busy && found.x64)
            .name(format!("Version of {name}"))
            .width(300.0)
        };
        // The version swapped in already needs no swap.
        let already = matches!(
            &found.state,
            State::Swapped { version, .. }
                if versions.get(chosen).is_some_and(|available| available.version.to_string() == *version)
        );
        let working_here = self.upscalers.row.as_deref() == Some(key.as_str())
            && matches!(self.mod_job, Some((_, ModKind::Upscaler)));
        let swap = if working_here {
            working_button()
        } else {
            crate::ui::widgets::accent_button("Swap")
                .enabled(has_versions && !busy && found.x64 && !already)
                .name(format!("Swap {name}"))
                .tooltip(if already {
                    "This version is the one in the game"
                } else {
                    "Download the version picked if needed, and put it in place of the game's DLL"
                })
                .on_click(UpscalerMessage::Swap(key.clone()).into())
        };
        let restorable = !matches!(found.state, State::Game);
        let restore = remove_button(format!("Restore the game's {name}"))
            .enabled(restorable && !busy)
            .on_click(UpscalerMessage::Restore(key).into());
        let tint = match vendor {
            "NVIDIA" => theme::GREEN,
            "AMD" => theme::RED,
            "Intel" => theme::BLUE,
            _ => theme::SLATE,
        };
        card([
            row([
                icon_square(g::CHIP, tint, 36.0),
                col([
                    text(name).text_size(theme::TEXT_SUBTITLE).bold(),
                    text(state).color(theme::TEXT_SECONDARY).wrap(),
                ])
                .gap(2.0)
                .fill_width(),
                picker,
                swap,
                restore,
            ])
            .gap(10.0),
            hint(place),
        ])
    }
}
