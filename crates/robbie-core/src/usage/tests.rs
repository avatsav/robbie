use super::*;

fn report() -> ReportedUsage {
    ReportedUsage {
        input: Some(100),
        output: Some(20),
        cache_read: Some(30),
        cache_write: Some(10),
    }
}

#[test]
fn context_measurement_includes_each_disjoint_category_once() {
    assert_eq!(report().context_tokens(), Some(160));
    let zero = ReportedUsage {
        input: Some(0),
        output: Some(0),
        cache_read: Some(0),
        cache_write: Some(0),
    };
    assert_eq!(zero.context_tokens(), None);
    for partial in [
        ReportedUsage {
            input: None,
            ..report()
        },
        ReportedUsage {
            output: None,
            ..report()
        },
        ReportedUsage {
            cache_read: None,
            ..report()
        },
        ReportedUsage {
            cache_write: None,
            ..report()
        },
    ] {
        assert_eq!(partial.context_tokens(), None);
    }
    assert_eq!(
        ReportedUsage {
            input: Some(u64::MAX),
            ..report()
        }
        .context_tokens(),
        None
    );
}

#[test]
fn missing_reports_remain_distinct_from_zero_and_known_totals_are_preserved() {
    let ledger = UsageLedger::default();
    assert_eq!(ledger.snapshot(), UsageTotals::default());
    ledger.record(ReportedUsage {
        output: Some(0),
        ..ReportedUsage::default()
    });
    let first = ledger.snapshot();
    assert_eq!(first.responses, 1);
    assert_eq!(
        first.output,
        TokenTotal {
            reported: 0,
            complete: true
        }
    );
    assert_eq!(
        first.input,
        TokenTotal {
            reported: 0,
            complete: false
        }
    );
    ledger.record(report());
    let next = ledger.snapshot();
    assert_eq!(
        next.input,
        TokenTotal {
            reported: 100,
            complete: false
        }
    );
    assert_eq!(
        next.output,
        TokenTotal {
            reported: 20,
            complete: true
        }
    );
    assert_eq!(
        next.cache_read,
        TokenTotal {
            reported: 30,
            complete: false
        }
    );
    assert_eq!(
        first.responses, 1,
        "snapshots don't change underneath a caller"
    );
}

#[test]
fn overflow_is_a_saturated_lower_bound_not_a_wrapped_exact_total() {
    let ledger = UsageLedger::default();
    ledger.record(ReportedUsage {
        input: Some(u64::MAX),
        ..report()
    });
    ledger.record(report());
    let totals = ledger.snapshot();
    assert_eq!(
        totals.input,
        TokenTotal {
            reported: u64::MAX,
            complete: false
        }
    );
    assert_eq!(
        totals.output,
        TokenTotal {
            reported: 40,
            complete: true
        }
    );
}

#[test]
fn shared_accounting_records_whole_responses_without_lost_updates() {
    let ledger = std::sync::Arc::new(UsageLedger::default());
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let ledger = std::sync::Arc::clone(&ledger);
            std::thread::spawn(move || {
                for _ in 0..40 {
                    ledger.record(report());
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let totals = ledger.snapshot();
    assert_eq!(totals.responses, 320);
    assert_eq!(
        totals.input,
        TokenTotal {
            reported: 32000,
            complete: true
        }
    );
    assert_eq!(
        totals.cache_read,
        TokenTotal {
            reported: 9600,
            complete: true
        }
    );
}
