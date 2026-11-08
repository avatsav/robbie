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
        Self {
            source,
            cached: RefCell::new(None),
        }
    }

    pub(super) fn rows(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(1);
        let mut cached = self.cached.borrow_mut();
        if cached
            .as_ref()
            .is_none_or(|rendered| rendered.width != width)
        {
            let options =
                Options::new(ConversationStyle).table_width(width.min(u16::MAX as usize) as u16);
            let mut text = tui_markdown::from_str_with_options(&self.source, &options);
            // Markdown entities can introduce controls even after input was
            // sanitized (e.g. &#27;). Never send those to the terminal either.
            for line in &mut text.lines {
                line.style.add_modifier.remove(Modifier::DIM);
                for span in &mut line.spans {
                    if span.content.chars().any(char::is_control) {
                        span.content = span
                            .content
                            .replace('\t', "    ")
                            .chars()
                            .filter(|ch| !ch.is_control())
                            .collect::<String>()
                            .into();
                    }
                    span.style.add_modifier.remove(Modifier::DIM);
                }
            }
            let rows = text
                .lines
                .iter()
                .flat_map(|line| wrap_line(line, width))
                .collect();
            *cached = Some(Rendered { width, rows });
        }
        cached
            .as_ref()
            .map(|rendered| rendered.rows.clone())
            .unwrap_or_default()
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
    fn heading(&self, _level: u8) -> Style {
        Style::new().bold()
    }
    fn heading_marker(&self, _level: u8) -> &str {
        ""
    }
    fn code(&self) -> Style {
        Style::new().cyan()
    }
    fn code_block_fence(&self) -> &str {
        ""
    }
    fn link(&self) -> Style {
        Style::new().cyan().underlined()
    }
    fn blockquote(&self) -> Style {
        muted().italic()
    }
    fn heading_meta(&self) -> Style {
        muted()
    }
    fn html(&self) -> Style {
        muted()
    }
    fn footnote_ref(&self) -> Style {
        muted().italic()
    }
    fn footnote_def(&self) -> Style {
        muted()
    }
    fn image_alt(&self) -> Style {
        muted().italic()
    }
    fn table_border(&self) -> Style {
        muted()
    }
    fn list_marker(&self) -> Style {
        Style::new()
    }
    fn alert_icon(&self, _kind: tui_markdown::AlertKind) -> &str {
        ""
    }
}

fn muted() -> Style {
    Style::new().fg(Color::Rgb(130, 132, 138))
}

fn wrap_line(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    // tui-markdown 0.3 emits each structural quote prefix as a borrowed, raw
    // ">" span, followed by a separate space. Ordinary text spans are owned:
    // inspecting this structure (before flattening) leaves escaped/literal >
    // text and code untouched. Regression tests guard this library boundary.
    let depth = line
        .spans
        .iter()
        .take_while(|span| matches!(span.content, Cow::Borrowed(">")) && span.style == Style::new())
        .count();
    if depth == 0 || line.spans.get(depth).is_none_or(|span| span.content != " ") {
        return wrap_body(line, width);
    }

    // Reserve the gutter before wrapping and repeat it on continuation and blank
    // rows. On tiny viewports, shorten the gutter to leave room for a wide glyph.
    let gutter: String = "│ "
        .repeat(depth)
        .chars()
        .take(width.saturating_sub(2))
        .collect();
    let mut body =
        Line::from(line.spans[depth + 1..].to_vec()).style(line.style.patch(muted().italic()));
    // Quote color wins over child code/link/heading colors. Keep their text
    // modifiers (bold, underline, etc.), but use one muted foreground throughout.
    for span in &mut body.spans {
        span.style = span.style.patch(muted());
    }
    let mut rows = wrap_body(&body, width.saturating_sub(gutter.chars().count()).max(1));
    for row in &mut rows {
        row.spans.insert(
            0,
            Span::styled(gutter.clone(), muted().remove_modifier(Modifier::ITALIC)),
        );
    }
    rows
}

// textwrap chooses word boundaries on the plain text. Map each resulting byte
// range back onto its original spans, so wrapping never discards inline styles.
// No generated indentation/hyphens: every wrapped fragment remains a substring.
fn wrap_body(line: &Line<'_>, width: usize) -> Vec<Line<'static>> {
    let plain = line.to_string();
    let mut cursor = 0;
    crate::terminal_text::wrap(&plain, width)
        .into_iter()
        .map(|part| {
            let start = cursor + plain[cursor..].find(part).unwrap_or(0);
            let end = start + part.len();
            cursor = end;
            let mut offset = 0;
            let spans = line
                .spans
                .iter()
                .filter_map(|span| {
                    let span_start = offset;
                    offset += span.content.len();
                    let from = start.max(span_start);
                    let to = end.min(offset);
                    (from < to).then(|| {
                        Span::styled(
                            span.content[from - span_start..to - span_start].to_string(),
                            span.style,
                        )
                    })
                })
                .collect::<Vec<_>>();
            Line::from(spans).style(line.style)
        })
        .collect()
}

#[cfg(test)]
mod tests;
