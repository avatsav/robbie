use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    row: usize,
    column: usize,
}

#[derive(Default)]
pub(super) struct Selection {
    anchor: Option<Position>,
    focus: Option<Position>,
    dragged: bool,
}

impl Selection {
    pub(super) fn is_active(&self) -> bool {
        self.anchor.is_some()
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(super) fn begin(&mut self, row: usize, column: usize) {
        let position = Position { row, column };
        self.anchor = Some(position);
        self.focus = Some(position);
        self.dragged = false;
    }

    pub(super) fn extend(&mut self, row: usize, column: usize) {
        if let Some(anchor) = self.anchor {
            let position = Position { row, column };
            self.focus = Some(position);
            self.dragged |= anchor != position;
        }
    }

    fn bounds(&self, row: usize) -> Option<(usize, usize)> {
        if !self.dragged {
            return None;
        }
        let anchor = self.anchor?;
        let focus = self.focus?;
        let (start, end) = (anchor.min(focus), anchor.max(focus));
        if row < start.row || row > end.row {
            return None;
        }
        Some((
            if row == start.row { start.column } else { 0 },
            if row == end.row {
                end.column
            } else {
                usize::MAX
            },
        ))
    }

    // Positions are display cells, not bytes/chars. Keep grapheme clusters whole
    // when either half of a wide glyph or a combining sequence is selected.
    pub(super) fn highlight(
        &self,
        buffer: &mut Buffer,
        area: Rect,
        start: usize,
        rows: &[Line<'_>],
    ) {
        let style = Style::new().fg(Color::White).bg(Color::Rgb(50, 75, 110));
        for (offset, line) in rows
            .iter()
            .skip(start)
            .take(area.height as usize)
            .enumerate()
        {
            let Some((left, right)) = self.bounds(start + offset) else {
                continue;
            };
            let mut column = 0;
            for grapheme in line.to_string().graphemes(true) {
                let width = grapheme.width();
                if column <= right && column + width > left {
                    for x in column..(column + width).min(area.width as usize) {
                        buffer[(area.x + x as u16, area.y + offset as u16)].set_style(style);
                    }
                }
                column += width;
                if column >= area.width as usize {
                    break;
                }
            }
        }
    }

    pub(super) fn finish(&mut self, rows: &[Line<'_>]) -> Option<String> {
        let mut selected = Vec::new();
        for (row, line) in rows.iter().enumerate() {
            let Some((left, right)) = self.bounds(row) else {
                continue;
            };
            let mut column = 0;
            let mut text = String::new();
            for grapheme in line.to_string().graphemes(true) {
                let width = grapheme.width();
                if column <= right && column + width > left {
                    text.push_str(grapheme);
                }
                column += width;
            }
            selected.push(text);
        }
        self.clear();
        let text = selected.join("\n");
        (!text.is_empty()).then_some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_without_drag_never_copies() {
        let mut selection = Selection::default();
        selection.begin(0, 1);
        selection.extend(0, 1);
        assert_eq!(selection.finish(&[Line::from("hello")]), None);
        assert!(!selection.is_active());
    }

    #[test]
    fn forward_and_reverse_multiline_drags_copy_once_and_clear() {
        let rows = [Line::from("alpha"), Line::from(""), Line::from("bravo")];
        for (anchor, focus) in [((0, 2), (2, 1)), ((2, 1), (0, 2))] {
            let mut selection = Selection::default();
            selection.begin(anchor.0, anchor.1);
            selection.extend(focus.0, focus.1);
            assert_eq!(selection.finish(&rows).as_deref(), Some("pha\n\nbr"));
            assert!(!selection.is_active());
            assert_eq!(selection.finish(&rows), None);
        }
    }

    #[test]
    fn unicode_is_selected_by_cells_without_splitting_graphemes() {
        for text in ["a猫e\u{301}z", "a👩‍💻e\u{301}z"] {
            let mut selection = Selection::default();
            selection.begin(0, 2); // Inside the two-cell glyph.
            selection.extend(0, 3); // The e + combining accent occupies one cell.
            assert_eq!(
                selection.finish(&[Line::from(text)]),
                Some(text.trim_start_matches('a').trim_end_matches('z').into())
            );
        }
    }

    #[test]
    fn blank_or_cancelled_drags_do_not_copy() {
        let mut selection = Selection::default();
        selection.begin(0, 10);
        selection.extend(0, 15);
        assert_eq!(selection.finish(&[Line::from("short")]), None);
        selection.begin(0, 0);
        selection.extend(0, 2);
        selection.clear();
        assert_eq!(selection.finish(&[Line::from("hello")]), None);
    }

    #[test]
    fn selected_source_whitespace_is_preserved() {
        let mut selection = Selection::default();
        selection.begin(0, 0);
        selection.extend(0, 2);
        assert_eq!(
            selection.finish(&[Line::from("   code")]).as_deref(),
            Some("   ")
        );
    }

    #[test]
    fn highlighting_uses_transcript_rows_and_preserves_text() {
        let mut selection = Selection::default();
        selection.begin(2, 2);
        selection.extend(2, 3);
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 4));
        buffer.set_string(0, 1, "a猫e\u{301}z", Style::new());
        let original = buffer.clone();
        let rows = [
            Line::from("old"),
            Line::from("older"),
            Line::from("a猫e\u{301}z"),
        ];
        selection.highlight(&mut buffer, Rect::new(0, 1, 10, 2), 2, &rows);
        for x in [1, 2, 3] {
            assert_eq!(buffer[(x, 1)].bg, Color::Rgb(50, 75, 110));
        }
        assert_eq!(buffer[(0, 1)].bg, Color::Reset);
        assert_eq!(buffer[(4, 1)].bg, Color::Reset);
        assert_eq!(buffer[(2, 0)].bg, Color::Reset);
        for (before, after) in original.content.iter().zip(&buffer.content) {
            assert_eq!(before.symbol(), after.symbol());
        }
    }
}
