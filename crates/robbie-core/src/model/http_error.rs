//! Bounded evidence for non-success HTTP responses, never retry policy.
//! Free-form provider text is content; only recognized labels enter metadata.
use std::time::{Duration, SystemTime};

use anyhow::{Result, bail};
use reqwest::{Response, header::HeaderValue};
use serde::Serialize;
use serde_json::Value;

pub(super) const MAX_ERROR_BODY: usize = 16 * 1024;
const ERROR_BODY_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RetryAfter {
    Missing,
    Invalid,
    Delay { delay_ms: u64 },
    TooLarge,
}

impl RetryAfter {
    pub(super) fn from_header(value: Option<&HeaderValue>, now: SystemTime) -> Self {
        match value {
            None => Self::Missing,
            Some(value) => match value.to_str() {
                Ok(value) => Self::parse(value, now),
                Err(_) => Self::Invalid,
            },
        }
    }

    pub(super) fn parse(value: &str, now: SystemTime) -> Self {
        match super::retry_after_delay(value, now) {
            // Round up: millisecond metadata must not shorten an HTTP-date wait.
            Ok(Some(delay)) => match u64::try_from(delay.as_nanos().div_ceil(1_000_000)) {
                Ok(delay_ms) => Self::Delay { delay_ms },
                Err(_) => Self::TooLarge,
            },
            Ok(None) => Self::Invalid,
            Err(_) => Self::TooLarge,
        }
    }

    pub(super) fn delay(self) -> Result<Duration> {
        match self {
            Self::Missing | Self::Invalid => Ok(Duration::ZERO),
            Self::Delay { delay_ms } => Ok(Duration::from_millis(delay_ms)),
            Self::TooLarge => bail!("Retry-After exceeds the automatic wait limit"),
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorBodyState {
    Complete,
    Truncated,
    ReadFailed,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Unknown,
    InvalidRequest,
    Authentication,
    PaymentRequired,
    PermissionDenied,
    RateLimited,
    QuotaExceeded,
    Overloaded,
    ServerError,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorLabel {
    RateLimitError,
    RateLimitExceeded,
    InsufficientQuota,
    QuotaExceeded,
    OverloadedError,
    AuthenticationError,
    PermissionError,
    InvalidRequestError,
    ApiError,
}

impl ErrorLabel {
    fn inspect(value: &Value) -> Option<Self> {
        Some(match value.as_str()? {
            "rate_limit_error" => Self::RateLimitError,
            "rate_limit_exceeded" => Self::RateLimitExceeded,
            "insufficient_quota" => Self::InsufficientQuota,
            "quota_exceeded" => Self::QuotaExceeded,
            "overloaded_error" => Self::OverloadedError,
            "authentication_error" => Self::AuthenticationError,
            "permission_error" => Self::PermissionError,
            "invalid_request_error" => Self::InvalidRequestError,
            "api_error" => Self::ApiError,
            _ => return None,
        })
    }

    fn category(self) -> ErrorCategory {
        match self {
            Self::RateLimitError | Self::RateLimitExceeded => ErrorCategory::RateLimited,
            Self::InsufficientQuota | Self::QuotaExceeded => ErrorCategory::QuotaExceeded,
            Self::OverloadedError => ErrorCategory::Overloaded,
            Self::AuthenticationError => ErrorCategory::Authentication,
            Self::PermissionError => ErrorCategory::PermissionDenied,
            Self::InvalidRequestError => ErrorCategory::InvalidRequest,
            Self::ApiError => ErrorCategory::ServerError,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(untagged)]
pub enum ErrorCode {
    Status(u16),
    Named(ErrorLabel),
}

impl ErrorCode {
    fn inspect(value: &Value) -> Option<Self> {
        if let Some(label) = ErrorLabel::inspect(value) {
            return Some(Self::Named(label));
        }
        let code = u16::try_from(value.as_u64()?).ok()?;
        // Do not copy arbitrary numbers, strings, or upstream metadata.
        matches!(
            code,
            400 | 401 | 402 | 403 | 404 | 408 | 413 | 422 | 429 | 500 | 502 | 503 | 504 | 529
        )
        .then_some(Self::Status(code))
    }

    fn category(self) -> ErrorCategory {
        match self {
            Self::Named(label) => label.category(),
            Self::Status(400 | 404 | 413 | 422) => ErrorCategory::InvalidRequest,
            Self::Status(401) => ErrorCategory::Authentication,
            Self::Status(402) => ErrorCategory::PaymentRequired,
            Self::Status(403) => ErrorCategory::PermissionDenied,
            Self::Status(429) => ErrorCategory::RateLimited,
            Self::Status(529) => ErrorCategory::Overloaded,
            Self::Status(500 | 502 | 503 | 504) => ErrorCategory::ServerError,
            Self::Status(_) => ErrorCategory::Unknown,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
pub struct HttpErrorMetadata {
    category: ErrorCategory,
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<ErrorCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error_type: Option<ErrorLabel>,
    body_state: ErrorBodyState,
    captured_bytes: usize,
    retry_after: RetryAfter,
}

pub(super) struct HttpError {
    pub metadata: HttpErrorMetadata,
    pub body: Vec<u8>,
}

impl HttpError {
    pub(super) async fn read(mut response: Response, retry_after: RetryAfter) -> Self {
        let mut body = Vec::new();
        let deadline = tokio::time::Instant::now() + ERROR_BODY_TIMEOUT;
        let state = loop {
            // Reqwest's original per-attempt deadline also covers these reads.
            match tokio::time::timeout_at(deadline, response.chunk()).await {
                Ok(Ok(Some(chunk))) => {
                    let remaining = MAX_ERROR_BODY - body.len();
                    body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                    if chunk.len() > remaining {
                        break ErrorBodyState::Truncated;
                    }
                }
                Ok(Ok(None)) => break ErrorBodyState::Complete,
                Ok(Err(error)) if error.is_timeout() => break ErrorBodyState::TimedOut,
                Ok(Err(_)) => break ErrorBodyState::ReadFailed,
                Err(_) => break ErrorBodyState::TimedOut,
            }
        };
        Self::inspect(body, state, retry_after)
    }

    fn inspect(body: Vec<u8>, body_state: ErrorBodyState, retry_after: RetryAfter) -> Self {
        // Never classify a partial/truncated prefix as a complete error document.
        let parsed: Option<Value> = if matches!(body_state, ErrorBodyState::Complete) {
            serde_json::from_slice(&body).ok()
        } else {
            None
        };
        let error = parsed.as_ref().and_then(|value| value.get("error"));
        let code = error
            .and_then(|error| error.get("code"))
            .and_then(ErrorCode::inspect);
        let error_type = error
            .and_then(|error| error.get("type"))
            .and_then(ErrorLabel::inspect);
        // A named code (e.g. insufficient_quota) is more specific than a generic
        // numeric 429 or rate_limit_error type. Never inspect free-form messages.
        let category = match code {
            Some(ErrorCode::Named(label)) => label.category(),
            _ => error_type
                .map(ErrorLabel::category)
                .or_else(|| code.map(ErrorCode::category))
                .unwrap_or(ErrorCategory::Unknown),
        };
        Self {
            metadata: HttpErrorMetadata {
                category,
                code,
                error_type,
                body_state,
                captured_bytes: body.len(),
                retry_after,
            },
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_after_dates_round_up_without_shortening_the_requested_wait() {
        let whole_second = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let now = whole_second + Duration::from_nanos(123_456);
        let target = whole_second + Duration::from_secs(3);
        let header = HeaderValue::from_str(&httpdate::fmt_http_date(target)).unwrap();
        let parsed = RetryAfter::from_header(Some(&header), now);
        assert_eq!(parsed, RetryAfter::Delay { delay_ms: 3000 });
        assert!(parsed.delay().unwrap() >= target.duration_since(now).unwrap());
        assert_eq!(
            RetryAfter::from_header(Some(&header), target + Duration::from_secs(1)),
            RetryAfter::Delay { delay_ms: 0 }
        );
    }

    #[test]
    fn retry_after_invalid_and_overflowing_values_never_copy_raw_headers() {
        let now = SystemTime::UNIX_EPOCH;
        assert_eq!(RetryAfter::from_header(None, now), RetryAfter::Missing);
        let invalid = HeaderValue::from_bytes(b"\xffPRIVATE_HEADER").unwrap();
        assert_eq!(
            RetryAfter::from_header(Some(&invalid), now),
            RetryAfter::Invalid
        );
        for value in ["18446744073709551615", "99999999999999999999999"] {
            assert_eq!(RetryAfter::parse(value, now), RetryAfter::TooLarge);
            assert!(RetryAfter::parse(value, now).delay().is_err());
        }
    }
}
