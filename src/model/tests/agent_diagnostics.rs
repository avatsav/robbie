//! Exercise the real agent loop, independently of frontend consumption.
use super::diagnostics::{events, log};
use super::*;
use crate::{Agent, ToolRegistry};
use serde_json::json;

pub(super) fn agent(model: Arc<ModelClient>) -> Agent {
    // Intentionally closed: handled text must not be mistaken for UI receipt.
    let (events, _) = tokio::sync::mpsc::unbounded_channel();
    Agent {
        model,
        tools: ToolRegistry::new(),
        events,
        source: "main",
        messages: Vec::new(),
    }
}

#[tokio::test]
async fn turns_correlate_decisions_and_count_raw_text_not_rendered_cells() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![
            Reply::Http(
                200,
                "",
                r#"{"content":[{"type":"text","text":"α"},{"type":"text","text":"🙂"},{"type":"text","text":""}]}"#,
            ),
            Reply::Http(200, "", SUCCESS),
        ]);
        let mut agent = agent(server.model_with_recorder(recorder));
        assert_eq!(agent.run("private first prompt").await.unwrap(), "α\n🙂\n");
        assert_eq!(agent.run("private second prompt").await.unwrap(), "done");
        let records = log(root.path());
        assert_eq!(
            records
                .iter()
                .map(|r| r["sequence"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            (1..=records.len() as u64).collect::<Vec<_>>()
        );
        let starts = events(&records, "agent_turn_started");
        let ends = events(&records, "agent_turn_finished");
        let decisions = events(&records, "agent_decision");
        let requests = events(&records, "request_started");
        assert_eq!(starts.len(), 2);
        assert_eq!(ends.len(), 2);
        assert_eq!(decisions.len(), 2);
        assert_eq!(requests.len(), 2);
        assert_ne!(starts[0]["turn_id"], starts[1]["turn_id"]);
        for i in 0..2 {
            assert_eq!(starts[i]["turn_id"], requests[i]["turn_id"]);
            assert_eq!(decisions[i]["request_id"], requests[i]["request_id"]);
            assert_eq!(decisions[i]["turn_id"], starts[i]["turn_id"]);
            assert_eq!(decisions[i]["decision"], "stop_no_tool_results");
            assert_eq!(decisions[i]["tool_result_count"], 0);
            assert_eq!(ends[i]["turn_id"], starts[i]["turn_id"]);
            assert_eq!(ends[i]["reason"], "no_tool_results");
            assert!(ends[i]["elapsed_ms"].is_u64());
            let turn_records: Vec<_> = records
                .iter()
                .filter(|r| r["turn_id"] == starts[i]["turn_id"])
                .collect();
            assert_eq!(
                turn_records
                    .iter()
                    .filter(|r| r["event"] != "agent_text_emitted")
                    .map(|r| r["event"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                [
                    "agent_turn_started",
                    "request_started",
                    "attempt_started",
                    "attempt_finished",
                    "model_response",
                    "request_finished",
                    "agent_decision",
                    "agent_turn_finished"
                ]
            );
            let emitted = events(&records, "agent_text_emitted")
                .into_iter()
                .filter(|r| r["turn_id"] == starts[i]["turn_id"])
                .collect::<Vec<_>>();
            assert_eq!(emitted.len(), if i == 0 { 3 } else { 1 });
            for (index, text) in emitted.iter().enumerate() {
                assert_eq!(text["text_sequence"], index + 1);
                assert_eq!(text["request_id"], requests[i]["request_id"]);
                assert_eq!(text["delivery"], "receiver_closed");
            }
            assert!(turn_records.iter().all(|r| r["source"] == "main"));
        }
        assert_eq!(
            ends[0]["totals"],
            json!({"model_calls":1,"assistant_text_blocks":3,"assistant_text_bytes":6,"completed_tool_results":0})
        );
        // Reply::text inserts newlines; these are not bytes in the original blocks.
        assert_eq!(ends[0]["returned_text_bytes"], 8);
        assert_eq!(ends[1]["totals"]["assistant_text_bytes"], 4);
        assert!(
            records
                .iter()
                .filter(|r| r["event"].as_str().unwrap().starts_with("agent_"))
                .all(|r| r.get("content").is_none())
        );
        if !transcript {
            assert!(records.iter().all(|r| r.get("content").is_none()));
            let serialized = serde_json::to_string(&records).unwrap();
            assert!(!serialized.contains("private"));
            assert!(!serialized.contains("🙂"));
        }
        // Local completion IDs must not enter continuation blocks sent upstream.
        let requests = server.requests.lock().unwrap();
        let body = requests[1].split_once("\r\n\r\n").unwrap().1;
        assert!(!body.contains("turn_id"));
        assert!(!body.contains("request_id"));
    }
}

#[tokio::test]
async fn tool_error_results_still_continue_then_empty_reply_stops() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"stop_reason":"tool_use","content":[{"type":"text","text":"private preface"},{"type":"tool_use","id":"private-call","name":"bash","input":{"command":"printf private-output; exit 7"}},{"type":"tool_use","id":"private-unknown","name":"private-missing-tool","input":{}}]}"#,
        ),
        Reply::Http(200, "", r#"{"stop_reason":"end_turn","content":[]}"#),
    ]);
    let mut agent = agent(server.model_with_recorder(recorder));
    assert_eq!(agent.run("private task").await.unwrap(), "");
    let records = log(root.path());
    let decisions = events(&records, "agent_decision");
    let responses = events(&records, "model_response");
    assert_eq!(decisions.len(), 2);
    assert_eq!(decisions[0]["decision"], "continue_after_tool_results");
    assert_eq!(decisions[0]["tool_result_count"], 2);
    assert_eq!(decisions[1]["decision"], "stop_no_tool_results");
    for i in 0..2 {
        assert_eq!(decisions[i]["request_id"], responses[i]["request_id"]);
        assert_eq!(decisions[i]["turn_id"], responses[i]["turn_id"]);
    }
    let ends = events(&records, "agent_turn_finished");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["reason"], "no_tool_results");
    assert_eq!(
        ends[0]["totals"],
        json!({"model_calls":2,"assistant_text_blocks":1,"assistant_text_bytes":15,"completed_tool_results":2})
    );
    assert_eq!(ends[0]["returned_text_bytes"], 0);
    assert!(!serde_json::to_string(&records).unwrap().contains("private"));
    let requests = server.requests.lock().unwrap();
    let body: Value = serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
    let results = body["messages"][2]["content"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r["is_error"] == true));
}

#[tokio::test]
async fn model_failures_end_the_turn_as_failed_not_cancelled_or_empty_success() {
    for response in [
        Reply::Http(401, "", "private error body"),
        Reply::Http(200, "", "private malformed JSON"),
        Reply::Http(200, "", r#"{"content":[{"type":"text"}]}"#),
    ] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![response]);
        let mut agent = agent(server.model_with_recorder(recorder));
        assert!(agent.run("private task").await.is_err());
        let records = log(root.path());
        assert!(events(&records, "agent_decision").is_empty());
        let ends = events(&records, "agent_turn_finished");
        assert_eq!(ends.len(), 1);
        assert_eq!(ends[0]["reason"], "model_failed");
        assert!(ends[0].get("returned_text_bytes").is_none());
        assert_eq!(ends[0]["totals"]["model_calls"], 1);
        assert_eq!(ends[0]["totals"]["assistant_text_bytes"], 0);
        assert_eq!(records.last().unwrap()["event"], "agent_turn_finished");
        assert!(!serde_json::to_string(&records).unwrap().contains("private"));
        assert_eq!(server.count(), 1);
    }
}

#[tokio::test]
async fn dropped_turn_preserves_completed_work_and_records_cancellation_after_request_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"private preface"},{"type":"tool_use","id":"one","name":"bash","input":{"command":"printf once"}}]}"#,
        ),
        Reply::Stall,
    ]);
    let mut agent = agent(server.model_with_recorder(recorder));
    let mut run = Box::pin(agent.run("private task"));
    // Wait for the follow-up request, rather than assuming tool/HTTP scheduling
    // completed within a fixed sleep on every machine.
    tokio::select! {
        result = run.as_mut() => panic!("turn completed unexpectedly: {result:?}"),
        ready = tokio::time::timeout(Duration::from_secs(5), async {
            while server.count() < 2 {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }) => ready.unwrap(),
    }
    let before = log(root.path());
    assert_eq!(events(&before, "request_started").len(), 2);
    assert_eq!(events(&before, "agent_turn_started").len(), 1);
    assert!(events(&before, "agent_turn_finished").is_empty());
    assert_eq!(
        events(&before, "agent_decision")[0]["decision"],
        "continue_after_tool_results"
    );
    drop(run);
    let after = log(root.path());
    let ends = events(&after, "agent_turn_finished");
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["reason"], "cancelled");
    assert_eq!(
        ends[0]["totals"],
        json!({"model_calls":2,"assistant_text_blocks":1,"assistant_text_bytes":15,"completed_tool_results":1})
    );
    assert!(ends[0].get("returned_text_bytes").is_none());
    assert_eq!(
        events(&after, "request_finished")[1]["outcome"],
        "cancelled"
    );
    assert_eq!(after.last().unwrap()["event"], "agent_turn_finished");
    assert_eq!(events(&after, "agent_decision").len(), 1);
    assert_eq!(server.count(), 2);
}

#[tokio::test]
async fn retries_stay_within_one_agent_model_call_and_one_turn() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(503, "", "private failure"),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut agent = agent(server.model_with_recorder(recorder));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), agent.run("private task"))
            .await
            .unwrap()
            .unwrap(),
        "done"
    );
    let records = log(root.path());
    let turn_id = &events(&records, "agent_turn_started")[0]["turn_id"];
    assert!(records.iter().all(|r| &r["turn_id"] == turn_id));
    assert_eq!(events(&records, "attempt_started").len(), 2);
    assert_eq!(events(&records, "retry_scheduled").len(), 1);
    assert_eq!(events(&records, "request_started").len(), 1);
    assert_eq!(events(&records, "agent_decision").len(), 1);
    assert_eq!(
        events(&records, "agent_turn_finished")[0]["totals"]["model_calls"],
        1
    );
}

#[tokio::test]
async fn nested_child_turn_does_not_replace_the_parents_request_context_or_totals() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"private-delegation","name":"sub_agent","input":{"prompt":"private child task"}}]}"#,
        ),
        Reply::Http(200, "", SUCCESS),
        Reply::Http(200, "", SUCCESS),
    ]);
    let mut parent = agent(server.model_with_recorder(recorder));
    parent
        .tools
        .enable_sub_agent(Arc::clone(&parent.model), parent.events.clone());
    assert_eq!(parent.run("private parent task").await.unwrap(), "done");
    let records = log(root.path());
    let turns = events(&records, "agent_turn_started");
    assert_eq!(turns.len(), 2);
    assert_ne!(turns[0]["turn_id"], turns[1]["turn_id"]);
    let requests = events(&records, "request_started");
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0]["turn_id"], turns[0]["turn_id"]);
    assert_eq!(requests[1]["turn_id"], turns[1]["turn_id"]);
    assert_eq!(requests[2]["turn_id"], turns[0]["turn_id"]);
    assert_eq!(requests[1]["source"], "subagent");
    assert_eq!(requests[2]["source"], "main");
    let ends = events(&records, "agent_turn_finished");
    assert_eq!(ends.len(), 2);
    assert_eq!(ends[0]["turn_id"], turns[1]["turn_id"]);
    assert_eq!(ends[0]["totals"]["model_calls"], 1);
    assert_eq!(ends[0]["totals"]["completed_tool_results"], 0);
    assert_eq!(ends[1]["turn_id"], turns[0]["turn_id"]);
    assert_eq!(ends[1]["totals"]["model_calls"], 2);
    assert_eq!(ends[1]["totals"]["completed_tool_results"], 1);
    assert_eq!(ends[1]["totals"]["assistant_text_bytes"], 4);
    assert!(!serde_json::to_string(&records).unwrap().contains("private"));
}
