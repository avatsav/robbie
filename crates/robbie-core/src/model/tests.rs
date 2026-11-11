use super::*;
use crate::test_support::{Reply, SUCCESS, Server};

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
mod diagnostics;

mod requests;

mod budgets;

mod agent;

mod retries;
