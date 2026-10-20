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

/// Yields events until the `End` sentinel, which it reports as `Ok(None)`.
///
/// Implementations may block: the sweep runs on the scan's blocking thread and
/// the production source is a bounded channel drained with `blocking_recv`.
pub trait SpanSource {
    /// # Errors
    ///
    /// Returns an error when the underlying stream failed, was truncated, or
    /// the caller's own validation refused the event.
    fn next(&mut self) -> Result<Option<SpanEvent>, SpanStreamError>;
}

/// A one-event-lookahead cursor: the sweep must know whether the next span
/// starts at or before the current sentence without consuming it.
pub struct SpanEvents {
    source: Box<dyn SpanSource + Send>,
    peeked: Option<SpanEvent>,
    ended: bool,
    previous_first: Option<u64>,
    /// The first terminal error, re-reported for every later `peek`.
    ///
    /// Without it a refused event is simply dropped and the next `peek` can
    /// succeed, which would turn a detected violation into a silent omission
    /// for any caller that did not abort on the first error.
    poisoned: Option<SpanStreamError>,
}

// `Box<dyn SpanSource>` is not `Debug`, and the whole point of the type is that
// its contents are a stream position rather than a value. Report the position.
impl std::fmt::Debug for SpanEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SpanEvents")
            .field("peeked", &self.peeked)
            .field("ended", &self.ended)
            .field("previous_first", &self.previous_first)
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl SpanEvents {
    pub fn new(source: Box<dyn SpanSource + Send>) -> Self {
        Self {
            source,
            peeked: None,
            ended: false,
            previous_first: None,
            poisoned: None,
        }
    }

    /// The next event without consuming it, or `None` once `End` was seen.
    ///
    /// # Errors
    ///
    /// Returns [`SpanStreamError::DecreasingStart`] when the stream violates
    /// the `ORDER BY span_id` contract, or the source's own terminal error.
    pub fn peek(&mut self) -> Result<Option<SpanEvent>, SpanStreamError> {
        if let Some(error) = &self.poisoned {
            return Err(error.clone());
        }
        if self.peeked.is_none() && !self.ended {
            match self.source.next().map_err(|error| self.poison(error))? {
                Some(event) => {
                    if let Some(previous) = self.previous_first
                        && event.first < previous
                    {
                        return Err(self.poison(SpanStreamError::DecreasingStart {
                            previous,
                            found: event.first,
                        }));
                    }
                    self.previous_first = Some(event.first);
                    self.peeked = Some(event);
                }
                None => self.ended = true,
            }
        }
        Ok(self.peeked)
    }

    /// Records the first terminal error and returns it, so every later `peek`
    /// reports the same refusal rather than resuming past it.
    fn poison(&mut self, error: SpanStreamError) -> SpanStreamError {
        self.poisoned.get_or_insert(error).clone()
    }

    /// Consumes the peeked event. Call only after `peek` returned `Some`.
    pub fn take(&mut self) {
        self.peeked = None;
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
        Self::new(Box::new(VecSpanSource::new(events)))
    }
}

/// Backs `SpanEvents::from_events`. `truncated` omits the `End` sentinel and is
/// test-only, since no caller outside this crate needs to forge a truncation.
struct VecSpanSource {
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
}

impl SpanSource for VecSpanSource {
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
        let mut events = SpanEvents::new(Box::new(VecSpanSource::truncated(vec![event(0, 0, 5)])));
        events.peek().unwrap();
        events.take();
        assert!(matches!(events.peek(), Err(SpanStreamError::Truncated)));
    }

    #[test]
    fn a_refusal_is_sticky() {
        // Resuming past a refused event would turn a detected violation into a
        // silent omission for any caller that did not abort on the first error.
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
}
