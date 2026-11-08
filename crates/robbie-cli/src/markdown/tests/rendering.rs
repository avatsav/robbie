use super::*;

#[test]
fn headings_and_nested_inline_styles_do_not_leak() {
    let rows = render(
        "# Heading\n\nNormal **bold *nested*** and *italic*, ~~gone~~, `code`, normal.",
        100,
    );
    assert!(plain(&rows).starts_with("Heading\n\nNormal bold nested"));
    assert!(
        effective_style(&rows, "Heading")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        effective_style(&rows, "bold")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert!(
        effective_style(&rows, "nested")
            .add_modifier
            .contains(Modifier::BOLD | Modifier::ITALIC)
    );
    assert!(
        effective_style(&rows, "italic")
            .add_modifier
            .contains(Modifier::ITALIC)
    );
    assert!(
        effective_style(&rows, "gone")
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
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
    let rows = render(
        "See [Rust](https://www.rust-lang.org) and <https://example.com>.",
        100,
    );
    let text = plain(&rows);
    assert!(text.contains("Rust"));
    assert!(text.contains("https://www.rust-lang.org"));
    assert!(text.contains("https://example.com"));
    assert!(
        effective_style(&rows, "Rust")
            .add_modifier
            .contains(Modifier::UNDERLINED)
    );
}

#[test]
fn wrapping_retains_styles_across_span_and_unicode_boundaries() {
    let rows = render("**alpha beta gamma** plain 猫猫 end", 10);
    assert!(rows.len() > 2);
    assert!(rows.iter().all(|line| line.width() <= 10));
    for word in ["alpha", "beta", "gamma"] {
        assert!(
            effective_style(&rows, word)
                .add_modifier
                .contains(Modifier::BOLD)
        );
    }
    assert_eq!(effective_style(&rows, "plain"), Style::new());
    assert!(plain(&rows).contains("猫猫"));
    // Repeated words must map to their own spans, not the first occurrence.
    let rows = render("**same** same **same**", 4);
    assert_eq!(rows.len(), 3);
    assert!(
        effective_style(&rows[..1], "same")
            .add_modifier
            .contains(Modifier::BOLD)
    );
    assert_eq!(effective_style(&rows[1..2], "same"), Style::new());
    assert!(
        effective_style(&rows[2..], "same")
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn wrapping_keeps_styled_graphemes_whole() {
    let rows = render("**👩‍💻👩‍💻**", 2);
    assert_eq!(plain(&rows), "👩‍💻\n👩‍💻");
    assert!(rows.iter().all(|row| row.width() == 2));
    assert!(rows.iter().all(|row| {
        effective_style(std::slice::from_ref(row), "👩‍💻")
            .add_modifier
            .contains(Modifier::BOLD)
    }));
}

#[test]
fn resize_reflows_text_and_tables_and_handles_tiny_widths() {
    let markdown = Markdown::new(
        "**alpha beta gamma delta**\n\n| Name | Value |\n| --- | --- |\n| long name | long value |"
            .into(),
    );
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
    let text = plain(&render(
        "one\nsoft break\n\ntwo  \nhard break\n\n\\*literal\\* <b>html</b>",
        80,
    ));
    assert!(text.contains("one soft break\n\ntwo\nhard break"), "{text}");
    assert!(text.contains("*literal* <b>html</b>"));
    assert!(render("", 80).is_empty());
}
