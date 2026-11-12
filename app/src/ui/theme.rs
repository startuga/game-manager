//! The window's design tokens: colours, sizes and fonts, in one place so the views never spell a
//! colour or a size of their own.
//!
//! The palette is a dark theme: a near-black base, translucent white
//! layers for the sidebar, cards and controls, rimless controls, text at 100/85/68/45% white, and a
//! tint palette whose 15% "washes" back chips and icon squares. The accent is one token, so it can
//! change without touching a view.
//!
//! Sizes keep the compact density the WinUI window had: controls about 24–32 DIPs high, 4–8 DIPs
//! apart within a group and 12–24 between groups.

use windows_canvas::ColorF;

/// A colour from `0xAARRGGBB`, as XAML writes them.
pub const fn argb(value: u32) -> ColorF {
    ColorF::from_rgba8(
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    )
}

/// `color` at `alpha` (0–255), for washes and hover fills of a tint.
pub const fn with_alpha(color: ColorF, alpha: u8) -> ColorF {
    ColorF::new(color.r, color.g, color.b, alpha as f32 / 255.0)
}

/// The window behind everything.
pub const BASE: ColorF = argb(0xFF17_181C);
/// The sidebar and the title bar: a light layer over the base.
pub const LAYER: ColorF = argb(0x14FF_FFFF);
/// Cards on a page.
pub const CARD: ColorF = argb(0x0FFF_FFFF);
/// A card's faint outline.
pub const CARD_STROKE: ColorF = argb(0x14FF_FFFF);
/// Lines between areas.
pub const DIVIDER: ColorF = argb(0x1AFF_FFFF);

/// Standard controls: rimless, a translucent fill that brightens under the pointer.
pub const CONTROL: ColorF = argb(0x14FF_FFFF);
pub const CONTROL_HOVER: ColorF = argb(0x22FF_FFFF);
pub const CONTROL_PRESSED: ColorF = argb(0x0CFF_FFFF);
/// Subtle controls (list rows, nav items, icon buttons): no fill until hovered.
pub const SUBTLE_HOVER: ColorF = argb(0x14FF_FFFF);
pub const SUBTLE_PRESSED: ColorF = argb(0x0AFF_FFFF);
/// Text boxes: darker than the layer they sit on.
pub const INPUT: ColorF = argb(0xFF20_2127);
pub const INPUT_STROKE: ColorF = argb(0x24FF_FFFF);

pub const TEXT: ColorF = argb(0xFFFF_FFFF);
pub const TEXT_SECONDARY: ColorF = argb(0xD9FF_FFFF);
pub const TEXT_TERTIARY: ColorF = argb(0xADFF_FFFF);

/// The accent: selections, the focus ring, accent buttons.
pub const ACCENT: ColorF = argb(0xFFD7_143A);
/// The selected row, nav item or tab: the accent at 23%.
pub const SELECTED: ColorF = with_alpha(ACCENT, 0x3A);
/// The red ✕ and errors.
pub const CRITICAL: ColorF = argb(0xFFFF_6B6B);
pub const ERROR_FILL: ColorF = argb(0x33C4_2B1C);

/// The tint palette; each has a 15% wash for chip and icon-square backgrounds (`wash`).
pub const AMBER: ColorF = argb(0xFFFF_C857);
pub const ORANGE: ColorF = argb(0xFFF0_7F2A);
pub const VIOLET: ColorF = argb(0xFFB9_A3FF);
pub const SLATE: ColorF = argb(0xFF8B_94A8);
pub const BLUE: ColorF = argb(0xFF5F_A8FF);
pub const CYAN: ColorF = argb(0xFF4F_C3F7);
pub const PINK: ColorF = argb(0xFFE0_5CB8);
pub const GREEN: ColorF = argb(0xFF3F_C26A);
/// FSR's chip.
pub const RED: ColorF = argb(0xFFE5_484D);

/// A tint's background wash.
pub const fn wash(tint: ColorF) -> ColorF {
    with_alpha(tint, 0x26)
}

/// The Play button's face.
pub const PLAY: ColorF = argb(0xFF2F_8CF0);

/// The dimmed backdrop behind a dialog or panel.
pub const SCRIM: ColorF = argb(0x99_000000);
/// Menus, drop-down lists and dialogs.
pub const FLYOUT: ColorF = argb(0xFF24_252B);
pub const FLYOUT_STROKE: ColorF = argb(0x29FF_FFFF);
/// Tooltips.
pub const TOOLTIP: ColorF = argb(0xFF2C_2D33);

/// Corner radii.
pub const RADIUS_CONTROL: f32 = 4.0;
pub const RADIUS_CARD: f32 = 8.0;
pub const RADIUS_TAB: f32 = 7.0;
pub const RADIUS_FLYOUT: f32 = 8.0;

/// Spacing within a group (12–24 between groups).
pub const GAP: f32 = 8.0;

/// Control heights.
pub const CONTROL_HEIGHT: f32 = 30.0;
pub const TITLE_BAR_HEIGHT: f32 = 40.0;
/// A caption button (minimize, maximize, close).
pub const CAPTION_BUTTON_WIDTH: f32 = 46.0;
pub const SIDEBAR_WIDTH: f32 = 300.0;
pub const SIDEBAR_MIN_WIDTH: f32 = 220.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 480.0;

/// Text sizes.
pub const TEXT_SIZE: f32 = 13.0;
pub const TEXT_SMALL: f32 = 12.0;
pub const TEXT_CAPTION: f32 = 11.0;
pub const TEXT_SUBTITLE: f32 = 15.0;
pub const TEXT_TITLE: f32 = 22.0;
pub const ICON_SIZE: f32 = 14.0;

/// Fonts: the UI's text, page titles and the icon glyphs.
pub const FONT_TEXT: &str = "Segoe UI Variable Text";
pub const FONT_DISPLAY: &str = "Segoe UI Variable Display";
/// Oxanium (SIL Open Font License, `app/assets/Oxanium-OFL.txt`), embedded in the exe.
pub const FONT_TITLE: &str = "Oxanium";
pub const FONT_ICONS: &str = "Segoe Fluent Icons";

/// How long the pointer rests on something before its tooltip shows, in milliseconds.
pub const TOOLTIP_DELAY_MS: u32 = 600;

/// Glyphs from Segoe Fluent Icons.
pub mod glyph {
    pub const GAME: &str = "\u{E7FC}";
    pub const ADD: &str = "\u{E710}";
    pub const SEARCH: &str = "\u{E721}";
    pub const REFRESH: &str = "\u{E72C}";
    pub const FOLDER: &str = "\u{E8B7}";
    pub const SETTINGS: &str = "\u{E713}";
    pub const PLAY: &str = "\u{E768}";
    pub const CLOSE: &str = "\u{E8BB}";
    pub const MINIMIZE: &str = "\u{E921}";
    pub const MAXIMIZE: &str = "\u{E922}";
    pub const RESTORE: &str = "\u{E923}";
    pub const CHEVRON_DOWN: &str = "\u{E70D}";
    pub const CHEVRON_RIGHT: &str = "\u{E76C}";
    pub const CHECK: &str = "\u{E73E}";
    pub const INFO: &str = "\u{E946}";
    pub const ERROR: &str = "\u{E783}";
    pub const LINK: &str = "\u{E71B}";
    pub const LIBRARY: &str = "\u{E8F1}";
    pub const BRUSH: &str = "\u{E790}";
    pub const PUZZLE: &str = "\u{EA86}";
    pub const CHIP: &str = "\u{E950}";
    pub const DOWNLOAD: &str = "\u{E896}";
    pub const DELETE: &str = "\u{E74D}";
    pub const OVERVIEW: &str = "\u{E80F}";
    pub const NVIDIA: &str = "\u{E7F4}";
    pub const WRENCH: &str = "\u{E90F}";
    pub const EDIT: &str = "\u{E70F}";
    pub const SAVE: &str = "\u{E74E}";
    pub const EXPORT: &str = "\u{EDE1}";
    pub const IMPORT: &str = "\u{E8B5}";
    /// My PC: the computer, its displays, memory, Windows and drives.
    pub const PC: &str = "\u{E977}";
    pub const MONITOR: &str = "\u{E7F4}";
    pub const MEMORY: &str = "\u{EEA0}";
    pub const SYSTEM: &str = "\u{E770}";
    pub const STORAGE: &str = "\u{EDA2}";
    /// The game menu's Pin / Unpin, Rename…, Hide from / Show in the list, and Remove.
    pub const PIN: &str = "\u{E718}";
    pub const UNPIN: &str = "\u{E77A}";
    pub const RENAME: &str = "\u{E8AC}";
    /// Choose cover… and Reset cover.
    pub const PICTURE: &str = "\u{E8B9}";
    pub const UNDO: &str = "\u{E7A7}";
    pub const HIDE: &str = "\u{ED1A}";
    pub const VIEW: &str = "\u{E890}";
    pub const FILTER: &str = "\u{E71C}";
    pub const REMOVE: &str = "\u{E738}";
}
