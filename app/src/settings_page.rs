//! The Settings page (the sidebar's last page). Its Appearance card sets how the window looks:
//! Theme (as Windows' apps are, light or dark), Background (Mica, Mica Alt or the theme's solid
//! colour) and Accent colour (Windows' or Game Manager's red). By default it follows Windows.
//!
//! - **Saved** in `game-settings.json` (`gm_core::config::save_appearance`) in the background, and
//!   applied at once (`apply_appearance`): the palette the views draw with (`theme::set_palette`,
//!   the dark or light one with the accent), and the window's look (`WindowCommand::Look`: a dark
//!   or light frame, and Windows' backdrop). Mica needs Windows 11 22H2; without it, or when
//!   Windows' transparency effects are off, the window is solid (Windows draws Mica's own solid
//!   colour then).
//! - **The first frame**: `main` reads the choice and Windows' look before the window opens
//!   (`starting_look`), so the window never shows the wrong theme first.
//! - **Windows' changes** follow live: the shell passes Windows' colour messages on
//!   (`App::look_changed`), and Windows' look is read again in the background (`WindowsChanged`,
//!   then `WindowsRead`).

use gm_core::appearance::{Accent, Appearance, Backdrop, Theme, WindowsLook};
use windows_canvas::ColorF;

use crate::shell::ctx::{Ctx, WindowCommand};
use crate::shell::window::{self, Look};
use crate::ui::el::{El, col, row};
use crate::ui::theme::{self, glyph as g};
use crate::ui::widgets::{card, card_header, hint, page_header, segmented};
use crate::{GameManager, Message, app_view::field_label};

#[derive(Clone)]
pub(crate) enum SettingsMessage {
    /// A new choice in Appearance.
    Choose(Appearance),
    /// The choice was saved, or why not.
    Saved(Result<(), String>),
    /// Windows' colours or light or dark mode changed.
    WindowsChanged,
    /// Windows' look, read again.
    WindowsRead(WindowsLook),
}

impl From<SettingsMessage> for Message {
    fn from(message: SettingsMessage) -> Self {
        Message::Settings(message)
    }
}

/// The palette for `appearance` under Windows' `windows` look, made current, and the window's
/// look to go with it.
pub(crate) fn resolve(appearance: &Appearance, windows: &WindowsLook) -> Look {
    let dark = appearance.dark(windows);
    let palette = if dark { theme::DARK } else { theme::LIGHT };
    let accent = appearance
        .accent(windows, dark)
        .map_or(theme::GAME_MANAGER_ACCENT, |[r, g, b, _]| {
            ColorF::from_rgba8(r, g, b, 255)
        });
    theme::set_palette(theme::with_accent(palette, accent));
    let backdrop = match appearance.backdrop {
        Backdrop::Mica => Some(window::Backdrop::Mica),
        Backdrop::MicaAlt => Some(window::Backdrop::MicaAlt),
        Backdrop::Solid => None,
    };
    Look {
        dark,
        backdrop: backdrop.filter(|_| windows.mica),
    }
}

/// The user's choice and Windows' look before the window opens, the palette made current, and
/// the window's first look. Reads `game-settings.json` and the registry (briefly; there is no
/// window to keep responsive yet).
pub(crate) fn starting_look() -> (Appearance, WindowsLook, Look) {
    let appearance = gm_core::config::load_appearance().unwrap_or_default();
    let windows = gm_core::appearance::windows_look();
    let look = resolve(&appearance, &windows);
    (appearance, windows, look)
}

impl GameManager {
    /// Makes the current choice and Windows' look the palette and the window's look.
    fn apply_appearance(&self, context: &Ctx<Message>) {
        let look = resolve(&self.appearance, &self.windows_look);
        context.window(WindowCommand::Look(look));
    }

    pub(crate) fn handle_settings(&mut self, message: SettingsMessage, context: &Ctx<Message>) {
        match message {
            SettingsMessage::Choose(appearance) => {
                if appearance == self.appearance {
                    return;
                }
                self.appearance = appearance;
                self.apply_appearance(context);
                context.spawn_background(move || {
                    let saved =
                        gm_core::config::save_appearance(&appearance).map_err(|e| e.to_string());
                    SettingsMessage::Saved(saved).into()
                });
            }
            SettingsMessage::Saved(Err(error)) => {
                self.error = Some(format!("Settings: {error}"));
            }
            SettingsMessage::Saved(Ok(())) => {}
            SettingsMessage::WindowsChanged => {
                context.spawn_background(|| {
                    SettingsMessage::WindowsRead(gm_core::appearance::windows_look()).into()
                });
            }
            SettingsMessage::WindowsRead(windows) => {
                if windows != self.windows_look {
                    self.windows_look = windows;
                    self.apply_appearance(context);
                }
            }
        }
    }

    /// The Settings page: its Appearance card.
    pub(crate) fn settings_page(&self) -> El<Message> {
        let now = self.appearance;
        let choose = |appearance: Appearance| Message::from(SettingsMessage::Choose(appearance));
        let windows = &self.windows_look;

        let themes = segmented(
            [
                ("Windows", Theme::Windows),
                ("Light", Theme::Light),
                ("Dark", Theme::Dark),
            ]
            .into_iter()
            .map(|(label, theme)| {
                (
                    label,
                    now.theme == theme,
                    choose(Appearance { theme, ..now }),
                )
            })
            .collect(),
        );
        let theme_hint = format!(
            "Windows' apps are {} now.",
            if windows.light { "light" } else { "dark" }
        );

        let backdrops = segmented(
            [
                ("Mica", Backdrop::Mica),
                ("Mica Alt", Backdrop::MicaAlt),
                ("Solid colour", Backdrop::Solid),
            ]
            .into_iter()
            .map(|(label, backdrop)| {
                (
                    label,
                    now.backdrop == backdrop,
                    choose(Appearance { backdrop, ..now }),
                )
            })
            .collect(),
        )
        .enabled(windows.mica);
        let backdrop_hint = if !windows.mica {
            "Mica needs Windows 11 22H2 or later: the window has its solid colour."
        } else if now.backdrop == Backdrop::Solid {
            "The theme's own colour behind everything."
        } else if !windows.transparency {
            "Windows' transparency effects are off, so Mica shows as a solid colour. Turn them on in \
             Windows Settings ▸ Personalization ▸ Colours."
        } else {
            "Mica tints the window with the desktop's colours; Mica Alt shows more of them. Both \
             turn solid while the window isn't the active one."
        };

        let accents = segmented(
            [
                ("Windows'", Accent::Windows),
                ("Game Manager red", Accent::GameManager),
            ]
            .into_iter()
            .map(|(label, accent)| {
                (
                    label,
                    now.accent == accent,
                    choose(Appearance { accent, ..now }),
                )
            })
            .collect(),
        );
        let swatch = row([])
            .size(28.0, 18.0)
            .radius(theme::RADIUS_CONTROL)
            .bg(theme::accent())
            .stroke(theme::card_stroke(), 1.0);
        let accent_hint = match (now.accent, windows.accent) {
            (Accent::Windows, None) => {
                "Windows keeps no accent colour here: Game Manager's red stands in."
            }
            (Accent::Windows, Some(_)) => {
                "Personalization ▸ Colours ▸ Accent colour, as Windows' own apps use it."
            }
            (Accent::GameManager, _) => "Game Manager's own red.",
        };

        let setting = |label: &str, control: El<Message>, line: &str| {
            col([field_label(label), control, hint(line.to_owned()).wrap()]).gap(theme::space::S)
        };
        let appearance = card([
            card_header(
                g::PERSONALIZE,
                theme::violet(),
                "Appearance",
                "How the window looks. By default it follows Windows.",
            ),
            setting("Theme", themes, &theme_hint),
            setting("Background", backdrops, backdrop_hint),
            setting(
                "Accent colour",
                row([accents, swatch]).gap(theme::space::M),
                accent_hint,
            ),
        ])
        .gap(theme::space::L);
        col([
            page_header("Settings", "Game Manager", None, Vec::new()),
            appearance,
        ])
        .gap(theme::space::L)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_choice_makes_the_palette_and_the_look() {
        let windows = WindowsLook {
            light: false,
            transparency: true,
            mica: true,
            accent: Some([
                [0; 4],
                [0x4C, 0xC2, 0xFF, 0],
                [0; 4],
                [0; 4],
                [0x00, 0x67, 0xC0, 0],
                [0; 4],
                [0; 4],
            ]),
        };
        // Following Windows: dark, Mica, Windows' Light 2 with black text on it.
        let look = resolve(&Appearance::default(), &windows);
        assert_eq!(
            look,
            Look {
                dark: true,
                backdrop: Some(window::Backdrop::Mica)
            }
        );
        let palette = theme::palette();
        assert_eq!(palette.base, theme::DARK.base);
        assert_eq!(palette.accent, ColorF::from_rgba8(0x4C, 0xC2, 0xFF, 255));
        assert_eq!(palette.text_on_accent, theme::argb(0xFF00_0000));
        // Light with Game Manager's red: white text on it; Mica Alt.
        let chosen = Appearance {
            theme: Theme::Light,
            backdrop: Backdrop::MicaAlt,
            accent: Accent::GameManager,
        };
        let look = resolve(&chosen, &windows);
        assert_eq!(look.backdrop, Some(window::Backdrop::MicaAlt));
        assert!(!look.dark);
        let palette = theme::palette();
        assert_eq!(palette.base, theme::LIGHT.base);
        assert_eq!(palette.accent, theme::GAME_MANAGER_ACCENT);
        assert_eq!(palette.text_on_accent, theme::argb(0xFFFF_FFFF));
        // No Mica on this Windows, or a solid colour chosen: no backdrop.
        let older = WindowsLook {
            mica: false,
            ..windows
        };
        assert_eq!(resolve(&Appearance::default(), &older).backdrop, None);
        let solid = Appearance {
            backdrop: Backdrop::Solid,
            ..Appearance::default()
        };
        assert_eq!(resolve(&solid, &windows).backdrop, None);
        theme::set_palette(theme::DARK);
    }
}
