use super::*;
use ratatui::style::Modifier;

fn app_with_help() -> App {
    let mut app = App::default();
    app.handle_event(Event::Paste("/help".into()));
    assert!(app.submit().is_none());
    assert!(!app.working);
    app
}

fn rows(app: &App, width: usize) -> Vec<Line<'static>> {
    app.pending
        .iter()
        .flat_map(|line| line.rows(width))
        .collect()
}

#[test]
fn help_has_lowercase_headings_and_one_shared_description_column() {
    let app = app_with_help();
    let rows = rows(&app, 100);
    assert_eq!(rows[2].to_string(), "commands");
    assert_eq!(rows[10].to_string(), "shortcuts");
    for heading in [&rows[2], &rows[10]] {
        assert!(heading.style.add_modifier.contains(Modifier::BOLD));
    }
    for (label, description) in [
        ("/help", "show this help"),
        ("/compact", "compact context"),
        ("/transcript", "write sensitive model context"),
        ("/dump", "alias for /transcript"),
        ("/exit", "exit Robbie"),
        ("/quit", "alias for /exit"),
        ("enter", "submit prompt"),
        ("ctrl-c", "quit Robbie"),
        ("pgup / pgdn", "scroll the transcript"),
    ] {
        let row = rows
            .iter()
            .map(ToString::to_string)
            .find(|row| row.trim_start().starts_with(&format!("{label} ")))
            .unwrap();
        assert_eq!(row.find(label), Some(2));
        assert_eq!(row.find(description), Some(16), "{row:?}");
    }
}

#[test]
fn command_and_shortcut_labels_are_cyan_and_the_alias_row_is_muted() {
    let mut app = app_with_help();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    render_output(&mut terminal, &mut app).unwrap();
    let buffer = terminal.backend().buffer();
    assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(2, 3)].fg, Color::Rgb(0, 184, 190));
    assert_eq!(buffer[(16, 3)].fg, Color::Reset);
    assert_eq!(buffer[(2, 6)].fg, MUTED_CYAN);
    assert_eq!(buffer[(16, 6)].fg, MUTED_TEXT);
    assert_eq!(buffer[(2, 11)].fg, Color::Rgb(0, 184, 190));
    assert!(
        !buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::DIM))
    );
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("no model request"));
    assert!(text.contains("billed summary request"));
    assert!(!text.contains("Working…"));
}

#[test]
fn every_command_and_alias_is_described_once_without_duplicate_command_rows() {
    let app = app_with_help();
    let rows = rows(&app, 120)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    for definition in crate::ui::commands::COMMANDS {
        for name in definition.names() {
            let matching = rows
                .iter()
                .filter(|row| row.trim_start().starts_with(&format!("{name} ")))
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 1, "{name}");
            let description = if name == definition.name {
                definition.description.to_owned()
            } else {
                format!("alias for {}", definition.name)
            };
            assert!(matching[0].contains(&description));
        }
    }
    assert_eq!(
        rows.iter()
            .filter(|row| row.contains("exit Robbie"))
            .count(),
        1
    );
}

#[test]
fn entries_wrap_or_stack_without_losing_text_at_tiny_widths() {
    let app = app_with_help();
    let compact_text = |rows: Vec<Line<'static>>| {
        rows.iter()
            .map(ToString::to_string)
            .collect::<String>()
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
    };
    for entry in app
        .pending
        .iter()
        .filter(|line| matches!(line, OutputLine::HelpEntry(_)))
    {
        let expected = compact_text(entry.rows(120));
        for width in [0, 1, 2, 8, 12, 27, 28, 50, 100] {
            let rows = entry.rows(width);
            assert!(rows.iter().all(|row| row.width() <= width.max(2)));
            assert_eq!(compact_text(rows), expected, "width {width}");
        }
    }
}

#[test]
fn wrapped_descriptions_align_under_the_description_column_and_reflow_back() {
    let app = app_with_help();
    let entry = app
        .pending
        .iter()
        .find(|line| {
            matches!(line, OutputLine::HelpEntry(_)) && line.text().starts_with("compact context")
        })
        .unwrap();
    let wide = entry.rows(100);
    let narrow = entry.rows(32);
    assert!(narrow.len() > 1);
    assert!(
        narrow
            .iter()
            .skip(1)
            .all(|row| row.to_string().starts_with(&" ".repeat(16)))
    );
    assert_eq!(entry.rows(100), wide);
}

#[test]
fn help_lists_only_implemented_commands_and_shortcuts() {
    let app = app_with_help();
    let text = rows(&app, 120)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    for shortcut in [
        "enter",
        "tab",
        "← / →",
        "↑ / ↓",
        "esc",
        "ctrl-c",
        "ctrl-d",
        "ctrl-o",
        "ctrl-g",
        "pgup / pgdn",
        "end",
    ] {
        assert!(
            text.lines()
                .any(|row| row.trim_start().starts_with(&format!("{shortcut} "))),
            "{shortcut}"
        );
    }
    for unsupported in [
        "/new",
        "/resume",
        "/provider",
        "/model",
        "shift-enter",
        "ctrl-l",
    ] {
        assert!(!text.contains(unsupported), "{unsupported}");
    }
}
