use super::*;
use robbie_core::ModelLimits;

#[test]
fn model_derived_options_bound_recent_input_and_output_without_a_trigger_reserve() {
    for (window, max_input, max_output) in [
        (131_072, None, None),
        (32_768, Some(16_000), Some(512)),
        // Explicit actions need headroom, not the automatic 16K trigger reserve.
        (16_000, None, Some(512)),
    ] {
        let options = CompactionOptions::for_model(ModelLimits {
            context_window: window,
            max_input_tokens: max_input,
            max_output_tokens: max_output,
        })
        .unwrap();
        assert!(options.keep_recent_tokens <= 20_000);
        assert!(options.max_output_tokens <= 8_000);
        assert!(u64::from(options.max_output_tokens) <= max_output.unwrap_or(u64::MAX));
        let input_tokens = options.max_input_chars as u64 / 4;
        assert!(input_tokens <= max_input.unwrap_or(u64::MAX));
        assert!(input_tokens + u64::from(options.max_output_tokens) + 4_096 <= window);
    }
    let options = CompactionOptions::for_model(ModelLimits {
        context_window: 131_072,
        max_input_tokens: None,
        max_output_tokens: None,
    })
    .unwrap();
    assert_eq!(options.keep_recent_tokens, 20_000);
    assert_eq!(options.max_output_tokens, 8_000);
}

#[test]
fn model_derived_options_reject_invalid_or_impossible_limits() {
    for limits in [
        ModelLimits {
            context_window: 0,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        ModelLimits {
            context_window: 32_768,
            max_input_tokens: Some(0),
            max_output_tokens: None,
        },
        ModelLimits {
            context_window: 32_768,
            max_input_tokens: None,
            max_output_tokens: Some(0),
        },
        ModelLimits {
            context_window: 4_096,
            max_input_tokens: None,
            max_output_tokens: None,
        },
    ] {
        assert!(CompactionOptions::for_model(limits).is_err());
    }
}
