//! Connection/authentication policy for an API host, independent of wire format.
use anyhow::Result;
use reqwest::{Client, RequestBuilder};
use serde_json::{Value, json};

use super::protocol::EncodedRequest;

mod limits;
pub use limits::{LimitResolution, LimitSource, ModelLimits};

// Retain the final JSON body independently of headers/authentication. Diagnostics
// can capture exactly the provider-prepared protocol payload without inspecting
// or serializing a credential-bearing HTTP request.
pub struct PreparedRequest {
    pub request: RequestBuilder,
    pub body: Value,
}

pub trait Provider {
    fn prepare(&self, http: &Client, request: EncodedRequest) -> Result<PreparedRequest>;
}

pub struct OpenRouter {
    api_key: String,
    // This selects a backend within OpenRouter, not Robbie's API host.
    upstream: Option<String>,
    base_url: String,
}

impl OpenRouter {
    pub fn new(api_key: String, upstream: Option<String>) -> Self {
        Self {
            api_key,
            upstream,
            base_url: "https://openrouter.ai/api/v1".into(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }
}

impl Provider for OpenRouter {
    fn prepare(&self, http: &Client, mut request: EncodedRequest) -> Result<PreparedRequest> {
        if let Some(upstream) = &self.upstream {
            request.body["provider"] = json!({"only": [upstream]});
        }
        let prepared = http
            .post(format!(
                "{}{}",
                self.base_url.trim_end_matches('/'),
                request.path
            ))
            .headers(request.headers)
            .bearer_auth(&self.api_key)
            // Full-response caching, not prompt caching or idempotency. It can
            // miss/expire and concurrent requests can still both be billed.
            // Uses OpenRouter's default TTL; cached responses may include tools.
            .header("X-OpenRouter-Cache", "true")
            .json(&request.body);
        Ok(PreparedRequest {
            request: prepared,
            body: request.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnthropicMessages, Message, PromptCache, Protocol};
    use serde_json::Value;

    #[test]
    fn openrouter_adds_auth_routing_and_response_cache_without_losing_protocol_fields() {
        let http = Client::new();
        let encoded = AnthropicMessages.encode(
            "test/model",
            "instructions",
            &[Message::User("hi".into())],
            &[],
            PromptCache::Short,
        );
        let request = OpenRouter::new("test-key".into(), Some("test-upstream".into()))
            .prepare(&http, encoded)
            .unwrap()
            .request
            .build()
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://openrouter.ai/api/v1/messages"
        );
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.headers()["authorization"], "Bearer test-key");
        assert_eq!(request.headers()["anthropic-version"], "2023-06-01");
        assert_eq!(request.headers()["x-openrouter-cache"], "true");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body["model"], "test/model");
        assert_eq!(body["provider"], json!({"only": ["test-upstream"]}));
        assert_eq!(body["messages"][0]["content"][0]["text"], "hi");
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            json!({"type": "ephemeral"})
        );
    }

    #[test]
    fn provider_does_not_choose_or_invent_protocol_headers() {
        let encoded = EncodedRequest {
            path: "/another-protocol",
            headers: reqwest::header::HeaderMap::new(),
            body: json!({"some_protocol_field": "untouched"}),
        };
        let request = OpenRouter::new("test".into(), None)
            .prepare(&Client::new(), encoded)
            .unwrap()
            .request
            .build()
            .unwrap();
        assert_eq!(
            request.url().as_str(),
            "https://openrouter.ai/api/v1/another-protocol"
        );
        assert!(!request.headers().contains_key("anthropic-version"));
        assert_eq!(request.headers()["x-openrouter-cache"], "true");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body, json!({"some_protocol_field": "untouched"}));
    }

    #[test]
    fn absent_upstream_does_not_invent_a_routing_restriction() {
        let encoded = AnthropicMessages.encode("test", "", &[], &[], PromptCache::Short);
        let original = encoded.body.clone();
        let request = OpenRouter::new("test".into(), None)
            .prepare(&Client::new(), encoded)
            .unwrap()
            .request
            .build()
            .unwrap();
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body, original);
        assert!(body.get("provider").is_none());
    }
}
