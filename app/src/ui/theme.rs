//! The window's design tokens: colours, sizes and fonts, in one place so the views never spell a
//! colour or a size of their own.
//!
//! Two palettes, dark and light (`DARK`, `LIGHT`): a base, translucent layers for the sidebar,
//! cards and controls, rimless controls, three shades of text, and a tint palette whose 15%
//! "washes" back chips and icon squares. The app sets the one the user's choice and Windows ask for
//! (`set_palette`), with Windows' accent or Game Manager's red (`with_accent`); views read each
//! colour when they are built (`theme::text()`), so a change shows at the next frame.
//!
//! Sizes keep the compact density the WinUI window had: controls about 24–32 DIPs high, 4–8 DIPs
//! apart within a group and 12–24 between groups.

use std::cell::RefCell;

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

/// The colours of one theme. Views take them through the functions below (`theme::text()`), which
/// read the palette the app set last (`set_palette`), so a change of theme or accent shows at the
/// next frame. Only the UI thread draws, and it alone holds the current palette.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    /// The window behind everything (where no Mica shows through).
    pub base: ColorF,
    /// The sidebar and the title bar: a layer over the base.
    pub layer: ColorF,
    /// Cards on a page.
    pub card: ColorF,
    /// A card's faint outline.
    pub card_stroke: ColorF,
    /// Lines between areas.
    pub divider: ColorF,
    /// Standard controls: rimless, a translucent fill that changes under the pointer.
    pub control: ColorF,
    pub control_hover: ColorF,
    pub control_pressed: ColorF,
    /// Subtle controls (list rows, nav items, icon buttons): no fill until hovered.
    pub subtle_hover: ColorF,
    pub subtle_pressed: ColorF,
    /// Text boxes.
    pub input: ColorF,
    pub input_stroke: ColorF,
    pub text: ColorF,
    pub text_secondary: ColorF,
    pub text_tertiary: ColorF,
    /// The accent: selections, the focus ring, accent buttons.
    pub accent: ColorF,
    /// Text on the accent: black on a light accent, white on a dark one (`on_accent`).
    pub text_on_accent: ColorF,
    /// The red ✕ and errors.
    pub critical: ColorF,
    pub error_fill: ColorF,
    /// The tint palette; each has a 15% wash for chip and icon-square backgrounds (`wash`).
    pub amber: ColorF,
    pub orange: ColorF,
    pub violet: ColorF,
    pub slate: ColorF,
    pub blue: ColorF,
    pub cyan: ColorF,
    pub pink: ColorF,
    pub green: ColorF,
    /// FSR's chip.
    pub red: ColorF,
    /// The Play button's face.
    pub play: ColorF,
    /// The dimmed backdrop behind a dialog or panel.
    pub scrim: ColorF,
    /// Menus, drop-down lists and dialogs.
    pub flyout: ColorF,
    pub flyout_stroke: ColorF,
    /// Tooltips.
    pub tooltip: ColorF,
}

/// Game Manager's own accent, when the user chooses it over Windows'.
pub const GAME_MANAGER_ACCENT: ColorF = argb(0xFFD7_143A);

/// The dark theme: a near-black base, translucent white layers, text at 100/85/68% white.
pub const DARK: Palette = Palette {
    base: argb(0xFF17_181C),
    layer: argb(0x14FF_FFFF),
    card: argb(0x0FFF_FFFF),
    card_stroke: argb(0x14FF_FFFF),
    divider: argb(0x1AFF_FFFF),
    control: argb(0x14FF_FFFF),
    control_hover: argb(0x22FF_FFFF),
    control_pressed: argb(0x0CFF_FFFF),
    subtle_hover: argb(0x14FF_FFFF),
    subtle_pressed: argb(0x0AFF_FFFF),
    input: argb(0xFF20_2127),
    input_stroke: argb(0x24FF_FFFF),
    text: argb(0xFFFF_FFFF),
    text_secondary: argb(0xD9FF_FFFF),
    text_tertiary: argb(0xADFF_FFFF),
    accent: GAME_MANAGER_ACCENT,
    text_on_accent: argb(0xFFFF_FFFF),
    critical: argb(0xFFFF_6B6B),
    error_fill: argb(0x33C4_2B1C),
    amber: argb(0xFFFF_C857),
    orange: argb(0xFFF0_7F2A),
    violet: argb(0xFFB9_A3FF),
    slate: argb(0xFF8B_94A8),
    blue: argb(0xFF5F_A8FF),
    cyan: argb(0xFF4F_C3F7),
    pink: argb(0xFFE0_5CB8),
    green: argb(0xFF3F_C26A),
    red: argb(0xFFE5_484D),
    play: argb(0xFF2F_8CF0),
    scrim: argb(0x9900_0000),
    flyout: argb(0xFF24_252B),
    flyout_stroke: argb(0x29FF_FFFF),
    tooltip: argb(0xFF2C_2D33),
};

/// The light theme: a light grey base, white layers, translucent black controls (rimless, as in
/// the dark theme), text at 90/75/60% black, and deeper tints that read on white.
pub const LIGHT: Palette = Palette {
    base: argb(0xFFF3_F3F3),
    layer: argb(0x80FF_FFFF),
    card: argb(0xB3FF_FFFF),
    card_stroke: argb(0x14_000000),
    divider: argb(0x17_000000),
    control: argb(0x0B_000000),
    control_hover: argb(0x14_000000),
    control_pressed: argb(0x07_000000),
    subtle_hover: argb(0x0B_000000),
    subtle_pressed: argb(0x06_000000),
    input: argb(0xFFFF_FFFF),
    input_stroke: argb(0x29_000000),
    text: argb(0xE6_000000),
    text_secondary: argb(0xBF_000000),
    text_tertiary: argb(0x99_000000),
    accent: GAME_MANAGER_ACCENT,
    text_on_accent: argb(0xFFFF_FFFF),
    critical: argb(0xFFC4_2B1C),
    error_fill: argb(0x29C4_2B1C),
    amber: argb(0xFF9D_5D00),
    orange: argb(0xFFC2_4E00),
    violet: argb(0xFF6B_4FBB),
    slate: argb(0xFF5B_6478),
    blue: argb(0xFF0F_6CBD),
    cyan: argb(0xFF00_7A9E),
    pink: argb(0xFFB4_009E),
    green: argb(0xFF0F_7B0F),
    red: argb(0xFFC4_2B1C),
    play: argb(0xFF2F_8CF0),
    scrim: argb(0x4D00_0000),
    flyout: argb(0xFFF9_F9F9),
    flyout_stroke: argb(0x1F_000000),
    tooltip: argb(0xFFF9_F9F9),
};

thread_local! {
    static CURRENT: RefCell<Palette> = const { RefCell::new(DARK) };
}

/// Makes `palette` the one the views and the renderer draw with, from the next frame on.
pub fn set_palette(palette: Palette) {
    CURRENT.with(|current| *current.borrow_mut() = palette);
}

/// The palette drawn with now.
pub fn palette() -> Palette {
    CURRENT.with(|current| *current.borrow())
}

/// `palette` with `accent`, and the text that reads on it: black on a light accent, white on a
/// dark one (by its relative luminance, as WCAG works it out).
pub fn with_accent(palette: Palette, accent: ColorF) -> Palette {
    let linear = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance =
        0.2126 * linear(accent.r) + 0.7152 * linear(accent.g) + 0.0722 * linear(accent.b);
    Palette {
        accent,
        text_on_accent: if luminance > 0.4 {
            argb(0xFF00_0000)
        } else {
            argb(0xFFFF_FFFF)
        },
        ..palette
    }
}

/// One function per colour of the current palette.
macro_rules! colors {
    ($($name:ident),* $(,)?) => {
        $(
            #[doc = concat!("The current palette's `", stringify!($name), "` (`Palette`).")]
            pub fn $name() -> ColorF {
                CURRENT.with(|current| current.borrow().$name)
            }
        )*
    };
}

colors!(
    base,
    layer,
    card,
    card_stroke,
    divider,
    control,
    control_hover,
    control_pressed,
    subtle_hover,
    subtle_pressed,
    input,
    input_stroke,
    text,
    text_secondary,
    text_tertiary,
    accent,
    text_on_accent,
    critical,
    error_fill,
    amber,
    orange,
    violet,
    slate,
    blue,
    cyan,
    pink,
    green,
    red,
    play,
    scrim,
    flyout,
    flyout_stroke,
    tooltip,
);

/// The selected row, nav item or tab: the accent at 23%.
pub fn selected() -> ColorF {
    with_alpha(accent(), 0x3A)
}

/// Selected text in a text box.
pub fn text_selection() -> ColorF {
    with_alpha(accent(), 0x8C)
}

/// How opaque a disabled control is drawn (its children too).
pub const DISABLED_OPACITY: f32 = 0.42;

/// A tint's background wash.
pub const fn wash(tint: ColorF) -> ColorF {
    with_alpha(tint, 0x26)
}

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
/// A game's name over its art, at the top of its page.
pub const TEXT_HERO: f32 = 32.0;
pub const ICON_SIZE: f32 = 14.0;

/// The band of art across the top of the game page: as high as a share of its width
/// (`ART_BAND_RATIO`, wider than Steam's 3.1:1 heroes, so the tabs stay near), within these.
pub const ART_BAND_RATIO: f32 = 3.6;
pub const ART_BAND_MIN_HEIGHT: f32 = 270.0;
pub const ART_BAND_MAX_HEIGHT: f32 = 380.0;
/// Where the band's picture starts to fade out, as a share of its height; the text sits above.
pub const ART_FADE_FROM: f32 = 0.7;
/// The largest a game's logo is drawn over its art (width, height).
pub const ART_LOGO_MAX: (f32, f32) = (480.0, 112.0);

/// Fonts, all Windows' own: the UI's text, headings and page titles, and the icon glyphs.
pub const FONT_TEXT: &str = "Segoe UI Variable Text";
pub const FONT_DISPLAY: &str = "Segoe UI Variable Display";
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
    pub const PERSONALIZE: &str = "\u{E771}";
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
