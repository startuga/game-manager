//! The My PC page: a header with the computer's name
//! and what it is in one line, then HARDWARE and DISPLAYS AND WINDOWS SETTINGS. The state and
//! messages are in `my_pc.rs`.
//!
//! - **HARDWARE**: one card per graphics card, then the processor, memory (a usage bar and the
//!   modules), system (Windows, board, BIOS) and storage (a bar per drive). The cards sit two to
//!   a row on wide pages and one on narrow ones, each row's cards as tall as the row; the last
//!   card fills its row. A part that could not be read says so on its card.
//! - **DISPLAYS AND WINDOWS SETTINGS**: a card per display, with the switches card beside them on
//!   wide pages and below them otherwise.
//!   - A display card: Resolution and Refresh rate (a change asks to be kept, see `my_pc.rs`),
//!     HDR, brightness over DDC/CI, the colour profile, and links to Windows' own settings.
//!   - Windows' game switches: GPU scheduling (administrator rights, then a restart), Game Mode,
//!     optimizations for windowed games, and Auto HDR (when a display has HDR).
//! - Controls wait while a change runs (`MyPcState::busy`).

use crate::my_pc::{MyPcMessage, Switch, mode_for_size, rates, sizes};
use crate::shell::WindowInfo;
use crate::ui::el::{Align, El, col, dropdown, row, slider, text};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{
    card, card_header, chip, heading, hint, icon_text_button, link, meter, toggle, working,
};
use crate::{GameManager, Message};
use gm_core::pc::{Display, DisplayMode, Drive, Gpu, Memory, Snapshot, format_bytes};

/// Pages narrower than this show one card a row.
const TWO_COLUMNS: f32 = 760.0;

impl GameManager {
    pub(crate) fn my_pc_page(&self, window: &WindowInfo) -> El<Message> {
        let state = &self.my_pc;
        let Some(pc) = state.snapshot.as_deref() else {
            return col([self.my_pc_header(None), working("Reading the PC…")]).gap(16.0);
        };
        let wide = self.page_width(window) >= TWO_COLUMNS;
        let mut cards = Vec::new();
        match &pc.gpus {
            Ok(gpus) if !gpus.is_empty() => cards.extend(gpus.iter().map(gpu_card)),
            Ok(_) => cards.push(failed_card(
                g::MONITOR,
                theme::PINK,
                "Graphics card",
                "No graphics card was found in the registry.",
            )),
            Err(error) => cards.push(failed_card(
                g::MONITOR,
                theme::PINK,
                "Graphics card",
                &error.to_string(),
            )),
        }
        cards.push(match &pc.cpu {
            Ok(cpu) => card([
                card_header(g::CHIP, theme::BLUE, "Processor", ""),
                big(&cpu.name),
                facts(vec![
                    ("Cores", known(cpu.cores)),
                    ("Threads", known(cpu.threads)),
                    (
                        "Base clock",
                        if cpu.base_mhz > 0 {
                            format!("{:.1} GHz", f64::from(cpu.base_mhz) / 1000.0)
                        } else {
                            String::new()
                        },
                    ),
                ]),
            ]),
            Err(error) => failed_card(g::CHIP, theme::BLUE, "Processor", &error.to_string()),
        });
        cards.push(match &pc.memory {
            Ok(memory) => memory_card(memory),
            Err(error) => failed_card(g::MEMORY, theme::GREEN, "Memory", &error.to_string()),
        });
        cards.push(match &pc.system {
            Ok(system) => card([
                card_header(g::SYSTEM, theme::SLATE, "System", ""),
                facts(vec![
                    (
                        "Windows",
                        format!("{} (build {})", system.windows, system.build),
                    ),
                    ("Motherboard", or_unknown(&system.board)),
                    ("BIOS", or_unknown(&system.bios)),
                ]),
            ]),
            Err(error) => failed_card(g::SYSTEM, theme::SLATE, "System", &error.to_string()),
        });
        cards.push(match &pc.drives {
            Ok(drives) => storage_card(drives),
            Err(error) => failed_card(g::STORAGE, theme::ORANGE, "Storage", &error.to_string()),
        });

        let displays: Vec<El<Message>> = match &pc.displays {
            Ok(displays) if !displays.is_empty() => displays
                .iter()
                .map(|display| self.display_card(display))
                .collect(),
            Ok(_) => vec![failed_card(
                g::MONITOR,
                theme::CYAN,
                "Displays",
                "No active display was found.",
            )],
            Err(error) => vec![failed_card(
                g::MONITOR,
                theme::CYAN,
                "Displays",
                &error.to_string(),
            )],
        };
        let any_hdr = pc
            .displays
            .as_ref()
            .is_ok_and(|displays| displays.iter().any(|display| display.hdr_supported));
        let switches = self.switches_card(pc, any_hdr);
        let bottom = if wide {
            row([
                col(displays)
                    .gap(12.0)
                    .fill_width_by(1.6)
                    .valign(Align::Start),
                switches.fill_width().valign(Align::Start),
            ])
            .gap(12.0)
        } else {
            let mut stack = displays;
            stack.push(switches);
            col(stack).gap(12.0)
        };
        col([
            self.my_pc_header(Some(pc)),
            group_label("HARDWARE"),
            grid(cards, if wide { 2 } else { 1 }),
            group_label("DISPLAYS AND WINDOWS SETTINGS"),
            bottom,
        ])
        .gap(12.0)
    }

    /// "MY PC", the computer's name, its graphics card, processor and memory, and Refresh.
    fn my_pc_header(&self, pc: Option<&Snapshot>) -> El<Message> {
        let name = pc
            .map(|pc| pc.machine.clone())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "My PC".to_owned());
        let summary = pc.map_or_else(String::new, |pc| {
            let mut parts = Vec::new();
            if let Ok(Some(gpu)) = pc.gpus.as_ref().map(|gpus| gpus.first()) {
                parts.push(gpu.name.clone());
            }
            if let Ok(cpu) = &pc.cpu {
                parts.push(cpu.name.clone());
            }
            if let Ok(memory) = &pc.memory
                && memory.total > 0
            {
                parts.push(format!("{} RAM", format_bytes(memory.total)));
            }
            parts.join("  ·  ")
        });
        let refresh = if self.my_pc.pending {
            working("Reading…")
        } else {
            icon_text_button(g::REFRESH, "Refresh")
                .tooltip("Read the hardware and the display settings again")
                .on_click(MyPcMessage::Refresh.into())
        };
        row([
            col([
                text("MY PC")
                    .text_size(theme::TEXT_CAPTION)
                    .weight(700)
                    .color(theme::TEXT_TERTIARY),
                heading(name),
                hint(summary),
            ])
            .gap(4.0)
            .fill_width(),
            refresh.valign(Align::Start),
        ])
        .gap(12.0)
        .pad(20.0)
        .bg(theme::CARD)
        .stroke(theme::CARD_STROKE, 1.0)
        .radius(theme::RADIUS_CARD)
    }

    fn display_card(&self, display: &Display) -> El<Message> {
        let state = &self.my_pc;
        let idle = !state.busy && state.trial.is_none();
        let hdr = if display.hdr_on {
            "HDR on"
        } else if display.hdr_supported {
            "HDR off"
        } else {
            "SDR"
        };
        let mut header = vec![
            card_header(
                g::MONITOR,
                theme::CYAN,
                display.name.clone(),
                format!(
                    "{} × {}  ·  {} Hz  ·  {hdr}",
                    display.width,
                    display.height,
                    hz(display.refresh_hz)
                ),
            )
            .fill_width(),
        ];
        if display.primary {
            header.push(chip("MAIN DISPLAY", theme::ACCENT));
        }
        let mut lines = vec![row(header).gap(8.0)];
        if display.gdi_name.is_empty() {
            lines.push(hint(
                "Windows did not give this display's name: change its settings in Windows' \
                 Settings ▸ Display.",
            ));
            return card(lines);
        }
        let name = display.gdi_name.clone();

        // Resolution and refresh rate.
        if let Some(current) = display.current
            && !display.modes.is_empty()
        {
            let sizes = sizes(&display.modes);
            let size_index = sizes
                .iter()
                .position(|&size| size == (current.width, current.height));
            let modes = display.modes.clone();
            let picked = name.clone();
            let resolution = dropdown(
                format!("pc:resolution:{name}"),
                sizes
                    .iter()
                    .map(|(width, height)| format!("{width} × {height}"))
                    .collect(),
                size_index,
                move |index| {
                    let mode = sizes
                        .get(index)
                        .and_then(|&size| mode_for_size(&modes, size, current.hz))
                        .unwrap_or(current);
                    MyPcMessage::ModeChosen(picked.clone(), mode).into()
                },
            )
            .name(format!("Resolution of {}", display.name))
            .enabled(idle)
            .width(200.0);
            let rates = rates(&display.modes, (current.width, current.height));
            let rate_index = rates.iter().position(|&hz| hz == current.hz);
            let picked = name.clone();
            let refresh = dropdown(
                format!("pc:refresh:{name}"),
                rates.iter().map(|hz| format!("{hz} Hz")).collect(),
                rate_index,
                move |index| {
                    let hz = rates.get(index).copied().unwrap_or(current.hz);
                    MyPcMessage::ModeChosen(picked.clone(), DisplayMode { hz, ..current }).into()
                },
            )
            .name(format!("Refresh rate of {}", display.name))
            .enabled(idle)
            .width(200.0);
            lines.push(setting("Resolution", "", Some(resolution)));
            lines.push(setting(
                "Refresh rate",
                "The highest rate the display offers is usually the right one for games.",
                Some(refresh),
            ));
        }

        // HDR.
        if display.hdr_supported {
            lines.push(setting(
                "HDR",
                "Brighter highlights and wider colours in HDR games: Windows' own switch \
                 (Settings ▸ Display ▸ HDR).",
                Some(
                    switch(
                        &format!("HDR on {}", display.name),
                        display.hdr_on,
                        MyPcMessage::Hdr(name.clone(), !display.hdr_on),
                    )
                    .enabled(idle),
                ),
            ));
        } else {
            lines.push(setting("HDR", "This display does not support HDR.", None));
        }

        // Brightness, once read.
        lines.push(match state.brightness.get(&name) {
            None => setting("Brightness", "", Some(working("Reading…"))),
            Some(None) if display.built_in => setting(
                "Brightness",
                "A built-in screen's brightness is set in Windows' quick settings.",
                None,
            ),
            Some(None) => setting(
                "Brightness",
                "This monitor does not take brightness from Windows. Turn on DDC/CI in the \
                 monitor's own menu, then Refresh.",
                None,
            ),
            Some(Some(percent)) => {
                let picked = name.clone();
                setting(
                    "Brightness",
                    "Set over DDC/CI, like the monitor's own buttons.",
                    Some(
                        slider(
                            format!("pc:brightness:{name}"),
                            *percent,
                            (0, 100),
                            "%",
                            move |percent| MyPcMessage::Brightness(picked.clone(), percent).into(),
                        )
                        .enabled(!state.busy)
                        .name(format!("Brightness of {}", display.name))
                        .width(220.0),
                    ),
                )
            }
        });

        // Colour profile.
        let mode = if display.hdr_on { "HDR" } else { "SDR" };
        match display.color_profiles.as_slice() {
            [] => lines.push(setting(
                "Colour profile",
                "None linked: Windows' default. Add profiles in Windows' colour settings.",
                None,
            )),
            [only] if display.color_profile.as_deref() == Some(only.as_str()) => {
                lines.push(setting(
                    "Colour profile",
                    &format!(
                        "{} (for {mode}). Add more profiles in Windows' colour settings, then \
                         pick them here.",
                        profile_name(only)
                    ),
                    None,
                ));
            }
            profiles => {
                let selected = display.color_profile.as_ref().and_then(|current| {
                    profiles
                        .iter()
                        .position(|profile| profile.eq_ignore_ascii_case(current))
                });
                let picked = name.clone();
                lines.push(setting(
                    "Colour profile",
                    &format!("Profiles for {mode} mode"),
                    Some(
                        dropdown(
                            format!("pc:profile:{name}"),
                            profiles
                                .iter()
                                .map(|profile| profile_name(profile))
                                .collect(),
                            selected,
                            move |index| {
                                MyPcMessage::ColorProfile(picked.clone(), Some(index)).into()
                            },
                        )
                        .name(format!("Colour profile of {}", display.name))
                        .enabled(idle)
                        .width(220.0),
                    ),
                ));
            }
        }
        lines.push(
            row([
                link(
                    "Display settings",
                    MyPcMessage::OpenSettings("ms-settings:display").into(),
                ),
                link(
                    "Advanced display",
                    MyPcMessage::OpenSettings("ms-settings:display-advanced").into(),
                ),
                link(
                    "Colour management",
                    MyPcMessage::OpenSettings("ms-settings:display-advancedcolor").into(),
                ),
            ])
            .gap(4.0),
        );
        card(lines)
    }

    /// Windows' switches for games.
    fn switches_card(&self, pc: &Snapshot, any_hdr: bool) -> El<Message> {
        let idle = !self.my_pc.busy && self.my_pc.trial.is_none();
        let switches = &pc.switches;
        let mut lines = vec![card_header(
            g::SETTINGS,
            theme::VIOLET,
            "Graphics settings for games",
            "Windows' own switches, the same as in its Settings app",
        )];
        lines.push(match switches.gpu_scheduling {
            Some(on) => setting(
                "Hardware-accelerated GPU scheduling",
                "Needed by DLSS Frame Generation and recommended for any frame generation. \
                 Windows asks for administrator rights; it applies after a restart.",
                Some(
                    switch(
                        "Hardware-accelerated GPU scheduling",
                        on,
                        MyPcMessage::Switched(Switch::GpuScheduling, !on),
                    )
                    .enabled(idle),
                ),
            ),
            None => setting(
                "Hardware-accelerated GPU scheduling",
                "Not offered by this graphics card or its driver.",
                None,
            ),
        });
        lines.push(setting(
            "Game Mode",
            "Windows puts the game first: fewer background tasks and no driver updates while \
             you play.",
            Some(
                switch(
                    "Game Mode",
                    switches.game_mode,
                    MyPcMessage::Switched(Switch::GameMode, !switches.game_mode),
                )
                .enabled(idle),
            ),
        ));
        lines.push(setting(
            "Optimizations for windowed games",
            "Lower latency, and Auto HDR, for DirectX 10 and 11 games in a window or borderless. \
             Games pick it up when they start.",
            Some(
                switch(
                    "Optimizations for windowed games",
                    switches.windowed_optimizations,
                    MyPcMessage::Switched(
                        Switch::WindowedOptimizations,
                        !switches.windowed_optimizations,
                    ),
                )
                .enabled(idle),
            ),
        ));
        if any_hdr {
            lines.push(setting(
                "Auto HDR",
                "HDR for DirectX 11 and 12 games made for SDR, while HDR is on. Games pick it up \
                 when they start.",
                Some(
                    switch(
                        "Auto HDR",
                        switches.auto_hdr,
                        MyPcMessage::Switched(Switch::AutoHdr, !switches.auto_hdr),
                    )
                    .enabled(idle),
                ),
            ));
        }
        lines.push(
            row([
                link(
                    "Graphics settings",
                    MyPcMessage::OpenSettings("ms-settings:display-advancedgraphics").into(),
                ),
                link(
                    "HDR settings",
                    MyPcMessage::OpenSettings("ms-settings:display-hdr").into(),
                ),
                link(
                    "Game Mode",
                    MyPcMessage::OpenSettings("ms-settings:gaming-gamemode").into(),
                ),
            ])
            .gap(4.0),
        );
        card(lines)
    }
}

fn gpu_card(gpu: &Gpu) -> El<Message> {
    card([
        card_header(g::MONITOR, theme::PINK, "Graphics card", ""),
        big(&gpu.name),
        facts(vec![
            ("Vendor", gpu.vendor.name().to_owned()),
            (
                "Video memory",
                if gpu.vram > 0 {
                    format_bytes(gpu.vram)
                } else {
                    "Unknown".to_owned()
                },
            ),
            ("Driver", or_unknown(&gpu.driver)),
            ("Driver date", or_unknown(&gpu.driver_date)),
        ]),
    ])
}

fn memory_card(memory: &Memory) -> El<Message> {
    let mut lines = vec![
        card_header(g::MEMORY, theme::GREEN, "Memory", ""),
        big(&if memory.total > 0 {
            format_bytes(memory.total)
        } else {
            "Unknown".to_owned()
        }),
    ];
    if memory.total > 0 {
        let used = memory.total.saturating_sub(memory.available);
        let ratio = used as f64 / memory.total as f64;
        lines.push(meter(ratio as f32));
        lines.push(hint(format!(
            "{} in use ({:.0}%)",
            format_bytes(used),
            ratio * 100.0
        )));
    }
    if !memory.modules.is_empty() {
        // "2 × 16 GB", by size.
        let mut groups: Vec<(u64, usize)> = Vec::new();
        for module in &memory.modules {
            match groups.iter_mut().find(|(size, _)| *size == module.size) {
                Some((_, count)) => *count += 1,
                None => groups.push((module.size, 1)),
            }
        }
        let kind = memory
            .modules
            .iter()
            .map(|module| module.kind)
            .find(|kind| !kind.is_empty())
            .unwrap_or("");
        let speed = memory
            .modules
            .iter()
            .map(|module| module.speed)
            .max()
            .unwrap_or(0);
        let mut parts: Vec<String> = Vec::new();
        for module in &memory.modules {
            let part = format!("{} {}", module.manufacturer, module.part_number)
                .trim()
                .to_owned();
            if !part.is_empty() && !parts.contains(&part) {
                parts.push(part);
            }
        }
        lines.push(facts(vec![
            (
                "Modules",
                groups
                    .iter()
                    .map(|(size, count)| format!("{count} × {}", format_bytes(*size)))
                    .collect::<Vec<_>>()
                    .join(" + "),
            ),
            ("Type", or_unknown(kind)),
            (
                "Speed",
                if speed > 0 {
                    format!("{speed} MT/s")
                } else {
                    "Unknown".to_owned()
                },
            ),
            ("Part", parts.join(", ")),
        ]));
    }
    card(lines)
}

fn storage_card(drives: &[Drive]) -> El<Message> {
    let mut lines = vec![card_header(g::STORAGE, theme::ORANGE, "Storage", "")];
    if drives.is_empty() {
        lines.push(hint("No fixed drive was found."));
    }
    for drive in drives {
        let used = drive.total.saturating_sub(drive.free);
        let ratio = if drive.total == 0 {
            0.0
        } else {
            used as f64 / drive.total as f64
        };
        let mut detail = format!(
            "{} free of {}",
            format_bytes(drive.free),
            format_bytes(drive.total)
        );
        if !drive.file_system.is_empty() {
            detail.push_str(&format!("  ·  {}", drive.file_system));
        }
        lines.push(
            col([
                text(format!("{}  {}", drive.root, drive.label).trim().to_owned()).bold(),
                hint(detail),
                meter(ratio as f32),
            ])
            .gap(3.0),
        );
    }
    card(lines)
}

/// A card for a part that could not be read.
fn failed_card(icon: &str, tint: windows_canvas::ColorF, title: &str, why: &str) -> El<Message> {
    card([card_header(icon, tint, title, ""), hint(why.to_owned())])
}

/// Cards in rows of `columns`, each row's cards as tall as the row; the last card fills its row.
fn grid(cards: Vec<El<Message>>, columns: usize) -> El<Message> {
    let mut rows = Vec::new();
    let mut cards = cards.into_iter().peekable();
    while cards.peek().is_some() {
        let line: Vec<El<Message>> = cards
            .by_ref()
            .take(columns)
            .map(|card| card.fill_width().fill_height())
            .collect();
        rows.push(row(line).gap(12.0));
    }
    col(rows).gap(12.0)
}

/// A group's name over its cards ("HARDWARE").
fn group_label(label: &str) -> El<Message> {
    text(label)
        .text_size(theme::TEXT_CAPTION)
        .weight(700)
        .color(theme::TEXT_TERTIARY)
        .padding(crate::ui::el::Edges::new(2.0, 8.0, 0.0, 0.0))
}

/// The card's main fact, large.
fn big(value: &str) -> El<Message> {
    text(value)
        .text_size(theme::TEXT_SUBTITLE + 3.0)
        .weight(600)
        .wrap()
}

/// Label and value pairs in two aligned columns; empty values are left out.
fn facts(rows: Vec<(&str, String)>) -> El<Message> {
    col(rows
        .into_iter()
        .filter(|(_, value)| !value.trim().is_empty())
        .map(|(label, value)| {
            row([
                text(label).color(theme::TEXT_TERTIARY).width(110.0),
                text(value).wrap().fill_width(),
            ])
            .gap(12.0)
        }))
    .gap(4.0)
}

/// A setting: its name and description on the left, its control on the right.
fn setting(title: &str, description: &str, control: Option<El<Message>>) -> El<Message> {
    let mut texts = vec![text(title).bold().wrap()];
    if !description.is_empty() {
        texts.push(hint(description.to_owned()));
    }
    let mut items = vec![col(texts).gap(2.0).fill_width()];
    items.extend(control);
    row(items)
        .gap(16.0)
        .pad_xy(12.0, 10.0)
        .bg(theme::CARD)
        .radius(theme::RADIUS_CONTROL)
}

/// An On/Off switch named for UI Automation.
fn switch(name: &str, on: bool, message: MyPcMessage) -> El<Message> {
    toggle(if on { "On" } else { "Off" }, on, message.into())
        .name(name.to_owned())
        .width(84.0)
}

fn known(count: u32) -> String {
    if count > 0 {
        count.to_string()
    } else {
        "Unknown".to_owned()
    }
}

fn or_unknown(value: &str) -> String {
    if value.trim().is_empty() {
        "Unknown".to_owned()
    } else {
        value.to_owned()
    }
}

/// A refresh rate: whole when it is, else with its decimals (59.94).
fn hz(rate: f64) -> String {
    let text = format!("{rate:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A profile's name without its extension.
fn profile_name(file: &str) -> String {
    std::path::Path::new(file).file_stem().map_or_else(
        || file.to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_profiles() {
        assert_eq!(hz(120.0), "120");
        assert_eq!(hz(59.94), "59.94");
        assert_eq!(hz(143.9), "143.9");
        assert_eq!(
            profile_name("Perfil Calibrado HDR 24-05-2026 194430.icc"),
            "Perfil Calibrado HDR 24-05-2026 194430"
        );
    }

    #[test]
    fn unknown_values() {
        assert_eq!(known(0), "Unknown");
        assert_eq!(known(12), "12");
        assert_eq!(or_unknown("  "), "Unknown");
    }
}
