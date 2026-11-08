//! Immutable text correlation and producer delivery evidence, independent of any frontend.
use serde::Serialize;

use super::{Recorder, model::RequestOrigin};

#[derive(Clone, Copy, Debug, Serialize)]
pub struct TextContext {
    #[serde(flatten)]
    pub(super) origin: RequestOrigin,
    request_id: u64,
    text_sequence: usize,
}

impl TextContext {
    pub(super) fn new(origin: RequestOrigin, request_id: u64, text_sequence: usize) -> Self {
        Self {
            origin,
            request_id,
            text_sequence,
        }
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Delivery {
    Enqueued,
    ReceiverClosed,
}

#[derive(Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    AgentTextEmitted {
        #[serde(flatten)]
        context: TextContext,
        raw_bytes: usize,
        delivery: Delivery,
    },
}

impl Recorder {
    pub(super) fn text_emitted(&self, context: TextContext, text: &str, enqueued: bool) {
        self.record(
            Event::AgentTextEmitted {
                context,
                raw_bytes: text.len(),
                delivery: if enqueued {
                    Delivery::Enqueued
                } else {
                    Delivery::ReceiverClosed
                },
            },
            || None,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn producer_evidence_preserves_the_flattened_schema_for_both_delivery_outcomes() {
        let context = TextContext::new(RequestOrigin::in_turn("main", 7), 11, 2);
        for (delivery, expected) in [
            (Delivery::Enqueued, "enqueued"),
            (Delivery::ReceiverClosed, "receiver_closed"),
        ] {
            let event = Event::AgentTextEmitted {
                context,
                raw_bytes: "é猫".len(),
                delivery,
            };
            assert_eq!(
                serde_json::to_value(event).unwrap(),
                json!({
                    "event": "agent_text_emitted",
                    "source": "main",
                    "turn_id": 7,
                    "request_id": 11,
                    "text_sequence": 2,
                    "raw_bytes": 5,
                    "delivery": expected,
                })
            );
        }
    }
}
