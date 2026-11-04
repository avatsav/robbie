//! Frontend receipt/append evidence, not proof of a terminal draw or visible pixels.
use std::time::Instant;

use serde::Serialize;

use super::{Content, Recorder, model::RequestOrigin};

#[derive(Clone, Copy, Debug, Serialize)]
pub struct TextContext {
    #[serde(flatten)]
    origin: RequestOrigin,
    request_id: u64,
    text_sequence: usize,
}

impl TextContext {
    pub(super) fn new(origin: RequestOrigin, request_id: u64, text_sequence: usize) -> Self {
        Self {
            origin,
            request_id,
            text_sequence,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Enqueued,
    ReceiverClosed,
}

// Returned only after the UI inserts the sanitized Markdown source into its
// pending transcript state. Counts do not describe rendered cells.
pub struct TextAppend {
    pub context: TextContext,
    pub sanitized_bytes: usize,
    pub non_whitespace_chars: usize,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event<'a> {
    AgentTextEmitted {
        #[serde(flatten)]
        context: TextContext,
        raw_bytes: usize,
        delivery: Delivery,
    },
    UiTurnStarted {
        #[serde(flatten)]
        origin: RequestOrigin,
        submitted_bytes: usize,
    },
    UiTextReceived {
        ui_turn: RequestOrigin,
        #[serde(flatten)]
        context: TextContext,
        raw_bytes: usize,
    },
    UiTextAppended {
        ui_turn: RequestOrigin,
        #[serde(flatten)]
        context: TextContext,
        sanitized_bytes: usize,
        non_whitespace_chars: usize,
    },
    UiTurnFinished {
        #[serde(flatten)]
        origin: RequestOrigin,
        elapsed_ms: u128,
        #[serde(flatten)]
        end: UiEnd,
        totals: &'a UiTotals,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UiEnd {
    Completed { returned_text_bytes: usize },
    AgentFailed,
    Quit,
    FrontendFailed,
    Abandoned,
}

#[derive(Default, Serialize)]
struct TextTotals {
    received_blocks: usize,
    received_bytes: usize,
    appended_blocks: usize,
    sanitized_bytes: usize,
    non_whitespace_chars: usize,
}

#[derive(Default, Serialize)]
struct UiTotals {
    root: TextTotals,
    other_turns: TextTotals,
}

impl Recorder {
    pub(super) fn text_emitted(&self, context: TextContext, text: &str, enqueued: bool) {
        self.record(
            Event::AgentTextEmitted {
                context,
                raw_bytes: text.len(),
                delivery: if enqueued {
                    Delivery::Enqueued
                } else {
                    Delivery::ReceiverClosed
                },
            },
            || None,
        );
    }

    pub fn ui_turn(&self, origin: RequestOrigin, prompt: &str) -> UiTurn {
        let turn = UiTurn {
            recorder: self.clone(),
            origin,
            started: Instant::now(),
            totals: UiTotals::default(),
            finished: false,
        };
        self.record(
            Event::UiTurnStarted {
                origin,
                submitted_bytes: prompt.len(),
            },
            || Some(Content::UserPrompt { text: prompt }),
        );
        turn
    }
}

pub struct UiTurn {
    recorder: Recorder,
    origin: RequestOrigin,
    started: Instant,
    totals: UiTotals,
    finished: bool,
}

impl UiTurn {
    fn counts(&mut self, context: TextContext) -> &mut TextTotals {
        if context.origin == self.origin {
            &mut self.totals.root
        } else {
            &mut self.totals.other_turns
        }
    }

    pub fn received(&mut self, context: TextContext, text: &str) {
        let counts = self.counts(context);
        counts.received_blocks += 1;
        counts.received_bytes += text.len();
        self.recorder.record(
            Event::UiTextReceived {
                ui_turn: self.origin,
                context,
                raw_bytes: text.len(),
            },
            || Some(Content::UiText { text }),
        );
    }

    pub fn appended(&mut self, append: TextAppend) {
        let counts = self.counts(append.context);
        counts.appended_blocks += 1;
        counts.sanitized_bytes += append.sanitized_bytes;
        counts.non_whitespace_chars += append.non_whitespace_chars;
        self.recorder.record(
            Event::UiTextAppended {
                ui_turn: self.origin,
                context: append.context,
                sanitized_bytes: append.sanitized_bytes,
                non_whitespace_chars: append.non_whitespace_chars,
            },
            || None,
        );
    }

    pub fn finish(mut self, end: UiEnd) {
        self.end(end);
    }

    fn end(&mut self, end: UiEnd) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.recorder.record(
            Event::UiTurnFinished {
                origin: self.origin,
                elapsed_ms: self.started.elapsed().as_millis(),
                end,
                totals: &self.totals,
            },
            || None,
        );
    }
}

impl Drop for UiTurn {
    fn drop(&mut self) {
        self.end(UiEnd::Abandoned);
    }
}
