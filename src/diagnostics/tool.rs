//! Local execution evidence, not a replay log or a guarantee of rolled-back effects.
use std::time::Instant;

use serde::Serialize;

use super::{Content, Recorder, model::RequestOrigin};
use crate::model::ToolCall;

#[derive(Clone, Copy, Serialize)]
pub struct ToolContext {
    #[serde(flatten)]
    origin: RequestOrigin,
    request_id: u64,
    tool_execution_id: u64,
}

// Model-supplied names can be arbitrary content. Classify known tools rather
// than copying an unknown name into the metadata-only journal.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolKind {
    Bash,
    ReadFile,
    WriteFile,
    EditFile,
    LoadSkill,
    Todo,
    SubAgent,
    Unknown,
}

impl ToolKind {
    fn from_name(name: &str) -> Self {
        match name {
            "bash" => Self::Bash,
            "read_file" => Self::ReadFile,
            "write_file" => Self::WriteFile,
            "edit_file" => Self::EditFile,
            "load_skill" => Self::LoadSkill,
            "todo" => Self::Todo,
            "sub_agent" => Self::SubAgent,
            _ => Self::Unknown,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "event")]
enum Event {
    #[serde(rename = "tool_execution_started")]
    Started {
        #[serde(flatten)]
        context: ToolContext,
        tool: ToolKind,
    },
    #[serde(rename = "tool_execution_finished")]
    Finished {
        #[serde(flatten)]
        context: ToolContext,
        tool: ToolKind,
        elapsed_ms: u128,
        outcome: ToolOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_bytes: Option<usize>,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum ToolOutcome {
    Succeeded,
    ReportedError,
    AbandonedOutcomeUnknown,
}

impl Recorder {
    pub(super) fn tool_execution(
        &self,
        origin: RequestOrigin,
        request_id: u64,
        call: &ToolCall,
    ) -> ToolExecution {
        let tool_execution_id = {
            let mut journal = self.lock();
            journal.next_tool += 1;
            journal.next_tool
        };
        let execution = ToolExecution {
            recorder: self.clone(),
            context: ToolContext {
                origin,
                request_id,
                tool_execution_id,
            },
            tool: ToolKind::from_name(&call.name),
            started: Instant::now(),
            finished: false,
        };
        self.record(
            Event::Started {
                context: execution.context,
                tool: execution.tool,
            },
            || {
                Some(Content::ToolCall {
                    tool_call_id: &call.id,
                    name: &call.name,
                    arguments: &call.arguments,
                })
            },
        );
        execution
    }
}

pub struct ToolExecution {
    recorder: Recorder,
    context: ToolContext,
    tool: ToolKind,
    started: Instant,
    finished: bool,
}

impl ToolExecution {
    pub fn context(&self) -> ToolContext {
        self.context
    }

    pub fn finish(mut self, output: &str, is_error: bool) {
        self.end(
            if is_error {
                ToolOutcome::ReportedError
            } else {
                ToolOutcome::Succeeded
            },
            Some(output),
        );
    }

    fn end(&mut self, outcome: ToolOutcome, output: Option<&str>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.recorder.record(
            Event::Finished {
                context: self.context,
                tool: self.tool,
                elapsed_ms: self.started.elapsed().as_millis(),
                outcome,
                output_bytes: output.map(str::len),
            },
            || output.map(|output| Content::ToolResult { output }),
        );
    }
}

impl Drop for ToolExecution {
    fn drop(&mut self) {
        // Dropping an async waiter does not establish whether an OS process,
        // blocking task, delegated agent, or external side effect has stopped.
        self.end(ToolOutcome::AbandonedOutcomeUnknown, None);
    }
}
