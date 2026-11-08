use super::*;

#[test]
fn lists_quotes_and_tasks_have_structure() {
    let text = plain(&render(
        "- first\n  - nested\n- second\n\n3. third\n4. fourth\n\n> quoted **words**\n\n- [x] done\n- [ ] next",
        80,
    ));
    for word in [
        "first",
        "nested",
        "second",
        "3.",
        "4.",
        "quoted words",
        "done",
        "next",
    ] {
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
        assert!(
            row.to_string().starts_with("│ "),
            "missing quote gutter: {row:?}"
        );
        assert!(row.width() <= 14);
    }
    let style = effective_style(&rows, "alpha");
    assert_eq!(style.fg, muted().fg);
    assert!(style.add_modifier.contains(Modifier::ITALIC));
}

#[test]
fn blockquotes_keep_blank_lines_nesting_and_inline_styles_without_leaking() {
    let rows = render("> first **bold**\n>\n> second\n>\n>> nested\n\noutside", 80);
    assert_eq!(
        plain(&rows),
        "│ first bold\n│ \n│ second\n│ \n│ │ nested\n\noutside"
    );
    let bold = effective_style(&rows, "bold");
    assert!(
        bold.add_modifier
            .contains(Modifier::BOLD | Modifier::ITALIC)
    );
    assert_eq!(bold.fg, muted().fg);
    assert_eq!(effective_style(&rows, "outside"), Style::new());
    let quote = Markdown::new("> first paragraph wraps over several rows\n>\n>> nested paragraph also wraps over several rows".into());
    let wide = quote.rows(80);
    let narrow = quote.rows(20);
    assert!(narrow.len() > wide.len());
    assert!(
        narrow
            .iter()
            .all(|row| row.to_string().starts_with("│ ") && row.width() <= 20)
    );
    assert!(
        narrow
            .iter()
            .filter(|row| row.to_string().starts_with("│ │ "))
            .count()
            > 1
    );
    assert_eq!(quote.rows(80), wide);
}

#[test]
fn literal_greater_than_symbols_are_not_quote_gutters() {
    for source in [
        "\\> **bold**",
        "# \\> **bold**",
        "\\>\nsoft break",
        "&#62;\nsoft break",
        "`>`\nsoft break",
        "```\n> literal\n>> literal nesting\n```",
    ] {
        let text = plain(&render(source, 80));
        assert!(
            text.contains('>'),
            "lost literal marker: {source:?} -> {text}"
        );
        assert!(!text.contains('│'), "false quote: {source:?} -> {text}");
    }
    let text = plain(&render(
        "> \\> literal\n>\n> ```\n> > literal code\n> ```",
        80,
    ));
    assert!(text.contains("│ > literal"), "{text}");
    assert!(text.contains("│ > literal code"), "{text}");
}

#[test]
fn quote_gutters_render_safely_in_tiny_viewports() {
    for width in 0..8 {
        let rows = render(">> **猫猫 xyz**", width);
        assert!(rows.iter().all(|row| row.width() <= width.max(2)));
        let body: String = plain(&rows)
            .chars()
            .filter(|ch| !ch.is_whitespace() && *ch != '│')
            .collect();
        assert_eq!(body, "猫猫xyz");
    }
}

#[test]
fn code_blocks_hide_presentation_fences_but_keep_literal_backticks() {
    for source in [
        "```rust\nlet x = 1;\n```",
        "~~~rust\nlet x = 1;\n~~~",
        "```rust\nlet x = 1;",
    ] {
        assert_eq!(plain(&render(source, 80)), "let x = 1;");
    }
    assert_eq!(plain(&render("````text\n```\n````", 80)), "```");
}

#[test]
fn blockquotes_keep_all_content_muted_even_when_children_set_colors() {
    let rows = render(
        "> # Heading\n>\n> **bold** `code` [link](https://example.com)\n>\n> ```rust\n> let x = 1;\n> ```\n\noutside `normal code`",
        80,
    );
    for word in ["Heading", "bold", "code", "link", "let x"] {
        assert_eq!(
            effective_style(&rows[..rows.len() - 1], word).fg,
            muted().fg,
            "{word}"
        );
    }
    assert!(
        effective_style(&rows, "bold")
            .add_modifier
            .contains(Modifier::BOLD | Modifier::ITALIC)
    );
    assert!(
        effective_style(&rows, "link")
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
    assert_eq!(effective_style(&rows, "normal code").fg, Some(Color::Cyan));
}

#[test]
fn code_keeps_indentation_blank_lines_and_literal_markdown() {
    let rows = render(
        "```rust\nfn main() {\n    **not bold**\n\n    println!(\"猫\");\n}\n```",
        80,
    );
    let text = plain(&rows);
    assert!(
        text.contains("    **not bold**\n\n    println!(\"猫\");"),
        "{text}"
    );
    assert!(
        !effective_style(&rows, "not bold")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    let unfinished = plain(&render("```unknown\n    let x = `literal`;", 80));
    assert!(unfinished.contains("    let x = `literal`;"));
}
