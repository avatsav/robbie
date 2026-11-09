use super::*;
use crate::{
    diagnostics::Recorder,
    test_support::{events, log},
};

#[tokio::test]
async fn summary_requests_preserve_identity_and_existing_capture_policy() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![Reply::Http(200, "", SUMMARY)]);
        let model = server.model_with_recorder(recorder);
        let mut messages = history();
        let mut previous = Some("PRIVATE_PREVIOUS_SUMMARY".into());
        let bounds = limits(&messages, 2);
        let mut turn = model.agent_turn("main", None);
        let committed = compact_history(
            &model,
            turn.model_call(),
            &mut messages,
            &mut previous,
            bounds,
            estimate,
        )
        .await
        .unwrap()
        .unwrap();
        drop(turn);

        let records = log(root.path());
        let requests = events(&records, "request_started");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["request_id"], committed.request_id);
        assert_eq!(requests[0]["source"], "main");
        assert_eq!(
            requests[0]["turn_id"],
            events(&records, "agent_turn_started")[0]["turn_id"]
        );
        assert_eq!(
            events(&records, "model_response")[0]["response"]["usage"]["input_tokens"],
            900
        );
        assert_eq!(
            events(&records, "request_finished")[0]["outcome"],
            "succeeded"
        );
        if transcript {
            assert_eq!(requests[0]["content"]["body"], bodies(&server)[0]);
            let prompt = requests[0]["content"]["body"]["messages"][0]["content"]
                .as_str()
                .unwrap();
            assert!(prompt.contains("PRIVATE_PREVIOUS_SUMMARY"));
            assert!(prompt.contains("PRIVATE_GOAL"));
            assert!(!prompt.contains("PRIVATE_SIGNATURE"));
            assert_eq!(
                events(&records, "attempt_finished")[0]["content"]["body"],
                SUMMARY
            );
        } else {
            assert!(records.iter().all(|record| record.get("content").is_none()));
            let serialized = serde_json::to_string(&records).unwrap();
            assert!(!serialized.contains("PRIVATE"));
            assert!(!serialized.contains("FIRST_SUMMARY"));
        }
        let serialized = serde_json::to_string(&records).unwrap().to_lowercase();
        for forbidden in [
            "test-secret",
            "authorization",
            "127.0.0.1",
            "x-openrouter-cache",
        ] {
            assert!(!serialized.contains(forbidden));
        }
    }
}

#[tokio::test]
async fn recording_failure_neither_blocks_compaction_nor_replays_the_request() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    recorder.break_writer_for_test();
    let server = Server::new(vec![Reply::Http(200, "", SUMMARY)]);
    let model = server.model_with_recorder(recorder.clone());
    let mut messages = history();
    let old_tail = wire(&messages[3..]);
    let bounds = limits(&messages, 2);
    let mut previous = None;

    compact_history(
        &model,
        "compaction",
        &mut messages,
        &mut previous,
        bounds,
        estimate,
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(wire(&messages), old_tail);
    assert!(previous.as_deref().unwrap().contains("FIRST_SUMMARY"));
    assert_eq!(server.count(), 1);
    assert!(recorder.take_warning().unwrap().contains("disabled"));
    assert!(recorder.take_warning().is_none());
}
