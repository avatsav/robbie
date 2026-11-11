//! Pure prospective budgeting. Estimates are heuristics, never a provider fit guarantee.
use crate::ModelLimits;
use anyhow::{Result, ensure};

pub(crate) const SAFETY_TOKENS: u64 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactionPolicy {
    /// Enable automatic preflight after explicit session configuration.
    pub enabled: bool,
    /// Soft trigger reserve, raised to at least ordinary output + safety headroom.
    pub reserve_tokens: u64,
    /// Soft recent-history target, bounded to leave summary/input/output space.
    pub keep_recent_tokens: usize,
    /// Positive desired cap; bounded against model output and available input room.
    pub summary_output_tokens: u32,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            reserve_tokens: 16384,
            keep_recent_tokens: 20000,
            summary_output_tokens: 8000,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Plan {
    pub compact: bool,
    pub fits: bool,
    pub summary_output_tokens: u32,
    pub keep_recent_tokens: usize,
    pub max_summary_chars: usize,
}

pub(crate) fn output_tokens(limits: ModelLimits) -> u32 {
    limits.max_output_tokens.unwrap_or(8000).min(8000) as u32
}

pub(crate) fn fits(limits: ModelLimits, input: u64, output: u32) -> bool {
    limits.max_input_tokens.is_none_or(|max| input <= max)
        && limits
            .max_output_tokens
            .is_none_or(|max| u64::from(output) <= max)
        && input
            .checked_add(u64::from(output))
            .and_then(|n| n.checked_add(SAFETY_TOKENS))
            .is_some_and(|n| n <= limits.context_window)
}

pub(crate) fn plan(limits: ModelLimits, policy: CompactionPolicy, input: u64) -> Result<Plan> {
    limits.validate()?;
    ensure!(
        policy.summary_output_tokens > 0,
        "summary output limit must be positive"
    );
    let output_tokens = output_tokens(limits);
    let room = limits
        .context_window
        .checked_sub(u64::from(output_tokens) + SAFETY_TOKENS)
        .map(|room| room.min(limits.max_input_tokens.unwrap_or(u64::MAX)))
        .filter(|room| *room > 1)
        .ok_or_else(|| {
            anyhow::anyhow!("model window leaves no room for input and output headroom")
        })?;
    let summary_output_tokens = u64::from(policy.summary_output_tokens)
        .min(limits.max_output_tokens.unwrap_or(u64::MAX))
        .min(room / 2)
        .max(1) as u32;
    let reserve = policy
        .reserve_tokens
        .max(u64::from(output_tokens) + SAFETY_TOKENS);
    ensure!(
        reserve < limits.context_window,
        "compaction reserve must be smaller than the model window"
    );
    let tail_room = room.saturating_sub(u64::from(summary_output_tokens));
    let summary_room = limits
        .context_window
        .saturating_sub(u64::from(summary_output_tokens) + SAFETY_TOKENS)
        .min(limits.max_input_tokens.unwrap_or(u64::MAX));
    Ok(Plan {
        compact: policy.enabled
            && (input > limits.context_window - reserve || !fits(limits, input, output_tokens)),
        fits: fits(limits, input, output_tokens),
        summary_output_tokens,
        keep_recent_tokens: policy
            .keep_recent_tokens
            .min(usize::try_from(tail_room).unwrap_or(usize::MAX)),
        max_summary_chars: usize::try_from(summary_room.saturating_mul(4)).unwrap_or(usize::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> ModelLimits {
        ModelLimits {
            context_window: 128000,
            max_input_tokens: None,
            max_output_tokens: Some(8000),
        }
    }

    #[test]
    fn exact_threshold_is_strict_and_disabled_policy_does_not_trigger() {
        let threshold = 128000 - 16384;
        assert!(
            !plan(limits(), CompactionPolicy::default(), threshold)
                .unwrap()
                .compact
        );
        assert!(
            plan(limits(), CompactionPolicy::default(), threshold + 1)
                .unwrap()
                .compact
        );
        assert!(
            !plan(
                limits(),
                CompactionPolicy {
                    enabled: false,
                    ..CompactionPolicy::default()
                },
                u64::MAX
            )
            .unwrap()
            .compact
        );
    }

    #[test]
    fn input_output_and_overflow_limits_are_independent() {
        let limited = ModelLimits {
            max_input_tokens: Some(1000),
            max_output_tokens: Some(100),
            ..limits()
        };
        assert!(fits(limited, 1000, 100));
        assert!(!fits(limited, 1001, 100));
        assert!(!fits(limited, 1000, 101));
        assert!(!fits(limits(), u64::MAX, 8000));
        let planned = plan(limited, CompactionPolicy::default(), 1001).unwrap();
        assert!(planned.compact);
        assert!(!planned.fits);
        assert_eq!(output_tokens(limited), 100);
    }

    #[test]
    fn recent_target_and_summary_output_are_bounded_for_small_windows() {
        let planned = plan(
            ModelLimits {
                context_window: 32000,
                ..limits()
            },
            CompactionPolicy::default(),
            20000,
        )
        .unwrap();
        assert!(planned.keep_recent_tokens < 20000);
        assert!(planned.max_summary_chars > 0);
        assert!(planned.summary_output_tokens > 0);
        assert!(fits(
            ModelLimits {
                context_window: 32000,
                ..limits()
            },
            planned.keep_recent_tokens as u64 + u64::from(planned.summary_output_tokens),
            output_tokens(limits())
        ));
    }

    #[test]
    fn invalid_or_impossible_configuration_is_not_silently_repaired() {
        assert!(
            plan(
                ModelLimits {
                    context_window: 100,
                    ..limits()
                },
                CompactionPolicy::default(),
                0
            )
            .is_err()
        );
        assert!(
            plan(
                limits(),
                CompactionPolicy {
                    summary_output_tokens: 0,
                    ..CompactionPolicy::default()
                },
                0
            )
            .is_err()
        );
        assert!(
            plan(
                limits(),
                CompactionPolicy {
                    reserve_tokens: 128000,
                    ..CompactionPolicy::default()
                },
                0
            )
            .is_err()
        );
    }
}
