//! Manual compaction lifecycle, distinct from model exchange success.
use std::time::Instant;

use serde::Serialize;

use super::{Recorder, model::RequestOrigin};

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    CompactionStarted {
        #[serde(flatten)]
        origin: RequestOrigin,
        raw_messages: usize,
        had_previous_summary: bool,
    },
    CompactionFinished {
        #[serde(flatten)]
        origin: RequestOrigin,
        elapsed_ms: u128,
        #[serde(flatten)]
        end: End,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum End {
    Committed {
        request_id: u64,
        retired_messages: usize,
        retained_messages: usize,
    },
    Unchanged,
    Failed,
    Cancelled,
}

impl Recorder {
    pub fn compaction(
        &self,
        origin: RequestOrigin,
        raw_messages: usize,
        had_previous_summary: bool,
    ) -> Compaction {
        let id = {
            let mut journal = self.lock();
            journal.next_compaction += 1;
            journal.next_compaction
        };
        let operation = Compaction {
            recorder: self.clone(),
            origin: origin.in_compaction(id),
            started: Instant::now(),
            finished: false,
        };
        self.record(
            Event::CompactionStarted {
                origin: operation.origin,
                raw_messages,
                had_previous_summary,
            },
            || None,
        );
        operation
    }
}

pub struct Compaction {
    recorder: Recorder,
    origin: RequestOrigin,
    started: Instant,
    finished: bool,
}

impl Compaction {
    pub fn origin(&self) -> RequestOrigin {
        self.origin
    }

    pub fn finish(mut self, end: End) {
        self.record_end(end);
    }

    fn record_end(&mut self, end: End) {
        self.finished = true;
        self.recorder.record(
            Event::CompactionFinished {
                origin: self.origin,
                elapsed_ms: self.started.elapsed().as_millis(),
                end,
            },
            || None,
        );
    }
}

impl Drop for Compaction {
    fn drop(&mut self) {
        if !self.finished {
            self.record_end(End::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compaction_origin_preserves_a_supplied_turn_without_changing_it() {
        let recorder = Recorder::disabled();
        let turn = recorder.agent_turn("main", None);
        let original = serde_json::to_value(turn.origin()).unwrap();
        let operation = recorder.compaction(turn.origin(), 4, false);
        let origin = serde_json::to_value(operation.origin()).unwrap();
        assert_eq!(origin["source"], original["source"]);
        assert_eq!(origin["turn_id"], original["turn_id"]);
        assert!(origin["compaction_id"].as_u64().unwrap() > 0);
        assert_eq!(serde_json::to_value(turn.origin()).unwrap(), original);
        assert!(original.get("compaction_id").is_none());
        operation.finish(End::Unchanged);
    }

    #[test]
    fn shared_recorders_allocate_unique_compaction_ids() {
        let recorder = Recorder::disabled();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let recorder = recorder.clone();
                std::thread::spawn(move || {
                    let operation = recorder.compaction("main".into(), 0, false);
                    let id = serde_json::to_value(operation.origin()).unwrap()["compaction_id"]
                        .as_u64()
                        .unwrap();
                    operation.finish(End::Unchanged);
                    id
                })
            })
            .collect();
        let mut ids: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=8).collect::<Vec<_>>());
    }
}
