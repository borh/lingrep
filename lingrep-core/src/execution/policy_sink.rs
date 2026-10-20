//! The publication seam where declared result policy is applied.
//!
//! Policy is a property of *publication*, not of matching, so it is applied
//! here — decorating a [`MatchSink`] — rather than inside the matcher. The
//! canonical match stream is this decorator's input and never its output, so
//! policy cannot change which sentences match or alter the raw stream.

use rayon::prelude::*;

use crate::execution::{
    CancellationFlag, ExecutionError, MatchSink, SentenceLocation, SinkControl,
};
use crate::fsm::AnchorEvaluator;
use crate::fsm::ProgramStats;
use crate::fsm::policy::{CatalogPolicy, PolicyStats, apply_policy_records, strip_hidden};
use crate::fsm::policy_trace::PolicyTrace;
use crate::fsm::types::Match;
use crate::token_match::TokenSequence;

/// Applies a catalog's declared result policy to everything an inner sink
/// receives.
///
/// Two modes, and *both* strip synthesized emitter records. There is no mode in
/// which a `__policy__` record reaches a published stream: they are evidence
/// the engine reads, and a raw view that leaked them would be reporting
/// definitions no researcher wrote.
///
/// The lifetime is the compiled catalog's, borrowed for the anchor pre-pass
///. Every constructor demands one: a sink that could be built without
/// saying where anchor positions come from would be a sink that could be
/// wired against the wrong catalog's, and the anchor is what decides admission.
pub struct PolicySink<'a, S> {
    inner: S,
    policy: CatalogPolicy,
    /// The compiled catalog's anchor programs, borrowed rather than owned so
    /// that per-sentence evaluation can happen inside [`MatchSink::push`],
    /// which cannot reach a `&CompiledCatalog` any other way.
    anchors: AnchorEvaluator<'a>,
    mode: PolicyMode,
    stats: PolicyStats,
    /// Per-operation counters for the declared policy program, accumulated
    /// across every sentence this sink sees.
    ///
    /// Owned here rather than per sentence because that is the whole point of
    /// the shape: one slot per authored operation, whatever the corpus size.
    program_stats: ProgramStats,
    /// The per-record explanation trace, when a caller asked for one.
    ///
    /// `None` by default and on every surface that did not opt in, so the
    /// engine's tracing branch is not merely cheap but unreached.
    trace: Option<PolicyTrace>,
}

/// A caller's choice at the publication seam.
///
/// Deliberately two-valued and deliberately not defaulted: "no seam" is not a
/// choice a caller can express, because being able to express it is what let
/// `summarize` ship without one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PolicySelection {
    /// Publish post-policy records — the default on every execution surface.
    Apply,
    /// Publish the raw canonical stream (`--raw-matches`), synthesized evidence
    /// still stripped.
    Raw,
}

/// Everything a caller asks of the publication seam.
///
/// [`crate::execution::execute`] owns the seam, so a caller wanting a trace
/// cannot install one on a decorator it does not construct — the request has to
/// travel. It travels *inside the selection* rather than as another parameter,
/// because `execute` takes `impl Into<Self>`: a caller wanting nothing but
/// policy still writes [`PolicySelection::Apply`], and only the one surface that
/// wants an explanation says so. A parameter would make every other call site
/// decline a trace out loud, which is the same reason [`PolicySink::with_trace`]
/// is a builder rather than a fourth constructor argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publication {
    selection: PolicySelection,
    trace_capacity: Option<usize>,
}

impl Publication {
    /// Explains at most `capacity` of the records this run judges.
    ///
    /// Accepted on a raw selection and collects nothing there; see
    /// [`PolicySink::with_trace`].
    #[must_use]
    pub fn with_trace(mut self, capacity: usize) -> Self {
        self.trace_capacity = Some(capacity);
        self
    }

    /// Which stream to publish.
    #[must_use]
    pub fn selection(&self) -> PolicySelection {
        self.selection
    }

    /// How many records to explain, if the caller asked for an explanation.
    #[must_use]
    pub fn trace_capacity(&self) -> Option<usize> {
        self.trace_capacity
    }
}

impl From<PolicySelection> for Publication {
    fn from(selection: PolicySelection) -> Self {
        Self {
            selection,
            trace_capacity: None,
        }
    }
}

/// What this sink does to the records passing through it.
///
/// `Passthrough` exists so every execution surface can wrap unconditionally.
/// Without it, a caller would need an enum or a boxed sink to keep the run
/// path's sink type uniform across "policy" and "no policy", and a
/// policy-free catalog would pay a per-sentence copy for a transformation
/// that is the identity. Selecting it is not a caller decision: it is
/// derived from the policy itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PolicyMode {
    /// Apply the declared policy.
    Apply,
    /// Strip synthesized records only (`--raw-matches`).
    Raw,
    /// Forward untouched — nothing is declared and nothing was synthesized.
    Passthrough,
}

impl<'a, S: MatchSink> PolicySink<'a, S> {
    /// Applies the declared policy — the default behavior on every execution
    /// surface.
    ///
    /// A catalog that declares nothing forwards records untouched, so this is
    /// always safe to install.
    ///
    /// `anchors` must come from the same compiled catalog as `policy`; a
    /// mismatch is refused per sentence rather than silently answered, and
    /// [`crate::fsm::CompiledCatalog::anchor_evaluator`] is where one comes from.
    pub fn new(inner: S, policy: CatalogPolicy, anchors: AnchorEvaluator<'a>) -> Self {
        let mode = if policy.declared() {
            PolicyMode::Apply
        } else {
            PolicyMode::Passthrough
        };
        let program_stats = ProgramStats::for_program(policy.program());
        Self {
            inner,
            policy,
            anchors,
            mode,
            stats: PolicyStats::default(),
            program_stats,
            trace: None,
        }
    }

    /// Publishes the canonical stream, applying no policy (`--raw-matches`).
    ///
    /// Synthesized records are still stripped, so raw output equals exactly
    /// what the catalog's own definitions matched. A catalog that synthesized
    /// nothing has nothing to strip and forwards untouched — which is why this
    /// is also always safe to install, rather than something the caller has to
    /// remember to install only when `hidden` is non-empty.
    ///
    /// `anchors` is never evaluated here: raw publication applies no policy, so
    /// no `admit` is consulted and no position is read. It is taken anyway
    /// rather than made optional, for three reasons that outlive the symmetry.
    /// [`Self::with_selection`] dispatches to either constructor and would
    /// otherwise have to supply two different shapes. An
    /// `Option<AnchorEvaluator<'a>>` would leave `'a` unconstrained in this
    /// function, so every caller passing `None` would need a turbofish. And it
    /// would put a `None` into the field that the applying mode then has to
    /// interpret — and the only reading available to it is "measure against the
    /// raw sentence end", silently changing admission.
    pub fn raw(inner: S, policy: CatalogPolicy, anchors: AnchorEvaluator<'a>) -> Self {
        let mode = if policy.hidden.is_empty() {
            PolicyMode::Passthrough
        } else {
            PolicyMode::Raw
        };
        let program_stats = ProgramStats::for_program(policy.program());
        Self {
            inner,
            policy,
            anchors,
            mode,
            stats: PolicyStats::default(),
            program_stats,
            trace: None,
        }
    }

    /// Selects between the two constructors above from a caller's choice.
    ///
    /// [`crate::execution::execute`] owns the publication seam and needs to
    /// build it from whatever the caller asked for. The caller's choice is
    /// genuinely two-valued — `Passthrough` is derived, never selected — so
    /// this takes [`PolicySelection`] rather than exposing the internal mode.
    pub fn with_selection(
        inner: S,
        policy: CatalogPolicy,
        anchors: AnchorEvaluator<'a>,
        selection: PolicySelection,
    ) -> Self {
        match selection {
            PolicySelection::Apply => Self::new(inner, policy, anchors),
            PolicySelection::Raw => Self::raw(inner, policy, anchors),
        }
    }

    /// What policy has suppressed or dropped so far.
    ///
    /// Always zero in raw mode: stripping a synthetic is not a suppression, it
    /// is the absence of a record that was never publishable.
    pub fn stats(&self) -> PolicyStats {
        self.stats
    }

    /// What each policy-program statement did, in authored order.
    ///
    /// Borrowed rather than copied, unlike [`Self::stats`]: this is one entry
    /// per authored operation and a catalog may declare many. Empty when the
    /// catalog declares no `policy` block.
    pub fn program_stats(&self) -> &ProgramStats {
        &self.program_stats
    }

    /// Collects a bounded explanation trace for the records this sink judges.
    ///
    /// Opt-in, and a builder rather than a fourth constructor parameter: three
    /// constructors already exist and every caller in the workspace uses one,
    /// so a parameter would make every call site say "no trace" out loud.
    ///
    /// Raw mode applies no policy and so has nothing to explain. Installing a
    /// trace on one is accepted and collects nothing, rather than refused: the
    /// selection is the caller's, and a sink that rejected a harmless request
    /// would push that branch into every caller.
    #[must_use]
    pub fn with_trace(mut self, capacity: usize) -> Self {
        self.trace = Some(PolicyTrace::with_capacity(capacity));
        self
    }

    /// The explanation trace, if one was requested.
    pub fn trace(&self) -> Option<&PolicyTrace> {
        self.trace.as_ref()
    }

    /// The accumulated per-statement counters and explanation trace, by move.
    ///
    /// [`Self::program_stats`] and [`Self::trace`] borrow, which is right for a
    /// caller inspecting a live sink. A run's teardown holds the last reference
    /// and is about to drop the sink, so it wants both handed on rather than
    /// cloned out of something being discarded. One method rather than two
    /// because each would consume the sink and the teardown wants both.
    pub fn into_reporting(self) -> (ProgramStats, Option<PolicyTrace>) {
        (self.program_stats, self.trace)
    }

    /// Whether this sink publishes the raw canonical stream.
    ///
    /// True for `--raw-matches` over a catalog that synthesized emitters. A
    /// catalog with nothing to strip reports `false`, because there is no
    /// observable difference between raw and applied for it.
    pub fn is_raw(&self) -> bool {
        self.mode == PolicyMode::Raw
    }

    /// The decorated sink, for run facts it owns.
    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

impl<S: MatchSink> MatchSink for PolicySink<'_, S> {
    /// Publishes one sentence's records after policy.
    ///
    /// `sentence.len()` is the sentence's true token count — a projected source
    /// narrows which *attributes* each token carries and never how many tokens
    /// there are — so `terminal` admission is measured against the real
    /// sentence end. The same fact is what makes the anchor pre-pass sound
    /// here: it runs the `exclude` automaton over `sentence`, and a truncated
    /// one would move the anchor.
    ///
    /// The inner sink's [`SinkControl`] passes through unchanged, so its
    /// `records_emitted` — in either variant — counts **published** records,
    /// not raw ones. That is the intended reading for a limit (a researcher
    /// asking for 10 results wants 10 published results) and the honest one
    /// for accounting (a run reports what reached a consumer). It stays within
    /// the runner's validation, which checks the count against the *raw* slice
    /// this decorator was handed, because policy only ever removes records:
    /// published are never more numerous than raw.
    fn push(
        &mut self,
        location: &SentenceLocation,
        sentence: &dyn TokenSequence,
        matches: &[Match],
    ) -> Result<SinkControl, ExecutionError> {
        let published = match self.mode {
            // No copy at all: a policy-free catalog must not pay for the
            // engine's existence.
            PolicyMode::Passthrough => return self.inner.push(location, sentence, matches),
            PolicyMode::Raw => strip_hidden(&self.policy, matches),
            // The only mode with derived fields to offer, and so the only one
            // that takes the record-carrying entry point. Its default projects
            // back to `push`, so a sink that ignores derived fields sees the
            // same stream as the other two modes.
            PolicyMode::Apply => {
                // `positions` returns before building a session for a catalog
                // declaring no anchor, which is most of them: `Apply` is
                // selected by declaring *any* policy, not by declaring an
                // anchor, so this arm must stay free for the anchor-free case.
                let anchors = self.anchors.positions(sentence);
                // The engine is in `fsm` and `SentenceLocation` is an
                // `execution` type. This sink is the one place holding both, so
                // it formats the label and the layer below stays free of the
                // layer above. `source_name:document_sentence_ordinal` locates a
                // sentence in the file a researcher has open, which the corpus
                // ordinal does not.
                if let Some(trace) = self.trace.as_mut() {
                    trace.begin_sentence(&format!(
                        "{}:{}",
                        location.source_name, location.document_sentence_ordinal
                    ));
                }
                let records = apply_policy_records(
                    &self.policy,
                    matches,
                    sentence,
                    &anchors,
                    &mut self.stats,
                    &mut self.program_stats,
                    self.trace.as_mut(),
                )
                .map_err(ExecutionError::PolicyAnchor)?;
                return self.inner.push_records(location, sentence, records);
            }
        };
        self.inner.push(location, sentence, &published)
    }

    fn push_partition<V: TokenSequence + Sync>(
        &mut self,
        pool: &rayon::ThreadPool,
        locations: &[SentenceLocation],
        sentences: &[V],
        found: &[Vec<Match>],
        cancel: &CancellationFlag,
    ) -> Result<(Vec<SinkControl>, bool), ExecutionError> {
        // Trace order is observable, and the raw and passthrough modes have no
        // owned records whose preparation can move. Keep all three on the
        // established ordered path. Only ordinary applied publication opts in.
        if self.mode != PolicyMode::Apply
            || self.trace.is_some()
            || !self.inner.wants_parallel_records_partition()
        {
            let mut controls = Vec::with_capacity(found.len());
            for ((location, sentence), matches) in locations.iter().zip(sentences).zip(found) {
                if cancel.is_cancelled() {
                    return Ok((controls, true));
                }
                let control = self.push(location, sentence, matches)?;
                controls.push(control);
                if matches!(control, SinkControl::StopAfter { .. }) {
                    return Ok((controls, false));
                }
                if cancel.is_cancelled() {
                    return Ok((controls, true));
                }
            }
            return Ok((controls, false));
        }

        struct AppliedSentence {
            records: Vec<crate::fsm::PublishedRecord>,
            stats: PolicyStats,
            program_stats: ProgramStats,
        }

        let policy = &self.policy;
        let anchors = &self.anchors;
        // Bounded fan-out: at most PARTITION_PUBLICATION_WIDTH concurrent
        // tasks, however wide the shared pool is.
        let chunk = sentences
            .len()
            .div_ceil(super::runner::PARTITION_PUBLICATION_WIDTH)
            .max(1);
        let applied = pool.install(|| {
            sentences
                .par_chunks(chunk)
                .zip(found.par_chunks(chunk))
                .map(|(sentences, found)| {
                    sentences
                        .iter()
                        .zip(found)
                        .map(|(sentence, matches)| {
                            let positions = anchors.positions(sentence);
                            let mut stats = PolicyStats::default();
                            let mut program_stats = ProgramStats::for_program(policy.program());
                            apply_policy_records(
                                policy,
                                matches,
                                sentence,
                                &positions,
                                &mut stats,
                                &mut program_stats,
                                None,
                            )
                            .map(|records| AppliedSentence {
                                records,
                                stats,
                                program_stats,
                            })
                            .map_err(ExecutionError::PolicyAnchor)
                        })
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        });

        // Preserve the serial seam's first-error rule. Speculative work after
        // this point is discarded and contributes neither counters nor output.
        let mut successful = Vec::new();
        let mut first_error = None;
        for result in applied.into_iter().flatten() {
            match result {
                Ok(sentence) if first_error.is_none() => successful.push(sentence),
                Ok(_) => {}
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(_) => {}
            }
        }

        let records = successful
            .iter_mut()
            .map(|sentence| std::mem::take(&mut sentence.records))
            .collect();
        let prefix_len = successful.len();
        let (controls, cancelled) = self.inner.push_records_partition(
            pool,
            &locations[..prefix_len],
            &sentences[..prefix_len],
            records,
            cancel,
        )?;
        for sentence in successful.into_iter().take(controls.len()) {
            self.stats.merge_from(sentence.stats);
            self.program_stats.merge_from(sentence.program_stats);
        }

        let stopped = controls
            .last()
            .is_some_and(|control| matches!(control, SinkControl::StopAfter { .. }));
        if !cancelled
            && !stopped
            && let Some(error) = first_error
        {
            return Err(error);
        }
        Ok((controls, cancelled))
    }

    fn finish_batch(&mut self) -> Result<(), ExecutionError> {
        self.inner.finish_batch()
    }
}
