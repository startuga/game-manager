//! What a text box does to its text, with no drawing or window: the caret, the selection, typing,
//! deleting, moving, words, the clipboard's part and undo. The toolkit keeps one `TextEdit` per
//! text box (`UiState::edits`); the renderer draws it, the shell feeds it keys.
//!
//! - **Offsets** are UTF-8 byte offsets into the text, always on a character boundary: what
//!   Rust's strings and windows-text's hit-testing use.
//! - **The app owns the text**. A change returns the new text, which the box sends to the app
//!   (`on_change`); the next view gives it back. `sync` takes the view's text: when it isn't the
//!   one the box last knew (Escape emptied the search, Reset, a rescan), the caret goes to the
//!   end and the history starts over, as when a program sets an EDIT control's text.
//! - **Words**, for Ctrl+arrows, Ctrl+Backspace/Delete and double-click: runs of letters and
//!   digits, of spaces, and of anything else, as Windows' EDIT controls see them.
//!   Ctrl+→ goes to the start of the next word, Ctrl+← to the start of this or the previous one.
//! - **Undo**: each change keeps what was there before. Typing in a row is one step, and so is
//!   deleting in a row; moving the caret, a paste or a cut ends a run. Redo is cleared by any new
//!   change.
//! - **One line**: a paste keeps its first line, as a single-line EDIT does; tabs become spaces
//!   and other control characters are dropped (`one_line`).

/// The caret's moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    WordLeft,
    WordRight,
    Home,
    End,
}

/// What a run of changes is, so a run undoes as one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Run {
    Typing,
    Deleting,
}

/// The text, caret and selection before a change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    text: String,
    caret: usize,
    anchor: usize,
}

/// A text box's editing state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    /// The text as the box last knew it: sent to the app, or taken from the view.
    text: String,
    caret: usize,
    /// The selection's other end; equal to `caret` when nothing is selected.
    anchor: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    run: Option<Run>,
}

/// How many changes undo goes back at most.
const UNDO_LIMIT: usize = 100;

/// A character's word class: letters and digits, spaces, or anything else.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

fn class(c: char) -> Class {
    if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else if c.is_whitespace() {
        Class::Space
    } else {
        Class::Other
    }
}

/// `text` as one line of a text box: up to its first line break, without control characters
/// (tabs become spaces).
pub fn one_line(text: &str) -> String {
    text.split(['\r', '\n'])
        .next()
        .unwrap_or_default()
        .chars()
        .filter_map(|c| match c {
            '\t' => Some(' '),
            c if c.is_control() => None,
            c => Some(c),
        })
        .collect()
}

impl TextEdit {
    /// A box showing `text`, its caret at the end.
    pub fn new(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            caret: text.len(),
            anchor: text.len(),
            undo: Vec::new(),
            redo: Vec::new(),
            run: None,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    #[cfg(test)]
    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// The selection, start to end; empty at the caret when nothing is selected.
    pub fn selection(&self) -> std::ops::Range<usize> {
        self.caret.min(self.anchor)..self.caret.max(self.anchor)
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selected_text(&self) -> &str {
        &self.text[self.selection()]
    }

    #[cfg(test)]
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    #[cfg(test)]
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Takes the view's text. Returns whether it was new to the box (then the caret went to the
    /// end and the history was cleared).
    pub fn sync(&mut self, text: &str) -> bool {
        if self.text == text {
            return false;
        }
        *self = Self::new(text);
        true
    }

    /// `offset` on the character boundary at or before it, inside the text.
    fn boundary(&self, offset: usize) -> usize {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        offset
    }

    fn previous_char(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(at, _)| at)
    }

    fn next_char(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(offset, |c| offset + c.len_utf8())
    }

    /// The start of the word at or before `offset`: spaces skipped, then that word's characters.
    fn word_start(&self, offset: usize) -> usize {
        let mut at = offset;
        let mut chars = self.text[..offset].chars().rev().peekable();
        while chars.next_if(|c| class(*c) == Class::Space).is_some() {
            at = self.previous_char(at);
        }
        if let Some(&first) = chars.peek() {
            let kind = class(first);
            while chars.next_if(|c| class(*c) == kind).is_some() {
                at = self.previous_char(at);
            }
        }
        at
    }

    /// The start of the next word after `offset`: this word's characters, then spaces.
    fn word_end(&self, offset: usize) -> usize {
        let mut at = offset;
        let mut chars = self.text[offset..].chars().peekable();
        if let Some(&first) = chars.peek() {
            let kind = class(first);
            if kind != Class::Space {
                while let Some(c) = chars.next_if(|c| class(*c) == kind) {
                    at += c.len_utf8();
                }
            }
        }
        while let Some(c) = chars.next_if(|c| class(*c) == Class::Space) {
            at += c.len_utf8();
        }
        at
    }

    /// Where a motion takes the caret from `from`.
    fn target(&self, motion: Motion, from: usize) -> usize {
        match motion {
            Motion::Left => self.previous_char(from),
            Motion::Right => self.next_char(from),
            Motion::WordLeft => self.word_start(from),
            Motion::WordRight => self.word_end(from),
            Motion::Home => 0,
            Motion::End => self.text.len(),
        }
    }

    /// Moves the caret; with `extend` (Shift) the selection grows or shrinks with it. Without
    /// it, ← and → first collapse a selection to its start or end.
    pub fn move_caret(&mut self, motion: Motion, extend: bool) {
        self.run = None;
        if !extend && self.has_selection() {
            let range = self.selection();
            let to = match motion {
                Motion::Left => range.start,
                Motion::Right => range.end,
                Motion::WordLeft => self.word_start(range.start),
                Motion::WordRight => self.word_end(range.end),
                Motion::Home | Motion::End => self.target(motion, self.caret),
            };
            self.caret = to;
            self.anchor = to;
            return;
        }
        self.caret = self.target(motion, self.caret);
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Puts the caret at `offset` (a click), or extends the selection there (Shift, a drag).
    pub fn set_caret(&mut self, offset: usize, extend: bool) {
        self.run = None;
        self.caret = self.boundary(offset);
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Selects from `anchor` to `caret` (a drag that started on a word).
    pub fn select(&mut self, anchor: usize, caret: usize) {
        self.run = None;
        self.anchor = self.boundary(anchor);
        self.caret = self.boundary(caret);
    }

    pub fn select_all(&mut self) {
        self.run = None;
        self.anchor = 0;
        self.caret = self.text.len();
    }

    /// The word (or run of spaces or other characters) around `offset`: a double-click.
    pub fn word_at(&self, offset: usize) -> std::ops::Range<usize> {
        let offset = self.boundary(offset);
        // The character after the offset, or before it at the end.
        let Some(c) = self.text[offset..]
            .chars()
            .next()
            .or_else(|| self.text[..offset].chars().next_back())
        else {
            return offset..offset;
        };
        let kind = class(c);
        let start = offset
            - self.text[..offset]
                .chars()
                .rev()
                .take_while(|c| class(*c) == kind)
                .map(char::len_utf8)
                .sum::<usize>();
        let end = offset
            + self.text[offset..]
                .chars()
                .take_while(|c| class(*c) == kind)
                .map(char::len_utf8)
                .sum::<usize>();
        start..end
    }

    pub fn select_word_at(&mut self, offset: usize) {
        let word = self.word_at(offset);
        self.select(word.start, word.end);
    }

    /// Replaces `range` with `with`, keeping what was there for undo (one step per run).
    fn replace(&mut self, range: std::ops::Range<usize>, with: &str, run: Option<Run>) -> String {
        if run.is_none() || run != self.run || self.undo.is_empty() {
            self.undo.push(Snapshot {
                text: self.text.clone(),
                caret: self.caret,
                anchor: self.anchor,
            });
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.run = run;
        self.text.replace_range(range.clone(), with);
        self.caret = range.start + with.len();
        self.anchor = self.caret;
        self.text.clone()
    }

    /// Types `text` over the selection. Returns the new text, or `None` when nothing changed.
    pub fn insert(&mut self, text: &str) -> Option<String> {
        self.put(text, Some(Run::Typing))
    }

    /// Pastes `text` over the selection: one undo step of its own.
    pub fn paste(&mut self, text: &str) -> Option<String> {
        self.put(text, None)
    }

    fn put(&mut self, text: &str, run: Option<Run>) -> Option<String> {
        let text = one_line(text);
        if text.is_empty() && !self.has_selection() {
            return None;
        }
        // A space after typing letters starts a new step, as words do.
        let run = match run {
            Some(Run::Typing)
                if self.run == Some(Run::Typing)
                    && text.chars().all(char::is_whitespace)
                    && !self.text[..self.caret].ends_with(char::is_whitespace) =>
            {
                self.run = None;
                Some(Run::Typing)
            }
            run => run,
        };
        Some(self.replace(self.selection(), &text, run))
    }

    /// Backspace: the selection, else the character (Ctrl: the word) before the caret.
    pub fn backspace(&mut self, word: bool) -> Option<String> {
        let range = if self.has_selection() {
            self.selection()
        } else if word {
            self.word_start(self.caret)..self.caret
        } else {
            self.previous_char(self.caret)..self.caret
        };
        self.delete_range(range)
    }

    /// Delete: the selection, else the character (Ctrl: up to the next word) after the caret.
    pub fn delete(&mut self, word: bool) -> Option<String> {
        let range = if self.has_selection() {
            self.selection()
        } else if word {
            self.caret..self.word_end(self.caret)
        } else {
            self.caret..self.next_char(self.caret)
        };
        self.delete_range(range)
    }

    fn delete_range(&mut self, range: std::ops::Range<usize>) -> Option<String> {
        if range.is_empty() {
            return None;
        }
        Some(self.replace(range, "", Some(Run::Deleting)))
    }

    /// Cut: the selected text for the clipboard, and the new text. `None` with no selection.
    pub fn cut(&mut self) -> Option<(String, String)> {
        if !self.has_selection() {
            return None;
        }
        let taken = self.selected_text().to_owned();
        let text = self.replace(self.selection(), "", None);
        Some((taken, text))
    }

    /// Goes back one step. Returns the text it had then.
    pub fn undo(&mut self) -> Option<String> {
        let before = self.undo.pop()?;
        self.redo.push(Snapshot {
            text: std::mem::replace(&mut self.text, before.text),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.caret = before.caret;
        self.anchor = before.anchor;
        self.run = None;
        Some(self.text.clone())
    }

    /// Goes forward again after an undo.
    pub fn redo(&mut self) -> Option<String> {
        let after = self.redo.pop()?;
        self.undo.push(Snapshot {
            text: std::mem::replace(&mut self.text, after.text),
            caret: self.caret,
            anchor: self.anchor,
        });
        self.caret = after.caret;
        self.anchor = after.anchor;
        self.run = None;
        Some(self.text.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box with `text` and the caret at `caret`.
    fn at(text: &str, caret: usize) -> TextEdit {
        let mut edit = TextEdit::new(text);
        edit.set_caret(caret, false);
        edit
    }

    #[test]
    fn typing_goes_in_at_the_caret_and_over_the_selection() {
        let mut edit = TextEdit::new("");
        assert_eq!(edit.insert("H").as_deref(), Some("H"));
        assert_eq!(edit.insert("alf").as_deref(), Some("Half"));
        edit.move_caret(Motion::Left, false);
        assert_eq!(edit.insert("-").as_deref(), Some("Hal-f"));
        assert_eq!(edit.caret(), 4);
        // Over a selection; a selection replaced by nothing is a change too.
        edit.select_all();
        assert_eq!(edit.insert("Ç").as_deref(), Some("Ç"));
        assert_eq!(edit.caret(), 2);
        edit.select_all();
        assert_eq!(edit.insert("").as_deref(), Some(""));
        assert_eq!(edit.insert(""), None);
    }

    #[test]
    fn one_line_keeps_the_first_line_without_control_characters() {
        assert_eq!(one_line("Far Cry\r\nsecond"), "Far Cry");
        assert_eq!(one_line("a\tb\u{7}c"), "a bc");
        let mut edit = TextEdit::new("x");
        assert_eq!(edit.paste("\ny").as_deref(), None);
        assert_eq!(edit.paste("y\nz").as_deref(), Some("xy"));
    }

    #[test]
    fn the_caret_moves_by_character_word_and_end() {
        let mut edit = at("Half-Life 2: Episode", 0);
        let mut stops = Vec::new();
        for _ in 0..7 {
            edit.move_caret(Motion::WordRight, false);
            stops.push(edit.caret());
        }
        assert_eq!(stops, [4, 5, 10, 11, 13, 20, 20]);
        let mut back = Vec::new();
        for _ in 0..7 {
            edit.move_caret(Motion::WordLeft, false);
            back.push(edit.caret());
        }
        assert_eq!(back, [13, 11, 10, 5, 4, 0, 0]);
        edit.move_caret(Motion::End, false);
        assert_eq!(edit.caret(), 20);
        edit.move_caret(Motion::Home, false);
        assert_eq!(edit.caret(), 0);
        edit.move_caret(Motion::Left, false);
        assert_eq!(edit.caret(), 0);
        // Characters wider than a byte are stepped over whole.
        let mut edit = at("Pokémon 😀", 0);
        let mut steps = Vec::new();
        while edit.caret() < edit.text().len() {
            edit.move_caret(Motion::Right, false);
            steps.push(edit.caret());
        }
        assert_eq!(steps, [1, 2, 3, 5, 6, 7, 8, 9, 13]);
        edit.move_caret(Motion::Left, false);
        assert_eq!(edit.caret(), 9);
    }

    #[test]
    fn shift_selects_and_an_arrow_collapses_the_selection() {
        let mut edit = at("Far Cry 3", 4);
        edit.move_caret(Motion::WordRight, true);
        assert_eq!((edit.selection(), edit.selected_text()), (4..8, "Cry "));
        edit.move_caret(Motion::Left, true);
        assert_eq!(edit.selected_text(), "Cry");
        // Backwards past the anchor.
        let mut back = at("Far Cry 3", 4);
        back.move_caret(Motion::Home, true);
        assert_eq!(
            (back.caret(), back.anchor(), back.selected_text()),
            (0, 4, "Far ")
        );
        // ← and → go to the selection's ends.
        edit.move_caret(Motion::Left, false);
        assert_eq!((edit.caret(), edit.has_selection()), (4, false));
        edit.move_caret(Motion::End, true);
        edit.move_caret(Motion::Right, false);
        assert_eq!((edit.caret(), edit.has_selection()), (9, false));
        // A click inside a character lands on its start.
        let mut edit = TextEdit::new("é");
        edit.set_caret(1, false);
        assert_eq!(edit.caret(), 0);
        edit.set_caret(99, false);
        assert_eq!(edit.caret(), 2);
    }

    #[test]
    fn backspace_and_delete_take_characters_words_or_the_selection() {
        let mut edit = at("Grand Theft Auto", 11);
        assert_eq!(edit.backspace(false).as_deref(), Some("Grand Thef Auto"));
        assert_eq!(edit.backspace(true).as_deref(), Some("Grand  Auto"));
        assert_eq!(edit.delete(true).as_deref(), Some("Grand Auto"));
        assert_eq!(edit.delete(false).as_deref(), Some("Grand uto"));
        edit.move_caret(Motion::Home, false);
        assert_eq!(edit.backspace(false), None);
        edit.move_caret(Motion::End, false);
        assert_eq!(edit.delete(false), None);
        edit.select_word_at(0);
        assert_eq!(edit.delete(false).as_deref(), Some(" uto"));
        // Ctrl+Backspace after spaces takes them and the word before.
        let mut edit = TextEdit::new("Far Cry  ");
        assert_eq!(edit.backspace(true).as_deref(), Some("Far "));
    }

    #[test]
    fn a_double_click_selects_the_word_under_it() {
        let edit = TextEdit::new("The Witcher 3: Wild Hunt");
        assert_eq!(edit.word_at(6), 4..11);
        assert_eq!(edit.word_at(4), 4..11);
        assert_eq!(edit.word_at(11), 11..12);
        assert_eq!(edit.word_at(13), 13..14);
        assert_eq!(edit.word_at(24), 20..24);
        assert_eq!(TextEdit::new("").word_at(0), 0..0);
        let mut edit = edit;
        edit.select_word_at(16);
        assert_eq!(edit.selected_text(), "Wild");
    }

    #[test]
    fn cut_and_copy_take_the_selection() {
        let mut edit = at("Dead Space", 0);
        assert_eq!(edit.cut(), None);
        edit.move_caret(Motion::WordRight, true);
        assert_eq!(edit.selected_text(), "Dead ");
        assert_eq!(edit.cut(), Some(("Dead ".to_owned(), "Space".to_owned())));
        assert_eq!(edit.undo().as_deref(), Some("Dead Space"));
        assert_eq!(edit.selected_text(), "Dead ");
    }

    #[test]
    fn undo_takes_back_a_run_at_a_time_and_redo_goes_forward() {
        let mut edit = TextEdit::new("");
        for c in "Half".chars() {
            edit.insert(&c.to_string());
        }
        edit.insert(" ");
        for c in "Life".chars() {
            edit.insert(&c.to_string());
        }
        assert_eq!(edit.text(), "Half Life");
        // Moving ends the run; so does deleting after typing.
        edit.move_caret(Motion::Left, false);
        edit.move_caret(Motion::Right, false);
        edit.insert("!");
        edit.backspace(false);
        edit.backspace(false);
        assert_eq!(edit.text(), "Half Lif");
        assert_eq!(edit.undo().as_deref(), Some("Half Life!"));
        assert_eq!(edit.undo().as_deref(), Some("Half Life"));
        assert_eq!(edit.undo().as_deref(), Some("Half"));
        assert_eq!(edit.undo().as_deref(), Some(""));
        assert_eq!(edit.undo(), None);
        assert!(edit.can_redo());
        assert_eq!(edit.redo().as_deref(), Some("Half"));
        assert_eq!(edit.redo().as_deref(), Some("Half Life"));
        assert_eq!(edit.caret(), 9);
        // A new change clears redo; a paste is a step of its own.
        edit.paste(" 2");
        assert!(!edit.can_redo());
        edit.insert("x");
        assert_eq!(edit.undo().as_deref(), Some("Half Life 2"));
        assert_eq!(edit.undo().as_deref(), Some("Half Life"));
    }

    #[test]
    fn a_text_from_the_view_starts_over() {
        let mut edit = TextEdit::new("abc");
        edit.insert("d");
        assert!(!edit.sync("abcd"));
        assert!(edit.can_undo());
        // Escape emptied it, or Reset gave another name.
        assert!(edit.sync(""));
        assert_eq!((edit.caret(), edit.can_undo()), (0, false));
        assert!(edit.sync("Crysis"));
        assert_eq!((edit.caret(), edit.anchor()), (6, 6));
    }

    #[test]
    fn the_history_is_bounded() {
        let mut edit = TextEdit::new("");
        for i in 0..UNDO_LIMIT + 20 {
            edit.paste(&(i % 10).to_string());
        }
        let mut steps = 0;
        while edit.undo().is_some() {
            steps += 1;
        }
        assert_eq!(steps, UNDO_LIMIT);
    }
}
