//! Process-level terminal behavior; child fixtures never call a model.
use std::time::Duration;

use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use regex::bytes::Regex;

use super::{
    BLOCKED_FIXTURE, COMPLETED_FIXTURE,
    pty::{TerminalProcess, contains, verify_fixture},
};

#[test]
fn fixture_selectors_are_exact() -> Result<()> {
    verify_fixture(COMPLETED_FIXTURE)?;
    verify_fixture(BLOCKED_FIXTURE)?;
    assert!(verify_fixture("fullscreen::tests::terminal::missing_fixture").is_err());
    Ok(())
}

#[test]
#[ignore = "opt-in Unix PTY scenario"]
fn completed_read_response() -> Result<()> {
    check_screen(true)
}

#[test]
#[ignore = "opt-in Unix PTY scenario"]
fn blocked_read_responsiveness() -> Result<()> {
    check_screen(false)
}

fn check_screen(completed_read: bool) -> Result<()> {
    let clipboard = Regex::new(r"\x1b\]52;c;([A-Za-z0-9+/=]+)\x1b\\")?;
    let csi = Regex::new(r"\x1b\[[0-?]*[ -/]*[@-~]")?;
    let mut terminal = TerminalProcess::spawn(if completed_read {
        COMPLETED_FIXTURE
    } else {
        BLOCKED_FIXTURE
    })?;
    if completed_read {
        terminal.wait_for("completed response", Duration::from_secs(5), |output| {
            contains(output, b"Dependency-summary-rendered-successfully")
        })?;
        terminal.wait_for("code and quote output", Duration::from_secs(3), |output| {
            contains(output, b"markdown_fixture") && contains(output, b"Quoted-fixture")
        })?;
        let plain = csi.replace_all(terminal.output(), b"".as_slice());
        assert!(contains(&plain, b"[read] Cargo.toml"));
        assert!(
            contains(&plain, b"Dependencies"),
            "Markdown heading missing"
        );
        assert!(
            !contains(&plain, b"## Dependencies"),
            "raw heading syntax leaked"
        );
        assert!(
            !contains(&plain, b"**Dependency"),
            "raw emphasis syntax leaked"
        );
        assert!(
            !contains(&plain, b"```"),
            "code-fence presentation markers leaked"
        );
        assert!(contains(&plain, "│".as_bytes()), "quote gutter missing");
        assert!(
            !contains(&plain, b"> Quoted-fixture"),
            "raw quote marker leaked"
        );
    } else {
        terminal.wait_for("blocked read", Duration::from_secs(3), |output| {
            contains(output, b"Blocked-read-test")
        })?;
        terminal.wait_for("working label", Duration::from_secs(3), |output| {
            contains(output, "Working…".as_bytes())
        })?;
        let mark = terminal.output().len();
        terminal.wait_for(
            "animation during blocked read",
            Duration::from_secs(3),
            |output| {
                ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
                    .iter()
                    .any(|glyph| contains(&output[mark..], glyph.as_bytes()))
            },
        )?;
    }

    assert!(
        contains(terminal.output(), b"\x1b[?1049h"),
        "fullscreen mode not entered"
    );
    assert!(
        contains(terminal.output(), b"\x1b[?1000h"),
        "mouse reporting not enabled"
    );
    assert!(
        contains(terminal.output(), b"\x1b[?1006h"),
        "SGR mouse reporting not enabled"
    );
    terminal.type_and_wait(b"Z", b"Z")?;
    if completed_read {
        for marker in *b"123" {
            terminal.type_and_wait(&[0x0f, marker], &[marker])?;
        }
        assert!(
            !contains(terminal.output(), b"Ctrl+O"),
            "mode hint should stay hidden"
        );
        assert!(
            !contains(terminal.output(), b"[dependencies]"),
            "mode change replayed tool output"
        );
    }

    // Reach the transcript start regardless of footer height or wrapping.
    let mut page_up = b"\x1b[5~".repeat(20);
    page_up.push(b'P');
    terminal.type_and_wait(&page_up, b"P")?;
    let mark = terminal.output().len();
    terminal.send(b"\x1b[<0;1;1M\x1b[<32;8;1M")?;
    terminal.wait_for(
        "drag selection highlight",
        Duration::from_secs(3),
        |output| contains(&output[mark..], b"48;2;50;75;110"),
    )?;
    assert!(
        !clipboard.is_match(terminal.output()),
        "copied before mouse release"
    );
    terminal.send(b"\x1b[<0;8;1m")?;
    terminal.wait_for("clipboard request", Duration::from_secs(3), |output| {
        clipboard.is_match(output)
    })?;
    let copied = clipboard
        .captures(terminal.output())
        .context("clipboard request missing")?;
    let text = String::from_utf8(STANDARD.decode(&copied[1])?)?;
    assert_eq!(
        text,
        if completed_read {
            "▌ robbie"
        } else {
            "history "
        }
    );
    terminal.type_and_wait(b"K", b"K")?;
    assert_eq!(
        clipboard.find_iter(terminal.output()).count(),
        1,
        "redraw copied the selection again"
    );

    // Plain clicks must not copy. Wheel events still scroll without a mode toggle.
    terminal.type_and_wait(b"\x1b[<0;1;1M\x1b[<0;1;1mC", b"C")?;
    assert_eq!(
        clipboard.find_iter(terminal.output()).count(),
        1,
        "a click copied text"
    );
    for (report, marker) in [(b"\x1b[<65;10;10M", b'R'), (b"\x1b[<64;10;10M", b'U')] {
        let mark = terminal.output().len();
        let mut input = report.to_vec();
        input.push(marker);
        terminal.type_and_wait(&input, &[marker])?;
        if !completed_read {
            // Ratatui need not repaint unchanged spaces.
            assert!(
                contains(&terminal.output()[mark..], b"history"),
                "wheel input did not scroll the transcript"
            );
        }
    }

    terminal.resize(20, 60)?;
    terminal.type_and_wait(b"@", b"@")?;
    assert!(
        !contains(terminal.output(), b"\x1b[6n"),
        "obsolete cursor-position query returned"
    );
    assert!(
        !contains(terminal.output(), b"F2"),
        "obsolete mouse-mode toggle returned"
    );
    let shutdown_mark = terminal.output().len();
    terminal.send(b"\x03")?;
    terminal.finish(shutdown_mark)
}
