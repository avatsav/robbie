//! Retry a single, buffered model request, never an agent turn or tool execution.
//!
//! Retrying a POST after a lost response can duplicate provider work/billing. It
//! cannot replay local tools: callers only consume a successfully decoded reply.
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use reqwest::{Client, RequestBuilder, StatusCode, header::RETRY_AFTER};
use serde_json::Value;

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

pub async fn send(request: RequestBuilder) -> Result<Value> {
    send_with_policy(request, Policy::default()).await
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
    fn delay(self, retry: usize, retry_after: Option<&str>) -> Result<Duration> {
        let backoff = self.backoff * (1 << retry);
        let requested = retry_after
            .map(|value| retry_after_delay(value, SystemTime::now()))
            .transpose()?
            .flatten()
            .unwrap_or_default();
        let delay = backoff.max(requested);
        // Never shorten a server-requested wait and hammer it early.
        if delay > MAX_RETRY_WAIT {
            bail!("Retry-After exceeds the 30-second automatic wait limit");
        }
        Ok(delay)
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

async fn send_with_policy(request: RequestBuilder, policy: Policy) -> Result<Value> {
    for retry in 0..=MAX_RETRIES {
        let attempt = retry + 1;
        // JSON requests are buffered/cloneable; streaming bodies must not be
        // partially consumed and then silently replayed.
        let next = request
            .try_clone()
            .context("model request body cannot be replayed")?
            .timeout(policy.attempt_timeout);
        let outcome = async {
            let response = next.send().await?;
            let status = response.status();
            if !status.is_success() {
                let retry_after = response
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                // Status classification must not depend on an error body's
                // format: gateways commonly return HTML rather than JSON.
                return Ok(Err((status, retry_after)));
            }
            response.bytes().await.map(Ok)
        }
        .await;

        let delay = match outcome {
            Ok(Ok(bytes)) => {
                // A fully received but invalid response is not a transport
                // failure. Do not retry JSON errors or execute partial output.
                return serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid model response after {attempt} attempt(s)"));
            }
            Ok(Err((status, retry_after))) => {
                let message = format!("model API returned {status} after {attempt} attempt(s)");
                if !retryable_status(status) || retry == MAX_RETRIES {
                    bail!(message);
                }
                policy
                    .delay(retry, retry_after.as_deref())
                    .with_context(|| message)?
            }
            Err(error) => {
                let retryable = retryable_transport(&error);
                // URLs may contain sensitive parameters; do not expose them.
                let error = error.without_url();
                if !retryable || retry == MAX_RETRIES {
                    return Err(error).with_context(|| {
                        format!("model request failed after {attempt} attempt(s)")
                    });
                }
                policy.delay(retry, None)?
            }
        };
        // Dropping this future cancels the wait/request; no detached retry task.
        tokio::time::sleep(delay).await;
    }
    unreachable!("the final attempt always returns")
}

#[cfg(test)]
mod tests {
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

    enum Reply {
        Http(u16, &'static str, &'static str),
        Disconnect,
        TruncatedBody,
        Stall,
    }

    struct Server {
        url: String,
        requests: Arc<Mutex<Vec<String>>>,
        stop: Arc<AtomicBool>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Server {
        fn new(replies: Vec<Reply>) -> Self {
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
                    match replies
                        .pop_front()
                        .unwrap_or(Reply::Http(400, "", "unexpected request"))
                    {
                        Reply::Http(status, headers, body) => {
                            let response = format!(
                                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}",
                                body.len()
                            );
                            let _ = socket.write_all(response.as_bytes());
                        }
                        Reply::Disconnect => {}
                        Reply::TruncatedBody => {
                            let _ = socket
                                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{");
                        }
                        Reply::Stall => {
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

        fn count(&self) -> usize {
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
            client: client().unwrap(),
            api_key: "test-secret".into(),
            model: "test".into(),
            provider: Some("test-provider".into()),
            tools: ToolRegistry::new(),
            events,
            source: "main",
            messages: Vec::new(),
        };
        let text = tokio::time::timeout(
            Duration::from_secs(10),
            agent.run_at("use the tool then reply", &server.url),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(text, "done");
        assert_eq!(agent.messages.len(), 4);
        assert_eq!(agent.messages[2]["content"][0]["tool_use_id"], "one-call");
        assert_eq!(
            agent.messages[2]["content"][0]["content"],
            "one-execution\n[exit 0]"
        );
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
                EventKind::AssistantText(_) => replies += 1,
                _ => {}
            }
        }
        assert_eq!((started, finished, replies), (1, 1, 1));
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[1], requests[2]);
        assert!(requests[1].contains("test-provider"));
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
