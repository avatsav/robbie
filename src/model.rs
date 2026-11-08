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
    model::{
        AttemptOutcome, ModelRequest, RequestOrigin, RequestOutcome, RetrySource, RetryStop,
        TransportFailure,
    },
    tool::ToolContext,
};

mod conversation;
mod http_error;
mod metadata;
mod prompt_cache;
mod protocol;
mod provider;

pub use conversation::{Message, Output, Reply, ToolCall, ToolResult};
pub use http_error::HttpErrorMetadata;
use http_error::{HttpError, RetryAfter};
pub use metadata::ResponseMetadata;
pub use prompt_cache::PromptCache;
pub use protocol::{AnthropicMessages, Protocol};
pub use provider::{OpenRouter, Provider};

// Concrete composition for today's one provider/protocol pair. Traits define
// the seams without a registry, trait objects, or runtime protocol selection.
pub struct ModelClient {
    http: Client,
    provider: OpenRouter,
    protocol: AnthropicMessages,
    prompt_cache: PromptCache,
    model: String,
    recorder: Recorder,
}

// Local correlation belongs to the exchange, not protocol continuation/history.
pub struct Completion {
    pub reply: Reply,
    pub request_id: u64,
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

    pub fn agent_turn(&self, source: &'static str, parent: Option<ToolContext>) -> AgentTurn {
        self.recorder.agent_turn(source, parent)
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
        let encoded = self
            .protocol
            .encode(&self.model, system, messages, tools, self.prompt_cache);
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
        trace.response(&self.protocol.response_metadata(&body));
        match self.protocol.decode(body) {
            Ok(reply) => {
                trace.finish(RequestOutcome::Succeeded);
                Ok(Completion {
                    reply,
                    request_id: trace.id(),
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
pub(crate) mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
    };

    const SUCCESS: &str = r#"{"content":[{"type":"text","text":"done"}]}"#;

    pub(crate) enum Reply {
        Http(u16, &'static str, &'static str),
        HttpBytes(u16, &'static [u8]),
        TruncatedHttpBody(u16),
        StallHttpBody(u16),
        Disconnect,
        TruncatedBody,
        Stall,
        StallBody,
    }

    pub(crate) struct Server {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        pub(crate) fn new(replies: Vec<Reply>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}/messages", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let stop = Arc::new(AtomicBool::new(false));
            let worker_requests = Arc::clone(&requests);
            let worker_stop = Arc::clone(&stop);
            let worker = thread::spawn(move || {
                let mut replies = VecDeque::from(replies);
                'accept: while !worker_stop.load(Ordering::SeqCst) {
                    let (mut socket, _) = match listener.accept() {
                        Ok(pair) => pair,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(1));
                            continue;
                        }
                        Err(error) => panic!("accept failed: {error}"),
                    };
                    // Darwin can inherit the listener's nonblocking mode.
                    socket.set_nonblocking(false).unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = Vec::new();
                    let mut buffer = [0; 4096];
                    loop {
                        let count = match socket.read(&mut buffer) {
                            Ok(0) | Err(_) => continue 'accept,
                            Ok(count) => count,
                        };
                        request.extend_from_slice(&buffer[..count]);
                        if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&request[..end]);
                            let length: usize = headers
                                .lines()
                                .find_map(|line| {
                                    let (name, value) = line.split_once(':')?;
                                    name.eq_ignore_ascii_case("content-length")
                                        .then(|| value.trim().parse().unwrap())
                                })
                                .unwrap_or(0);
                            if request.len() >= end + 4 + length {
                                break;
                            }
                        }
                    }
                    worker_requests
                        .lock()
                        .unwrap()
                        .push(String::from_utf8(request).unwrap());
                    let reply =
                        replies
                            .pop_front()
                            .unwrap_or(Reply::Http(400, "", "unexpected request"));
                    match reply {
                        Reply::Http(status, headers, body) => {
                            let response = format!(
                                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                                body.len()
                            );
                            let _ = socket.write_all(response.as_bytes());
                        }
                        Reply::HttpBytes(status, body) => {
                            let headers = format!(
                                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = socket.write_all(headers.as_bytes());
                            let _ = socket.write_all(body);
                        }
                        Reply::TruncatedHttpBody(status) | Reply::StallHttpBody(status) => {
                            let response = format!(
                                "HTTP/1.1 {status} Test\r\nContent-Length: 100\r\n\r\nPRIVATE_PREFIX"
                            );
                            let _ = socket.write_all(response.as_bytes());
                            if matches!(reply, Reply::StallHttpBody(_)) {
                                while !worker_stop.load(Ordering::SeqCst) {
                                    thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }
                        Reply::Disconnect => {}
                        Reply::TruncatedBody => {
                            let _ = socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{");
                        }
                        Reply::Stall | Reply::StallBody => {
                            if matches!(reply, Reply::StallBody) {
                                let _ = socket
                                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{");
                            }
                            while !worker_stop.load(Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(1));
                            }
                        }
                    }
                }
            });
            Self {
                url,
                requests,
                stop,
                worker: Some(worker),
            }
        }

        fn request(&self) -> RequestBuilder {
            client()
                .unwrap()
                .post(&self.url)
                .bearer_auth("test-secret")
                .header("anthropic-version", "2023-06-01")
                .json(&serde_json::json!({
                    "model": "test",
                    "messages": [{"role": "user", "content": [{
                        "type": "tool_result", "tool_use_id": "already-executed", "content": "done"
                    }]}]
                }))
        }

        fn model(&self) -> Arc<ModelClient> {
            self.model_with_recorder(Recorder::disabled())
        }

        pub(crate) fn model_with_recorder(&self, recorder: Recorder) -> Arc<ModelClient> {
            self.configured_model(recorder, PromptCache::default())
        }

        fn model_with_prompt_cache(&self, prompt_cache: PromptCache) -> Arc<ModelClient> {
            self.configured_model(Recorder::disabled(), prompt_cache)
        }

        fn configured_model(
            &self,
            recorder: Recorder,
            prompt_cache: PromptCache,
        ) -> Arc<ModelClient> {
            Arc::new(
                ModelClient::new(
                    client().unwrap(),
                    OpenRouter::new("test-secret".into(), Some("test-provider".into()))
                        .with_base_url(self.url.strip_suffix("/messages").unwrap().to_owned()),
                    AnthropicMessages,
                    "test".into(),
                    recorder,
                )
                .with_prompt_cache(prompt_cache),
            )
        }

        pub(crate) fn count(&self) -> usize {
            self.requests.lock().unwrap().len()
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.worker.take().unwrap().join().unwrap();
        }
    }

    fn fast_policy() -> Policy {
        Policy {
            backoff: Duration::from_millis(1),
            attempt_timeout: Duration::from_secs(2),
        }
    }

    // All callers here use a nonempty system, tools, and user/tool-result tail.
    // This asserts the wire request, not a remote cache hit.
    fn assert_prompt_cache_anchors(body: &Value, prompt_cache: PromptCache) {
        let system = &body["system"];
        let tool = body["tools"].as_array().unwrap().last().unwrap();
        let tail = &body["messages"].as_array().unwrap().last().unwrap()["content"];
        match prompt_cache {
            PromptCache::Short => {
                let marker = serde_json::json!({"type":"ephemeral"});
                assert_eq!(system[0]["cache_control"], marker);
                assert_eq!(tool["cache_control"], marker);
                assert_eq!(
                    tail.as_array().unwrap().last().unwrap()["cache_control"],
                    marker
                );
            }
            PromptCache::Off => {
                assert!(system.is_string());
                assert!(tool.get("cache_control").is_none());
                if let Some(blocks) = tail.as_array() {
                    assert!(
                        blocks
                            .iter()
                            .all(|block| block.get("cache_control").is_none())
                    );
                } else {
                    assert!(tail.is_string());
                }
            }
        }
        assert!(body.get("cache_control").is_none());
    }

    #[cfg(unix)]
    mod agent_diagnostics;
    #[cfg(unix)]
    pub(crate) mod diagnostics;
    #[cfg(unix)]
    mod tool_diagnostics;

    #[tokio::test]
    async fn success_uses_one_attempt_through_the_production_interface() {
        let server = Server::new(vec![Reply::Http(200, "", SUCCESS)]);
        let body = send(server.request()).await.unwrap();
        assert_eq!(body["content"][0]["text"], "done");
        assert_eq!(server.count(), 1);
    }

    #[tokio::test]
    async fn agent_retry_after_a_tool_does_not_reexecute_it_or_duplicate_history() {
        use crate::{Agent, ToolRegistry, events::EventKind};
        for prompt_cache in [PromptCache::Short, PromptCache::Off] {
            let server = Server::new(vec![
                Reply::Http(
                    200,
                    "",
                    r#"{"content":[{"type":"tool_use","id":"one-call","name":"bash","input":{"command":"printf one-execution"}}]}"#,
                ),
                Reply::Http(503, "", "temporarily unavailable"),
                Reply::Http(200, "", SUCCESS),
            ]);
            let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut agent = Agent {
                model: server.model_with_prompt_cache(prompt_cache),
                tools: ToolRegistry::new(),
                events,
                source: "main",
                messages: Vec::new(),
            };
            let text = tokio::time::timeout(
                Duration::from_secs(10),
                agent.run("use the tool then reply"),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(text, "done");
            assert_eq!(agent.messages.len(), 4);
            let Message::ToolResults(results) = &agent.messages[2] else {
                panic!("expected tool results");
            };
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].tool_call_id, "one-call");
            assert_eq!(results[0].content, "one-execution\n[exit 0]");
            let mut started = 0;
            let mut finished = 0;
            let mut replies = 0;
            while let Ok(event) = receiver.try_recv() {
                match event.kind {
                    EventKind::ToolStarted { .. } => started += 1,
                    EventKind::ToolFinished { is_error, .. } => {
                        assert!(!is_error);
                        finished += 1;
                    }
                    EventKind::AssistantText(..) => replies += 1,
                    _ => {}
                }
            }
            assert_eq!((started, finished, replies), (1, 1, 1));
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[1], requests[2]);
            assert!(requests[1].contains("test-provider"));
            for request in requests.iter() {
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("x-openrouter-cache: true")
                );
                assert!(
                    request
                        .to_ascii_lowercase()
                        .contains("anthropic-version: 2023-06-01")
                );
            }
            let body: Value =
                serde_json::from_str(requests[1].split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["messages"][2]["role"], "user");
            assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "one-call");
            assert_prompt_cache_anchors(&body, prompt_cache);
            // Generated markers never enter the agent's durable user/tool history.
            assert!(
                matches!(&agent.messages[0], Message::User(text) if text == "use the tool then reply")
            );
        }
    }

    #[tokio::test]
    async fn parent_and_real_child_share_provider_protocol_and_cache_but_not_history() {
        use crate::{Agent, ToolRegistry, events::EventKind};
        for prompt_cache in [PromptCache::Short, PromptCache::Off] {
            let server = Server::new(vec![
                Reply::Http(200, "", SUCCESS),
                Reply::Http(
                    200,
                    "",
                    r#"{"content":[{"type":"tool_use","id":"child-call","name":"bash","input":{"command":"printf child-execution"}}]}"#,
                ),
                Reply::Http(200, "", SUCCESS),
            ]);
            let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut parent = Agent {
                model: server.model_with_prompt_cache(prompt_cache),
                tools: ToolRegistry::new(),
                events,
                source: "main",
                messages: Vec::new(),
            };
            parent
                .tools
                .enable_sub_agent(Arc::clone(&parent.model), parent.events.clone());
            assert_eq!(parent.run("parent-only-secret").await.unwrap(), "done");
            let (text, is_error) = tokio::time::timeout(
                Duration::from_secs(10),
                parent.tools.execute(
                    "sub_agent",
                    &serde_json::json!({"prompt": "child-only-task"}),
                ),
            )
            .await
            .unwrap();
            assert!(!is_error, "{text}");
            assert_eq!(text, "done");
            assert_eq!(parent.messages.len(), 2);
            let mut child_tool_calls = 0;
            while let Ok(event) = receiver.try_recv() {
                if matches!(event.kind, EventKind::ToolStarted { .. }) {
                    assert_eq!(event.source, "subagent");
                    child_tool_calls += 1;
                }
            }
            assert_eq!(child_tool_calls, 1);
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 3);
            for request in requests.iter() {
                let headers = request
                    .split_once("\r\n\r\n")
                    .unwrap()
                    .0
                    .to_ascii_lowercase();
                assert!(headers.contains("authorization: bearer test-secret"));
                assert!(headers.contains("x-openrouter-cache: true"));
                assert!(headers.contains("anthropic-version: 2023-06-01"));
                assert!(request.contains("test-provider"));
                let body: Value =
                    serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
                assert_prompt_cache_anchors(&body, prompt_cache);
            }
            assert!(requests[0].contains("parent-only-secret"));
            assert!(!requests[1].contains("parent-only-secret"));
            assert!(requests[1].contains("child-only-task"));
            assert!(requests[2].contains("child-call"));
            assert!(requests[2].contains("child-execution"));
        }
    }

    #[tokio::test]
    async fn prompt_cache_rejection_does_not_retry_or_silently_change_policy() {
        let server = Server::new(vec![Reply::Http(400, "", "unsupported cache control")]);
        let model = server.model();
        let error = model
            .complete_with_policy(
                "main",
                "instructions",
                &[Message::User("task".into())],
                &[serde_json::json!({"name":"fixture", "input_schema":{"type":"object"}})],
                fast_policy(),
            )
            .await
            .err()
            .unwrap();
        assert!(format!("{error:#}").contains("400"));
        assert_eq!(server.count(), 1, "no hidden retry with caching disabled");
        let requests = server.requests.lock().unwrap();
        let body: Value =
            serde_json::from_str(requests[0].split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_prompt_cache_anchors(&body, PromptCache::Short);
    }

    #[tokio::test]
    async fn malformed_protocol_reply_never_retries_or_executes_partial_tool_calls() {
        use crate::{Agent, ToolRegistry, events::EventKind};
        let server = Server::new(vec![Reply::Http(
            200,
            "",
            r#"{"content":[{"type":"tool_use","id":"valid","name":"bash","input":{"command":"printf should-not-run"}},{"type":"tool_use","name":"bash","input":{}}]}"#,
        )]);
        let (events, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut agent = Agent {
            model: server.model(),
            tools: ToolRegistry::new(),
            events,
            source: "main",
            messages: Vec::new(),
        };
        assert!(
            agent
                .run("test malformed response")
                .await
                .unwrap_err()
                .to_string()
                .contains("no ID")
        );
        assert_eq!(server.count(), 1);
        assert_eq!(agent.messages.len(), 1);
        while let Ok(event) = receiver.try_recv() {
            assert!(!matches!(event.kind, EventKind::ToolStarted { .. }));
        }
    }

    #[tokio::test]
    async fn retries_transient_statuses_even_with_non_json_error_bodies() {
        for status in [408, 429, 500, 502, 503, 504, 529] {
            let server = Server::new(vec![
                Reply::Http(status, "", "<html>busy</html>"),
                Reply::Http(200, "", SUCCESS),
            ]);
            let body = send_with_policy(server.request(), fast_policy())
                .await
                .unwrap();
            assert_eq!(body["content"][0]["text"], "done");
            let requests = server.requests.lock().unwrap();
            assert_eq!(requests.len(), 2, "status {status}");
            assert_eq!(
                requests[0], requests[1],
                "must replay the exact buffered request"
            );
            assert!(requests[0].contains("already-executed"));
        }
    }

    #[tokio::test]
    async fn permanent_statuses_and_invalid_success_json_do_not_retry() {
        for status in [400, 401, 403, 404, 413, 422, 501, 505] {
            let server = Server::new(vec![Reply::Http(status, "", "secret error payload")]);
            let error = send_with_policy(server.request(), fast_policy())
                .await
                .unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains(&status.to_string()));
            assert!(message.contains("1 attempt(s)"));
            assert!(!message.contains("secret"));
            assert_eq!(server.count(), 1);
        }
        let server = Server::new(vec![Reply::Http(200, "", "not JSON")]);
        let error = send_with_policy(server.request(), fast_policy())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("invalid model response"));
        assert_eq!(server.count(), 1);
    }

    #[tokio::test]
    async fn exhausted_budget_is_four_attempts_and_resets_for_the_next_request() {
        let server = Server::new((0..8).map(|_| Reply::Http(503, "", "busy")).collect());
        for expected in [4, 8] {
            let error = send_with_policy(server.request(), fast_policy())
                .await
                .unwrap_err();
            assert!(error.to_string().contains("503"));
            assert!(error.to_string().contains("4 attempt(s)"));
            assert_eq!(server.count(), expected);
        }
    }

    #[tokio::test]
    async fn disconnects_and_incomplete_bodies_retry_without_exposing_partial_output() {
        for reply in [Reply::Disconnect, Reply::TruncatedBody] {
            let server = Server::new(vec![reply, Reply::Http(200, "", SUCCESS)]);
            assert_eq!(
                send_with_policy(server.request(), fast_policy())
                    .await
                    .unwrap()["content"][0]["text"],
                "done"
            );
            assert_eq!(server.count(), 2);
        }
    }

    #[tokio::test]
    async fn timeouts_are_bounded_and_report_the_attempt_count() {
        let server = Server::new(vec![Reply::Stall]);
        let policy = Policy {
            attempt_timeout: Duration::from_millis(20),
            ..fast_policy()
        };
        let error = tokio::time::timeout(
            Duration::from_secs(2),
            send_with_policy(server.request(), policy),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("4 attempt(s)"));
        assert!(
            error.downcast_ref::<reqwest::Error>().unwrap().is_timeout(),
            "{error:#}"
        );
    }

    #[test]
    fn backoff_and_retry_after_are_bounded_without_shortening_server_delays() {
        let policy = Policy::default();
        for (retry, seconds) in [(0, 1), (1, 2), (2, 4)] {
            assert_eq!(
                policy.delay(retry, None).unwrap(),
                Duration::from_secs(seconds)
            );
        }
        for (header, seconds) in [
            ("0", 4),
            (" 12 ", 12),
            ("30", 30),
            ("garbage", 4),
            ("-1", 4),
        ] {
            assert_eq!(
                policy.delay(2, Some(header)).unwrap(),
                Duration::from_secs(seconds)
            );
        }
        assert!(policy.delay(0, Some("31")).is_err());
        assert!(policy.delay(0, Some("18446744073709551615")).is_err());
        assert!(policy.delay(0, Some("99999999999999999999999")).is_err());
    }

    #[test]
    fn retry_after_understands_http_dates_and_ignores_invalid_values() {
        let now = httpdate::parse_http_date("Wed, 21 Oct 2015 07:28:00 GMT").unwrap();
        assert_eq!(
            retry_after_delay("Wed, 21 Oct 2015 07:28:12 GMT", now).unwrap(),
            Some(Duration::from_secs(12))
        );
        assert_eq!(
            retry_after_delay("Wed, 21 Oct 2015 07:27:00 GMT", now).unwrap(),
            Some(Duration::ZERO)
        );
        assert_eq!(retry_after_delay("invalid", now).unwrap(), None);
        assert_eq!(retry_after_delay("", now).unwrap(), None);
    }

    #[tokio::test]
    async fn excessive_retry_after_stops_instead_of_retrying_early() {
        let server = Server::new(vec![Reply::Http(429, "Retry-After: 60\r\n", "busy")]);
        let error = send_with_policy(server.request(), fast_policy())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("30-second"));
        assert_eq!(server.count(), 1);
    }

    #[tokio::test]
    async fn cancelling_during_backoff_leaves_no_background_retry() {
        let server = Server::new(vec![Reply::Http(429, "Retry-After: 1\r\n", "busy")]);
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            send_with_policy(server.request(), fast_policy()),
        )
        .await;
        assert!(result.is_err(), "Retry-After must delay the second attempt");
        tokio::time::sleep(Duration::from_millis(1050)).await;
        assert_eq!(server.count(), 1);
    }
}
