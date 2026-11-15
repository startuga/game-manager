//! The Upscalers tab's Versions section: the library of the vendors' DLL versions, folded at the
//! bottom of the tab until its header is clicked (`UpscalerState::versions_open`). Unfolded: one
//! section per kind (DLSS Super Resolution, Streamline, …), folded until clicked, listing its
//! versions newest first: downloaded or not (a release archive's size), how many games use it,
//! and Download or Delete. Deleting is always safe: a game keeps its own copy of what was swapped
//! in. The library is the one the tab reads (`request_upscalers`); the state is `UpscalerState`
//! (`upscalers_view.rs`).

use crate::ui::el::{El, col, glyph, row, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{card, hint, icon_text_button, working};
use crate::upscalers_view::{UpscalerLibrary, UpscalerMessage, megabytes};
use crate::{GameManager, Message};
use gm_core::mods::upscalers::{Available, Kind};

/// How far a version's row is indented: past its kind's chevron, under its name.
const VERSION_INDENT: f32 = 30.0;

impl GameManager {
    /// The Versions card: its header (folds it, with what is downloaded), then, unfolded, Refresh
    /// list, Open cache folder and the kinds.
    pub(crate) fn versions_section(&self) -> El<Message> {
        let library = match &self.upscalers.library {
            Some(Ok(library)) => Some(library),
            _ => None,
        };
        let open = self.upscalers.versions_open;
        let summary = library.map_or_else(String::new, |library| {
            let size: u64 = library.downloaded.iter().map(|item| item.size).sum();
            format!(
                "{} downloaded, {}",
                library.downloaded.len(),
                megabytes(size)
            )
        });
        let header = row([
            glyph(
                if open {
                    g::CHEVRON_DOWN
                } else {
                    g::CHEVRON_RIGHT
                },
                10.0,
            )
            .color(theme::text_secondary())
            .width(16.0),
            col([
                text("Versions").text_size(theme::TEXT_SUBTITLE).bold(),
                hint(format!(
                    "Download or delete the vendors' versions{}",
                    if summary.is_empty() {
                        String::new()
                    } else {
                        format!(" · {summary}")
                    }
                )),
            ])
            .gap(theme::space::XXS)
            .fill_width(),
        ])
        .gap(theme::space::S)
        .pad_xy(theme::space::XS, theme::space::XS)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::subtle_hover())
        .name("Versions")
        .on_click(UpscalerMessage::VersionsToggled.into());
        if !open {
            return card([header]).pad(theme::space::S);
        }
        let busy = self.upscalers.library_pending || self.upscalers.download.is_some();
        let actions = row([
            icon_text_button(g::REFRESH, "Refresh list")
                .enabled(!busy)
                .tooltip("Ask NVIDIA's, AMD's and Intel's repositories for new versions")
                .on_click(UpscalerMessage::Refresh.into()),
            icon_text_button(g::FOLDER, "Open cache folder")
                .on_click(UpscalerMessage::OpenCacheFolder.into()),
        ])
        .gap(theme::space::S);
        let mut rows = vec![
            header,
            actions,
            hint(
                "The versions are NVIDIA's, AMD's and Intel's own, from their GitHub repositories, \
                 each checked for a valid signature. A game keeps its own copy of what was \
                 swapped in, so deleting is always safe.",
            ),
        ];
        if let Some(library) = library
            && !library.failed.is_empty()
        {
            rows.push(hint(format!(
                "Some versions could not be listed: {}",
                library.failed.join("; ")
            )));
        }
        match (&self.upscalers.library, library) {
            (_, Some(library)) => {
                rows.extend(
                    library
                        .catalog
                        .kinds
                        .iter()
                        .map(|kind| self.kind_section(library, kind)),
                );
            }
            (Some(Err(error)), None) => {
                rows.push(hint(format!("The versions could not be read: {error}")));
            }
            _ => rows.push(working("Listing the versions…")),
        }
        card(rows).gap(theme::space::M).pad(theme::space::S)
    }

    /// A kind's section: a header that folds it, then its versions, each with Download or Delete.
    fn kind_section(&self, library: &UpscalerLibrary, kind: &Kind) -> El<Message> {
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
        let unfolded = self.upscalers.unfolded.contains(&kind.id);
        let chevron = if unfolded {
            g::CHEVRON_DOWN
        } else {
            g::CHEVRON_RIGHT
        };
        let header = row([
            glyph(chevron, 10.0)
                .color(theme::text_secondary())
                .width(16.0),
            col([
                text(kind.name.as_str())
                    .text_size(theme::TEXT_SUBTITLE)
                    .bold(),
                hint(format!(
                    "{} · {} versions, {downloaded} downloaded",
                    kind.vendor,
                    versions.len()
                )),
            ])
            .gap(theme::space::XXS)
            .fill_width(),
        ])
        .gap(theme::space::S)
        .pad_xy(theme::space::XS, theme::space::XS)
        .radius(theme::RADIUS_CONTROL)
        .hover_bg(theme::subtle_hover())
        .name(kind.name.as_str())
        .on_click(UpscalerMessage::KindToggled(kind.id.clone()).into());
        let mut lines = vec![header];
        if unfolded {
            if versions.is_empty() {
                lines.push(hint("No version listed."));
            }
            lines.extend(
                versions
                    .iter()
                    .map(|available| self.version_row(library, kind, available)),
            );
        }
        card(lines).gap(theme::space::XS).pad(theme::space::S)
    }

    /// One version: its label, whether it is downloaded and used, and Download or Delete.
    fn version_row(
        &self,
        library: &UpscalerLibrary,
        kind: &Kind,
        available: &Available,
    ) -> El<Message> {
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
        let action = if running {
            working("Downloading…")
        } else if cached.is_some() {
            icon_text_button(g::DELETE, "Delete")
                .enabled(self.upscalers.download.is_none())
                .name(format!("Delete {} {version}", kind.name))
                .tooltip("Remove it from the cache; the games it is in keep their copy")
                .on_click(UpscalerMessage::Delete(kind.id.clone(), version.clone()).into())
        } else {
            icon_text_button(g::DOWNLOAD, "Download")
                .enabled(self.upscalers.download.is_none())
                .name(format!("Download {} {version}", kind.name))
                .on_click(UpscalerMessage::Download(kind.id.clone(), version.clone()).into())
        };
        row([
            text(library.label(&kind.id, available)).fill_width(),
            text(status)
                .text_size(theme::TEXT_SMALL)
                .color(theme::text_tertiary()),
            row([action]).width(140.0),
        ])
        .gap(theme::space::M)
        .padding(crate::ui::el::Edges::new(
            VERSION_INDENT,
            theme::space::XXS,
            theme::space::XS,
            theme::space::XXS,
        ))
    }
}
