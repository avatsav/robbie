//! Controlled HTTP → real agent/tools → channel → frontend state → TestBackend.
//! We exercise the completion-drain handlers, not terminal input or real pixels.
use super::*;
use crate::{
    ToolRegistry,
    diagnostics::agent::AgentTurn,
    test_support::{Reply, Server, events, log},
};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::Value;
use std::time::Duration;

const PROMPT: &str = "PRIVATE_UI_PROMPT_726";
const TEXT: &str = r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"**é猫**\u001b"},{"type":"text","text":"**é猫**\u001b"}]}"#;
const EMPTY: &str = r#"{"stop_reason":"end_turn","content":[]}"#;
const TOOL: &str = r#"{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"fixture-call","name":"bash","input":{"command":"printf fixture-output"}}]}"#;
const PREFACE_TOOL: &str = r#"{"stop_reason":"tool_use","content":[{"type":"text","text":"working"},{"type":"tool_use","id":"fixture-call","name":"bash","input":{"command":"printf fixture-output"}}]}"#;

struct Fixture {
    agent: Agent,
    receiver: UnboundedReceiver<AgentEvent>,
    app: App,
    turn: Option<AgentTurn>,
}

impl Fixture {
    fn new(server: &Server, recorder: &Recorder) -> Self {
        Self::with_ui_start_fault(server, recorder, false)
    }

    fn with_ui_start_fault(server: &Server, recorder: &Recorder, fail: bool) -> Self {
        let model = server.model_with_recorder(recorder.clone());
        let (events, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut tools = ToolRegistry::new();
        tools.enable_sub_agent(model.clone(), events.clone());
        let agent = Agent::new(model, tools, events, "main");
        let mut app = App {
            recorder: Some(recorder.clone()),
            ..App::default()
        };
        app.handle_event(Event::Paste(PROMPT.into()));
        let prompt = app.content.submit();
        assert_eq!(prompt, PROMPT);
        let turn = agent.prepare_turn(None);
        if fail {
            recorder.break_writer_for_test();
        }
        app.ui_turn = Some(recorder.ui_turn(turn.origin(), &prompt));
        Self {
            agent,
            receiver,
            app,
            turn: Some(turn),
        }
    }

    async fn run(&mut self) -> Result<String> {
        let turn = self.turn.take().unwrap();
        tokio::time::timeout(Duration::from_secs(5), self.agent.run_in_turn(PROMPT, turn))
            .await
            .expect("controlled exchange timed out")
    }

    fn consume(&mut self, discard_text: bool) {
        while let Ok(event) = self.receiver.try_recv() {
            // Deliberate downstream-loss fixture, not a production recovery path.
            if discard_text && matches!(event.kind, EventKind::AssistantText(..)) {
                continue;
            }
            self.app.activity(event);
        }
    }

    fn finish(&mut self, result: Result<String>) -> Option<String> {
        let returned = result.as_ref().ok().cloned();
        self.app.finish_turn(result);
        returned
    }

    fn screen(&mut self) -> String {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| self.app.view(frame)).unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }
}

fn one<'a>(records: &'a [Value], name: &str) -> &'a Value {
    let matching = events(records, name);
    assert_eq!(matching.len(), 1, "{name}");
    matching[0]
}

#[tokio::test]
async fn text_is_correlated_received_and_appended_with_private_opt_in_content() {
    for transcript in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), transcript);
        let server = Server::new(vec![Reply::Http(200, "", TEXT)]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        // Agent completion alone does not acknowledge frontend consumption.
        let before = log(root.path());
        assert_eq!(events(&before, "agent_text_emitted").len(), 2);
        assert!(events(&before, "ui_text_received").is_empty());
        assert!(events(&before, "ui_turn_finished").is_empty());
        fixture.consume(false);
        assert_eq!(fixture.finish(result).unwrap().len(), 21);
        assert!(fixture.screen().contains("é猫"));
        let records = log(root.path());
        let start = one(&records, "ui_turn_started");
        let end = one(&records, "ui_turn_finished");
        assert_eq!(
            start["turn_id"],
            one(&records, "agent_turn_started")["turn_id"]
        );
        assert_eq!(end["turn_id"], start["turn_id"]);
        assert_eq!(end["outcome"], "completed");
        assert_eq!(end["returned_text_bytes"], 21);
        assert_eq!(end["totals"]["root"]["received_blocks"], 2);
        assert_eq!(end["totals"]["root"]["received_bytes"], 20);
        assert_eq!(end["totals"]["root"]["appended_blocks"], 2);
        assert_eq!(end["totals"]["root"]["sanitized_bytes"], 18);
        assert_eq!(end["totals"]["root"]["non_whitespace_chars"], 12);
        let emitted = events(&records, "agent_text_emitted");
        let received = events(&records, "ui_text_received");
        let appended = events(&records, "ui_text_appended");
        for i in 0..2 {
            for field in ["turn_id", "request_id", "text_sequence", "source"] {
                assert_eq!(emitted[i][field], received[i][field]);
                assert_eq!(received[i][field], appended[i][field]);
            }
            assert_eq!(emitted[i]["delivery"], "enqueued");
            assert_eq!(received[i]["text_sequence"], i + 1);
            assert_eq!(received[i]["ui_turn"]["turn_id"], start["turn_id"]);
            assert!(received[i]["sequence"].as_u64() < appended[i]["sequence"].as_u64());
            assert_eq!(received[i].get("content").is_some(), transcript);
            assert!(appended[i].get("content").is_none());
            if transcript {
                assert_eq!(received[i]["content"]["text"], "**é猫**\u{1b}");
            }
        }
        if transcript {
            assert_eq!(start["content"]["text"], PROMPT);
        } else {
            assert!(records.iter().all(|r| r.get("content").is_none()));
            let serialized = serde_json::to_string(&records).unwrap();
            assert!(!serialized.contains(PROMPT));
            assert!(!serialized.contains("é猫"));
        }
        let serialized = serde_json::to_string(&records).unwrap();
        for secret in [
            "test-secret",
            "authorization",
            "http://",
            "x-openrouter-cache",
        ] {
            assert!(!serialized.to_lowercase().contains(secret));
        }
        assert_eq!(server.count(), 1);
        drop(fixture);
        assert_eq!(events(&log(root.path()), "ui_turn_finished").len(), 1);
    }
}

#[tokio::test]
async fn empty_and_whitespace_blocks_are_appends_not_a_claim_of_visible_text() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(
        200,
        "",
        r#"{"content":[{"type":"text","text":""},{"type":"text","text":" \n\t"}]}"#,
    )]);
    let mut fixture = Fixture::new(&server, &recorder);
    let result = fixture.run().await;
    fixture.consume(false);
    assert_eq!(fixture.finish(result).unwrap().len(), 4);
    let records = log(root.path());
    let counts = &one(&records, "ui_turn_finished")["totals"]["root"];
    assert_eq!(counts["received_blocks"], 2);
    assert_eq!(counts["appended_blocks"], 2);
    assert_eq!(counts["received_bytes"], 3);
    assert_eq!(counts["sanitized_bytes"], 6); // Tabs expand to four spaces.
    assert_eq!(counts["non_whitespace_chars"], 0);
}

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
            let turn = fixture.agent.prepare_turn(None);
            fixture.app.ui_turn = Some(recorder.ui_turn(turn.origin(), &prompt));
            fixture.turn = Some(turn);
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
async fn empty_follow_up_after_tools_is_not_downstream_text_loss() {
    for (first, preface_bytes) in [(TOOL, 0), (PREFACE_TOOL, 7)] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![
            Reply::Http(200, "", first),
            Reply::Http(200, "", EMPTY),
        ]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        fixture.consume(false);
        assert_eq!(fixture.finish(result).unwrap(), "");
        let records = log(root.path());
        assert_eq!(server.count(), 2);
        assert_eq!(
            one(&records, "tool_execution_finished")["outcome"],
            "succeeded"
        );
        assert_eq!(
            events(&records, "agent_decision")[0]["decision"],
            "continue_after_tool_results"
        );
        assert_eq!(
            events(&records, "agent_decision")[1]["decision"],
            "stop_no_tool_results"
        );
        assert_eq!(
            events(&records, "model_response")[1]["response"]["text_bytes"],
            0
        );
        let end = one(&records, "ui_turn_finished");
        assert_eq!(end["returned_text_bytes"], 0);
        assert_eq!(end["totals"]["root"]["received_bytes"], preface_bytes);
        assert_eq!(end["totals"]["root"]["sanitized_bytes"], preface_bytes);
        assert_eq!(end["outcome"], "completed");
    }
}

#[tokio::test]
async fn token_limit_is_observed_without_automatic_continuation() {
    for (body, bytes) in [
        (r#"{"stop_reason":"max_tokens","content":[]}"#, 0),
        (
            r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"partial"}]}"#,
            7,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let recorder = Recorder::for_test(root.path(), false);
        let server = Server::new(vec![Reply::Http(200, "", body)]);
        let mut fixture = Fixture::new(&server, &recorder);
        let result = fixture.run().await;
        fixture.consume(false);
        fixture.finish(result);
        let records = log(root.path());
        assert_eq!(server.count(), 1);
        assert_eq!(
            one(&records, "model_response")["response"]["stop_reason"],
            "max_tokens"
        );
        assert_eq!(
            one(&records, "agent_decision")["decision"],
            "stop_no_tool_results"
        );
        let end = one(&records, "ui_turn_finished");
        assert_eq!(end["outcome"], "completed");
        assert_eq!(end["returned_text_bytes"], bytes);
        assert_eq!(end["totals"]["root"]["received_bytes"], bytes);
    }
}

#[tokio::test]
async fn enqueued_but_discarded_text_stays_distinct_from_empty_model_output() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![Reply::Http(200, "", TEXT)]);
    let mut fixture = Fixture::new(&server, &recorder);
    let result = fixture.run().await;
    fixture.consume(true);
    assert_eq!(fixture.finish(result).unwrap().len(), 21);
    assert!(
        !fixture.screen().contains("é猫"),
        "do not silently repair the loss from the returned string"
    );
    let records = log(root.path());
    assert_eq!(
        one(&records, "model_response")["response"]["text_bytes"],
        20
    );
    assert_eq!(
        one(&records, "agent_turn_finished")["totals"]["assistant_text_bytes"],
        20
    );
    assert!(
        events(&records, "agent_text_emitted")
            .iter()
            .all(|r| r["delivery"] == "enqueued")
    );
    assert!(events(&records, "ui_text_received").is_empty());
    assert!(events(&records, "ui_text_appended").is_empty());
    let end = one(&records, "ui_turn_finished");
    assert_eq!(end["returned_text_bytes"], 21);
    assert_eq!(end["totals"]["root"]["received_blocks"], 0);
}

#[tokio::test]
async fn child_text_joins_the_root_ui_turn_without_losing_its_own_request_identity() {
    let root = tempfile::tempdir().unwrap();
    let recorder = Recorder::for_test(root.path(), false);
    let server = Server::new(vec![
        Reply::Http(503, "Retry-After: 0\r\n", "private unavailable body"),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"delegating"},{"type":"tool_use","id":"child","name":"sub_agent","input":{"prompt":"private child task"}}]}"#,
        ),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"CHILD_OK"}]}"#,
        ),
        Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"text","text":"PARENT_OK"}]}"#,
        ),
    ]);
    let mut fixture = Fixture::new(&server, &recorder);
    let result = fixture.run().await;
    fixture.consume(false);
    assert_eq!(fixture.finish(result).unwrap(), "PARENT_OK");
    let screen = fixture.screen();
    assert!(screen.contains("CHILD_OK") && screen.contains("PARENT_OK"));
    let records = log(root.path());
    let start = one(&records, "ui_turn_started");
    let turns = events(&records, "agent_turn_started");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["parent"]["turn_id"], start["turn_id"]);
    assert_eq!(events(&records, "attempt_started").len(), 4);
    assert_eq!(events(&records, "retry_scheduled").len(), 1);
    assert_eq!(events(&records, "tool_execution_started").len(), 1);
    let received = events(&records, "ui_text_received");
    assert_eq!(received.len(), 3);
    assert_eq!(received[0]["turn_id"], start["turn_id"]);
    assert_eq!(received[1]["turn_id"], turns[1]["turn_id"]);
    assert_eq!(received[2]["turn_id"], start["turn_id"]);
    assert_eq!(received[0]["text_sequence"], 1);
    assert_eq!(received[1]["text_sequence"], 1);
    assert_eq!(received[2]["text_sequence"], 2);
    assert_ne!(received[0]["request_id"], received[2]["request_id"]);
    assert_ne!(received[1]["request_id"], received[2]["request_id"]);
    assert!(
        received
            .iter()
            .all(|r| r["ui_turn"]["turn_id"] == start["turn_id"])
    );
    let end = one(&records, "ui_turn_finished");
    assert_eq!(end["totals"]["root"]["received_bytes"], 19);
    assert_eq!(end["totals"]["other_turns"]["received_bytes"], 8);
    assert_eq!(end["totals"]["root"]["appended_blocks"], 2);
    assert_eq!(end["totals"]["other_turns"]["appended_blocks"], 1);
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
    let turn = fixture.turn.take().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(15),
        fixture.agent.run_in_turn(PROMPT, turn),
    )
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
    let turn = fixture.agent.prepare_turn(None);
    fixture.app.ui_turn = Some(recorder.ui_turn(turn.origin(), &prompt));
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        fixture.agent.run_in_turn(&prompt, turn),
    )
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
    let turn = fixture.turn.take().unwrap();
    let run = fixture.agent.run_in_turn(PROMPT, turn);
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
