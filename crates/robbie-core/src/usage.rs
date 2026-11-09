//! Reported usage is accounting, not conversation history or a tokenizer.
use std::sync::{Mutex, MutexGuard};

/// Sum of the available reports for one token category.
/// This is not a guarantee of billing completeness: lost responses are unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TokenTotal {
    /// Known sum, saturating at u64::MAX. With incomplete data this is a lower bound.
    pub reported: u64,
    /// Every counted response reported this category and its sum did not overflow.
    pub complete: bool,
}

impl Default for TokenTotal {
    fn default() -> Self {
        Self {
            reported: 0,
            complete: true,
        }
    }
}

impl TokenTotal {
    fn add(&mut self, tokens: Option<u64>) {
        match tokens {
            Some(tokens) => match self.reported.checked_add(tokens) {
                Some(sum) => self.reported = sum,
                None => {
                    self.reported = u64::MAX;
                    self.complete = false;
                }
            },
            None => self.complete = false,
        }
    }
}

/// Session-wide totals, including child and summary requests. Never reset by compaction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageTotals {
    /// Uncached input; cache reads and writes are separate, nonoverlapping categories.
    pub input: TokenTotal,
    pub output: TokenTotal,
    pub cache_read: TokenTotal,
    pub cache_write: TokenTotal,
    /// Successful-HTTP JSON responses inspected for usage, even if reply decoding failed.
    /// Failed HTTP attempts, invalid JSON and dropped responses are not invented reports.
    pub responses: u64,
}

/// Latest usable ordinary-response measurement for one conversation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextUsage {
    /// Measured input (including cache reads/writes) plus that response's output.
    /// Not the estimated size of the next request or cumulative session usage.
    pub reported_tokens: u64,
    /// Raw messages appended since the measurement. Their size is not included above.
    pub trailing_messages: usize,
}

/// Immutable statistics. In an activity, context belongs to the emitting source;
/// `Session::statistics()` returns the root conversation's context. Totals are shared.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UsageSnapshot {
    pub totals: UsageTotals,
    pub context: Option<ContextUsage>,
}

/// Protocol-normalized, disjoint categories for one received response.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReportedUsage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_read: Option<u64>,
    pub cache_write: Option<u64>,
}

impl ReportedUsage {
    pub fn context_tokens(self) -> Option<u64> {
        let tokens = self
            .input?
            .checked_add(self.output?)?
            .checked_add(self.cache_read?)?
            .checked_add(self.cache_write?)?;
        (tokens > 0).then_some(tokens)
    }
}

/// Shared by the session's immutable model client and its children. No journal dependency.
#[derive(Default)]
pub(crate) struct UsageLedger(Mutex<UsageTotals>);

impl UsageLedger {
    fn lock(&self) -> MutexGuard<'_, UsageTotals> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn snapshot(&self) -> UsageTotals {
        *self.lock()
    }

    pub fn record(&self, usage: ReportedUsage) {
        let mut totals = self.lock();
        totals.responses = totals.responses.saturating_add(1);
        totals.input.add(usage.input);
        totals.output.add(usage.output);
        totals.cache_read.add(usage.cache_read);
        totals.cache_write.add(usage.cache_write);
    }
}

#[cfg(test)]
mod tests;
