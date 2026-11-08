use super::*;

#[test]
fn clipping_respects_grapheme_boundaries_and_terminal_cell_budgets() {
    for glyph in ["#\u{fe0f}", "1\u{fe0f}\u{20e3}", "👩‍💻", "👍🏽", "e\u{301}"] {
        let text = format!("abc{glyph}xyz{glyph}");
        let boundaries: Vec<_> = text
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain([text.len()])
            .collect();
        for width in 0..20 {
            let head = single_line(&text, width);
            assert!(head.width() <= width, "{text:?}: {head:?} at {width}");
            let tail = input_tail(&text, width);
            assert!(tail.width() <= width);
            assert!(boundaries.contains(&(text.len() - tail.len())));
            if text.width() > width && width > 0 {
                assert!(head.ends_with('…'));
                assert!(boundaries.contains(&(head.len() - '…'.len_utf8())));
            }
        }
    }
}

#[test]
fn submitted_prompts_wrap_without_splitting_graphemes() {
    let rows = user_prompt_rows("👩‍💻👩‍💻", 4);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["▌ 👩‍💻", "▌ 👩‍💻"]
    );
    assert!(rows.iter().all(|row| row.width() == 4));
}

#[test]
fn notices_are_visible_plain_text_without_marking_a_turn_failed() {
    let mut app = App::default();
    app.notice("[warning] Diagnostics unavailable\u{1b}[31m");
    let output = app.take_output();
    assert!(matches!(&output[0], OutputLine::Wrapped((text, _))
            if text.contains("[warning] Diagnostics unavailable") && !text.contains('\u{1b}')));
    assert!(!app.failed);
    assert!(!app.working);
}

#[test]
fn banner_brand_is_bold_white_and_metadata_is_regular_white() -> Result<()> {
    use ratatui::style::Modifier;
    let mut terminal = Terminal::new(TestBackend::new(100, 12))?;
    let mut app = App::default();
    app.banner("test/model");
    render_output(&mut terminal, &mut app)?;
    let buffer = terminal.backend().buffer();
    for x in 2..8 {
        assert_eq!(buffer[(x, 0)].fg, Color::White);
        assert!(buffer[(x, 0)].modifier.contains(Modifier::BOLD));
    }
    for x in 9..38 {
        assert_eq!(buffer[(x, 0)].fg, Color::White);
        assert!(!buffer[(x, 0)].modifier.contains(Modifier::BOLD));
    }
    // The banner is a single row: the model line. No keyboard or
    // shell-approval hints follow it.
    assert_eq!(buffer[(0, 0)].fg, Color::Cyan);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    for removed in [
        "Enter send",
        "Ctrl+O cycle tool output",
        "Shell commands run without approval",
    ] {
        assert!(!text.contains(removed), "banner still shows {removed:?}");
    }
    assert!(
        !buffer
            .content
            .iter()
            .any(|cell| cell.modifier.contains(Modifier::DIM))
    );
    Ok(())
}

#[test]
fn submitted_prompt_repeats_its_gutter_on_wrapped_rows() {
    let mut app = App {
        input: "alpha beta gamma".into(),
        ..App::default()
    };
    assert_eq!(app.submit(), "alpha beta gamma");
    let rows = app.pending[0].rows(12);
    assert_eq!(
        rows.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["▌ alpha beta", "▌ gamma"]
    );
    for row in rows {
        assert_eq!(row.style.fg, Some(USER_ORANGE));
        assert!(row.width() <= 12);
    }
}

#[test]
fn submitted_prompt_keeps_literal_text_and_fits_tiny_widths() {
    let mut app = App {
        input: "**literal**\n\n猫猫 abcdefghijklmnop\u{1b}".into(),
        ..App::default()
    };
    let prompt = app.submit();
    assert!(
        prompt.ends_with('\u{1b}'),
        "display sanitizing must not change the submitted prompt"
    );
    let output = &app.pending[0];
    let wide = output.rows(80);
    assert_eq!(
        wide.iter().map(ToString::to_string).collect::<Vec<_>>(),
        ["▌ **literal**", "▌ ", "▌ 猫猫 abcdefghijklmnop",]
    );
    for width in 0..20 {
        let rows = output.rows(width);
        assert!(rows.iter().all(|row| row.width() <= width.max(2)));
        let body: String = rows
            .iter()
            .flat_map(|row| row.to_string().chars().collect::<Vec<_>>())
            .filter(|ch| !ch.is_whitespace() && *ch != '▌')
            .collect();
        assert_eq!(body, "**literal**猫猫abcdefghijklmnop");
        if width >= 4 {
            assert!(rows.iter().all(|row| row.to_string().starts_with("▌ ")));
        }
    }
    assert_eq!(output.rows(80), wide);
}

#[test]
fn submitted_prompt_has_an_aligned_orange_gutter_on_each_row() -> Result<()> {
    let mut app = App {
        input: "alpha beta gamma".into(),
        ..App::default()
    };
    app.submit();
    let mut terminal = Terminal::new(TestBackend::new(12, 12))?;
    render_output(&mut terminal, &mut app)?;
    let buffer = terminal.backend().buffer();
    for (y, first) in [(0, "a"), (1, "g")] {
        assert_eq!(buffer[(0, y)].symbol(), "▌");
        assert_eq!(buffer[(1, y)].symbol(), " ");
        assert_eq!(buffer[(2, y)].symbol(), first);
        assert_eq!(buffer[(0, y)].fg, USER_ORANGE);
        assert_eq!(buffer[(2, y)].fg, USER_ORANGE);
    }
    assert_eq!(
        buffer[(0, 2)].symbol(),
        " ",
        "trailing separator stays unmarked"
    );
    assert!(app.pending.is_empty());
    Ok(())
}

#[test]
fn assistant_markdown_preserves_styles_and_drains_once() -> Result<()> {
    use ratatui::style::Modifier;
    let mut app = App::default();
    app.activity(AgentEvent { source: "main", kind: EventKind::AssistantText("## Heading\n\n**bold** and `code`\n\n```rust\n    literal **code**\n```\n\n> quoted `code`".into(), None) });
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    render_output(&mut terminal, &mut app)?;
    let buffer = terminal.backend().buffer();
    assert_eq!(buffer[(0, 0)].symbol(), "H");
    assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(0, 2)].symbol(), "b");
    assert!(buffer[(0, 2)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(9, 2)].fg, Color::Cyan);
    assert_eq!(buffer[(4, 4)].symbol(), "l");
    assert!(!buffer[(4, 4)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(0, 6)].symbol(), "│");
    assert_eq!(buffer[(2, 6)].fg, MUTED_TEXT);
    assert_eq!(buffer[(9, 6)].fg, MUTED_TEXT);
    assert!(buffer[(2, 6)].modifier.contains(Modifier::ITALIC));
    assert!(!buffer[(0, 6)].modifier.contains(Modifier::ITALIC));
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!text.contains("```"));
    assert!(app.pending.is_empty());
    assert!(app.take_output().is_empty());
    Ok(())
}

#[test]
fn only_assistant_text_is_markdown_and_child_blocks_keep_their_label() {
    let mut app = App {
        input: "**literal prompt**".into(),
        ..App::default()
    };
    app.submit();
    assert_eq!(
        app.pending[0].rows(80)[0].to_string(),
        "▌ **literal prompt**"
    );
    app.pending.clear();
    app.output_mode = ToolOutputMode::Full;
    app.activity(AgentEvent {
        source: "main",
        kind: EventKind::ToolFinished {
            name: "read_file".into(),
            output: "# literal file\n**not bold**".into(),
            is_error: false,
        },
    });
    let rows = app
        .take_output()
        .iter()
        .flat_map(|line| line.rows(80))
        .collect::<Vec<_>>();
    assert!(rows.iter().any(|line| line.to_string() == "# literal file"));
    assert!(rows.iter().any(|line| line.to_string() == "**not bold**"));
    app.activity(AgentEvent {
        source: "subagent",
        kind: EventKind::AssistantText(
            "# Child\n\n```\n    **literal**\n```\n\nhello\u{1b}[31m".into(),
            None,
        ),
    });
    let rows = app
        .take_output()
        .iter()
        .flat_map(|line| line.rows(80))
        .collect::<Vec<_>>();
    let text = rows
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(text.starts_with("[agent]\nChild\n"), "{text}");
    assert!(text.contains("    **literal**"));
    assert!(!text.contains('\u{1b}'));
}
