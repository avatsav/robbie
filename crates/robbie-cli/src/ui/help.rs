//! Local help stays structured so column styles and wrapping survive resizing.
use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthStr;

use super::{App, MUTED_CYAN, MUTED_TEXT, OutputLine, commands::COMMANDS};

const ACCENT: Color = Color::Rgb(0, 184, 190);
const SHORTCUTS: &[(&str, &str)] = &[
    (
        "enter",
        "submit prompt or complete a partial command (when idle)",
    ),
    ("tab", "complete the selected command suggestion"),
    ("← / →", "move the prompt cursor"),
    ("↑ / ↓", "select a suggestion or move the prompt cursor"),
    ("esc", "close suggestions until the next draft edit"),
    ("ctrl-c", "quit Robbie"),
    ("ctrl-d", "quit when the prompt is empty"),
    ("ctrl-o", "cycle tool output detail"),
    ("ctrl-g", "show or hide the mascot"),
    ("pgup / pgdn", "scroll the transcript"),
    ("end", "follow the latest transcript output"),
];

pub(crate) struct Entry {
    label: String,
    pub(super) description: String,
    alias: bool,
    label_width: usize,
}

impl Entry {
    pub(super) fn rows(&self, width: usize) -> Vec<Line<'static>> {
        let width = width.max(1);
        let label_style = Style::new().fg(if self.alias { MUTED_CYAN } else { ACCENT });
        let description_style = if self.alias {
            Style::new().fg(MUTED_TEXT)
        } else {
            Style::new()
        };
        let description_column = 2 + self.label_width + 2;
        if width >= description_column + 12 {
            return crate::terminal_text::wrap(&self.description, width - description_column)
                .into_iter()
                .enumerate()
                .map(|(index, part)| {
                    let prefix = if index == 0 {
                        format!(
                            "  {}{}",
                            self.label,
                            " ".repeat(self.label_width - self.label.width() + 2)
                        )
                    } else {
                        " ".repeat(description_column)
                    };
                    Line::from(vec![
                        Span::styled(prefix, label_style),
                        Span::styled(part.to_string(), description_style),
                    ])
                })
                .collect();
        }
        // Small terminals stack the label and description instead of squeezing
        // either column to zero or truncating commands/shortcut explanations.
        let mut rows = Vec::new();
        for (text, indent, style) in [
            (
                self.label.as_str(),
                if width >= 4 { 2 } else { 0 },
                label_style,
            ),
            (
                self.description.as_str(),
                if width >= 8 { 4 } else { 0 },
                description_style,
            ),
        ] {
            for part in crate::terminal_text::wrap(text, width - indent) {
                rows.push(Line::styled(format!("{}{part}", " ".repeat(indent)), style));
            }
        }
        rows
    }
}

impl App {
    pub(super) fn show_help(&mut self) {
        // Both sections share a description column, like a terminal help listing.
        let label_width = COMMANDS
            .iter()
            .flat_map(|definition| definition.names())
            .chain(SHORTCUTS.iter().map(|(label, _)| *label))
            .map(UnicodeWidthStr::width)
            .max()
            .unwrap_or(0)
            .max(12);
        self.push("commands", Style::new().bold());
        for definition in COMMANDS {
            self.pending.push(OutputLine::HelpEntry(Entry {
                label: definition.name.into(),
                description: definition.description.into(),
                alias: false,
                label_width,
            }));
            for alias in definition.aliases {
                self.pending.push(OutputLine::HelpEntry(Entry {
                    label: (*alias).into(),
                    description: format!("alias for {}", definition.name),
                    alias: true,
                    label_width,
                }));
            }
        }
        self.blank();
        self.push("shortcuts", Style::new().bold());
        for (label, description) in SHORTCUTS {
            self.pending.push(OutputLine::HelpEntry(Entry {
                label: (*label).into(),
                description: (*description).into(),
                alias: false,
                label_width,
            }));
        }
        self.blank();
        self.push(
            "Commands are case-sensitive. For paths, start with text, e.g. explain /tmp/file.",
            Style::new().fg(MUTED_TEXT),
        );
        self.blank();
    }
}
