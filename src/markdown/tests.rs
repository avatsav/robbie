use super::*;

fn render(source: &str, width: usize) -> Vec<Line<'static>> {
    Markdown::new(source.to_string()).rows(width)
}

fn plain(rows: &[Line<'_>]) -> String {
    rows.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}

fn effective_style(rows: &[Line<'_>], word: &str) -> Style {
    for line in rows {
        for span in &line.spans {
            if span.content.contains(word) {
                return line.style.patch(span.style);
            }
        }
    }
    panic!("missing {word:?} in {}", plain(rows));
}

mod blocks;

mod rendering;
