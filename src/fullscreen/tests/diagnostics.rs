//! Controlled HTTP → real agent/tools → channel → frontend state → TestBackend.
//! We exercise the completion-drain handlers, not terminal input or real pixels.
use super::*;
use crate::{
    diagnostics::Recorder,
    session::PreparedTurn,
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
    session: Session,
    receiver: UnboundedReceiver<AgentEvent>,
    app: App,
    ui_start_fault: Option<Recorder>,
}

impl Fixture {
    fn new(server: &Server, recorder: &Recorder) -> Self {
        Self::with_ui_start_fault(server, recorder, false)
    }

    fn with_ui_start_fault(server: &Server, recorder: &Recorder, fail: bool) -> Self {
        let model = server.model_with_recorder(recorder.clone());
        let (session, receiver) =
            Session::for_test(model, std::env::current_dir().unwrap(), recorder.clone());
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
            ui_start_fault: fail.then(|| recorder.clone()),
        }
    }

    fn prepare(&mut self, submitted: &str) -> PreparedTurn<'_> {
        let turn = self.session.prepare_turn();
        if let Some(recorder) = self.ui_start_fault.take() {
            recorder.break_writer_for_test();
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
