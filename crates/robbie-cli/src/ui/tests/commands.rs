use super::*;

#[test]
fn local_commands_echo_and_clear_the_draft_without_starting_work() {
    for (input, expected) in [
        ("/help", "commands"),
        ("/help extra", "[error] Usage: /help"),
        ("/compact extra", "[error] Usage: /compact"),
        ("/unknown", "[error] Unknown command."),
    ] {
        let mut app = App::default();
        assert_eq!(
            app.handle_event(Event::Paste(input.into())),
            InputAction::None
        );
        assert!(app.pending.is_empty(), "typing is not execution");
        assert_eq!(
            app.handle_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE
            ))),
            InputAction::Submit
        );
        assert!(app.submit().is_none());
        assert!(app.input.is_empty());
        assert!(!app.working);
        assert_eq!(app.pending[0].text(), input);
        assert!(
            app.pending
                .iter()
                .any(|line| line.text().contains(expected))
        );
    }
}

#[test]
fn local_commands_do_not_replace_the_previous_turns_completion_state() {
    for result in [Ok("done".into()), Err(anyhow::anyhow!("request failed"))] {
        let mut app = App::default();
        app.submit().unwrap();
        app.finish_turn(result);
        let failed = app.failed;
        app.handle_event(Event::Paste("/help".into()));
        assert!(app.submit().is_none());
        assert_eq!(app.failed, failed);
    }
}

#[test]
fn an_ordinary_prompt_after_a_command_starts_work_and_preserves_literal_text() {
    let mut app = App::default();
    app.handle_event(Event::Paste("/unknown".into()));
    assert!(app.submit().is_none());
    let prompt = "  explain /tmp/猫  ";
    app.handle_event(Event::Paste(prompt.into()));
    assert_eq!(app.submit(), Some(Submission::Prompt(prompt.into())));
    assert!(app.working);
}

#[test]
fn exit_is_local_and_starts_no_work() {
    for command in ["/exit", "  /quit  "] {
        let mut app = App::default();
        app.handle_event(Event::Paste(command.into()));
        assert_eq!(app.submit(), Some(Submission::Exit));
        assert!(app.input.is_empty());
        assert!(!app.working);
        assert_eq!(app.pending[0].text(), command);
    }
}

#[test]
fn transcript_is_local_dispatch_not_a_model_prompt() {
    let mut app = App::default();
    app.handle_event(Event::Paste("  /transcript  ".into()));
    assert_eq!(app.submit(), Some(Submission::Transcript));
    assert!(app.input.is_empty());
    assert!(!app.working, "the host writes the dump after dispatch");
    assert_eq!(app.pending[0].text(), "  /transcript  ");
}

#[test]
fn compact_is_typed_session_work_not_a_model_prompt() {
    let mut app = App::default();
    app.handle_event(Event::Paste("  /compact  ".into()));
    assert_eq!(app.submit(), Some(Submission::Compact));
    assert!(app.input.is_empty());
    assert!(!app.working, "the host starts the operation after dispatch");
    app.start_compaction();
    assert!(app.working);
    app.handle_event(Event::Paste("next draft".into()));
    app.finish_compaction(Ok(crate::session::CompactionOutcome::Unchanged));
    assert!(!app.working);
    assert_eq!(app.input, "next draft");
    assert_eq!(app.submit(), Some(Submission::Prompt("next draft".into())));
}

#[test]
fn compact_outcomes_are_local_not_assistant_text_and_leave_input_available() {
    for (result, expected) in [
        (
            Ok(crate::session::CompactionOutcome::Unchanged),
            "[compact] Unchanged",
        ),
        (
            Ok(crate::session::CompactionOutcome::Compacted {
                retired_messages: 4,
                retained_messages: 2,
            }),
            "retired 4 messages; retained 2",
        ),
        (
            Err(anyhow::anyhow!("request failed")),
            "[compact] Failed; context unchanged",
        ),
    ] {
        let mut app = App::default();
        app.start_compaction();
        app.handle_event(Event::Paste("next draft".into()));
        app.finish_compaction(result);
        assert!(!app.working);
        assert_eq!(app.input, "next draft");
        assert!(
            app.pending
                .iter()
                .any(|line| line.text().contains(expected))
        );
        assert!(
            app.pending
                .iter()
                .all(|line| matches!(line, OutputLine::Wrapped(_)))
        );
        assert_eq!(app.submit(), Some(Submission::Prompt("next draft".into())));
    }
}

#[test]
fn help_wraps_in_the_existing_transcript_renderer() -> Result<()> {
    let mut app = App::default();
    app.handle_event(Event::Paste("/help".into()));
    assert!(app.submit().is_none());
    for width in [1, 12, 80] {
        assert!(
            app.pending
                .iter()
                .flat_map(|line| line.rows(width))
                .all(|row| row.width() <= width.max(2))
        );
    }
    let mut terminal = Terminal::new(TestBackend::new(80, 24))?;
    render_output(&mut terminal, &mut app)?;
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(text.contains("no model request"));
    assert!(!text.contains("Working…"));
    Ok(())
}
