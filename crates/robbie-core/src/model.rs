//! Compose protocol, provider, and one buffered HTTP exchange.
//!
//! Retry a model request, never an agent turn or tool execution.
//!
//! Retrying a POST after a lost response can duplicate provider work/billing. It
//! cannot replay local tools: callers only consume a successfully decoded reply.
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use reqwest::{Client, RequestBuilder, StatusCode, header::RETRY_AFTER};
use serde_json::Value;

use crate::diagnostics::{
    Recorder,
    agent::AgentTurn,
    compaction::Compaction,
    model::{
        AttemptOutcome, ModelRequest, RequestOrigin, RequestOutcome, RetrySource, RetryStop,
        TransportFailure,
    },
    tool::ToolContext,
};
use crate::usage::{ReportedUsage, UsageLedger, UsageTotals};

mod conversation;
mod http_error;
mod metadata;
mod prompt_cache;
mod protocol;
mod provider;

pub(crate) use conversation::summary_message;
pub use conversation::{Message, Output, Reply, ToolCall, ToolResult};
pub use http_error::HttpErrorMetadata;
use http_error::{HttpError, RetryAfter};
pub use metadata::ResponseMetadata;
pub(crate) use metadata::StopReason;
pub use prompt_cache::PromptCache;
use protocol::EncodedRequest;
pub use protocol::{AnthropicMessages, Protocol};
pub use provider::{LimitResolution, LimitSource, ModelLimits, OpenRouter, Provider};

// Concrete composition for today's one provider/protocol pair. Traits define
// the seams without a registry, trait objects, or runtime protocol selection.
pub struct ModelClient {
    http: Client,
    provider: OpenRouter,
    protocol: AnthropicMessages,
    prompt_cache: PromptCache,
    model: String,
    recorder: Recorder,
    usage: UsageLedger,
    limits: tokio::sync::OnceCell<LimitResolution>,
    compaction_policy: std::sync::OnceLock<crate::CompactionPolicy>,
}

// Local correlation belongs to the exchange, not protocol continuation/history.
pub struct Completion {
    pub reply: Reply,
    pub request_id: u64,
    pub(crate) usage: ReportedUsage,
}

// Exchange-only facts for callers that need acceptance checks. Neither this
// envelope nor its metadata belongs in protocol history.
pub(crate) struct CompletionWithMetadata {
    pub completion: Completion,
    pub metadata: ResponseMetadata,
}

impl ModelClient {
    pub fn new(
        http: Client,
        provider: OpenRouter,
        protocol: AnthropicMessages,
        model: String,
        recorder: Recorder,
    ) -> Self {
        Self {
            http,
            provider,
            protocol,
            prompt_cache: PromptCache::default(),
            model,
            recorder,
            usage: UsageLedger::default(),
            limits: tokio::sync::OnceCell::new(),
            compaction_policy: std::sync::OnceLock::new(),
        }
    }

    // Configure before sharing the immutable client with parent and children.
    pub fn with_prompt_cache(mut self, prompt_cache: PromptCache) -> Self {
        self.prompt_cache = prompt_cache;
        self
    }

    pub fn model_name(&self) -> &str {
        &self.model
    }

    pub fn configure_compaction(&self, policy: crate::CompactionPolicy) -> Result<()> {
        anyhow::ensure!(
            policy.summary_output_tokens > 0,
            "summary output limit must be positive"
        );
        let resolution = self
            .limit_resolution()
            .context("resolve model limits before configuring automatic compaction")?;
        if policy.enabled
            && let Some(limits) = resolution.limits
        {
            crate::budget::plan(limits, policy, 0)?;
        }
        if let Err(requested) = self.compaction_policy.set(policy) {
            anyhow::ensure!(
                self.compaction_policy.get() == Some(&requested),
                "automatic compaction is already configured for this session"
            );
        }
        Ok(())
    }

    pub fn compaction_policy(&self) -> Option<crate::CompactionPolicy> {
        self.compaction_policy.get().copied()
    }

    fn ordinary_output_tokens(&self) -> u32 {
        self.limit_resolution()
            .and_then(|resolution| resolution.limits)
            .map_or(8000, crate::budget::output_tokens)
    }

    pub fn limit_resolution(&self) -> Option<LimitResolution> {
        self.limits.get().copied()
    }

    pub async fn resolve_limits(&self, configured: Option<ModelLimits>) -> Result<LimitResolution> {
        let configured = configured.map(ModelLimits::validate).transpose()?;
        if let (Some(existing), Some(configured)) = (self.limit_resolution(), configured) {
            anyhow::ensure!(
                existing.limits == Some(configured),
                "model limits are already resolved for this session"
            );
        }
        Ok(*self
            .limits
            .get_or_init(|| async {
                if let Some(limits) = configured {
                    return LimitResolution {
                        limits: Some(limits),
                        source: LimitSource::Configured,
                    };
                }
                let limits = tokio::time::timeout(
                    Duration::from_secs(8),
                    self.provider.discover_limits(&self.http, &self.model),
                )
                .await
                .ok()
                .flatten();
                LimitResolution {
                    limits,
                    source: if limits.is_some() {
                        LimitSource::OpenRouter
                    } else {
                        LimitSource::Unknown
                    },
                }
            })
            .await)
    }

    pub fn usage_totals(&self) -> UsageTotals {
        self.usage.snapshot()
    }

    pub fn agent_turn(&self, source: &'static str, parent: Option<ToolContext>) -> AgentTurn {
        self.recorder.agent_turn(source, parent)
    }

    pub fn compaction_span(
        &self,
        origin: impl Into<RequestOrigin>,
        raw_messages: usize,
        had_previous_summary: bool,
    ) -> Compaction {
        self.recorder
            .compaction(origin.into(), raw_messages, had_previous_summary)
    }

    pub fn estimate_input_tokens(
        &self,
        system: &str,
        summary: Option<&str>,
        messages: &[Message],
        tools: &[Value],
    ) -> u64 {
        let encoded = match summary {
            Some(summary) => self.protocol.encode_with_summary(
                &self.model,
                system,
                summary,
                messages,
                tools,
                self.prompt_cache,
            ),
            None => self
                .protocol
                .encode(&self.model, system, messages, tools, self.prompt_cache),
        };
        encoded.body.to_string().chars().count().div_ceil(4) as u64
    }

    pub fn estimate_message_tokens(&self, message: &Message) -> usize {
        self.protocol.estimate_message_tokens(message)
    }

    pub async fn complete_with_summary(
        &self,
        origin: impl Into<RequestOrigin>,
        system: &str,
        summary: &str,
        messages: &[Message],
        tools: &[Value],
    ) -> Result<Completion> {
        let mut encoded = self.protocol.encode_with_summary(
            &self.model,
            system,
            summary,
            messages,
            tools,
            self.prompt_cache,
        );
        encoded.body["max_tokens"] = serde_json::json!(self.ordinary_output_tokens());
        Ok(self
            .complete_encoded(origin, encoded, Policy::default())
            .await?
            .completion)
    }

    pub async fn complete(
        &self,
        origin: impl Into<RequestOrigin>,
        system: &str,
        messages: &[Message],
        tools: &[Value],
    ) -> Result<Completion> {
        self.complete_with_policy(origin, system, messages, tools, Policy::default())
            .await
    }

    async fn complete_with_policy(
        &self,
        origin: impl Into<RequestOrigin>,
        system: &str,
        messages: &[Message],
        tools: &[Value],
        policy: Policy,
    ) -> Result<Completion> {
        let mut encoded =
            self.protocol
                .encode(&self.model, system, messages, tools, self.prompt_cache);
        encoded.body["max_tokens"] = serde_json::json!(self.ordinary_output_tokens());
        let CompletionWithMetadata {
            completion,
            metadata: _metadata,
        } = self.complete_encoded(origin, encoded, policy).await?;
        // Ordinary agent stopping policy is unchanged; it does not inspect stops.
        Ok(completion)
    }

    pub(crate) async fn complete_summary(
        &self,
        origin: impl Into<RequestOrigin>,
        prompt: String,
        max_output_tokens: u32,
    ) -> Result<CompletionWithMetadata> {
        anyhow::ensure!(
            max_output_tokens > 0,
            "summary output limit must be positive"
        );
        let encoded = self
            .protocol
            .encode_summary(&self.model, &prompt, max_output_tokens);
        drop(prompt);
        if let Some(limits) = self
            .limit_resolution()
            .and_then(|resolution| resolution.limits)
        {
            let input = encoded.body.to_string().chars().count().div_ceil(4) as u64;
            anyhow::ensure!(
                crate::budget::fits(limits, input, max_output_tokens),
                "summary input/output exceeds known model budget"
            );
        }
        self.complete_encoded(origin, encoded, Policy::default())
            .await
    }

    async fn complete_encoded(
        &self,
        origin: impl Into<RequestOrigin>,
        encoded: EncodedRequest,
        policy: Policy,
    ) -> Result<CompletionWithMetadata> {
        let prepared = self.provider.prepare(&self.http, encoded)?;
        let mut trace = self
            .recorder
            .model_request(origin, &self.model, &prepared.body);
        // Recording borrowed the body synchronously. Do not retain an extra
        // conversation-sized JSON value across HTTP waits and backoff.
        drop(prepared.body);
        let body = match exchange(prepared.request, policy, &trace).await {
            Ok(body) => body,
            Err(error) => {
                trace.finish(RequestOutcome::ExchangeFailed);
                return Err(error);
            }
        };
        // Preserve available facts even if executable-block validation fails.
        // Metadata stays out of history and never authorizes tool execution.
        let metadata = self.protocol.response_metadata(&body);
        let usage = self.protocol.reported_usage(&metadata);
        // Account once outside the transport retry loop, even if reply validation
        // fails. Unreceived bodies/invalid JSON cannot supply invented usage.
        self.usage.record(usage);
        trace.response(&metadata);
        match self.protocol.decode(body) {
            Ok(reply) => {
                trace.finish(RequestOutcome::Succeeded);
                Ok(CompletionWithMetadata {
                    completion: Completion {
                        reply,
                        request_id: trace.id(),
                        usage,
                    },
                    metadata,
                })
            }
            Err(error) => {
                trace.finish(RequestOutcome::ProtocolDecodeFailed);
                Err(error)
            }
        }
    }
}

const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
const MAX_RETRIES: usize = 3;

pub fn client() -> Result<Client> {
    Client::builder()
        // One retry owner keeps the attempt budget predictable.
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .build()
        .context("could not create model HTTP client")
}

#[cfg(test)]
async fn send(request: RequestBuilder) -> Result<Value> {
    send_with_policy(request, Policy::default()).await
}

#[cfg(test)]
async fn send_with_policy(request: RequestBuilder, policy: Policy) -> Result<Value> {
    let recorder = Recorder::disabled();
    let mut trace = recorder.model_request("test", "test", &Value::Null);
    let result = exchange(request, policy, &trace).await;
    trace.finish(if result.is_ok() {
        RequestOutcome::Succeeded
    } else {
        RequestOutcome::ExchangeFailed
    });
    result
}

// Private timing seam: tests exercise real HTTP with millisecond waits.
#[derive(Clone, Copy)]
struct Policy {
    backoff: Duration,
    attempt_timeout: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            backoff: Duration::from_secs(1),
            attempt_timeout: ATTEMPT_TIMEOUT,
        }
    }
}

impl Policy {
    #[cfg(test)]
    fn delay(self, retry: usize, retry_after: Option<&str>) -> Result<Duration> {
        let retry_after = retry_after
            .map(|value| RetryAfter::parse(value, SystemTime::now()))
            .unwrap_or(RetryAfter::Missing);
        self.delay_for(retry, retry_after).map(|(delay, _)| delay)
    }

    fn delay_for(self, retry: usize, retry_after: RetryAfter) -> Result<(Duration, RetrySource)> {
        let backoff = self.backoff * (1 << retry);
        let requested = retry_after.delay()?;
        let delay = backoff.max(requested);
        // Never shorten a server-requested wait and hammer it early.
        if delay > MAX_RETRY_WAIT {
            bail!("Retry-After exceeds the 30-second automatic wait limit");
        }
        // A tie is recorded as backoff: the header did not extend the wait.
        let source = if requested > backoff {
            RetrySource::RetryAfter
        } else {
            RetrySource::Backoff
        };
        Ok((delay, source))
    }
}

fn retry_after_delay(value: &str, now: SystemTime) -> Result<Option<Duration>> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        // An overflowing integer is a very long wait, not a malformed header
        // that permits an early retry.
        let seconds = value
            .parse::<u64>()
            .context("Retry-After exceeds the automatic wait limit")?;
        return Ok(Some(Duration::from_secs(seconds)));
    }
    // HTTP-date is the other standard form. Invalid headers use backoff;
    // dates in the past still leave the exponential backoff in place.
    Ok(httpdate::parse_http_date(value)
        .ok()
        .map(|date| date.duration_since(now).unwrap_or_default()))
}

fn retryable_status(status: StatusCode) -> bool {
    matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504 | 529)
}

fn retryable_transport(error: &reqwest::Error) -> bool {
    // reqwest also labels incomplete byte bodies as decode errors. JSON
    // decoding happens separately below, so these are still transport failures.
    error.is_connect()
        || error.is_timeout()
        || error.is_request()
        || error.is_body()
        || error.is_decode()
}

fn transport_failure(error: &reqwest::Error) -> TransportFailure {
    if error.is_timeout() {
        TransportFailure::Timeout
    } else if error.is_connect() {
        TransportFailure::Connect
    } else if error.is_body() || error.is_decode() {
        TransportFailure::Body
    } else if error.is_request() {
        TransportFailure::Request
    } else {
        TransportFailure::Other
    }
}

async fn exchange(request: RequestBuilder, policy: Policy, trace: &ModelRequest) -> Result<Value> {
    for retry in 0..=MAX_RETRIES {
        let attempt = retry + 1;
        let mut observation = trace.attempt(attempt);
        // JSON requests are buffered/cloneable; streaming bodies must not be
        // partially consumed and then silently replayed.
        let next = match request.try_clone() {
            Some(next) => next.timeout(policy.attempt_timeout),
            None => {
                observation.finish(AttemptOutcome::Unreplayable, None);
                trace.retry_stopped(attempt, RetryStop::PermanentFailure);
                bail!("model request body cannot be replayed");
            }
        };
        let outcome = async {
            let response = next.send().await?;
            let status = response.status();
            observation.status(status.as_u16());
            if !status.is_success() {
                // Parse at header receipt, before the diagnostic read. HTTP-date
                // waits must not get shortened by time spent reading the body.
                let retry_after =
                    RetryAfter::from_header(response.headers().get(RETRY_AFTER), SystemTime::now());
                // Body capture is best-effort evidence. A failed read must never
                // reclassify a known HTTP status as a transport failure.
                let error = HttpError::read(response, retry_after).await;
                return Ok(Err((status, retry_after, error)));
            }
            response.bytes().await.map(Ok)
        }
        .await;

        let (delay, delay_source) = match outcome {
            Ok(Ok(bytes)) => {
                // A fully received but invalid response is not a transport
                // failure. Do not retry JSON errors or execute partial output.
                let parsed = serde_json::from_slice(&bytes);
                observation.finish(
                    if parsed.is_ok() {
                        AttemptOutcome::Succeeded
                    } else {
                        AttemptOutcome::InvalidJson
                    },
                    Some(&bytes),
                );
                return parsed
                    .with_context(|| format!("invalid model response after {attempt} attempt(s)"));
            }
            Ok(Err((status, retry_after, error))) => {
                observation.finish_http_error(error.metadata, &error.body);
                let message = format!("model API returned {status} after {attempt} attempt(s)");
                if !retryable_status(status) || retry == MAX_RETRIES {
                    trace.retry_stopped(
                        attempt,
                        if !retryable_status(status) {
                            RetryStop::PermanentFailure
                        } else {
                            RetryStop::BudgetExhausted
                        },
                    );
                    bail!(message);
                }
                match policy.delay_for(retry, retry_after) {
                    Ok(delay) => delay,
                    Err(error) => {
                        trace.retry_stopped(attempt, RetryStop::DelayLimit);
                        return Err(error).with_context(|| message);
                    }
                }
            }
            Err(error) => {
                observation.finish(
                    AttemptOutcome::TransportError {
                        reason: transport_failure(&error),
                    },
                    None,
                );
                let retryable = retryable_transport(&error);
                // URLs may contain sensitive parameters; do not expose them.
                let error = error.without_url();
                if !retryable || retry == MAX_RETRIES {
                    trace.retry_stopped(
                        attempt,
                        if !retryable {
                            RetryStop::PermanentFailure
                        } else {
                            RetryStop::BudgetExhausted
                        },
                    );
                    return Err(error).with_context(|| {
                        format!("model request failed after {attempt} attempt(s)")
                    });
                }
                policy.delay_for(retry, RetryAfter::Missing)?
            }
        };
        trace.retry(attempt, delay, delay_source);
        // Dropping this future cancels the wait/request; no detached retry task.
        tokio::time::sleep(delay).await;
    }
    unreachable!("the final attempt always returns")
}

#[cfg(test)]
mod tests;
