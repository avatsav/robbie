//! A summary request and commit, isolated from tool execution and the frontend.
use anyhow::{Result, ensure};

use crate::{
    diagnostics::model::RequestOrigin,
    model::{Message, ModelClient, Output, ResponseMetadata, StopReason},
};

pub(super) struct Limits {
    pub keep_recent_tokens: usize,
    pub max_input_chars: usize,
    pub max_output_tokens: u32,
}

#[derive(Debug)]
pub(super) struct Compacted {
    pub request_id: u64,
    pub retired_messages: usize,
    pub retained_messages: usize,
    // Usage describes the summary request, not the new conversation's size.
    pub response: ResponseMetadata,
}

use crate::model::summary_message;

/// The mutable borrows prevent appending messages during this transaction.
/// No state changes before the await or acceptance checks; dropping the future
/// cancels waiting, with the old history and summary still intact.
/// The supplied estimator must be pure and use comparable units for all messages.
pub(super) async fn compact_history(
    model: &ModelClient,
    origin: impl Into<RequestOrigin>,
    messages: &mut Vec<Message>,
    previous_summary: &mut Option<String>,
    limits: Limits,
    estimate_tokens: impl Fn(&Message) -> usize,
) -> Result<Option<Compacted>> {
    let Some(cut) = super::find_cut(messages, limits.keep_recent_tokens, &estimate_tokens)? else {
        return Ok(None);
    };
    let prompt = super::summary::prepare_summary(
        &messages[..cut],
        previous_summary.as_deref(),
        limits.max_input_chars,
    )?;
    let response = model
        .complete_summary(origin, prompt, limits.max_output_tokens)
        .await?;
    ensure!(
        response.metadata.stop_reason == Some(StopReason::EndTurn),
        "summary response did not finish normally"
    );
    ensure!(
        !response
            .completion
            .reply
            .output
            .iter()
            .any(|output| matches!(output, Output::ToolCall(_))),
        "summary response attempted to call a tool"
    );
    let summary = response.completion.reply.text();
    ensure!(
        !summary.trim().is_empty(),
        "summary response contained no text"
    );

    // Only an estimated progress check. This neither discovers the model's
    // window nor guarantees that the complete future request will fit it.
    let mut before = messages[..cut].iter().fold(0usize, |size, message| {
        size.saturating_add(estimate_tokens(message))
    });
    if let Some(previous) = previous_summary.as_deref() {
        before = before.saturating_add(estimate_tokens(&summary_message(previous)));
    }
    ensure!(
        estimate_tokens(&summary_message(&summary)) < before,
        "compaction did not reduce estimated context size"
    );

    // No fallible work or await after this point. Move the retained messages in
    // place without reconstructing their opaque protocol continuation.
    drop(messages.drain(..cut));
    *previous_summary = Some(summary);
    Ok(Some(Compacted {
        request_id: response.completion.request_id,
        retired_messages: cut,
        retained_messages: messages.len(),
        response: response.metadata,
    }))
}

#[cfg(test)]
mod tests;
