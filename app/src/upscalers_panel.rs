//! Top bar ▸ Upscalers: the library of the vendors' DLL versions, drawn like ReShade's panels
//! (`panel_layer`). One expander per kind (DLSS Super Resolution, Streamline, …), listing its
//! versions newest first: downloaded or not (a release archive's size), how many games use it, and
//! Download or Delete. Deleting is always safe: a game keeps its own copy of what was swapped in.
//! The state is `UpscalerState` (`upscalers_view.rs`).

use crate::upscalers_view::{UpscalerLibrary, UpscalerMessage, megabytes, note, working};
use crate::{GameManager, icon_label};
use gm_core::mods::upscalers::{Available, Kind};
use windows_reactor::*;

impl GameManager {
    /// The top bar's Upscalers button.
    pub(crate) fn upscalers_button(&self, context: &ViewContext<Self>) -> View {
        Button::new()
            .is_enabled(self.mod_job.is_none() && !self.dialog_open())
            .on_click(context.message(UpscalerMessage::OpenPanel.into()))
            .automation_name("Upscalers")
            .content(icon_label(Symbol::ZoomIn, "Upscalers"))
            .tooltip("Download and manage DLSS, FSR, XeSS and Streamline versions")
    }

    /// The panel's layer, while it is open.
    pub(crate) fn upscalers_panel_view(&self, context: &ViewContext<Self>) -> Option<View> {
        if !self.upscalers.panel {
            return None;
        }
        let library = match &self.upscalers.library {
            Some(Ok(library)) => Some(library),
            _ => None,
        };
        let busy = self.upscalers.library_pending || self.upscalers.download.is_some();
        let title = Grid::new()
            .columns([GridLength::Star(1.0), GridLength::Auto, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                TextBlock::new()
                    .text("Upscaler DLLs")
                    .font_size(28.0)
                    .font_weight(FontWeight::SEMI_BOLD)
                    .grid_column(0),
                Button::new()
                    .is_enabled(!busy)
                    .on_click(context.message(UpscalerMessage::Refresh.into()))
                    .automation_name("Refresh list")
                    .content(icon_label(Symbol::Refresh, "Refresh list"))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1)
                    .tooltip("Ask NVIDIA's, AMD's and Intel's repositories for new versions"),
                Button::new()
                    .on_click(context.message(UpscalerMessage::OpenCacheFolder.into()))
                    .automation_name("Open cache folder")
                    .content(icon_label(Symbol::Folder, "Open cache folder"))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(2),
            ));
        let mut header: Vec<View> = vec![title.into()];
        header.push(note(&match library {
            Some(library) => {
                let size: u64 = library.downloaded.iter().map(|item| item.size).sum();
                format!(
                    "The versions are NVIDIA's, AMD's and Intel's own, from their GitHub \
                     repositories, each checked for a valid signature. {} downloaded ({}). A game \
                     keeps its own copy of what was swapped in, so deleting is always safe.",
                    library.downloaded.len(),
                    megabytes(size)
                )
            }
            None => "The versions are NVIDIA's, AMD's and Intel's own, from their GitHub \
                     repositories."
                .to_owned(),
        }));
        if let Some(library) = library
            && !library.failed.is_empty()
        {
            header.push(note(&format!(
                "Some versions could not be listed: {}",
                library.failed.join("; ")
            )));
        }
        let header = StackPanel::new().spacing(4.0).grid_row(0).children(header);

        let body: View = match (&self.upscalers.library, library) {
            (_, Some(library)) => {
                let kinds: Vec<View> = library
                    .catalog
                    .kinds
                    .iter()
                    .map(|kind| self.kind_section(library, kind, context))
                    .collect();
                ScrollViewer::new()
                    .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                    .content(
                        StackPanel::new()
                            .spacing(8.0)
                            // Room for the scroll bar, so it never covers the rows.
                            .margin(Thickness::new(0.0, 0.0, 16.0, 0.0))
                            .children(kinds),
                    )
                    .into()
            }
            (Some(Err(error)), None) => note(&format!("The versions could not be read: {error}")),
            _ => working("Listing the versions…"),
        };
        let close = Button::new()
            .horizontal_alignment(HorizontalAlignment::Right)
            .min_width(280.0)
            .on_click(context.message(UpscalerMessage::ClosePanel.into()))
            .automation_name("Close")
            .content("Close")
            .grid_row(2);
        let content = Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0), GridLength::Auto])
            .row_spacing(16.0)
            .children((header, Border::new().grid_row(1).content(body), close));
        Some(self.panel_layer(content.into()))
    }

    /// A kind's expander: its versions, each with Download or Delete.
    fn kind_section(
        &self,
        library: &UpscalerLibrary,
        kind: &Kind,
        context: &ViewContext<Self>,
    ) -> View {
        let versions = library
            .versions
            .get(&kind.id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let downloaded = versions
            .iter()
            .filter(|available| {
                library
                    .downloaded(&kind.id, &available.version.to_string())
                    .is_some()
            })
            .count();
        let header = format!(
            "{} · {} · {} versions, {downloaded} downloaded",
            kind.name,
            kind.vendor,
            versions.len()
        );
        let rows: Vec<View> = if versions.is_empty() {
            vec![note("No version listed.")]
        } else {
            versions
                .iter()
                .map(|available| self.version_row(library, kind, available, context))
                .collect()
        };
        Expander::new()
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name(kind.name.as_str())
            .header(header)
            .content(StackPanel::new().spacing(4.0).children(rows))
            .into()
    }

    /// One version: its label, whether it is downloaded and used, and Download or Delete.
    fn version_row(
        &self,
        library: &UpscalerLibrary,
        kind: &Kind,
        available: &Available,
        context: &ViewContext<Self>,
    ) -> View {
        let version = available.version.to_string();
        let cached = library.downloaded(&kind.id, &version);
        let used = self
            .mods
            .games
            .values()
            .filter(|entry| {
                entry
                    .upscalers
                    .iter()
                    .any(|swap| swap.kind == kind.id && swap.installed.version == version)
            })
            .count();
        let mut status = match cached {
            Some(cached) => format!("Downloaded, {}", megabytes(cached.size)),
            None => match available.asset {
                Some((_, size)) => format!("Not downloaded, {}", megabytes(size)),
                None => "Not downloaded".to_owned(),
            },
        };
        match used {
            0 => {}
            1 => status.push_str(" · in 1 game"),
            n => status.push_str(&format!(" · in {n} games")),
        }
        let running = self.upscalers.download.as_ref() == Some(&(kind.id.clone(), version.clone()));
        let action: View = if running {
            working("Downloading…")
        } else if cached.is_some() {
            Button::new()
                .is_enabled(self.upscalers.download.is_none())
                .on_click(
                    context
                        .message(UpscalerMessage::Delete(kind.id.clone(), version.clone()).into()),
                )
                .automation_name(format!("Delete {} {version}", kind.name))
                .content(icon_label(Symbol::Delete, "Delete"))
                .tooltip("Remove it from the cache; the games it is in keep their copy")
        } else {
            Button::new()
                .is_enabled(self.upscalers.download.is_none())
                .on_click(
                    context.message(
                        UpscalerMessage::Download(kind.id.clone(), version.clone()).into(),
                    ),
                )
                .automation_name(format!("Download {} {version}", kind.name))
                .content(icon_label(Symbol::Download, "Download"))
                .into()
        };
        Grid::new()
            .columns([
                GridLength::Star(1.0),
                GridLength::Auto,
                GridLength::Pixel(140.0),
            ])
            .column_spacing(12.0)
            .children((
                TextBlock::new()
                    .text(library.label(&kind.id, available))
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(0),
                TextBlock::new()
                    .text(status)
                    .font_size(12.0)
                    .opacity(0.7)
                    .vertical_alignment(VerticalAlignment::Center)
                    .grid_column(1),
                Border::new().grid_column(2).content(action),
            ))
            .into()
    }
}
