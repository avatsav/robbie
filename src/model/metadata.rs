//! Diagnostic facts about a received response, not proof of an executable reply.
use serde::Serialize;

#[derive(Clone, Debug, Default, Serialize)]
pub struct ResponseMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    // Missing/malformed content is not an empty block array. A malformed text
    // block also prevents an exact byte total, even when block types are known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocks: Option<BlockCounts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_bytes: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    ToolUse,
    StopSequence,
    PauseTurn,
    Refusal,
    ModelContextWindowExceeded,
    Unknown,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct TokenUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_5m_input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_creation_1h_input_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BlockCounts {
    pub text: usize,
    pub thinking: usize,
    pub redacted_thinking: usize,
    pub tool_use: usize,
    // Do not turn arbitrary future block-type strings into metadata payloads.
    pub unknown: usize,
}
