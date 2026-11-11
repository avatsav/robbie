//! Agent interpretation, distinct from provider stop reasons and UI consumption.
use std::time::Instant;

use serde::Serialize;

use super::{
    Recorder,
    activity::TextContext,
    model::RequestOrigin,
    tool::{ToolContext, ToolExecution},
};
use crate::model::ToolCall;

#[derive(Serialize)]
#[serde(tag = "event")]
enum Event<'a> {
    #[serde(rename = "agent_turn_started")]
    TurnStarted {
        #[serde(flatten)]
        origin: RequestOrigin,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<ToolContext>,
    },
    #[serde(rename = "agent_decision")]
    DecisionMade {
        #[serde(flatten)]
        origin: RequestOrigin,
        request_id: u64,
        decision: Decision,
        tool_result_count: usize,
    },
    #[serde(rename = "agent_turn_finished")]
    TurnFinished {
        #[serde(flatten)]
        origin: RequestOrigin,
        elapsed_ms: u128,
        #[serde(skip_serializing_if = "Option::is_none")]
        parent: Option<ToolContext>,
        #[serde(flatten)]
        end: TurnEnd,
        totals: &'a Totals,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    ContinueAfterToolResults,
    StopNoToolResults,
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum TurnEnd {
    NoToolResults { returned_text_bytes: usize },
    ModelFailed,
    ContextBudgetFailed,
    Cancelled,
}

#[derive(Default, Serialize)]
struct Totals {
    model_calls: usize,
    assistant_text_blocks: usize,
    assistant_text_bytes: usize,
    completed_tool_results: usize,
}

impl Recorder {
    pub fn agent_turn(&self, source: &'static str, parent: Option<ToolContext>) -> AgentTurn {
        let turn_id = {
            let mut journal = self.lock();
            journal.next_turn += 1;
            journal.next_turn
        };
        let turn = AgentTurn {
            recorder: self.clone(),
            origin: RequestOrigin::in_turn(source, turn_id),
            parent,
            started: Instant::now(),
            totals: Totals::default(),
            finished: false,
        };
        self.record(
            Event::TurnStarted {
                origin: turn.origin,
                parent,
            },
            || None,
        );
        turn
    }
}

pub struct AgentTurn {
    recorder: Recorder,
    origin: RequestOrigin,
    parent: Option<ToolContext>,
    started: Instant,
    totals: Totals,
    finished: bool,
}

impl AgentTurn {
    pub fn origin(&self) -> RequestOrigin {
        self.origin
    }

    pub fn model_call(&mut self) -> RequestOrigin {
        self.totals.model_calls += 1;
        self.origin
    }

    pub fn assistant_text(&mut self, request_id: u64, text: &str) -> TextContext {
        // Count raw UTF-8 bytes handled by the agent, even for a closed frontend.
        // This is neither rendered text nor acknowledgement of UI consumption.
        self.totals.assistant_text_blocks += 1;
        self.totals.assistant_text_bytes += text.len();
        TextContext::new(self.origin, request_id, self.totals.assistant_text_blocks)
    }

    pub fn text_emitted(&self, context: TextContext, text: &str, enqueued: bool) {
        self.recorder.text_emitted(context, text, enqueued);
    }

    pub fn tool_execution(&self, request_id: u64, call: &ToolCall) -> ToolExecution {
        self.recorder.tool_execution(self.origin, request_id, call)
    }

    pub fn tool_result(&mut self) {
        // Includes error results. Cancellation of an in-flight tool is not a result
        // and does not establish whether an external side effect happened.
        self.totals.completed_tool_results += 1;
    }

    pub fn decision(&self, request_id: u64, decision: Decision, tool_result_count: usize) {
        self.recorder.record(
            Event::DecisionMade {
                origin: self.origin,
                request_id,
                decision,
                tool_result_count,
            },
            || None,
        );
    }

    pub fn finish(&mut self, end: TurnEnd) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.recorder.record(
            Event::TurnFinished {
                origin: self.origin,
                elapsed_ms: self.started.elapsed().as_millis(),
                parent: self.parent,
                end,
                totals: &self.totals,
            },
            || None,
        );
    }
}

impl Drop for AgentTurn {
    fn drop(&mut self) {
        self.finish(TurnEnd::Cancelled);
    }
}
