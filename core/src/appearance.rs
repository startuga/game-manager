//! How the window looks: the user's choice in Settings (theme, background, accent), kept in
//! `game-settings.json` under `appearance` (`config::load_appearance`), and what Windows says
//! (`windows_look`): whether apps are light or dark, whether transparency effects are on, and the
//! accent palette Windows made from the user's accent colour.
//!
//! Windows keeps them in the registry, under the current user:
//! - `Software\Microsoft\Windows\CurrentVersion\Themes\Personalize`: `AppsUseLightTheme` (1 light,
//!   0 dark; light when missing, as Windows starts) and `EnableTransparency` (0 off).
//! - `Software\Microsoft\Windows\CurrentVersion\Explorer\Accent`: `AccentPalette`, 32 bytes: eight
//!   RGBA colours from lightest to darkest, Light 3, 2 and 1, the accent, Dark 1, 2 and 3, and one
//!   unused. WinUI fills accent controls with Light 2 in dark apps and with Dark 1 in light ones.
//!
//! Mica needs Windows 11 22H2 (build 22621, `DWMWA_SYSTEMBACKDROP_TYPE`): the build is
//! `CurrentBuildNumber` under `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion`.
//!
//! Reading them takes microseconds, but it is the registry: the app reads it before its window
//! opens (so the first frame has the right colours) and later in the background, when Windows says
//! its settings changed.

use serde::{Deserialize, Serialize};

const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
const VERSION: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
/// The first build with `DWMWA_SYSTEMBACKDROP_TYPE` (Windows 11 22H2).
const MICA_BUILD: u32 = 22621;

/// Light or dark: as Windows' apps are, or always one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Theme {
    #[default]
    Windows,
    Light,
    Dark,
}

/// What shows behind the window's layers on Windows 11: Mica (the desktop's colours, blurred),
/// Mica Alt (more of them, as tabbed apps use), or the theme's own solid colour. Windows 10 has
/// only the solid colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Backdrop {
    #[default]
    Mica,
    MicaAlt,
    Solid,
}

/// The accent colour: Windows' (Personalization ▸ Colours), or Game Manager's red.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Accent {
    #[default]
    Windows,
    GameManager,
}

/// The user's choice in Settings ▸ Appearance; by default the window follows Windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Appearance {
    pub theme: Theme,
    pub backdrop: Backdrop,
    pub accent: Accent,
}

/// What Windows' personalization settings say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowsLook {
    /// Apps are light (Personalization ▸ Colours ▸ Choose your mode).
    pub light: bool,
    /// Transparency effects are on.
    pub transparency: bool,
    /// Windows can draw Mica behind a window (Windows 11 22H2 or later).
    pub mica: bool,
    /// The accent palette, lightest first (Light 3 … Dark 3, as RGBA); `None` when Windows keeps
    /// none.
    pub accent: Option<[[u8; 4]; 7]>,
}

impl Default for WindowsLook {
    /// As Windows starts: light apps, transparency on, no Mica known, no accent palette.
    fn default() -> Self {
        Self {
            light: true,
            transparency: true,
            mica: false,
            accent: None,
        }
    }
}

impl Appearance {
    /// Whether the window is dark: as chosen, or as Windows' apps are.
    pub fn dark(&self, windows: &WindowsLook) -> bool {
        match self.theme {
            Theme::Windows => !windows.light,
            Theme::Light => false,
            Theme::Dark => true,
        }
    }

    /// The accent (RGBA) for a dark or light window: Windows' Light 2 or Dark 1; `None` for Game
    /// Manager's own, or when Windows keeps no palette.
    pub fn accent(&self, windows: &WindowsLook, dark: bool) -> Option<[u8; 4]> {
        match self.accent {
            Accent::Windows => windows
                .accent
                .map(|palette| if dark { palette[1] } else { palette[4] }),
            Accent::GameManager => None,
        }
    }
}

/// Windows' personalization settings, as the registry holds them now. Blocking (briefly).
pub fn windows_look() -> WindowsLook {
    let user = windows_registry::CURRENT_USER;
    let personalize = user.open(PERSONALIZE).ok();
    let flag = |name: &str, missing: bool| {
        personalize
            .as_ref()
            .and_then(|key| key.get_u32(name).ok())
            .map_or(missing, |value| value != 0)
    };
    WindowsLook {
        light: flag("AppsUseLightTheme", true),
        transparency: flag("EnableTransparency", true),
        mica: windows_registry::LOCAL_MACHINE
            .open(VERSION)
            .and_then(|key| key.get_string("CurrentBuildNumber"))
            .ok()
            .and_then(|build| build.trim().parse::<u32>().ok())
            .is_some_and(|build| build >= MICA_BUILD),
        accent: user
            .open(ACCENT)
            .and_then(|key| key.get_bytes("AccentPalette"))
            .ok()
            .and_then(|bytes| accent_palette(&bytes)),
    }
}

/// `AccentPalette`'s first seven colours; `None` when it is shorter.
fn accent_palette(bytes: &[u8]) -> Option<[[u8; 4]; 7]> {
    let (colours, _) = bytes.as_chunks::<4>();
    let mut palette = [[0; 4]; 7];
    for (slot, colour) in palette.iter_mut().zip(colours.get(..7)?) {
        *slot = *colour;
    }
    Some(palette)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real `AccentPalette` (Windows' default blue).
    const BLUE: [u8; 32] = [
        0x99, 0xEB, 0xFF, 0x00, 0x4C, 0xC2, 0xFF, 0x00, 0x00, 0x91, 0xF8, 0x00, 0x00, 0x78, 0xD4,
        0x00, 0x00, 0x67, 0xC0, 0x00, 0x00, 0x3E, 0x92, 0x00, 0x00, 0x1A, 0x68, 0x00, 0xF7, 0x63,
        0x0C, 0x00,
    ];

    #[test]
    fn the_accent_palette_is_read_lightest_first() {
        let palette = accent_palette(&BLUE).unwrap();
        assert_eq!(palette[0], [0x99, 0xEB, 0xFF, 0x00]);
        assert_eq!(palette[3], [0x00, 0x78, 0xD4, 0x00]);
        assert_eq!(palette[6], [0x00, 0x1A, 0x68, 0x00]);
        assert_eq!(accent_palette(&BLUE[..27]), None);
    }

    #[test]
    fn the_choice_or_windows_decides_the_theme_and_accent() {
        let windows = WindowsLook {
            light: false,
            transparency: true,
            mica: true,
            accent: accent_palette(&BLUE),
        };
        let follow = Appearance::default();
        assert!(follow.dark(&windows));
        assert!(!follow.dark(&WindowsLook::default()));
        let light = Appearance {
            theme: Theme::Light,
            ..Appearance::default()
        };
        assert!(!light.dark(&windows));
        // Light 2 in a dark window, Dark 1 in a light one; none for Game Manager's own.
        assert_eq!(
            follow.accent(&windows, true),
            Some([0x4C, 0xC2, 0xFF, 0x00])
        );
        assert_eq!(
            follow.accent(&windows, false),
            Some([0x00, 0x67, 0xC0, 0x00])
        );
        let red = Appearance {
            accent: Accent::GameManager,
            ..Appearance::default()
        };
        assert_eq!(red.accent(&windows, true), None);
        assert_eq!(follow.accent(&WindowsLook::default(), true), None);
        // As saved: the default leaves names out, a newer choice falls back field by field.
        let saved: Appearance =
            serde_json::from_str(r#"{"theme":"dark","backdrop":"micaAlt"}"#).unwrap();
        assert_eq!(
            saved,
            Appearance {
                theme: Theme::Dark,
                backdrop: Backdrop::MicaAlt,
                accent: Accent::Windows,
            }
        );
    }

    #[test]
    fn this_pcs_look_reads() {
        // Whatever this PC says, reading it never fails.
        let look = windows_look();
        println!("{look:?}");
    }
}
