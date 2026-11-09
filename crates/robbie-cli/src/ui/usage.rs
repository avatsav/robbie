//! One cell-width-aware status row, independent of transcript and activity state.
use unicode_width::UnicodeWidthStr;

use super::single_line;
use crate::session::{TokenTotal, UsageSnapshot};

pub(super) fn compact(tokens: u64) -> String {
    match tokens {
        0..1000 => tokens.to_string(),
        1000..10000 => format!("{:.1}k", tokens as f64 / 1000.0),
        10000..1000000 => format!("{:.0}k", tokens as f64 / 1000.0),
        1000000..10000000 => format!("{:.1}M", tokens as f64 / 1000000.0),
        _ => format!("{:.0}M", tokens as f64 / 1000000.0),
    }
}

fn total(total: TokenTotal) -> String {
    match (total.complete, total.reported) {
        (false, 0) => "?".into(),
        (false, tokens) => format!("≥{}", compact(tokens)),
        (true, tokens) => compact(tokens),
    }
}

pub(super) fn row(snapshot: UsageSnapshot, model: &str, width: usize) -> String {
    let totals = snapshot.totals;
    let context = snapshot.context.map_or_else(
        || "?".into(),
        |context| {
            format!(
                "{}{}",
                if context.trailing_messages > 0 {
                    "≥"
                } else {
                    ""
                },
                compact(context.reported_tokens)
            )
        },
    );
    let stats = format!(
        "↑{} ↓{} R{} W{} ctx {context}",
        total(totals.input),
        total(totals.output),
        total(totals.cache_read),
        total(totals.cache_write)
    );
    let model = single_line(model, width);
    let left = single_line(&stats, width.saturating_sub(model.width() + 2));
    format!(
        "{left}{}{model}",
        " ".repeat(width.saturating_sub(left.width() + model.width()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_distinguish_zero_unknown_and_partial_and_compact_large_values() {
        assert_eq!(total(TokenTotal::default()), "0");
        assert_eq!(
            total(TokenTotal {
                reported: 0,
                complete: false
            }),
            "?"
        );
        assert_eq!(
            total(TokenTotal {
                reported: 1234,
                complete: false
            }),
            "≥1.2k"
        );
        assert_eq!(compact(12345), "12k");
        assert_eq!(compact(1234567), "1.2M");
        assert_eq!(compact(u64::MAX), "18446744073710M");
    }

    #[test]
    fn status_and_unicode_model_never_overlap_or_exceed_the_viewport() {
        for width in 0..100 {
            let text = row(UsageSnapshot::default(), "模型/provider 🦀", width);
            assert_eq!(text.width(), width);
            assert!(!text.contains('\n'));
            assert!(!text.contains("(auto)"));
        }
        let text = row(UsageSnapshot::default(), "provider/model", 80);
        assert!(text.starts_with("↑0 ↓0 R0 W0 ctx ?"));
        assert!(text.ends_with("provider/model"));
    }
}
