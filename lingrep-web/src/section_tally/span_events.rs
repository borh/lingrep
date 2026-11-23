/// One section span, with its label already resolved to a slot index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanEvent {
    pub slot: u32,
    /// First corpus sentence ordinal in the span.
    pub first: u64,
    /// One past the last: the interval is half-open, as stored.
    pub end: u64,
}

/// Why a span stream stopped producing usable events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanStreamError {
    /// The channel closed without the `End` sentinel: a truncated stream, a
    /// producer panic, or a dropped task. Never an undercount.
    Truncated,
    /// The producer reported a read failure.
    Corpus(String),
    /// `region_spans` did not arrive in non-decreasing `first_sentence` order,
    /// which the `ORDER BY span_id` contract requires.
    DecreasingStart { previous: u64, found: u64 },
}

impl std::fmt::Display for SpanStreamError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => {
                formatter.write_str("the section span stream closed without its end sentinel")
            }
            Self::Corpus(cause) => write!(formatter, "section span read failed: {cause}"),
            Self::DecreasingStart { previous, found } => write!(
                formatter,
                "section spans arrived out of order: {previous} then {found}"
            ),
        }
    }
}

use super::producer::ChannelSource;

/// Yields events until the `End` sentinel, which it reports as `Ok(None)`.
///
/// The production variant drains a bounded channel with `blocking_recv`; the
/// `Vec` variant backs [`SpanEvents::from_events`] and unit tests without heap
/// boxing or dynamic dispatch.
pub(super) enum SpanSource {
    Channel(ChannelSource),
    Vec(VecSpanSource),
}

impl SpanSource {
    fn next(&mut self) -> Result<Option<SpanEvent>, SpanStreamError> {
        match self {
            Self::Channel(source) => source.next(),
            Self::Vec(source) => source.next(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum SpanCursor {
    /// Ready to pull the next event; carries the highest `first` ordinal
    /// accepted so far (`None` before the first event).
    Empty { previous_first: Option<u64> },
    /// Holding one validated lookahead event (`event.first` is the high-water
    /// mark).
    Peeked(SpanEvent),
}

enum SpanStreamState {
    Open {
        source: SpanSource,
        cursor: SpanCursor,
    },
    /// Clean `End` sentinel observed; the source has been dropped.
    Ended,
    /// Sticky terminal error; the source has been dropped.
    Poisoned(SpanStreamError),
}

pub struct SpanEvents {
    state: SpanStreamState,
}

// Report stream position rather than the underlying channel receiver.
impl std::fmt::Debug for SpanEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.state {
            SpanStreamState::Open { cursor, .. } => formatter
                .debug_struct("SpanEvents")
                .field("state", &"Open")
                .field("cursor", cursor)
                .finish_non_exhaustive(),
            SpanStreamState::Ended => formatter
                .debug_struct("SpanEvents")
                .field("state", &"Ended")
                .finish(),
            SpanStreamState::Poisoned(error) => formatter
                .debug_struct("SpanEvents")
                .field("state", &"Poisoned")
                .field("error", error)
                .finish(),
        }
    }
}

impl SpanEvents {
    pub(super) fn new(source: SpanSource) -> Self {
        Self {
            state: SpanStreamState::Open {
                source,
                cursor: SpanCursor::Empty {
                    previous_first: None,
                },
            },
        }
    }

    /// The next event without consuming it, or `None` once `End` was seen.
    ///
    /// # Errors
    ///
    /// Returns [`SpanStreamError::DecreasingStart`] when the stream violates
    /// the `ORDER BY span_id` contract, or the source's own terminal error.
    pub fn peek(&mut self) -> Result<Option<SpanEvent>, SpanStreamError> {
        match &mut self.state {
            SpanStreamState::Ended => Ok(None),
            SpanStreamState::Poisoned(error) => Err(error.clone()),
            SpanStreamState::Open { source, cursor } => match *cursor {
                SpanCursor::Peeked(event) => Ok(Some(event)),
                SpanCursor::Empty { previous_first } => match source.next() {
                    Ok(Some(event)) => {
                        if let Some(previous) = previous_first
                            && event.first < previous
                        {
                            let error = SpanStreamError::DecreasingStart {
                                previous,
                                found: event.first,
                            };
                            self.state = SpanStreamState::Poisoned(error.clone());
                            return Err(error);
                        }
                        *cursor = SpanCursor::Peeked(event);
                        Ok(Some(event))
                    }
                    Ok(None) => {
                        self.state = SpanStreamState::Ended;
                        Ok(None)
                    }
                    Err(error) => {
                        self.state = SpanStreamState::Poisoned(error.clone());
                        Err(error)
                    }
                },
            },
        }
    }

    /// Consumes the peeked event. Call only after `peek` returned `Some`.
    pub fn take(&mut self) {
        if let SpanStreamState::Open { cursor, .. } = &mut self.state
            && let SpanCursor::Peeked(event) = *cursor
        {
            *cursor = SpanCursor::Empty {
                previous_first: Some(event.first),
            };
        }
    }

    /// Consumes the remainder through `End`. Ordering is validated; overlap is
    /// not, because a trailing event has no sentence to be admitted at.
    ///
    /// # Errors
    ///
    /// Returns the first ordering or terminal error the remainder carries.
    pub fn drain(&mut self) -> Result<(), SpanStreamError> {
        while self.peek()?.is_some() {
            self.take();
        }
        Ok(())
    }

    /// A stream over a literal event list, terminating cleanly.
    ///
    /// Public rather than `#[cfg(test)]` because `lingrep-web/tests/suite` builds
    /// tallies too and cannot see the crate's test-only items. One named
    /// constructor is a smaller public surface than an exported source type.
    #[must_use]
    pub fn from_events(events: Vec<SpanEvent>) -> Self {
        Self::new(SpanSource::Vec(VecSpanSource::new(events)))
    }
}

/// Backs `SpanEvents::from_events`. `truncated` omits the `End` sentinel and is
/// test-only, since no caller outside this crate needs to forge a truncation.
pub(super) struct VecSpanSource {
    events: std::collections::VecDeque<SpanEvent>,
    ends: bool,
}

impl VecSpanSource {
    fn new(events: Vec<SpanEvent>) -> Self {
        Self {
            events: events.into(),
            ends: true,
        }
    }

    #[cfg(test)]
    fn truncated(events: Vec<SpanEvent>) -> Self {
        Self {
            events: events.into(),
            ends: false,
        }
    }

    fn next(&mut self) -> Result<Option<SpanEvent>, SpanStreamError> {
        match self.events.pop_front() {
            Some(event) => Ok(Some(event)),
            None if self.ends => Ok(None),
            None => Err(SpanStreamError::Truncated),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(slot: u32, first: u64, end: u64) -> SpanEvent {
        SpanEvent { slot, first, end }
    }

    #[test]
    fn peek_does_not_consume_and_take_advances() {
        let mut events = SpanEvents::from_events(vec![event(0, 0, 5), event(1, 5, 9)]);
        assert_eq!(events.peek().unwrap(), Some(event(0, 0, 5)));
        assert_eq!(events.peek().unwrap(), Some(event(0, 0, 5)));
        events.take();
        assert_eq!(events.peek().unwrap(), Some(event(1, 5, 9)));
        events.take();
        assert_eq!(events.peek().unwrap(), None);
    }

    #[test]
    fn a_decreasing_start_is_refused() {
        let mut events = SpanEvents::from_events(vec![event(0, 10, 12), event(1, 4, 6)]);
        events.peek().unwrap();
        events.take();
        assert!(matches!(
            events.peek(),
            Err(SpanStreamError::DecreasingStart {
                previous: 10,
                found: 4
            })
        ));
    }

    #[test]
    fn a_stream_closed_without_end_is_truncated() {
        let mut events = SpanEvents::new(SpanSource::Vec(VecSpanSource::truncated(vec![event(
            0, 0, 5,
        )])));
        events.peek().unwrap();
        events.take();
        assert!(matches!(events.peek(), Err(SpanStreamError::Truncated)));
    }

    #[test]
    fn a_refusal_is_sticky() {
        let mut events =
            SpanEvents::from_events(vec![event(0, 10, 12), event(1, 4, 6), event(2, 20, 22)]);
        events.peek().unwrap();
        events.take();
        let first = events.peek().unwrap_err();
        assert!(matches!(first, SpanStreamError::DecreasingStart { .. }));
        assert_eq!(events.peek().unwrap_err(), first, "the refusal is repeated");
        assert!(events.drain().is_err(), "and draining cannot clear it");
    }

    #[test]
    fn drain_consumes_to_end_and_validates_ordering() {
        let mut events = SpanEvents::from_events(vec![event(0, 0, 5), event(1, 7, 9)]);
        events.drain().unwrap();

        let mut bad = SpanEvents::from_events(vec![event(0, 7, 9), event(1, 1, 2)]);
        assert!(matches!(
            bad.drain(),
            Err(SpanStreamError::DecreasingStart { .. })
        ));
    }

    #[test]
    fn ended_and_poisoned_states_are_terminal_across_take_and_peek() {
        let mut ended = SpanEvents::from_events(vec![event(0, 3, 5)]);
        assert_eq!(ended.peek().unwrap(), Some(event(0, 3, 5)));
        ended.take();
        assert_eq!(ended.peek().unwrap(), None);
        ended.take();
        assert_eq!(ended.peek().unwrap(), None);
        assert!(ended.drain().is_ok());

        let mut poisoned =
            SpanEvents::from_events(vec![event(0, 10, 12), event(1, 4, 6), event(2, 20, 22)]);
        assert_eq!(poisoned.peek().unwrap(), Some(event(0, 10, 12)));
        poisoned.take();
        let error = poisoned.peek().unwrap_err();
        poisoned.take();
        assert_eq!(poisoned.peek().unwrap_err(), error);
        assert_eq!(poisoned.drain().unwrap_err(), error);
    }
}
