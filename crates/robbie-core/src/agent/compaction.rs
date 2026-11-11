//! Explicit compaction with safe selection, bounded preparation and atomic commit.
use std::collections::HashSet;

use anyhow::{Result, ensure};

use crate::{
    diagnostics::{compaction::End, model::RequestOrigin},
    model::{Message, ModelClient, Output},
};

mod summary;
mod transaction;

/// Explicit limits for one manual compaction. There are no runtime defaults.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Approximate recent-token target, not the model's context window.
    /// Uses serialized-message Unicode characters / 4, rounded up.
    pub keep_recent_tokens: usize,
    /// Limit for all prepared summary-input characters, including framing.
    pub max_input_chars: usize,
    /// Positive output-token cap sent to the summarization API.
    pub max_output_tokens: u32,
}

impl Options {
    fn limits(self) -> Result<transaction::Limits> {
        ensure!(
            self.max_input_chars > 0,
            "summary input limit must be positive"
        );
        ensure!(
            self.max_output_tokens > 0,
            "summary output limit must be positive"
        );
        Ok(transaction::Limits {
            keep_recent_tokens: self.keep_recent_tokens,
            max_input_chars: self.max_input_chars,
            max_output_tokens: self.max_output_tokens,
        })
    }
}

/// A successful manual operation either leaves context alone or commits a summary.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Unchanged,
    Compacted {
        retired_messages: usize,
        retained_messages: usize,
    },
}

pub(super) async fn compact(
    model: &ModelClient,
    origin: impl Into<RequestOrigin>,
    messages: &mut Vec<Message>,
    previous_summary: &mut Option<String>,
    options: Options,
) -> Result<Outcome> {
    let operation = model.compaction_span(origin, messages.len(), previous_summary.is_some());
    let result = match options.limits() {
        Ok(limits) => {
            transaction::compact_history(
                model,
                operation.origin(),
                messages,
                previous_summary,
                limits,
                |message| model.estimate_message_tokens(message),
            )
            .await
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(Some(transaction::Compacted {
            request_id,
            retired_messages,
            retained_messages,
            response: _response,
        })) => {
            operation.finish(End::Committed {
                request_id,
                retired_messages,
                retained_messages,
            });
            Ok(Outcome::Compacted {
                retired_messages,
                retained_messages,
            })
        }
        Ok(None) => {
            operation.finish(End::Unchanged);
            Ok(Outcome::Unchanged)
        }
        Err(error) => {
            operation.finish(End::Failed);
            Err(error)
        }
    }
}

/// Refuses incomplete or mismatched tool exchanges without repairing history.
fn find_cut(
    messages: &[Message],
    keep_recent_tokens: usize,
    estimate_tokens: impl Fn(&Message) -> usize,
) -> Result<Option<usize>> {
    validate_tool_exchanges(messages)?;

    let mut recent_tokens = 0usize;
    let mut first_kept = None;

    for (index, message) in messages.iter().enumerate().rev() {
        recent_tokens = recent_tokens.saturating_add(estimate_tokens(message));
        if !matches!(message, Message::ToolResults(_)) {
            first_kept = Some(index);
        }

        if recent_tokens >= keep_recent_tokens {
            // Prefer a legal start already visited. If the newest result alone
            // exceeds the target, keep its preceding assistant call too.
            let first_kept = first_kept.unwrap_or(index.saturating_sub(1));
            return Ok(if first_kept == 0 {
                None // Nothing before this point can be summarized.
            } else {
                Some(first_kept)
            });
        }
    }

    Ok(None) // The whole history fits within the recent target.
}

pub(super) fn validate_tool_exchanges(messages: &[Message]) -> Result<()> {
    const INVALID: &str = "cannot compact an incomplete or mismatched tool exchange";
    let mut pending = HashSet::new();

    for message in messages {
        match message {
            Message::User(_) => ensure!(pending.is_empty(), INVALID),
            Message::Assistant(reply) => {
                ensure!(pending.is_empty(), INVALID);
                for output in &reply.output {
                    if let Output::ToolCall(call) = output {
                        ensure!(pending.insert(call.id.as_str()), INVALID);
                    }
                }
            }
            Message::ToolResults(results) => {
                ensure!(!pending.is_empty(), INVALID);
                for result in results {
                    ensure!(pending.remove(result.tool_call_id.as_str()), INVALID);
                }
                ensure!(pending.is_empty(), INVALID);
            }
        }
    }

    ensure!(pending.is_empty(), INVALID);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AnthropicMessages, Protocol, ToolResult};
    use serde_json::json;

    fn calls(ids: &[&str]) -> Message {
        let blocks: Vec<_> = ids
            .iter()
            .map(|id| {
                json!({
                    "type": "tool_use", "id": id,
                    "name": "read_file", "input": {"path": "file.rs"}
                })
            })
            .collect();
        Message::Assistant(
            AnthropicMessages
                .decode(json!({"content": blocks}))
                .unwrap(),
        )
    }

    fn results(ids: &[&str]) -> Message {
        Message::ToolResults(
            ids.iter()
                .map(|id| ToolResult {
                    tool_call_id: (*id).into(),
                    content: "tool output".into(),
                    is_error: false,
                })
                .collect(),
        )
    }

    #[test]
    fn thousand_token_target_keeps_call_b_and_its_result() {
        let messages = [
            Message::User("Fix the failing tests.".into()),
            calls(&["A"]),
            results(&["A"]),
            calls(&["B"]),
            results(&["B"]),
        ];
        // The example's supplied sizes, not a real token estimator.
        let estimate_tokens = |message: &Message| match message {
            Message::User(_) => 200,
            Message::Assistant(_) => 100,
            Message::ToolResults(results) => match results[0].tool_call_id.as_str() {
                "A" => 3_000,
                "B" => 700,
                _ => panic!("unexpected fixture tool ID"),
            },
        };

        assert_eq!(
            find_cut(&messages, 1_000, estimate_tokens).unwrap(),
            Some(3)
        );
    }

    #[test]
    fn empty_and_small_histories_have_no_useful_cut() {
        assert_eq!(find_cut(&[], 1_000, |_| 1).unwrap(), None);
        let messages = [Message::User("goal".into()), calls(&["A"]), results(&["A"])];
        assert_eq!(find_cut(&messages, 1_000, |_| 1).unwrap(), None);
    }

    #[test]
    fn oversized_newest_result_keeps_its_assistant_call() {
        let messages = [Message::User("goal".into()), calls(&["A"]), results(&["A"])];
        let estimate_tokens = |message: &Message| match message {
            Message::User(_) => 200,
            Message::Assistant(_) => 100,
            Message::ToolResults(_) => 1_500,
        };
        assert_eq!(
            find_cut(&messages, 1_000, estimate_tokens).unwrap(),
            Some(1)
        );
    }

    #[test]
    fn completed_batches_allow_result_reordering_and_later_id_reuse() {
        let messages = [
            Message::User("goal".into()),
            calls(&["A", "B"]),
            results(&["B", "A"]),
            calls(&["A"]),
            results(&["A"]),
        ];
        assert_eq!(find_cut(&messages, 2, |_| 1).unwrap(), Some(3));
        assert_eq!(find_cut(&messages, usize::MAX, |_| 1).unwrap(), None);
    }

    #[test]
    fn incomplete_exchanges_fail_before_size_estimation() {
        for messages in [
            vec![calls(&["A"])],
            vec![calls(&["A"]), Message::User("next goal".into())],
            vec![calls(&["A"]), calls(&["B"])],
            vec![calls(&["A", "B"]), results(&["A"])],
            vec![calls(&["A"]), results(&[])],
        ] {
            assert!(
                find_cut(&messages, usize::MAX, |_| {
                    panic!("invalid history must fail before estimation")
                })
                .is_err()
            );
        }
    }

    #[test]
    fn orphan_duplicate_and_mismatched_ids_are_rejected() {
        for messages in [
            vec![results(&["A"])],
            vec![results(&[])],
            vec![calls(&["A", "A"]), results(&["A", "A"])],
            vec![calls(&["A", "B"]), results(&["A", "A"])],
            vec![calls(&["A"]), results(&["B"])],
            vec![calls(&["A"]), results(&["A", "B"])],
        ] {
            assert!(find_cut(&messages, 0, |_| 1).is_err());
        }
    }
}
