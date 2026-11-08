//! Controlled HTTP → real agent/tools → channel → frontend state → TestBackend.
//! We exercise the completion-drain handlers, not terminal input or real pixels.
use super::*;
use crate::{
    session::PreparedTurn,
    test_support::{RecordingControl, Reply, Server, events, log},
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
    session: Session,
    receiver: UnboundedReceiver<AgentEvent>,
    app: App,
    control: RecordingControl,
    ui_start_fault: bool,
}

impl Fixture {
    async fn new(server: &Server, root: &std::path::Path, transcript: bool) -> Self {
        let (session, receiver, control) = server
            .session(session_config(root, transcript))
            .await
            .unwrap();
        let mut app = App {
            recorder: Some(session.recording()),
            ..App::default()
        };
        app.handle_event(Event::Paste(PROMPT.into()));
        let prompt = app.content.submit();
        assert_eq!(prompt, PROMPT);
        Self {
            session,
            receiver,
            app,
            control,
            ui_start_fault: false,
        }
    }

    async fn with_ui_start_fault(server: &Server, root: &std::path::Path, fail: bool) -> Self {
        let mut fixture = Self::new(server, root, false).await;
        fixture.ui_start_fault = fail;
        fixture
    }

    fn prepare(&mut self, submitted: &str) -> PreparedTurn<'_> {
        let turn = self.session.prepare_turn();
        if std::mem::take(&mut self.ui_start_fault) {
            self.control.fail_writes();
        }
        self.app.ui_turn = Some(turn.record_frontend(submitted));
        turn
    }

    async fn run_prompt(&mut self, prompt: &str) -> Result<String> {
        self.prepare(prompt).run(prompt).await
    }

    async fn run(&mut self) -> Result<String> {
        tokio::time::timeout(Duration::from_secs(5), self.run_prompt(PROMPT))
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

mod text;

mod lifecycle;
