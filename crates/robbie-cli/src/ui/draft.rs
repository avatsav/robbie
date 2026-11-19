//! Cursor-aware prompt draft.
//!
//! The cursor is a byte index that always sits on a grapheme boundary. Wrapped
//! row positions are derived on demand, so rendering and vertical movement use
//! the same layout without storing terminal-dependent state in the text.
use std::ops::Deref;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug, Default)]
pub(super) struct Draft {
    text: String,
    cursor: usize,
}

impl Draft {
    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn insert(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.normalize_cursor();
    }

    pub(super) fn insert_char(&mut self, ch: char) {
        self.insert(ch.encode_utf8(&mut [0; 4]));
    }

    pub(super) fn backspace(&mut self) {
        let Some((start, _)) = self.text[..self.cursor].grapheme_indices(true).next_back() else {
            return;
        };
        self.text.replace_range(start..self.cursor, "");
        self.cursor = start;
        self.normalize_cursor();
    }

    pub(super) fn delete(&mut self) {
        let Some((_, glyph)) = self.text[self.cursor..].grapheme_indices(true).next() else {
            return;
        };
        self.text
            .replace_range(self.cursor..self.cursor + glyph.len(), "");
        self.normalize_cursor();
    }

    // An edit can join clusters on either side (ZWJ, combining marks, flags).
    // Keep the caret after the merged grapheme instead of inside it. Rendering
    // and later edits then agree on the same boundary.
    fn normalize_cursor(&mut self) {
        self.cursor = self
            .text
            .grapheme_indices(true)
            .map(|(byte, _)| byte)
            .find(|byte| *byte >= self.cursor)
            .unwrap_or(self.text.len());
    }

    pub(super) fn left(&mut self) {
        if let Some((start, _)) = self.text[..self.cursor].grapheme_indices(true).next_back() {
            self.cursor = start;
        }
    }

    pub(super) fn right(&mut self) {
        if let Some((_, glyph)) = self.text[self.cursor..].grapheme_indices(true).next() {
            self.cursor += glyph.len();
        }
    }

    pub(super) fn up(&mut self, width: usize) {
        let layout = layout(&self.text, self.cursor, width);
        if let Some(row) = layout.cursor_row.checked_sub(1) {
            self.cursor = layout.index(row, layout.cursor_column);
        }
    }

    pub(super) fn down(&mut self, width: usize) {
        let layout = layout(&self.text, self.cursor, width);
        if layout.cursor_row + 1 < layout.rows.len() {
            self.cursor = layout.index(layout.cursor_row + 1, layout.cursor_column);
        }
    }

    // Completion owns the whole first token, including preserved leading space.
    pub(super) fn complete(&mut self, leading: usize, command: &str) {
        self.text.truncate(leading);
        self.text.push_str(command);
        self.cursor = self.text.len();
    }

    pub(super) fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
}

impl Deref for Draft {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.text
    }
}

impl From<&str> for Draft {
    fn from(text: &str) -> Self {
        Self {
            cursor: text.len(),
            text: text.into(),
        }
    }
}

impl From<String> for Draft {
    fn from(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
        }
    }
}

impl PartialEq<&str> for Draft {
    fn eq(&self, other: &&str) -> bool {
        self.text == *other
    }
}

pub(super) struct Layout {
    pub(super) rows: Vec<String>,
    pub(super) cursor_row: usize,
    pub(super) cursor_column: usize,
    positions: Vec<Position>,
}

#[derive(Clone, Copy)]
struct Position {
    bytes: usize,
    row: usize,
    column: usize,
}

impl Layout {
    // Pick the boundary on `row` closest to the requested display column,
    // clamping to the row start when it is short. Wide glyphs can leave gaps
    // between columns, so exact equality is not guaranteed.
    fn index(&self, row: usize, column: usize) -> usize {
        let mut first = None;
        let mut best = None;
        for position in &self.positions {
            if position.row != row {
                continue;
            }
            first.get_or_insert(position.bytes);
            if position.column > column {
                break;
            }
            best = Some(position.bytes);
        }
        best.or(first).unwrap_or(0)
    }
}

pub(super) fn rows(input: &str, width: usize) -> Vec<String> {
    layout(input, input.len(), width).rows
}

// Preserve the draft verbatim, wrapping by cells without splitting graphemes.
// A full final row gets a new empty row so the cursor always has a real cell.
pub(super) fn layout(input: &str, cursor: usize, width: usize) -> Layout {
    if width == 0 {
        return Layout {
            rows: vec![String::new()],
            cursor_row: 0,
            cursor_column: 0,
            positions: vec![Position {
                bytes: 0,
                row: 0,
                column: 0,
            }],
        };
    }
    let mut rows = vec![String::new()];
    let mut column = 0;
    let mut positions = Vec::new();
    for (bytes, grapheme) in input.grapheme_indices(true) {
        let glyph = if grapheme.width() > width {
            "�"
        } else {
            grapheme
        };
        let cells = glyph.width();
        if column + cells > width {
            rows.push(String::new());
            column = 0;
        }
        positions.push(Position {
            bytes,
            row: rows.len() - 1,
            column,
        });
        let last = rows.len() - 1;
        rows[last].push_str(glyph);
        column += cells;
        if column == width {
            rows.push(String::new());
            column = 0;
        }
    }
    positions.push(Position {
        bytes: input.len(),
        row: rows.len() - 1,
        column,
    });
    let cursor = positions
        .iter()
        .find(|position| position.bytes == cursor)
        .unwrap_or(&positions[positions.len() - 1]);
    Layout {
        rows,
        cursor_row: cursor.row,
        cursor_column: cursor.column,
        positions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_boundary(draft: &Draft) {
        assert!(
            draft.cursor == draft.len()
                || draft
                    .grapheme_indices(true)
                    .any(|(byte, _)| byte == draft.cursor)
        );
    }

    #[test]
    fn inserting_joiners_or_flag_indicators_keeps_the_caret_after_the_merged_grapheme() {
        for (text, inserted) in [("👩💻", "\u{200d}"), ("🇸", "🇺")] {
            let mut draft = Draft::from(text);
            draft.left();
            draft.insert(inserted);
            assert_boundary(&draft);
            assert_eq!(draft.cursor(), draft.len());
            let layout = layout(&draft, draft.cursor(), 80);
            assert_eq!(layout.cursor_column, draft.width());
            draft.backspace();
            assert_eq!(draft, "");
        }
    }

    #[test]
    fn inserting_before_a_combining_mark_does_not_jump_to_the_end_of_the_draft() {
        let mut draft = Draft::from("\u{301}x");
        draft.left();
        draft.left();
        draft.insert_char('e');
        assert_eq!(draft, "e\u{301}x");
        assert_boundary(&draft);
        assert_eq!(draft.cursor(), "e\u{301}".len());
        assert_eq!(layout(&draft, draft.cursor(), 80).cursor_column, 1);
        draft.backspace();
        assert_eq!(draft, "x");
    }

    #[test]
    fn deleting_a_separator_normalizes_the_caret_if_adjacent_graphemes_merge() {
        for backspace in [false, true] {
            let mut draft = Draft::from("🇺x🇸");
            draft.left();
            if backspace {
                draft.backspace();
            } else {
                draft.left();
                draft.delete();
            }
            assert_eq!(draft, "🇺🇸");
            assert_boundary(&draft);
            assert_eq!(draft.cursor(), draft.len());
            draft.backspace();
            assert_eq!(draft, "");
        }
    }

    #[test]
    fn layout_tracks_the_cursor_on_wrapped_rows() {
        let input = "abcdefghij";
        let start = layout(input, 0, 4);
        assert_eq!(start.rows, ["abcd", "efgh", "ij"]);
        assert_eq!((start.cursor_row, start.cursor_column), (0, 0));
        let middle = layout(input, 6, 4);
        assert_eq!((middle.cursor_row, middle.cursor_column), (1, 2));
        let end = layout(input, input.len(), 4);
        assert_eq!((end.cursor_row, end.cursor_column), (2, 2));
    }

    #[test]
    fn layout_reserves_an_empty_row_when_the_last_row_is_full() {
        let input = "abcd";
        let layout = layout(input, input.len(), 4);
        assert_eq!(layout.rows, ["abcd", ""]);
        assert_eq!((layout.cursor_row, layout.cursor_column), (1, 0));
    }
}
