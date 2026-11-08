//! Terminal-cell wrapping that preserves extended grapheme clusters and source
//! slices. Word separation and optimal-fit layout stay delegated to textwrap.
use textwrap::{
    WordSeparator,
    core::Fragment,
    wrap_algorithms::{Penalties, wrap_first_fit, wrap_optimal_fit},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
struct Part<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    trailing_end: usize,
}

impl Fragment for Part<'_> {
    fn width(&self) -> f64 {
        self.text[self.start..self.end].width() as f64
    }

    fn whitespace_width(&self) -> f64 {
        self.text[self.end..self.trailing_end].width() as f64
    }

    fn penalty_width(&self) -> f64 {
        0.0
    }
}

pub(crate) fn wrap(text: &str, width: usize) -> Vec<&str> {
    text.split('\n')
        .flat_map(|line| wrap_line(line, width.max(1)))
        .collect()
}

fn wrap_line(text: &str, width: usize) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut offset = 0;
    for word in WordSeparator::new().find_words(text) {
        let mut start = offset;
        let end = offset + word.word.len();
        // Unlike textwrap's scalar-based break_words, every forced split is an
        // extended-grapheme boundary. An indivisible wide glyph may overflow a
        // one-cell viewport; never corrupt its source to make it fit.
        for (index, grapheme) in word.word.grapheme_indices(true) {
            let boundary = offset + index;
            if boundary > start && text[start..boundary + grapheme.len()].width() > width {
                parts.push(Part {
                    text,
                    start,
                    end: boundary,
                    trailing_end: boundary,
                });
                start = boundary;
            }
        }
        offset = end + word.whitespace.len();
        parts.push(Part {
            text,
            start,
            end,
            trailing_end: offset,
        });
    }
    if parts.is_empty() {
        return vec![text];
    }
    let widths = [width as f64];
    let rows = wrap_optimal_fit(&parts, &widths, &Penalties::new())
        .unwrap_or_else(|_| wrap_first_fit(&parts, &widths));
    rows.into_iter()
        .filter_map(|row| Some(&text[row.first()?.start..row.last()?.end]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_whole_graphemes_using_their_display_widths() {
        for glyph in ["👩‍💻", "#\u{fe0f}", "1\u{fe0f}\u{20e3}", "👍🏽", "e\u{301}"] {
            let text = glyph.repeat(3);
            let rows = wrap(&text, glyph.width());
            assert_eq!(rows, vec![glyph; 3], "{glyph:?}");
            assert_eq!(rows.concat(), text);
        }
    }

    #[test]
    fn keeps_word_breaks_indentation_empty_lines_and_oversized_glyphs() {
        assert_eq!(wrap("alpha beta gamma", 10), vec!["alpha beta", "gamma"]);
        assert_eq!(
            wrap("    indented\n\nend", 80),
            vec!["    indented", "", "end"]
        );
        assert_eq!(wrap("猫", 1), vec!["猫"]);
        assert_eq!(wrap("👩‍💻", 1), vec!["👩‍💻"]);
    }
}
