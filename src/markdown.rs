use std::{borrow::Cow, cell::RefCell};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use tui_markdown::{Options, StyleSheet};

// Keep source, rather than rendered terminal cells: full-screen history can
// reflow on resize. Inline mode drops this value after inserting its rows.
pub(super) struct Markdown {
    source: String,
    cached: RefCell<Option<Rendered>>,
}

struct Rendered {
    width: usize,
    rows: Vec<Line<'static>>,
}

impl Markdown {
    // The UI sanitizes terminal controls before constructing Markdown.
    pub(super) fn new(source: String) -> Self {
        Self { source, cached: RefCell::new(None) }
    }

    pub(super) fn rows(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(1);
        let mut cached = self.cached.borrow_mut();
        if cached.as_ref().is_none_or(|rendered| rendered.width != width) {
            let options = Options::new(ConversationStyle)
                .table_width(width.min(u16::MAX as usize) as u16);
            let mut text = tui_markdown::from_str_with_options(&self.source, &options);
            // Markdown entities can introduce controls even after input was
            // sanitized (e.g. &#27;). Never send those to the terminal either.
            for line in &mut text.lines {
                line.style.add_modifier.remove(Modifier::DIM);
                for span in &mut line.spans {
                    if span.content.chars().any(char::is_control) {
                        span.content = span.content.replace('\t', "    ").chars()
                            .filter(|ch| !ch.is_control()).collect::<String>().into();
                    }
                    span.style.add_modifier.remove(Modifier::DIM);
                }
            }
            let rows = text.lines.iter().flat_map(|line| wrap_line(line, width)).collect();
            *cached = Some(Rendered { width, rows });
        }
        cached.as_ref().map(|rendered| rendered.rows.clone()).unwrap_or_default()
    }

    #[cfg(test)]
    pub(super) fn source(&self) -> &str {
        &self.source
    }
}

// Use the existing borderless conversation hierarchy: normal body text, subtle
// accents, no opaque surfaces or terminal-dependent DIM. Code is deliberately
// not syntax-highlighted yet, keeping the dependency and visual footprint small.
#[derive(Clone)]
struct ConversationStyle;

impl StyleSheet for ConversationStyle {
    fn heading(&self, _level: u8) -> Style { Style::new().bold() }
    fn heading_marker(&self, _level: u8) -> &str { "" }
    fn code(&self) -> Style { Style::new().cyan() }
    fn code_block_fence(&self) -> &str { "" }
    fn link(&self) -> Style { Style::new().cyan().underlined() }
    fn blockquote(&self) -> Style { muted().italic() }
    fn heading_meta(&self) -> Style { muted() }
    fn html(&self) -> Style { muted() }
    fn footnote_ref(&self) -> Style { muted().italic() }
    fn footnote_def(&self) -> Style { muted() }
    fn image_alt(&self) -> Style { muted().italic() }
    fn table_border(&self) -> Style { muted() }
    fn list_marker(&self) -> Style { Style::new() }
    fn alert_icon(&self, _kind: tui_markdown::AlertKind) -> &str { "" }
}

fn muted() -> Style {
    Style::new().fg(Color::Rgb(130, 132, 138))
}

fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    // tui-markdown 0.3 emits each structural quote prefix as a borrowed, raw
    // ">" span, followed by a separate space. Ordinary text spans are owned:
    // inspecting this structure (before flattening) leaves escaped/literal >
    // text and code untouched. Regression tests guard this library boundary.
    let depth = line.spans.iter().take_while(|span| {
        matches!(span.content, Cow::Borrowed(">")) && span.style == Style::new()
    }).count();
    if depth == 0 || line.spans.get(depth).is_none_or(|span| span.content != " ") {
        return wrap_body(line, width);
    }

    // Reserve the gutter before wrapping and repeat it on continuation and blank
    // rows. On tiny viewports, shorten the gutter to leave room for a wide glyph.
    let gutter: String = "│ ".repeat(depth).chars().take(width.saturating_sub(2)).collect();
    let mut body = Line::from(line.spans[depth + 1..].to_vec())
        .style(line.style.patch(muted().italic()));
    // Quote color wins over child code/link/heading colors. Keep their text
    // modifiers (bold, underline, etc.), but use one muted foreground throughout.
    for span in &mut body.spans {
        span.style = span.style.patch(muted());
    }
    let mut rows = wrap_body(&body, width.saturating_sub(gutter.chars().count()).max(1));
    for row in &mut rows {
        row.spans.insert(0, Span::styled(gutter.clone(), muted().remove_modifier(Modifier::ITALIC)));
    }
    rows
}

// textwrap chooses word boundaries on the plain text. Map each resulting byte
// range back onto its original spans, so wrapping never discards inline styles.
// No generated indentation/hyphens: every wrapped fragment remains a substring.
fn wrap_body(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let plain = line.to_string();
    let options = textwrap::Options::new(width).word_splitter(textwrap::WordSplitter::NoHyphenation);
    let mut cursor = 0;
    textwrap::wrap(&plain, options).into_iter().map(|part| {
        let start = cursor + plain[cursor..].find(part.as_ref()).unwrap_or(0);
        let end = start + part.len();
        cursor = end;
        let mut offset = 0;
        let spans = line.spans.iter().filter_map(|span| {
            let span_start = offset;
            offset += span.content.len();
            let from = start.max(span_start);
            let to = end.min(offset);
            (from < to).then(|| Span::styled(
                span.content[from - span_start..to - span_start].to_string(), span.style,
            ))
        }).collect::<Vec<_>>();
        Line::from(spans).style(line.style)
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(source: &str, width: usize) -> Vec<Line<'static>> {
        Markdown::new(source.to_string()).rows(width)
    }

    fn plain(rows: &[Line<'_>]) -> String {
        rows.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
    }

    fn effective_style(rows: &[Line<'_>], word: &str) -> Style {
        for line in rows {
            for span in &line.spans {
                if span.content.contains(word) { return line.style.patch(span.style) }
            }
        }
        panic!("missing {word:?} in {}", plain(rows));
    }

    #[test]
    fn headings_and_nested_inline_styles_do_not_leak() {
        let rows = render("# Heading\n\nNormal **bold *nested*** and *italic*, ~~gone~~, `code`, normal.", 100);
        assert!(plain(&rows).starts_with("Heading\n\nNormal bold nested"));
        assert!(effective_style(&rows, "Heading").add_modifier.contains(Modifier::BOLD));
        assert!(effective_style(&rows, "bold").add_modifier.contains(Modifier::BOLD));
        assert!(effective_style(&rows, "nested").add_modifier.contains(Modifier::BOLD | Modifier::ITALIC));
        assert!(effective_style(&rows, "italic").add_modifier.contains(Modifier::ITALIC));
        assert!(effective_style(&rows, "gone").add_modifier.contains(Modifier::CROSSED_OUT));
        assert_eq!(effective_style(&rows, "code").fg, Some(Color::Cyan));
        assert_eq!(effective_style(&rows, "normal."), Style::new());
        for line in rows {
            assert!(line.style.bg.is_none());
            for span in line.spans {
                let style = line.style.patch(span.style);
                assert!(style.bg.is_none());
                assert!(!style.add_modifier.contains(Modifier::DIM));
            }
        }
    }

    #[test]
    fn links_remain_readable_without_terminal_hyperlinks() {
        let rows = render("See [Rust](https://www.rust-lang.org) and <https://example.com>.", 100);
        let text = plain(&rows);
        assert!(text.contains("Rust"));
        assert!(text.contains("https://www.rust-lang.org"));
        assert!(text.contains("https://example.com"));
        assert!(effective_style(&rows, "Rust").add_modifier.contains(Modifier::UNDERLINED));
    }

    #[test]
    fn lists_quotes_and_tasks_have_structure() {
        let text = plain(&render("- first\n  - nested\n- second\n\n3. third\n4. fourth\n\n> quoted **words**\n\n- [x] done\n- [ ] next", 80));
        for word in ["first", "nested", "second", "3.", "4.", "quoted words", "done", "next"] {
            assert!(text.contains(word), "missing {word:?}: {text}");
        }
        let first = text.lines().find(|line| line.contains("first")).unwrap();
        let nested = text.lines().find(|line| line.contains("nested")).unwrap();
        assert!(nested.len() - nested.trim_start().len() > first.len() - first.trim_start().len());
    }

    #[test]
    fn blockquotes_use_a_muted_italic_gutter_on_every_wrapped_row() {
        let rows = render("> alpha beta gamma delta epsilon", 14);
        assert!(rows.len() > 1);
        for row in &rows {
            assert!(row.to_string().starts_with("│ "), "missing quote gutter: {row:?}");
            assert!(row.width() <= 14);
        }
        let style = effective_style(&rows, "alpha");
        assert_eq!(style.fg, muted().fg);
        assert!(style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn blockquotes_keep_blank_lines_nesting_and_inline_styles_without_leaking() {
        let rows = render("> first **bold**\n>\n> second\n>\n>> nested\n\noutside", 80);
        assert_eq!(plain(&rows), "│ first bold\n│ \n│ second\n│ \n│ │ nested\n\noutside");
        let bold = effective_style(&rows, "bold");
        assert!(bold.add_modifier.contains(Modifier::BOLD | Modifier::ITALIC));
        assert_eq!(bold.fg, muted().fg);
        assert_eq!(effective_style(&rows, "outside"), Style::new());
        let quote = Markdown::new("> first paragraph wraps over several rows\n>\n>> nested paragraph also wraps over several rows".into());
        let wide = quote.rows(80);
        let narrow = quote.rows(20);
        assert!(narrow.len() > wide.len());
        assert!(narrow.iter().all(|row| row.to_string().starts_with("│ ") && row.width() <= 20));
        assert!(narrow.iter().filter(|row| row.to_string().starts_with("│ │ ")).count() > 1);
        assert_eq!(quote.rows(80), wide);
    }

    #[test]
    fn literal_greater_than_symbols_are_not_quote_gutters() {
        for source in [
            "\\> **bold**", "# \\> **bold**", "\\>\nsoft break", "&#62;\nsoft break",
            "`>`\nsoft break", "```\n> literal\n>> literal nesting\n```",
        ] {
            let text = plain(&render(source, 80));
            assert!(text.contains('>'), "lost literal marker: {source:?} -> {text}");
            assert!(!text.contains('│'), "false quote: {source:?} -> {text}");
        }
        let text = plain(&render("> \\> literal\n>\n> ```\n> > literal code\n> ```", 80));
        assert!(text.contains("│ > literal"), "{text}");
        assert!(text.contains("│ > literal code"), "{text}");
    }

    #[test]
    fn quote_gutters_render_safely_in_tiny_viewports() {
        for width in 0..8 {
            let rows = render(">> **猫猫 xyz**", width);
            assert!(rows.iter().all(|row| row.width() <= width.max(2)));
            let body: String = plain(&rows).chars().filter(|ch| !ch.is_whitespace() && *ch != '│').collect();
            assert_eq!(body, "猫猫xyz");
        }
    }

    #[test]
    fn code_blocks_hide_presentation_fences_but_keep_literal_backticks() {
        for source in ["```rust\nlet x = 1;\n```", "~~~rust\nlet x = 1;\n~~~", "```rust\nlet x = 1;"] {
            assert_eq!(plain(&render(source, 80)), "let x = 1;");
        }
        assert_eq!(plain(&render("````text\n```\n````", 80)), "```");
    }

    #[test]
    fn blockquotes_keep_all_content_muted_even_when_children_set_colors() {
        let rows = render("> # Heading\n>\n> **bold** `code` [link](https://example.com)\n>\n> ```rust\n> let x = 1;\n> ```\n\noutside `normal code`", 80);
        for word in ["Heading", "bold", "code", "link", "let x"] {
            assert_eq!(effective_style(&rows[..rows.len() - 1], word).fg, muted().fg, "{word}");
        }
        assert!(effective_style(&rows, "bold").add_modifier.contains(Modifier::BOLD | Modifier::ITALIC));
        assert!(effective_style(&rows, "link").add_modifier.contains(Modifier::UNDERLINED));
        assert_eq!(effective_style(&rows, "normal code").fg, Some(Color::Cyan));
    }

    #[test]
    fn code_keeps_indentation_blank_lines_and_literal_markdown() {
        let rows = render("```rust\nfn main() {\n    **not bold**\n\n    println!(\"猫\");\n}\n```", 80);
        let text = plain(&rows);
        assert!(text.contains("    **not bold**\n\n    println!(\"猫\");"), "{text}");
        assert!(!effective_style(&rows, "not bold").add_modifier.contains(Modifier::BOLD));
        let unfinished = plain(&render("```unknown\n    let x = `literal`;", 80));
        assert!(unfinished.contains("    let x = `literal`;"));
    }

    #[test]
    fn wrapping_retains_styles_across_span_and_unicode_boundaries() {
        let rows = render("**alpha beta gamma** plain 猫猫 end", 10);
        assert!(rows.len() > 2);
        assert!(rows.iter().all(|line| line.width() <= 10));
        for word in ["alpha", "beta", "gamma"] {
            assert!(effective_style(&rows, word).add_modifier.contains(Modifier::BOLD));
        }
        assert_eq!(effective_style(&rows, "plain"), Style::new());
        assert!(plain(&rows).contains("猫猫"));
        // Repeated words must map to their own spans, not the first occurrence.
        let rows = render("**same** same **same**", 4);
        assert_eq!(rows.len(), 3);
        assert!(effective_style(&rows[..1], "same").add_modifier.contains(Modifier::BOLD));
        assert_eq!(effective_style(&rows[1..2], "same"), Style::new());
        assert!(effective_style(&rows[2..], "same").add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn resize_reflows_text_and_tables_and_handles_tiny_widths() {
        let markdown = Markdown::new("**alpha beta gamma delta**\n\n| Name | Value |\n| --- | --- |\n| long name | long value |".into());
        let wide = markdown.rows(80);
        let narrow = markdown.rows(20);
        assert!(narrow.len() > wide.len());
        assert!(narrow.iter().all(|line| line.width() <= 20));
        assert_eq!(markdown.rows(80), wide);
        for width in 0..4 {
            let rows = render("**abc** `猫`\n\n```\n    xyz\n```", width);
            assert!(!rows.is_empty());
            assert!(plain(&rows).contains('猫'));
        }
    }

    #[test]
    fn decoded_entities_cannot_inject_terminal_controls() {
        let rows = render("before &#27;[31m after &Tab; end &NewLine; text", 80);
        assert!(plain(&rows).chars().all(|ch| !ch.is_control()));
        assert!(plain(&rows).contains("before"));
        assert!(plain(&rows).contains("text"));
    }

    #[test]
    fn paragraph_breaks_escapes_and_html_are_literal_where_expected() {
        let text = plain(&render("one\nsoft break\n\ntwo  \nhard break\n\n\\*literal\\* <b>html</b>", 80));
        assert!(text.contains("one soft break\n\ntwo\nhard break"), "{text}");
        assert!(text.contains("*literal* <b>html</b>"));
        assert!(render("", 80).is_empty());
    }
}

