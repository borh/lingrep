use std::collections::HashMap;
use std::sync::Arc;

use futures::StreamExt as _;
use lingrep_corpus::ProvenanceView;
use tokio::sync::mpsc;

use super::{SpanEvent, SpanEvents, SpanSource, SpanStreamError};

/// Bounded so a fast producer cannot outrun the sweep. 1024 × 32 B ≈ 32 KB.
pub(crate) const CHANNEL_CAPACITY: usize = 1024;

enum Message {
    Event(SpanEvent),
    /// The sentinel that distinguishes clean EOF from a dropped sender.
    End,
    Failed(String),
}

struct ChannelSource {
    receiver: mpsc::Receiver<Message>,
    ended: bool,
}

impl SpanSource for ChannelSource {
    fn next(&mut self) -> Result<Option<SpanEvent>, SpanStreamError> {
        if self.ended {
            return Ok(None);
        }
        match self.receiver.blocking_recv() {
            Some(Message::Event(event)) => Ok(Some(event)),
            Some(Message::End) => {
                self.ended = true;
                Ok(None)
            }
            Some(Message::Failed(cause)) => Err(SpanStreamError::Corpus(cause)),
            // Closed without `End`: a truncated stream or a producer panic.
            // Never treated as a clean end, which is what stops a truncated
            // stream from becoming a plausible undercount.
            None => Err(SpanStreamError::Truncated),
        }
    }
}

/// Streams `region_spans` into a bounded channel, mapping each label to its
/// slot at the source so no row is ever materialized on the consumer's side.
///
/// `labels` is the vocabulary in slot order — the same order
/// [`admit`](crate::section_tally::admit) assigns — so the mapping has exactly
/// one owner rather than being restated by the caller.
///
/// The returned [`SpanEvents`] blocks on its channel, so it must be drained
/// from a blocking thread, never from a runtime worker.
///
/// Read order is `span_id`, which is `first_sentence` order by the writer's
/// invariant; the consumer validates that contract and refuses a database that
/// violates it.
pub fn spawn_span_producer(
    provenance: Arc<ProvenanceView>,
    labels: Vec<String>,
) -> (tokio::task::JoinHandle<()>, SpanEvents) {
    let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
    let handle = tokio::spawn(async move {
        let slots = labels
            .into_iter()
            .enumerate()
            .filter_map(|(slot, label)| u32::try_from(slot).ok().map(|slot| (label, slot)))
            .collect::<HashMap<_, _>>();
        let mut rows = provenance.region_spans_in_span_order();
        let mut failure = None;
        while let Some(row) = rows.next().await {
            let row = match row {
                Ok(row) => row,
                Err(error) => {
                    failure = Some(error.to_string());
                    break;
                }
            };
            // A label outside the vocabulary contributes nothing. Dropping it
            // here keeps the delivered sequence a subsequence of the stored
            // one, so the ordering contract still holds downstream.
            let Some(slot) = slots.get(&row.label).copied() else {
                continue;
            };
            let event = SpanEvent {
                slot,
                first: row.first_sentence,
                end: row.end_sentence,
            };
            // A send error means the receiver was dropped: the scan was
            // cancelled or failed. Exit rather than park.
            if sender.send(Message::Event(event)).await.is_err() {
                return;
            }
        }
        drop(rows);
        let message = failure.map_or(Message::End, Message::Failed);
        let _ = sender.send(message).await;
    });
    (
        handle,
        SpanEvents::new(Box::new(ChannelSource {
            receiver,
            ended: false,
        })),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::section_tally::build;
    use crate::snapshot::PatternKey;
    use lingrep_corpus::SectionCoverage;

    fn one_label() -> Vec<SectionCoverage> {
        vec![SectionCoverage {
            label: "methods".to_string(),
            coverage: 1,
        }]
    }

    /// A producer over a literal event list, exercising the real channel and
    /// its `End` sentinel rather than the in-memory `from_events` source.
    fn producer_from(events: Vec<SpanEvent>) -> (tokio::task::JoinHandle<()>, SpanEvents) {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        let handle = tokio::spawn(async move {
            for event in events {
                if sender.send(Message::Event(event)).await.is_err() {
                    return;
                }
            }
            let _ = sender.send(Message::End).await;
        });
        (
            handle,
            SpanEvents::new(Box::new(ChannelSource {
                receiver,
                ended: false,
            })),
        )
    }

    /// Spec obligation 11, case two: more trailing events than the channel
    /// holds. Without `finish()` the producer parks and the join deadlocks.
    // Every test here consumes the stream synchronously via `blocking_recv`,
    // so it must not run on a current-thread runtime: the producer cannot make
    // progress while the only worker thread is blocked in `next`.
    #[tokio::test(flavor = "multi_thread")]
    async fn more_trailing_events_than_capacity_still_terminate() {
        let events = (0..(CHANNEL_CAPACITY * 3) as u64)
            .map(|index| SpanEvent {
                slot: 0,
                first: 1_000 + index,
                end: 1_001 + index,
            })
            .collect::<Vec<_>>();
        let (handle, stream) = producer_from(events);
        tokio::task::spawn_blocking(move || {
            let mut builder = build(&one_label(), 1, stream).expect("one label fits");
            builder
                .observe(0, &[PatternKey::new(0).unwrap()], &[])
                .unwrap();
            builder.finish().expect("drains rather than deadlocks");
        })
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the producer joins once the stream is drained")
            .unwrap();
    }

    /// Spec obligation 11, case three: cancellation with a full channel.
    /// Dropping the receiver must make the producer's next send fail.
    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_receiver_unblocks_a_parked_producer() {
        let events = (0..(CHANNEL_CAPACITY * 3) as u64)
            .map(|index| SpanEvent {
                slot: 0,
                first: index,
                end: index + 1,
            })
            .collect::<Vec<_>>();
        let (handle, stream) = producer_from(events);
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("the producer exits when its receiver goes away")
            .unwrap();
    }

    /// A stream that closes without `End` — a producer panic, or a truncated
    /// read — must never be mistaken for a clean end, because that is exactly
    /// how a truncation becomes a plausible undercount.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_sender_dropped_without_end_is_a_truncation() {
        let (sender, receiver) = mpsc::channel(CHANNEL_CAPACITY);
        tokio::spawn(async move {
            let _ = sender
                .send(Message::Event(SpanEvent {
                    slot: 0,
                    first: 0,
                    end: 1,
                }))
                .await;
        });
        let mut stream = SpanEvents::new(Box::new(ChannelSource {
            receiver,
            ended: false,
        }));
        let verdict = tokio::task::spawn_blocking(move || {
            stream.peek().unwrap();
            stream.take();
            stream.peek()
        })
        .await
        .unwrap();
        assert!(matches!(verdict, Err(SpanStreamError::Truncated)));
    }

    /// Spec obligation 15, at the stream boundary: `ORDER BY span_id` is a
    /// writer invariant, so a table that violates it must be detected rather
    /// than miscounted. The database-level fixture lives in
    /// `lingrep-web/tests/suite/section_span_stream_test.rs`.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_stream_whose_starts_decrease_is_refused() {
        let (handle, mut stream) = producer_from(vec![
            SpanEvent {
                slot: 0,
                first: 100,
                end: 110,
            },
            SpanEvent {
                slot: 0,
                first: 10,
                end: 20,
            },
        ]);
        let verdict = tokio::task::spawn_blocking(move || {
            stream.peek().unwrap();
            stream.take();
            stream.peek()
        })
        .await
        .unwrap();
        assert!(matches!(
            verdict,
            Err(SpanStreamError::DecreasingStart {
                previous: 100,
                found: 10
            })
        ));
        let _ = handle.await;
    }
}
