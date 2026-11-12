//! Loopback HTTP server and session fixtures; never calls a real provider.
#[cfg(test)]
use crate::{
    diagnostics::Recorder,
    model::{self, AnthropicMessages, ModelClient, OpenRouter, PromptCache},
};
#[cfg(test)]
use reqwest::RequestBuilder;
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

pub const SUCCESS: &str = r#"{"content":[{"type":"text","text":"done"}]}"#;

pub enum Reply {
    Http(u16, &'static str, &'static str),
    HttpBytes(u16, &'static [u8]),
    TruncatedHttpBody(u16),
    StallHttpBody(u16),
    Disconnect,
    TruncatedBody,
    Stall,
    StallBody,
}

pub struct Server {
    url: String,
    pub(crate) requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    pub fn new(replies: Vec<Reply>) -> Self {
        let mut replies = VecDeque::from(replies);
        Self::responding(move |_| {
            replies
                .pop_front()
                .unwrap_or(Reply::Http(400, "", "unexpected request"))
        })
    }

    /// Respond to captured requests, allowing fixtures to derive usage from body size.
    pub fn responding(mut respond: impl FnMut(&str) -> Reply + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}/messages", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_requests = Arc::clone(&requests);
        let worker_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
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
                let request = String::from_utf8(request).unwrap();
                let reply = respond(&request);
                worker_requests.lock().unwrap().push(request);
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
                        let _ =
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{");
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

    #[cfg(test)]
    pub(crate) fn request(&self) -> RequestBuilder {
        model::client()
            .unwrap()
            .post(&self.url)
            .bearer_auth("test-secret")
            .header("anthropic-version", "2023-06-01")
            .json(
                &serde_json::json!({"model":"test", "messages":[{"role":"user", "content":[{
                    "type":"tool_result", "tool_use_id":"already-executed", "content":"done"
                }]}]}),
            )
    }

    #[cfg(test)]
    pub(crate) fn model(&self) -> Arc<ModelClient> {
        self.model_with_recorder(Recorder::disabled())
    }

    #[cfg(test)]
    pub(crate) fn model_with_recorder(&self, recorder: Recorder) -> Arc<ModelClient> {
        self.configured_model(recorder, PromptCache::default())
    }

    #[cfg(test)]
    pub(crate) fn model_with_prompt_cache(&self, prompt_cache: PromptCache) -> Arc<ModelClient> {
        self.configured_model(Recorder::disabled(), prompt_cache)
    }

    #[cfg(test)]
    fn configured_model(&self, recorder: Recorder, prompt_cache: PromptCache) -> Arc<ModelClient> {
        Arc::new(
            ModelClient::new(
                model::client().unwrap(),
                OpenRouter::new("test-secret".into(), Some("test-provider".into()))
                    .with_base_url(self.url.strip_suffix("/messages").unwrap().to_owned()),
                AnthropicMessages,
                "test".into(),
                recorder,
            )
            .with_prompt_cache(prompt_cache),
        )
    }

    /// Construct a normal session whose model requests can only reach this server.
    #[cfg(unix)]
    pub async fn session(
        &self,
        config: crate::session::SessionConfig,
    ) -> anyhow::Result<(
        crate::session::Session,
        crate::session::ActivityReceiver,
        super::RecordingControl,
    )> {
        let base_url = self.url.strip_suffix("/messages").unwrap().to_owned();
        let (session, receiver) =
            crate::session::Session::connect(config, |provider| provider.with_base_url(base_url))
                .await?;
        let control = session.recording_control();
        Ok((session, receiver, control))
    }

    pub fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.take().unwrap().join().unwrap();
    }
}
