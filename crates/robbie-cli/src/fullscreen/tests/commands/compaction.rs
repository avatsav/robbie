use super::*;
use crate::session::CompactionOutcome;

#[tokio::test]
async fn unknown_capacity_and_empty_history_have_local_distinct_outcomes_without_http() {
    for known in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![]);
        let (mut session, mut receiver) = if known {
            known_session(&server, root.path(), false).await
        } else {
            let (session, receiver, _) = server
                .session(session_config(root.path(), false))
                .await
                .unwrap();
            (session, receiver)
        };
        let mut app = App::default();
        submit_compact(&mut app);
        let result = compact_session(&mut session).await;
        assert_eq!(result.is_ok(), known);
        consume(&mut app, &mut receiver);
        app.content.finish_compaction(result);
        let text = history(&mut app);
        assert!(text.contains(if known {
            "[compact] Unchanged"
        } else {
            "[compact] Failed; context unchanged"
        }));
        if !known {
            assert!(text.contains("ROBBIE_CONTEXT_WINDOW"));
        }
        assert_eq!(server.count(), 0);
        let records = log(root.path());
        for event in ["agent_turn_started", "ui_turn_started", "request_started"] {
            assert!(events(&records, event).is_empty(), "{event}");
        }
        let ends = events(&records, "compaction_finished");
        assert_eq!(ends.len(), usize::from(known));
        if known {
            assert_eq!(ends[0]["outcome"], "unchanged");
        }
        assert!(records.iter().all(|record| record.get("content").is_none()));
    }
}

#[tokio::test]
async fn explicit_compact_uses_bounded_summary_without_a_followup_or_fake_turn() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![
            Reply::Http(200, "", SUCCESS),
            Reply::Http(200, "", SUCCESS),
            Reply::Http(200, "", SUMMARY),
            Reply::Http(200, "", SUCCESS),
        ]);
        let (mut session, mut receiver) = known_session(&server, root.path(), transcript).await;
        let mut app = App::default();
        seed(&mut app, &mut session, &mut receiver).await;
        let before = session.statistics();
        submit_compact(&mut app);
        let result = compact_session(&mut session).await;
        assert_eq!(
            result.as_ref().unwrap(),
            &CompactionOutcome::Compacted {
                retired_messages: 2,
                retained_messages: 2
            }
        );
        consume(&mut app, &mut receiver);
        app.content.finish_compaction(result);
        assert_eq!(server.count(), 3, "no ordinary followup after the summary");
        let after = session.statistics();
        assert_eq!(after.totals.responses, before.totals.responses + 1);
        assert_eq!(
            after.totals.input.reported,
            before.totals.input.reported + 900
        );
        assert_eq!(
            after.totals.output.reported,
            before.totals.output.reported + 12
        );
        assert!(after.estimate.unwrap().tokens < before.estimate.unwrap().tokens);
        assert!(!after.estimate.unwrap().automatic);
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| app.view(frame)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            screen.contains("↑≥900 ↓≥12"),
            "summary usage reaches the footer"
        );
        assert!(!screen.contains("Working…"));
        let text = history(&mut app);
        assert!(text.contains("[compact] Compacted: retired 2 messages; retained 2"));
        assert!(
            text.contains("OLD_PRIVATE_MARKER"),
            "visible transcript is not compacted"
        );
        assert!(
            !text.contains("SAVED_CHECKPOINT"),
            "summary is context, not assistant output"
        );
        let records = log(root.path());
        for event in [
            "agent_turn_started",
            "ui_turn_started",
            "ui_turn_finished",
            "agent_text_emitted",
        ] {
            assert_eq!(events(&records, event).len(), 2, "{event}");
        }
        assert!(events(&records, "tool_started").is_empty());
        assert_eq!(
            events(&records, "compaction_finished")[0]["outcome"],
            "committed"
        );
        assert!(
            events(&records, "compaction_started")[0]
                .get("turn_id")
                .is_none()
        );
        if transcript {
            let bodies = request_bodies(root.path());
            assert_eq!(bodies[2]["max_tokens"], 512);
            assert!(bodies[2]["tools"].as_array().is_none_or(Vec::is_empty));
            assert!(!bodies[2].to_string().contains("cache_control"));
            assert!(!bodies[2]["messages"].to_string().contains("/compact"));
        } else {
            assert!(records.iter().all(|record| record.get("content").is_none()));
        }
        ordinary_prompt(&mut app, &mut session, &mut receiver, "AFTER_COMPACT").await;
        assert_eq!(server.count(), 4);
        if transcript {
            let bodies = request_bodies(root.path());
            let after = bodies[3]["messages"].to_string();
            assert_eq!(after.matches("SAVED_CHECKPOINT").count(), 1);
            assert!(after.contains("RECENT_PRIVATE_MARKER"));
            assert!(after.contains("AFTER_COMPACT"));
            assert!(!after.contains("OLD_PRIVATE_MARKER"));
            assert!(!after.contains("/compact"));
        }
    }
}

#[tokio::test]
async fn rejected_or_http_failed_summary_leaves_context_and_next_submission_usable() {
    for reply in [
        Reply::Http(200, "", BAD_SUMMARY),
        Reply::Http(403, "", "private forbidden response"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let server = Server::new(vec![
            Reply::Http(200, "", SUCCESS),
            Reply::Http(200, "", SUCCESS),
            reply,
            Reply::Http(200, "", SUCCESS),
        ]);
        let (mut session, mut receiver) = known_session(&server, root.path(), true).await;
        let mut app = App::default();
        seed(&mut app, &mut session, &mut receiver).await;
        submit_compact(&mut app);
        let result = compact_session(&mut session).await;
        assert!(result.is_err());
        consume(&mut app, &mut receiver);
        app.content.finish_compaction(result);
        let text = history(&mut app);
        assert!(text.contains("[compact] Failed; context unchanged"));
        assert!(!text.contains("private forbidden response"));
        assert_eq!(server.count(), 3);
        ordinary_prompt(&mut app, &mut session, &mut receiver, "AFTER_FAILURE").await;
        let bodies = request_bodies(root.path());
        let after = bodies[3]["messages"].to_string();
        assert!(after.contains("OLD_PRIVATE_MARKER"));
        assert!(after.contains("RECENT_PRIVATE_MARKER"));
        assert!(after.contains("AFTER_FAILURE"));
        assert!(!after.contains("/compact"));
        let records = log(root.path());
        assert_eq!(
            events(&records, "compaction_finished")[0]["outcome"],
            "failed"
        );
        assert_eq!(events(&records, "ui_turn_started").len(), 3);
    }
}
