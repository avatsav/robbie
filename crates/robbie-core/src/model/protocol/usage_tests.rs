use super::*;
use crate::usage::ReportedUsage;

#[test]
fn messages_categories_are_disjoint_and_cache_tiers_are_not_added_twice() {
    let metadata = AnthropicMessages.response_metadata(&json!({"usage": {
        "input_tokens": 100, "output_tokens": 5,
        "cache_read_input_tokens": 80, "cache_creation_input_tokens": 30,
        "cache_creation": {"ephemeral_5m_input_tokens":20,"ephemeral_1h_input_tokens":10},
        "output_tokens_details": {"reasoning_tokens": 4}, "total_tokens": 99999
    }}));
    let usage = AnthropicMessages.reported_usage(&metadata);
    assert_eq!(
        usage,
        ReportedUsage {
            input: Some(100),
            output: Some(5),
            cache_read: Some(80),
            cache_write: Some(30)
        }
    );
    assert_eq!(usage.context_tokens(), Some(215));
}

#[test]
fn missing_null_or_malformed_counters_are_unknown_not_zero() {
    for body in [
        json!({}),
        json!({"usage": null}),
        json!({"usage": {}}),
        json!({"usage": {"input_tokens":-1,"output_tokens":"12","cache_read_input_tokens":null,"cache_creation_input_tokens":false}}),
        json!({"usage": {"prompt_tokens":10,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":5}}}),
    ] {
        let metadata = AnthropicMessages.response_metadata(&body);
        assert_eq!(
            AnthropicMessages.reported_usage(&metadata),
            ReportedUsage::default()
        );
    }
    let partial = AnthropicMessages.response_metadata(&json!({"usage": {
        "input_tokens": 10, "output_tokens": 20,
        "cache_creation": {"ephemeral_5m_input_tokens":5,"ephemeral_1h_input_tokens":0}
    }}));
    let usage = AnthropicMessages.reported_usage(&partial);
    assert_eq!(
        usage,
        ReportedUsage {
            input: Some(10),
            output: Some(20),
            ..ReportedUsage::default()
        }
    );
    assert_eq!(usage.context_tokens(), None);
}

#[test]
fn explicit_zero_is_reported_even_though_an_all_zero_baseline_is_unusable() {
    let metadata = AnthropicMessages.response_metadata(&json!({"usage": {
        "input_tokens":0,"output_tokens":0,"cache_read_input_tokens":0,"cache_creation_input_tokens":0
    }}));
    let usage = AnthropicMessages.reported_usage(&metadata);
    assert_eq!(
        usage,
        ReportedUsage {
            input: Some(0),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0)
        }
    );
    assert_eq!(usage.context_tokens(), None);
}
