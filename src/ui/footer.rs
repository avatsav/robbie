use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Stylize},
    text::Line,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{App, MUTED_CYAN, MUTED_RED, MUTED_TEXT, single_line, workspace::Workspace};

const IDLE_RULE: Color = Color::Rgb(166, 118, 165);
const MAX_INPUT_ROWS: usize = 4;

impl App {
    pub(crate) fn set_workspace(&mut self, workspace: Workspace) {
        self.workspace = workspace;
    }

    pub(crate) fn live_height(&self, width: u16) -> u16 {
        draft_rows(&self.input, width as usize)
            .len()
            .min(MAX_INPUT_ROWS) as u16
            + 4
    }

    fn activity_title(&self) -> (String, bool) {
        if !self.working {
            return (String::new(), false);
        }
        let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'][self.tick % 10];
        (format!("{spinner} Working…"), true)
    }

    pub(crate) fn view_live(&self, frame: &mut Frame, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let (title, busy) = self.activity_title();
        let color = if self.failed {
            MUTED_RED
        } else if busy {
            MUTED_CYAN
        } else {
            IDLE_RULE
        };
        let (input_y, input_height, footer_y) = if area.height >= 3 {
            let metadata = if area.height >= 5 { 2 } else { 0 };
            let input_height = area.height - metadata - 2;
            frame.render_widget(
                rule(&title, area.width, color),
                Rect::new(area.x, area.y, area.width, 1),
            );
            let bottom = area.y + input_height + 1;
            frame.render_widget(
                rule("", area.width, color),
                Rect::new(area.x, bottom, area.width, 1),
            );
            (
                area.y + 1,
                input_height,
                (metadata > 0).then_some(bottom + 1),
            )
        } else {
            if area.height == 2 {
                frame.render_widget(
                    Line::from(single_line(&title, area.width as usize)).fg(color),
                    Rect::new(area.x, area.y, area.width, 1),
                );
            }
            (area.bottom() - 1, 1, None)
        };
        let rows = draft_rows(&self.input, area.width as usize);
        let visible = &rows[rows.len().saturating_sub(input_height as usize)..];
        for (offset, text) in visible.iter().enumerate() {
            frame.render_widget(
                Line::from(text.as_str()),
                Rect::new(area.x, input_y + offset as u16, area.width, 1),
            );
        }
        let cursor_column = visible.last().map(|row| row.width()).unwrap_or(0) as u16;
        frame.set_cursor_position((
            area.x + cursor_column.min(area.width - 1),
            input_y + visible.len().saturating_sub(1) as u16,
        ));
        if let Some(y) = footer_y {
            let branch = self
                .workspace
                .branch
                .as_ref()
                .map(|branch| format!(" ({branch})"))
                .unwrap_or_default();
            let suffix = single_line(&branch, (area.width as usize / 2).min(branch.width()));
            let directory = if self.workspace.directory.is_empty() {
                "workspace"
            } else {
                &self.workspace.directory
            };
            let path = tail_ellipsis(directory, area.width as usize - suffix.width());
            frame.render_widget(
                Line::from(format!("{path}{suffix}")).fg(MUTED_TEXT),
                Rect::new(area.x, y, area.width, 1),
            );
            let model = single_line(&self.model, area.width as usize);
            if !model.is_empty() {
                frame.render_widget(
                    Line::from(model).fg(MUTED_TEXT),
                    Rect::new(area.x, y + 1, area.width, 1),
                );
            }
        }
    }
}

fn rule(title: &str, width: u16, color: Color) -> Line<'static> {
    let title = if title.is_empty() || width < 5 {
        String::new()
    } else {
        format!(
            "─ {} ",
            single_line(title, width.saturating_sub(4) as usize)
        )
    };
    let remaining = width as usize - title.width();
    Line::from(format!("{title}{}", "─".repeat(remaining))).fg(color)
}

// Preserve the draft verbatim, wrapping by cells without splitting graphemes.
// A full final row gets a new empty row so the cursor always has a real cell.
fn draft_rows(input: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut rows = vec![String::new()];
    let mut column = 0;
    for grapheme in input.graphemes(true) {
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
        let last = rows.len() - 1;
        rows[last].push_str(glyph);
        column += cells;
        if column == width {
            rows.push(String::new());
            column = 0;
        }
    }
    rows
}

fn tail_ellipsis(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let mut used = 1;
    let mut tail = Vec::new();
    for grapheme in text.graphemes(true).rev() {
        if used + grapheme.width() > width {
            break;
        }
        used += grapheme.width();
        tail.push(grapheme);
    }
    format!("…{}", tail.into_iter().rev().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{AgentEvent, EventKind};
    use ratatui::{Terminal, backend::TestBackend};
    use serde_json::json;

    fn screen(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| app.view_live(frame, frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn footer_shows_context_and_model_without_mode_hint_or_metrics() {
        let app = App {
            model: "test/model".into(),
            workspace: Workspace {
                directory: "~/project".into(),
                branch: Some("main".into()),
            },
            ..App::default()
        };
        let text = screen(&app, 80, 5);
        let rows: Vec<_> = text
            .chars()
            .collect::<Vec<_>>()
            .chunks(80)
            .map(|row| row.iter().collect::<String>())
            .collect();
        assert_eq!(rows[0], "─".repeat(80));
        assert_eq!(rows[2], "─".repeat(80));
        assert!(rows[3].starts_with("~/project (main)"));
        assert_eq!(rows[4].trim_end(), "test/model");
        assert!(!text.contains("Ctrl+O"));
        for unwanted in ["pi-lens", "tokens", "context", "LSP"] {
            assert!(!text.contains(unwanted));
        }
    }

    #[test]
    fn shell_execution_keeps_only_the_working_indicator_visible() {
        let mut app = App::default();
        app.activity(AgentEvent {
            source: "main",
            kind: EventKind::ToolStarted {
                name: "bash".into(),
                input: json!({"command":"sleep 20"}),
            },
        });
        let first = screen(&app, 100, 5);
        assert!(first.contains("Working…"));
        for unwanted in ["Running", "shell", "sleep", "20", "0s"] {
            assert!(!first.contains(unwanted));
        }
        app.advance_tick();
        assert_ne!(screen(&app, 100, 5), first);
    }

    #[test]
    fn activity_label_stays_unchanged_between_model_and_parent_or_child_tools() {
        let mut app = App::default();
        app.submit();
        let working = screen(&app, 100, 5);
        for source in ["main", "subagent"] {
            for kind in [
                EventKind::ModelStarted,
                EventKind::ToolStarted {
                    name: "bash".into(),
                    input: json!({"command":"sleep 20"}),
                },
                EventKind::ToolFinished {
                    name: "bash".into(),
                    output: "[exit 0]".into(),
                    is_error: false,
                },
                EventKind::AssistantText("done".into()),
            ] {
                app.activity(AgentEvent { source, kind });
                assert_eq!(screen(&app, 100, 5), working);
            }
        }
    }

    #[test]
    fn completion_and_failure_leave_the_rule_unlabeled_without_spinning() {
        for result in [
            Ok("done".into()),
            Ok(String::new()),
            Ok("  ".into()),
            Err(anyhow::anyhow!("request failed")),
        ] {
            let mut app = App::default();
            app.submit();
            app.finish_turn(result);
            let text = screen(&app, 100, 5);
            assert!(text.starts_with(&"─".repeat(100)));
            app.advance_tick();
            assert_eq!(screen(&app, 100, 5), text);
        }
    }

    #[test]
    fn draft_wraps_graphemes_and_reserves_cursor_at_exact_boundary() {
        assert_eq!(draft_rows("abcd", 4), ["abcd", ""]);
        assert_eq!(draft_rows("a猫e\u{301}", 3), ["a猫", "e\u{301}"]);
        assert_eq!(draft_rows("👩‍💻a", 1), ["�", "a", ""]);
        for width in 0..12 {
            assert!(
                draft_rows("abc猫👩‍💻e\u{301}", width)
                    .iter()
                    .all(|row| row.width() <= width)
            );
        }
    }

    #[test]
    fn long_input_keeps_cursor_inside_small_or_offset_viewports() {
        let app = App {
            input: "猫 abc e\u{301} ".repeat(100),
            ..App::default()
        };
        assert_eq!(app.live_height(20), 8);
        for width in [0, 1, 2, 10, 80] {
            for height in [0, 1, 2, 3, 4, 5, 8] {
                let mut terminal = Terminal::new(TestBackend::new(width + 4, height + 4)).unwrap();
                terminal
                    .draw(|frame| app.view_live(frame, Rect::new(2, 2, width, height)))
                    .unwrap();
                if width > 0 && height > 0 {
                    let cursor = terminal.get_cursor_position().unwrap();
                    assert!(Rect::new(2, 2, width, height).contains(cursor));
                }
            }
        }
    }
}
