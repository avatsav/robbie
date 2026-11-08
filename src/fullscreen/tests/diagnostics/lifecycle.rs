use super::*;

#[tokio::test]
async fn sequential_ui_turns_reset_counts_and_text_sequence_without_reusing_turn_ids() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(200, "", TEXT), Reply::Http(200, "", TEXT)]);
    let mut fixture = Fixture::new(&server, &recorder);
    for i in 0..2 {
        if i == 1 {
            fixture.app.handle_event(Event::Paste(PROMPT.into()));
            let prompt = fixture.app.content.submit();
            assert_eq!(prompt, PROMPT);
        }
        let result = fixture.run().await;
        fixture.consume(false);
        fixture.finish(result);
    }
    let records = log(root.path());
    let starts = events(&records, "ui_turn_started");
    let ends = events(&records, "ui_turn_finished");
    assert_eq!(starts.len(), 2);
    assert_eq!(ends.len(), 2);
    assert_ne!(starts[0]["turn_id"], starts[1]["turn_id"]);
    let received = events(&records, "ui_text_received");
    for i in 0..2 {
        assert_eq!(ends[i]["turn_id"], starts[i]["turn_id"]);
        assert_eq!(ends[i]["totals"]["root"]["received_blocks"], 2);
        assert_eq!(received[i * 2]["text_sequence"], 1);
        assert_eq!(received[i * 2]["turn_id"], starts[i]["turn_id"]);
    }
}

#[tokio::test]
async fn frontend_acknowledges_agent_failure_without_inventing_an_empty_success() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![Reply::Http(403, "", "private forbidden body")]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        fixture.consume(false);
        assert!(fixture.finish(result).is_none());
        let screen = fixture.screen();
        assert!(screen.contains("[error]"));
        assert!(!screen.contains("private forbidden body"));
        let records = log(root.path());
        let end = one(&records, "ui_turn_finished");
        assert_eq!(end["outcome"], "agent_failed");
        assert!(end.get("returned_text_bytes").is_none());
        assert_eq!(
            serde_json::to_string(&records)
                .unwrap()
                .contains("private forbidden body"),
            transcript
        );
    }
}

#[tokio::test]
async fn exhausted_rate_limit_retries_allow_the_next_user_message_to_complete() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(429, "", "rate limited"),
        Reply::Http(429, "", "rate limited"),
        Reply::Http(429, "", "rate limited"),
        Reply::Http(429, "", "rate limited"),
        Reply::Http(200, "", TEXT),
    ]);
    let mut fixture = Fixture::new(&server, &recorder);
    // A draft typed during the failed turn must survive completion as well.
    fixture.app.handle_event(Event::Paste("next prompt".into()));
    let result = tokio::time::timeout(Duration::from_secs(15), fixture.run_prompt(PROMPT))
        .await
        .expect("bounded retries timed out");
    assert!(format!("{:#}", result.as_ref().unwrap_err()).contains("429"));
    fixture.consume(false);
    assert!(fixture.finish(result).is_none());
    assert!(fixture.screen().contains("[error]"));
    assert_eq!(server.count(), 4);

    assert_eq!(
        fixture
            .app
            .handle_event(Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ))),
        InputAction::Submit,
        "retry exhaustion must not lock the input for the rest of the session"
    );
    let prompt = fixture.app.content.submit();
    assert_eq!(prompt, "next prompt");
    let result = tokio::time::timeout(Duration::from_secs(5), fixture.run_prompt(&prompt))
        .await
        .expect("next turn timed out");
    fixture.consume(false);
    assert!(fixture.finish(result).is_some());
    assert!(fixture.screen().contains("é猫"));
    assert_eq!(server.count(), 5);

    let records = log(root.path());
    let endings = events(&records, "ui_turn_finished");
    assert_eq!(endings.len(), 2);
    assert_eq!(endings[0]["outcome"], "agent_failed");
    assert_eq!(endings[1]["outcome"], "completed");
    assert_ne!(endings[0]["turn_id"], endings[1]["turn_id"]);
    assert_eq!(events(&records, "retry_scheduled").len(), 3);
}

#[tokio::test]
async fn prepared_identity_survives_frontend_failure_before_work_is_polled() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![]);
    let mut fixture = Fixture::new(&server, &recorder);
    let run = fixture.prepare(PROMPT).run(PROMPT);
    // A draw/input failure can drop the prepared future before its first poll.
    drop(run);
    fixture.app.end_ui_turn(UiEnd::FrontendFailed);
    let records = log(root.path());
    assert_eq!(server.count(), 0);
    assert!(events(&records, "request_started").is_empty());
    let agent_end = one(&records, "agent_turn_finished");
    let ui_end = one(&records, "ui_turn_finished");
    assert_eq!(agent_end["reason"], "cancelled");
    assert_eq!(ui_end["outcome"], "frontend_failed");
    assert_eq!(ui_end["turn_id"], agent_end["turn_id"]);
}

#[tokio::test]
async fn quit_frontend_error_and_unexpected_drop_are_distinct_from_agent_failure() {
    for ending in [Some(UiEnd::Quit), Some(UiEnd::FrontendFailed), None] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![Reply::Stall]);
        let mut fixture = Fixture::new(&server, &recorder);
        let mut run = Box::pin(fixture.run());
        tokio::select! {
            result = &mut run => panic!("unexpected completion: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(2), async {
                while server.count() == 0 { tokio::time::sleep(Duration::from_millis(1)).await; }
            }) => result.unwrap(),
        }
        assert_eq!(events(&log(root.path()), "ui_turn_started").len(), 1);
        assert!(events(&log(root.path()), "ui_turn_finished").is_empty());
        drop(run);
        if let Some(end) = ending {
            fixture.app.end_ui_turn(end);
        }
        drop(fixture);
        let records = log(root.path());
        assert_eq!(one(&records, "agent_turn_finished")["reason"], "cancelled");
        let end = one(&records, "ui_turn_finished");
        assert_eq!(
            end["outcome"],
            match ending {
                Some(UiEnd::Quit) => "quit",
                Some(_) => "frontend_failed",
                None => "abandoned",
            }
        );
        assert!(end.get("returned_text_bytes").is_none());
    }
}

#[tokio::test]
async fn ui_recording_faults_warn_but_never_drop_text_or_replay_work() {
    for boundary in ["start", "receipt", "finish"] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![Reply::Http(200, "", TEXT)]);
        let mut fixture = Fixture::with_ui_start_fault(&server, &recorder, boundary == "start");
        let result = fixture.run().await;
        if boundary == "receipt" {
            recorder.break_writer_for_test();
        }
        fixture.consume(false);
        if boundary == "finish" {
            recorder.break_writer_for_test();
        }
        assert_eq!(fixture.finish(result).unwrap().len(), 21);
        let screen = fixture.screen();
        assert!(screen.contains("é猫"));
        assert!(screen.contains("Diagnostics recording failed and is disabled"));
        assert!(recorder.take_warning().is_none());
        assert_eq!(server.count(), 1);
    }
}
