//! The game page's art (`gm_core::page_art`): the selected game's wide background across the top
//! of its page and its logo, the user's own or the stores' original files, decoded in the
//! background. Every page has the band: without a wide picture it shows the game's cover blurred
//! (`picture::blurred`), without a cover its icon blurred, and until one of those is ready the
//! store's colour.
//!
//! - **One game at a time**: only the selected game's pictures are held. Selecting another drops
//!   them, so its band shows the store's colour until its own are decoded.
//! - **The art** (`request_page_art`, after every update, while the game page shows): when the
//!   selected game changes, one job reads what is on this PC (`page_art::local`: the user's own
//!   first) and sends it decoded (`Art`). When the stores may still have originals, it then asks
//!   them (`page_art::fetch`), once a session for each game, and sends what that found. Failures
//!   show nothing: the next start tries again.
//! - **The blur**: once the art has come without a background, the game's cover (whatever the
//!   Library has: the original or a stand-in), else its icon's best image, is made small and soft
//!   in another job (`Blur`), and again when what it was made from changes (the cover arriving
//!   after the icon, an original after a stand-in, Choose cover). The blur shown stays until the
//!   new one is ready.
//! - **The user's own art** is chosen and dropped in the Artwork dialog (`artwork.rs`, from the
//!   band's button or the game menu); the game's art is then read again (`reload`) while what
//!   shows stays.
//! - **Animated art** (an animated WebP background or logo, the user's own): its first frame shows
//!   until it plays, and it plays whenever the window is in front, unless Settings say animated
//!   covers never play (`game_page.rs`, `ui::animation`, `el::Play::Always`). A background's frames are made no larger than `BACKGROUND_FRAMES`; a
//!   logo's as large as it is drawn (`theme::ART_LOGO_MAX`), and not cut to what shows, as its
//!   frames could each be cut differently.
//! - Pictures are kept only for the game still selected; a stand-in never replaces an original
//!   (an older job's answer arriving late).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gm_core::page_art::{self, PageArt};
use gm_core::picture;

use crate::shell::ctx::Ctx;
use crate::ui::animation::Animation;
use crate::ui::image::{self, Picture};
use crate::ui::theme;
use crate::{GameManager, Message, Page};

/// The blur's size in pixels (its longer side): drawn stretched across the page, it is all colour.
const BLUR_SIZE: u32 = 48;
/// The most an animated background's frames are made to cover, in pixels: Steam's wide pictures at
/// their largest. A smaller animation keeps its own size.
const BACKGROUND_FRAMES: (u32, u32) = (3840, 1240);

#[derive(Clone)]
pub(crate) enum PageArtMessage {
    /// A game's art, decoded; `last` when no better is coming (the stores were asked, or have
    /// nothing more).
    Art(String, ArtPictures, bool),
    /// A game's cover or icon blurred, from that source; `None` when it couldn't be decoded.
    Blur(String, BlurSource, Option<Arc<Picture>>),
}

impl From<PageArtMessage> for Message {
    fn from(message: PageArtMessage) -> Self {
        Message::PageArt(message)
    }
}

/// What a blur is made from: the cover's file, or the icon's bytes (told apart by where they are
/// kept, so a new icon is blurred again).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum BlurSource {
    Cover(PathBuf),
    Icon(usize),
}

/// A game's page art, decoded.
#[derive(Clone, Default)]
pub(crate) struct ArtPictures {
    pub background: Option<Arc<Picture>>,
    pub logo: Option<Arc<Picture>>,
    /// The parts' animations, when they are animated pictures (their pictures are first frames).
    pub background_animation: Option<Arc<Animation>>,
    pub logo_animation: Option<Arc<Animation>>,
}

/// What the band at the top of the game page draws.
pub(crate) struct Shown<'a> {
    /// The game's wide picture, else its cover or icon blurred; `None` while there is none to
    /// draw (the band shows the store's colour).
    pub background: Option<&'a Arc<Picture>>,
    /// The background is the cover or the icon, blurred.
    pub blurred: bool,
    pub logo: Option<&'a Arc<Picture>>,
    /// The background's and the logo's animations, when they are animated pictures.
    pub background_animation: Option<&'a Arc<Animation>>,
    pub logo_animation: Option<&'a Arc<Animation>>,
}

#[derive(Default)]
pub(crate) struct PageArtState {
    /// The game whose art is held or being looked up.
    game: Option<String>,
    /// Its art once the first answer came, and whether no better is coming.
    art: Option<(ArtPictures, bool)>,
    /// Its cover or icon blurred, and what it was made from (the picture is the earlier one, or
    /// `None`, while the new one is made).
    blur: Option<(BlurSource, Option<Arc<Picture>>)>,
    /// Games whose stores were asked this session.
    fetched: HashSet<String>,
    /// The game's art is to be read again (its own picture changed); what shows stays meanwhile.
    reload: bool,
}

impl PageArtState {
    /// What the game's band draws: its background (its own wide picture, else its cover or icon
    /// blurred) and logo, once decoded.
    pub fn shown(&self, id: &str) -> Shown<'_> {
        if self.game.as_deref() != Some(id) {
            return Shown {
                background: None,
                blurred: false,
                logo: None,
                background_animation: None,
                logo_animation: None,
            };
        }
        let art = self.art.as_ref().map(|(art, _)| art);
        let blur = self.blur.as_ref().and_then(|(_, picture)| picture.as_ref());
        let (background, blurred) = match (art.and_then(|art| art.background.as_ref()), blur) {
            (Some(background), _) => (Some(background), false),
            (None, Some(blur)) => (Some(blur), true),
            (None, None) => (None, false),
        };
        Shown {
            background,
            blurred,
            logo: art.and_then(|art| art.logo.as_ref()),
            background_animation: art
                .filter(|_| !blurred)
                .and_then(|art| art.background_animation.as_ref()),
            logo_animation: art.and_then(|art| art.logo_animation.as_ref()),
        }
    }

    /// A game's art arrived: kept for the game still shown, unless it is a first answer and the
    /// last one is already here.
    fn art_arrived(&mut self, id: &str, pictures: ArtPictures, last: bool) {
        if self.game.as_deref() != Some(id)
            || (!last && self.art.as_ref().is_some_and(|(_, done)| *done))
        {
            return;
        }
        self.art = Some((pictures, last));
    }

    /// The blur arrived: kept when it is the game's and made from the source asked for.
    fn blur_arrived(&mut self, id: &str, source: BlurSource, picture: Option<Arc<Picture>>) {
        if self.game.as_deref() != Some(id) {
            return;
        }
        if let Some((asked, kept)) = &mut self.blur
            && *asked == source
        {
            *kept = picture;
        }
    }

    /// The game's own picture changed: its art is read again, and the answer taken even after a
    /// last one.
    pub(crate) fn reload(&mut self, id: &str) {
        if self.game.as_deref() == Some(id) {
            self.reload = true;
            if let Some((_, last)) = &mut self.art {
                *last = false;
            }
        }
    }
}

impl GameManager {
    /// Looks the selected game's page art up when the game changed (or its own art did), and
    /// blurs its cover or icon when the art came without a background (see the module's doc).
    /// Blocking work runs in the background.
    pub(crate) fn request_page_art(&mut self, context: &Ctx<Message>) {
        if self.page != Page::Game {
            return;
        }
        let Some(game) = self.selected_game().cloned() else {
            return;
        };
        let state = &mut self.page_art;
        let other_game = state.game.as_deref() != Some(game.id.as_str());
        if other_game || state.reload {
            if other_game {
                state.game = Some(game.id.clone());
                state.art = None;
                state.blur = None;
            }
            state.reload = false;
            let fetch = state.fetched.insert(game.id.clone());
            let scale = context.scale();
            context.spawn_stream(move |send| {
                let (art, ask) = page_art::local(&game);
                let last = !(ask && fetch);
                send(PageArtMessage::Art(game.id.clone(), decode(&art, scale), last).into());
                if last {
                    return;
                }
                // Nothing new (none, or a request that failed: the next start asks again) leaves
                // what was sent.
                if let Ok(found) = page_art::fetch(&game)
                    && found != art
                {
                    send(PageArtMessage::Art(game.id, decode(&found, scale), true).into());
                }
            });
            return;
        }
        // The blur, while the art has no background of its own: the cover, else the icon.
        let wants_blur = state
            .art
            .as_ref()
            .is_some_and(|(art, _)| art.background.is_none());
        if !wants_blur {
            return;
        }
        let cover = self.covers.file(&game.id).map(PathBuf::from);
        let icon = self.icons.bytes(&game.id).cloned();
        let source = match (&cover, &icon) {
            (Some(cover), _) => BlurSource::Cover(cover.clone()),
            (None, Some(icon)) => BlurSource::Icon(Arc::as_ptr(icon) as usize),
            (None, None) => return,
        };
        let state = &mut self.page_art;
        if state
            .blur
            .as_ref()
            .is_some_and(|(asked, _)| *asked == source)
        {
            return;
        }
        // The blur shown stays until the new one is ready.
        let shown = state.blur.take().and_then(|(_, picture)| picture);
        state.blur = Some((source.clone(), shown));
        context.spawn_background(move || {
            let picture = match (&source, cover, icon) {
                (BlurSource::Cover(_), Some(cover), _) => std::fs::read(cover)
                    .ok()
                    .and_then(|bytes| image::decode_blurred(&bytes, BLUR_SIZE)),
                (BlurSource::Icon(_), _, Some(icon)) => {
                    image::decode_blurred_icon(&icon, BLUR_SIZE)
                }
                _ => None,
            };
            PageArtMessage::Blur(game.id, source, picture.map(Arc::new)).into()
        });
    }

    pub(crate) fn handle_page_art(&mut self, message: PageArtMessage) {
        match message {
            PageArtMessage::Art(id, pictures, last) => {
                self.page_art.art_arrived(&id, pictures, last);
            }
            PageArtMessage::Blur(id, source, picture) => {
                self.page_art.blur_arrived(&id, source, picture);
            }
        }
    }
}

/// The art's files decoded at full size, a still logo cut to what shows, and the animations of
/// animated ones (their frames made for the window's `scale`); a part that can't be read is left
/// out.
fn decode(art: &PageArt, scale: f32) -> ArtPictures {
    let read = |part: &Option<page_art::Art>| std::fs::read(&part.as_ref()?.file).ok();
    // An animation is played from its file.
    let animation =
        |part: &Option<page_art::Art>, size| Animation::open(&part.as_ref()?.file, size);
    let background_bytes = read(&art.background);
    let background = background_bytes
        .as_deref()
        .and_then(image::decode_cover)
        .map(Arc::new);
    let background_animation = background_bytes
        .filter(|bytes| background.is_some() && picture::is_animated(bytes))
        .and_then(|_| animation(&art.background, BACKGROUND_FRAMES));
    let (logo, logo_animation) = match read(&art.logo) {
        Some(bytes) if picture::is_animated(&bytes) => {
            let logo = image::decode_cover(&bytes).map(Arc::new);
            // Its frames as large as the logo is drawn.
            let size = logo.as_ref().map_or((1, 1), |logo| {
                let (most_w, most_h) = theme::ART_LOGO_MAX;
                let (w, h) = (logo.width.max(1) as f32, logo.height.max(1) as f32);
                let fit = (most_w / w).min(most_h / h) * scale;
                ((w * fit).round() as u32, (h * fit).round() as u32)
            });
            let logo_animation = logo.as_ref().and_then(|_| animation(&art.logo, size));
            (logo, logo_animation)
        }
        Some(bytes) => (image::decode_logo(&bytes).map(Arc::new), None),
        None => (None, None),
    };
    ArtPictures {
        background,
        logo,
        background_animation,
        logo_animation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture() -> Arc<Picture> {
        Arc::new(Picture {
            width: 1,
            height: 1,
            pixels: vec![0; 4],
        })
    }

    fn state(game: &str) -> PageArtState {
        PageArtState {
            game: Some(game.to_owned()),
            ..PageArtState::default()
        }
    }

    fn with_background(background: Option<Arc<Picture>>) -> ArtPictures {
        ArtPictures {
            background,
            ..ArtPictures::default()
        }
    }

    #[test]
    fn the_background_is_the_games_own_else_its_cover_or_icon_blurred() {
        let mut state = state("steam:220");
        // Every page has its band: nothing to draw yet is the store's colour.
        assert!(state.shown("steam:220").background.is_none());
        // Art without a background: the blur, once made.
        let logo = picture();
        state.art_arrived(
            "steam:220",
            ArtPictures {
                logo: Some(Arc::clone(&logo)),
                ..ArtPictures::default()
            },
            false,
        );
        assert!(state.shown("steam:220").background.is_none());
        let icon = BlurSource::Icon(7);
        state.blur = Some((icon.clone(), None));
        // A blur of another source (the cover came since) is not kept.
        state.blur_arrived(
            "steam:220",
            BlurSource::Cover(PathBuf::from("old.jpg")),
            Some(picture()),
        );
        assert!(state.shown("steam:220").background.is_none());
        state.blur_arrived("steam:220", icon, Some(picture()));
        let shown = state.shown("steam:220");
        assert!(shown.blurred && shown.background.is_some());
        assert!(shown.logo.is_some_and(|l| Arc::ptr_eq(l, &logo)));
        // The game's own background, once found, wins over the blur.
        let background = picture();
        state.art_arrived(
            "steam:220",
            with_background(Some(Arc::clone(&background))),
            true,
        );
        let shown = state.shown("steam:220");
        assert!(
            !shown.blurred
                && shown
                    .background
                    .is_some_and(|b| Arc::ptr_eq(b, &background))
        );
        // Another game's page shows none of it.
        assert!(state.shown("steam:400").background.is_none());
    }

    #[test]
    fn late_answers_never_replace_better_ones_but_a_reload_does() {
        let mut state = state("gog:1");
        let original = picture();
        state.art_arrived("gog:1", with_background(Some(Arc::clone(&original))), true);
        // A first answer of an older job (a stand-in) arriving after the stores' is dropped.
        state.art_arrived("gog:1", with_background(Some(picture())), false);
        assert!(
            state
                .shown("gog:1")
                .background
                .is_some_and(|b| Arc::ptr_eq(b, &original))
        );
        // Another game's answers are dropped too.
        state.art_arrived("gog:2", ArtPictures::default(), true);
        assert!(state.shown("gog:1").background.is_some());
        // The user chose their own: it is read again, and its first answer taken.
        state.reload("gog:1");
        assert!(state.reload);
        let mine = picture();
        state.art_arrived("gog:1", with_background(Some(Arc::clone(&mine))), false);
        assert!(
            state
                .shown("gog:1")
                .background
                .is_some_and(|b| Arc::ptr_eq(b, &mine))
        );
    }
}
