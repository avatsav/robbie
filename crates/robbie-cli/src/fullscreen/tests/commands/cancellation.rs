use super::*;

#[tokio::test]
async fn quitting_during_summary_wait_drops_the_transaction_without_context_or_turn_pollution() {
    let root = tempfile::tempdir().unwrap();
    let server = Server::new(vec![
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", SUCCESS),
        Reply::Http(429, "Retry-After: 1\r\n", "busy"),
        Reply::Http(200, "", SUCCESS),
    ]);
    let (mut session, mut receiver) = known_session(&server, root.path(), true).await;
    let mut app = App::default();
    seed(&mut app, &mut session, &mut receiver).await;
    submit_compact(&mut app);
    let mut pending = Box::pin(compact_session(&mut session));
    // Reach a known pending request/backoff without launching a terminal.
    tokio::select! {
        result = pending.as_mut() => panic!("unexpected completion: {result:?}"),
        result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while server.count() < 3 { tokio::task::yield_now().await; }
        }) => result.unwrap(),
    }
    app.handle_event(Event::Paste("next draft".into()));
    assert_eq!(
        app.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )))
        .unwrap(),
        InputAction::Quit
    );
    // As in run_turn's Quit return: drop the borrowed future, don't finish it.
    drop(pending);
    let records = log(root.path());
    assert_eq!(
        events(&records, "compaction_finished")[0]["outcome"],
        "cancelled"
    );
    assert_eq!(events(&records, "agent_turn_started").len(), 2);
    assert_eq!(events(&records, "ui_turn_started").len(), 2);
    assert_eq!(
        events(&records, "request_finished")[2]["outcome"],
        "cancelled"
    );
    assert_eq!(server.count(), 3);

    // The real host exits on Quit; reusing it here checks that drop committed nothing.
    assert_eq!(
        app.content.submit(),
        Some(Submission::Prompt("next draft".into()))
    );
    let result = session.prepare_turn().run("AFTER_CANCEL").await;
    assert_eq!(result.as_deref().unwrap(), "done");
    let bodies = request_bodies(root.path());
    let after = bodies[3]["messages"].to_string();
    assert!(after.contains("OLD_PRIVATE_MARKER"));
    assert!(after.contains("RECENT_PRIVATE_MARKER"));
    assert!(after.contains("AFTER_CANCEL"));
    assert!(!after.contains("/compact"));
}
