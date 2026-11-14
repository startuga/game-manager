//! The game page's art (`gm_core::page_art`): the selected game's wide background across the top
//! of its page and its logo, the stores' original files decoded in the background. A game with no
//! wide picture shows its cover blurred instead (`picture::blurred`); one with neither shows the
//! plain header.
//!
//! - **One game at a time**: only the selected game's pictures are held. Selecting another drops
//!   them, so its page shows the plain header until its own are decoded.
//! - **The art** (`request_page_art`, after every update, while the game page shows): when the
//!   selected game changes, one job reads what is on this PC (`page_art::local`) and sends it
//!   decoded (`Art`). When the stores may still have originals, it then asks them
//!   (`page_art::fetch`), once a session for each game, and sends what that found. Failures show
//!   nothing: the next start tries again.
//! - **The blur**: once the art has come without a background, the game's cover (whatever the
//!   Library has: the original or a stand-in) is made small and soft in another job (`Blur`), and
//!   again when the cover's file changes (an original after a stand-in, Choose cover).
//! - Pictures are kept only for the game still selected; a stand-in never replaces an original
//!   (an older job's answer arriving late).

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use gm_core::page_art::{self, PageArt};

use crate::shell::ctx::Ctx;
use crate::ui::image::{self, Picture};
use crate::{GameManager, Message, Page};

/// The blur's size in pixels (its longer side): drawn stretched across the page, it is all colour.
const BLUR_SIZE: u32 = 48;

#[derive(Clone)]
pub(crate) enum PageArtMessage {
    /// A game's art, decoded; `last` when no better is coming (the stores were asked, or have
    /// nothing more).
    Art(String, ArtPictures, bool),
    /// A game's cover blurred, from that file; `None` when it couldn't be decoded.
    Blur(String, PathBuf, Option<Arc<Picture>>),
}

impl From<PageArtMessage> for Message {
    fn from(message: PageArtMessage) -> Self {
        Message::PageArt(message)
    }
}

/// A game's page art, decoded.
#[derive(Clone, Default)]
pub(crate) struct ArtPictures {
    pub background: Option<Arc<Picture>>,
    pub logo: Option<Arc<Picture>>,
}

/// What the band at the top of the game page draws.
pub(crate) struct Shown<'a> {
    pub background: &'a Arc<Picture>,
    /// The background is the cover blurred.
    pub blurred: bool,
    pub logo: Option<&'a Arc<Picture>>,
}

#[derive(Default)]
pub(crate) struct PageArtState {
    /// The game whose art is held or being looked up.
    game: Option<String>,
    /// Its art once the first answer came, and whether no better is coming.
    art: Option<(ArtPictures, bool)>,
    /// Its cover blurred, and the file it was made from (`None` while being made).
    blur: Option<(PathBuf, Option<Arc<Picture>>)>,
    /// Games whose stores were asked this session.
    fetched: HashSet<String>,
}

impl PageArtState {
    /// The game's background (its own wide picture, else its cover blurred) and logo, once
    /// decoded; `None` while there is no background to draw.
    pub fn shown(&self, id: &str) -> Option<Shown<'_>> {
        if self.game.as_deref() != Some(id) {
            return None;
        }
        let (art, _) = self.art.as_ref()?;
        let (background, blurred) = match (&art.background, &self.blur) {
            (Some(background), _) => (background, false),
            (None, Some((_, Some(blur)))) => (blur, true),
            (None, _) => return None,
        };
        Some(Shown {
            background,
            blurred,
            logo: art.logo.as_ref(),
        })
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

    /// The blur arrived: kept when it is the game's and made from the cover asked for.
    fn blur_arrived(&mut self, id: &str, file: PathBuf, picture: Option<Arc<Picture>>) {
        if self.game.as_deref() != Some(id) {
            return;
        }
        if let Some((asked, kept)) = &mut self.blur
            && *asked == file
        {
            *kept = picture;
        }
    }
}

impl GameManager {
    /// Looks the selected game's page art up when the game changed, and blurs its cover when the
    /// art came without a background (see the module's doc). Blocking work runs in the background.
    pub(crate) fn request_page_art(&mut self, context: &Ctx<Message>) {
        if self.page != Page::Game {
            return;
        }
        let Some(game) = self.selected_game().cloned() else {
            return;
        };
        let state = &mut self.page_art;
        if state.game.as_deref() != Some(game.id.as_str()) {
            state.game = Some(game.id.clone());
            state.art = None;
            state.blur = None;
            let fetch = state.fetched.insert(game.id.clone());
            context.spawn_stream(move |send| {
                let (art, ask) = page_art::local(&game);
                let last = !(ask && fetch);
                send(PageArtMessage::Art(game.id.clone(), decode(&art), last).into());
                if last {
                    return;
                }
                // Nothing new (none, or a request that failed: the next start asks again) leaves
                // what was sent.
                if let Ok(found) = page_art::fetch(&game)
                    && found != art
                {
                    send(PageArtMessage::Art(game.id, decode(&found), true).into());
                }
            });
            return;
        }
        // The blur, while the art has no background of its own.
        let wants_blur = state
            .art
            .as_ref()
            .is_some_and(|(art, _)| art.background.is_none());
        let Some(cover) = self.covers.file(&game.id).map(PathBuf::from) else {
            return;
        };
        let state = &mut self.page_art;
        if !wants_blur || state.blur.as_ref().is_some_and(|(file, _)| *file == cover) {
            return;
        }
        state.blur = Some((cover.clone(), None));
        context.spawn_background(move || {
            let picture = std::fs::read(&cover)
                .ok()
                .and_then(|bytes| image::decode_blurred(&bytes, BLUR_SIZE))
                .map(Arc::new);
            PageArtMessage::Blur(game.id, cover, picture).into()
        });
    }

    pub(crate) fn handle_page_art(&mut self, message: PageArtMessage) {
        match message {
            PageArtMessage::Art(id, pictures, last) => {
                self.page_art.art_arrived(&id, pictures, last);
            }
            PageArtMessage::Blur(id, file, picture) => {
                self.page_art.blur_arrived(&id, file, picture);
            }
        }
    }
}

/// The art's files decoded at full size, the logo cut to what shows; a part that can't be read is
/// left out.
fn decode(art: &PageArt) -> ArtPictures {
    let read = |part: &Option<page_art::Art>| std::fs::read(&part.as_ref()?.file).ok();
    ArtPictures {
        background: read(&art.background)
            .and_then(|bytes| image::decode_cover(&bytes))
            .map(Arc::new),
        logo: read(&art.logo)
            .and_then(|bytes| image::decode_logo(&bytes))
            .map(Arc::new),
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

    #[test]
    fn the_background_is_the_games_own_else_its_cover_blurred() {
        let mut state = state("steam:220");
        assert!(state.shown("steam:220").is_none());
        // Art without a background: nothing to draw until the blur arrives.
        let logo = picture();
        state.art_arrived(
            "steam:220",
            ArtPictures {
                background: None,
                logo: Some(Arc::clone(&logo)),
            },
            false,
        );
        assert!(state.shown("steam:220").is_none());
        let cover = PathBuf::from("cover.jpg");
        state.blur = Some((cover.clone(), None));
        // A blur of another file (the cover changed since) is not kept.
        state.blur_arrived("steam:220", PathBuf::from("old.jpg"), Some(picture()));
        assert!(state.shown("steam:220").is_none());
        state.blur_arrived("steam:220", cover, Some(picture()));
        let shown = state.shown("steam:220").unwrap();
        assert!(shown.blurred && shown.logo.is_some_and(|l| Arc::ptr_eq(l, &logo)));
        // The game's own background, once found, wins over the blur.
        let background = picture();
        state.art_arrived(
            "steam:220",
            ArtPictures {
                background: Some(Arc::clone(&background)),
                logo: None,
            },
            true,
        );
        let shown = state.shown("steam:220").unwrap();
        assert!(!shown.blurred && Arc::ptr_eq(shown.background, &background));
        // Another game's page shows nothing of it.
        assert!(state.shown("steam:400").is_none());
    }

    #[test]
    fn late_answers_never_replace_better_ones() {
        let mut state = state("gog:1");
        let original = picture();
        let last = ArtPictures {
            background: Some(Arc::clone(&original)),
            logo: None,
        };
        state.art_arrived("gog:1", last, true);
        // A first answer of an older job (a stand-in) arriving after the stores' is dropped.
        state.art_arrived(
            "gog:1",
            ArtPictures {
                background: Some(picture()),
                logo: None,
            },
            false,
        );
        assert!(Arc::ptr_eq(
            state.shown("gog:1").unwrap().background,
            &original
        ));
        // Another game's answers are dropped too.
        state.art_arrived("gog:2", ArtPictures::default(), true);
        assert!(state.shown("gog:1").is_some());
    }
}
