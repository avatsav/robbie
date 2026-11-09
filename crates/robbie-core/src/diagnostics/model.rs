//! Model exchange evidence. No free-form errors, headers, URLs, or agent policy.
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use super::{Content, Recorder};
use crate::model::{HttpErrorMetadata, ResponseMetadata};

// Bare sources support standalone model exchanges. Agent calls always carry
// their turn ID explicitly; shared clients never store a mutable "current turn".
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RequestOrigin {
    source: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    compaction_id: Option<u64>,
}

impl RequestOrigin {
    pub(super) fn in_turn(source: &'static str, turn_id: u64) -> Self {
        Self {
            source,
            turn_id: Some(turn_id),
            compaction_id: None,
        }
    }

    pub(super) fn in_compaction(mut self, compaction_id: u64) -> Self {
        self.compaction_id = Some(compaction_id);
        self
    }
}

impl From<&'static str> for RequestOrigin {
    fn from(source: &'static str) -> Self {
        Self {
            source,
            turn_id: None,
            compaction_id: None,
        }
    }
}

#[derive(Serialize)]
pub(super) struct RequestContext {
    request_id: u64,
    #[serde(flatten)]
    origin: RequestOrigin,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event<'a> {
    RequestStarted {
        #[serde(flatten)]
        request: &'a RequestContext,
        model: &'a str,
    },
    AttemptStarted {
        #[serde(flatten)]
        request: &'a RequestContext,
        attempt: usize,
    },
    AttemptFinished {
        #[serde(flatten)]
        request: &'a RequestContext,
        attempt: usize,
        elapsed_ms: u128,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        outcome: AttemptOutcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        http_error: Option<&'a HttpErrorMetadata>,
    },
    RetryScheduled {
        #[serde(flatten)]
        request: &'a RequestContext,
        attempt: usize,
        delay_ms: u128,
        delay_source: RetrySource,
    },
    RetryStopped {
        #[serde(flatten)]
        request: &'a RequestContext,
        attempt: usize,
        reason: RetryStop,
    },
    ModelResponse {
        #[serde(flatten)]
        request: &'a RequestContext,
        response: &'a ResponseMetadata,
    },
    RequestFinished {
        #[serde(flatten)]
        request: &'a RequestContext,
        elapsed_ms: u128,
        outcome: RequestOutcome,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    Succeeded,
    ExchangeFailed,
    ProtocolDecodeFailed,
    Cancelled,
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AttemptOutcome {
    Succeeded,
    HttpError,
    TransportError { reason: TransportFailure },
    InvalidJson,
    Unreplayable,
    Cancelled,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportFailure {
    Timeout,
    Connect,
    Request,
    Body,
    Other,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrySource {
    Backoff,
    RetryAfter,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryStop {
    PermanentFailure,
    BudgetExhausted,
    DelayLimit,
}

// Keep ordinary JSON bodies readable while preserving non-UTF-8 failure bytes.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum Body<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}

impl Recorder {
    pub fn model_request(
        &self,
        origin: impl Into<RequestOrigin>,
        model: &str,
        body: &Value,
    ) -> ModelRequest {
        let request_id = {
            let mut journal = self.lock();
            journal.next_request += 1;
            journal.next_request
        };
        let request = ModelRequest {
            recorder: self.clone(),
            context: RequestContext {
                request_id,
                origin: origin.into(),
            },
            started: Instant::now(),
            finished: false,
        };
        self.record(
            Event::RequestStarted {
                request: &request.context,
                model,
            },
            || Some(Content::ProtocolRequest { body }),
        );
        request
    }
}

pub struct ModelRequest {
    recorder: Recorder,
    context: RequestContext,
    started: Instant,
    finished: bool,
}

impl ModelRequest {
    pub fn id(&self) -> u64 {
        self.context.request_id
    }

    pub fn attempt(&self, number: usize) -> Attempt<'_> {
        self.recorder.record(
            Event::AttemptStarted {
                request: &self.context,
                attempt: number,
            },
            || None,
        );
        Attempt {
            request: self,
            number,
            started: Instant::now(),
            status: None,
            http_error: None,
            finished: false,
        }
    }

    pub fn retry(&self, attempt: usize, delay: Duration, delay_source: RetrySource) {
        // The preceding attempt records the HTTP/transport cause. This event is
        // the distinct decision to retry, not evidence a later attempt started.
        self.recorder.record(
            Event::RetryScheduled {
                request: &self.context,
                attempt,
                delay_ms: delay.as_millis(),
                delay_source,
            },
            || None,
        );
    }

    pub fn retry_stopped(&self, attempt: usize, reason: RetryStop) {
        self.recorder.record(
            Event::RetryStopped {
                request: &self.context,
                attempt,
                reason,
            },
            || None,
        );
    }

    pub fn response(&self, response: &ResponseMetadata) {
        self.recorder.record(
            Event::ModelResponse {
                request: &self.context,
                response,
            },
            || None,
        );
    }

    pub fn finish(&mut self, outcome: RequestOutcome) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.recorder.record(
            Event::RequestFinished {
                request: &self.context,
                elapsed_ms: self.started.elapsed().as_millis(),
                outcome,
            },
            || None,
        );
    }
}

impl Drop for ModelRequest {
    fn drop(&mut self) {
        self.finish(RequestOutcome::Cancelled);
    }
}

pub struct Attempt<'a> {
    request: &'a ModelRequest,
    number: usize,
    started: Instant,
    status: Option<u16>,
    http_error: Option<HttpErrorMetadata>,
    finished: bool,
}

impl Attempt<'_> {
    pub fn status(&mut self, status: u16) {
        self.status = Some(status);
    }

    pub fn finish(mut self, outcome: AttemptOutcome, body: Option<&[u8]>) {
        self.end(outcome, body);
    }

    pub fn finish_http_error(mut self, metadata: HttpErrorMetadata, body: &[u8]) {
        self.http_error = Some(metadata);
        self.end(AttemptOutcome::HttpError, Some(body));
    }

    fn end(&mut self, outcome: AttemptOutcome, body: Option<&[u8]>) {
        if self.finished {
            return;
        }
        self.finished = true;
        self.request.recorder.record(
            Event::AttemptFinished {
                request: &self.request.context,
                attempt: self.number,
                elapsed_ms: self.started.elapsed().as_millis(),
                status: self.status,
                outcome,
                http_error: self.http_error.as_ref(),
            },
            || {
                body.map(|bytes| {
                    let body = match std::str::from_utf8(bytes) {
                        Ok(text) => Body::Text(text),
                        Err(_) => Body::Bytes(bytes),
                    };
                    if self.http_error.is_some() {
                        Content::HttpErrorResponse { body }
                    } else {
                        Content::ProtocolResponse { body }
                    }
                })
            },
        );
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.end(AttemptOutcome::Cancelled, None);
    }
}
