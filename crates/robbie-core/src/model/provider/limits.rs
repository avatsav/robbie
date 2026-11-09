//! OpenRouter metadata, not inference. Never recorded as a conversation or usage report.
use std::time::Duration;

use anyhow::{Result, ensure};
use reqwest::{Client, Url};
use serde_json::Value;

use super::OpenRouter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelLimits {
    pub context_window: u64,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

impl ModelLimits {
    pub(crate) fn validate(self) -> Result<Self> {
        ensure!(
            self.context_window > 0
                && self.max_input_tokens != Some(0)
                && self.max_output_tokens != Some(0),
            "model limits must be positive"
        );
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitSource {
    Configured,
    OpenRouter,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitResolution {
    pub limits: Option<ModelLimits>,
    pub source: LimitSource,
}

impl LimitResolution {
    pub fn warning(self) -> Option<&'static str> {
        self.limits.is_none().then_some("Model context limit is unknown; automatic compaction is disabled. Set ROBBIE_CONTEXT_WINDOW or provide explicit model limits.")
    }
}

fn positive(value: &Value) -> Option<u64> {
    value.as_u64().filter(|value| *value > 0)
}

fn entry_limits(entry: &Value) -> Option<ModelLimits> {
    let context_window = match (
        positive(&entry["context_length"]),
        positive(&entry["top_provider"]["context_length"]),
    ) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(ModelLimits {
        context_window,
        max_input_tokens: positive(&entry["max_prompt_tokens"]),
        max_output_tokens: positive(&entry["max_completion_tokens"])
            .or_else(|| positive(&entry["top_provider"]["max_completion_tokens"])),
    })
}

fn route_matches(entry: &Value, route: &str) -> bool {
    let route = route.to_ascii_lowercase();
    ["tag", "provider_name"]
        .into_iter()
        .filter_map(|field| entry[field].as_str())
        .any(|value| {
            let value = value.to_ascii_lowercase();
            value == route
                || value
                    .strip_prefix(&route)
                    .is_some_and(|suffix| suffix.starts_with('/'))
        })
}

fn endpoint_limits(data: &Value, route: &str) -> Option<ModelLimits> {
    let mut matched = data["data"]["endpoints"]
        .as_array()?
        .iter()
        .filter(|entry| route_matches(entry, route));
    let mut limits = entry_limits(matched.next()?)?;
    for entry in matched {
        let next = entry_limits(entry)?; // Unknown capacity in any allowed route is not the largest known capacity.
        limits.context_window = limits.context_window.min(next.context_window);
        limits.max_input_tokens = match (limits.max_input_tokens, next.max_input_tokens) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        limits.max_output_tokens = match (limits.max_output_tokens, next.max_output_tokens) {
            (Some(a), Some(b)) => Some(a.min(b)),
            _ => None,
        };
    }
    Some(limits)
}

impl OpenRouter {
    async fn metadata(&self, http: &Client, url: Url) -> Option<Value> {
        let mut response = http
            .get(url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(4))
            .send()
            .await
            .ok()?;
        if !response.status().is_success() {
            return None;
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if body.len().saturating_add(chunk.len()) > 8 * 1024 * 1024 {
                return None;
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&body).ok()
    }

    pub(crate) async fn discover_limits(&self, http: &Client, model: &str) -> Option<ModelLimits> {
        // Segment construction prevents model IDs from changing the host or injecting query/path syntax.
        let mut url =
            Url::parse(&format!("{}/models", self.base_url.trim_end_matches('/'))).ok()?;
        if let Some(route) = &self.upstream {
            let mut segments = url.path_segments_mut().ok()?;
            for segment in model.split('/') {
                segments.push(segment);
            }
            segments.push("endpoints");
            drop(segments);
            return endpoint_limits(&self.metadata(http, url).await?, route);
        }
        let public_url = url.clone();
        url.path_segments_mut().ok()?.push("user");
        let data = match self.metadata(http, url).await {
            Some(data) => data,
            None => self.metadata(http, public_url).await?,
        };
        let entry = data["data"]
            .as_array()?
            .iter()
            .find(|entry| entry["id"].as_str() == Some(model))?;
        entry_limits(entry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn metadata_uses_get_auth_and_a_separate_non_inference_profile() {
        use crate::test_support::{Reply, Server};
        let server = Server::new(vec![Reply::Http(
            200,
            "",
            r#"{"data":{"endpoints":[{"tag":"test-provider","context_length":64000}]}}"#,
        )]);
        let model = server.model();
        assert_eq!(
            model
                .resolve_limits(None)
                .await
                .unwrap()
                .limits
                .unwrap()
                .context_window,
            64000
        );
        let requests = server.requests.lock().unwrap();
        assert!(requests[0].starts_with("GET /models/test/endpoints HTTP/1.1"));
        let headers = requests[0].to_ascii_lowercase();
        assert!(headers.contains("authorization: bearer test-secret"));
        assert!(!headers.contains("anthropic-version"));
        assert!(!headers.contains("x-openrouter-cache"));
        assert_eq!(model.usage_totals().responses, 0);
    }

    #[test]
    fn pinned_routes_use_the_smallest_allowed_capacity_not_the_aggregate() {
        let data = json!({"data":{"endpoints":[
            {"tag":"fixture/a","context_length":128000,"max_completion_tokens":8000},
            {"tag":"fixture/b","context_length":64000,"max_completion_tokens":4000},
            {"tag":"other","context_length":1000000}
        ]}});
        let limits = endpoint_limits(&data, "fixture").unwrap();
        assert_eq!(limits.context_window, 64000);
        assert_eq!(limits.max_output_tokens, Some(4000));
        assert_eq!(
            endpoint_limits(&data, "fixture/a").unwrap().context_window,
            128000
        );
        assert!(endpoint_limits(&data, "missing").is_none());
        let unknown = json!({"data":{"endpoints":[{"tag":"fixture/a","context_length":64000},{"tag":"fixture/b"}]}});
        assert!(endpoint_limits(&unknown, "fixture").is_none());
    }
}
