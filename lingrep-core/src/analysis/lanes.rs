use rayon::prelude::*;

use crate::token_match::{BoundProjection, TokenSchema};

use super::{
    AnalysisError, AnalyzerPlan, AnalyzerSession, TextUnit, ValidatedAnalysisBatch,
    analyzer::sealed,
};

/// Analysis lanes over one caller-owned Rayon pool.
///
/// A [`AnalyzerSession`] is lane-owned mutable scratch, so exactly one thread
/// may drive one. That is why an uncached run tokenizes on a single core while
/// every other core waits on it: matching is already spread across the pool,
/// and the analyzer that feeds it is not. `AnalyzerLanes` holds one session per
/// lane and spreads one bounded batch across them.
///
/// It is itself an `AnalyzerSession`, which is the point: nothing upstream of
/// it changes. An executor still sees one session and still hands it one
/// bounded batch, and sentence ordinals are still assigned sequentially from
/// the batch it returns — so a run's numbering, its records and their order do
/// not depend on how many lanes analyzed it.
///
/// # Scheduling
///
/// A batch is cut into at most one contiguous run of inputs per lane, balanced
/// by *bytes* rather than by count, because analysis cost tracks input length
/// and a batch's sentences do not have equal length. Contiguity is what keeps
/// the merge a concatenation instead of a sort, and one task per lane is what
/// keeps per-sentence scheduling out of a loop whose per-item work is
/// microseconds.
///
/// The pool is the caller's, and is the same one the executor matches on.
/// Analysis and matching are sequential phases of one batch, so sharing it
/// oversubscribes nothing.
///
/// # Where a lane's session comes from
///
/// A session is created *inside* the lane task rather than held across
/// batches, because an analyzer session is not `Send` — Vibrato's `Worker`
/// keeps raw pointers into its own lattice — so a set of sessions cannot be
/// dealt out to pool threads at all. Rayon's `map_init` builds one per job
/// split and reuses it for every part that split takes, which recovers the
/// scratch reuse that matters (within a batch) without ever moving a session
/// between threads. The lane driving the calling thread keeps a session across
/// batches, since it never moves.
///
/// # Residency
///
/// A lane costs its analyzer's own scratch — for Vibrato, a lattice sized to
/// the longest sentence that lane sees. The batch itself is the same size it
/// was, because the executor already collected a whole batch of tokenized
/// sentences before matching it; lanes change who fills that vector, not how
/// large it is. The bounds that govern it are unchanged and still
/// [`ExecutionLimits`](crate::execution::ExecutionLimits)'.
pub struct AnalyzerLanes<'pool, P: AnalyzerPlan> {
    plan: &'pool P,
    schema: TokenSchema,
    lanes: usize,
    resident: P::Session,
    pool: &'pool rayon::ThreadPool,
}

impl<'pool, P: AnalyzerPlan> AnalyzerLanes<'pool, P> {
    /// Spreads batches from `plan` across `lanes`, clamped to at least one.
    #[must_use]
    pub fn new(plan: &'pool P, lanes: usize, pool: &'pool rayon::ThreadPool) -> Self {
        Self {
            schema: plan.schema().clone(),
            lanes: lanes.max(1),
            resident: plan.create_session(),
            plan,
            pool,
        }
    }
}

impl<P: AnalyzerPlan> sealed::Session for AnalyzerLanes<'_, P> {}

impl<P> AnalyzerSession for AnalyzerLanes<'_, P>
where
    P: AnalyzerPlan + Sync,
{
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }

    fn analyze_bounded_batch(
        &mut self,
        inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        if self.lanes == 1 || inputs.len() < 2 {
            return self.resident.analyze_bounded_batch(inputs, projection);
        }
        let parts = partition_by_bytes(inputs, self.lanes);
        let plan = self.plan;
        let analyzed = self.pool.install(|| {
            parts
                .into_par_iter()
                .map_init(
                    || plan.create_session(),
                    |session, part| session.analyze_bounded_batch(part, projection),
                )
                .collect::<Result<Vec<_>, _>>()
        })?;
        let mut merged = Vec::with_capacity(analyzed.iter().map(|part| part.inputs().len()).sum());
        for part in analyzed {
            merged.extend(part.into_inputs().into_vec());
        }
        // Revalidated as one batch: each lane validated only its own run, and
        // the whole-batch invariant — strictly increasing input ids — is a
        // property of the concatenation, not of any part.
        ValidatedAnalysisBatch::new(merged)
    }
}

/// Cuts inputs into at most `parts` contiguous runs of roughly equal bytes.
///
/// Never returns an empty run: a lane with nothing to do is a task that costs
/// more to schedule than to run, so fewer inputs than lanes simply uses fewer
/// lanes.
fn partition_by_bytes(inputs: Vec<TextUnit>, parts: usize) -> Vec<Vec<TextUnit>> {
    debug_assert!(parts > 0, "a partition needs at least one part");
    let total = inputs.iter().map(TextUnit::len).sum::<usize>();
    // Each run must still be able to take at least one input, or a corpus of
    // long-then-short sentences would leave a tail for one lane.
    let per_part = total.div_ceil(parts).max(1);
    let remaining_inputs = inputs.len();
    let mut partitioned = Vec::with_capacity(parts);
    let mut current = Vec::new();
    let mut current_bytes = 0_usize;
    for (index, input) in inputs.into_iter().enumerate() {
        let bytes = input.len();
        current.push(input);
        current_bytes += bytes;
        let parts_left = parts - partitioned.len();
        let inputs_left = remaining_inputs - index - 1;
        // Cut when this run has taken its share, while a later run still
        // remains and still has an input to take.
        if current_bytes >= per_part && parts_left > 1 && inputs_left > 0 {
            partitioned.push(std::mem::take(&mut current));
            current_bytes = 0;
        }
    }
    if !current.is_empty() {
        partitioned.push(current);
    }
    partitioned
}

#[cfg(test)]
mod tests {
    use crate::analysis::{
        ExtractionProfile, InputUnitId, NormalizationPolicy, SegmentationMode, SudachiConfig,
        SudachiEdition, SudachiPlan, SudachiResources, SudachiSplit, TextUnitKind, VibratoPlan,
    };
    use crate::token_match::AttributeKey;

    use super::*;

    fn unit(id: u64, text: &str) -> TextUnit {
        TextUnit::admit(
            InputUnitId::new(id),
            TextUnitKind::Sentence,
            text,
            NormalizationPolicy::None,
        )
        .expect("admitted text")
    }

    fn units(texts: &[&str]) -> Vec<TextUnit> {
        texts
            .iter()
            .enumerate()
            .map(|(index, text)| unit(index as u64, text))
            .collect()
    }

    fn shapes(parts: &[Vec<TextUnit>]) -> Vec<Vec<usize>> {
        parts
            .iter()
            .map(|part| part.iter().map(TextUnit::len).collect())
            .collect()
    }

    #[test]
    fn a_partition_is_contiguous_complete_and_never_empty() {
        let texts = ["aaaa", "b", "cc", "dddddd", "e", "ff"];
        for parts in 1..=8 {
            let partitioned = partition_by_bytes(units(&texts), parts);
            assert!(
                partitioned.len() <= parts && !partitioned.is_empty(),
                "{parts} lanes produced {} runs",
                partitioned.len()
            );
            assert!(
                partitioned.iter().all(|part| !part.is_empty()),
                "{parts} lanes produced an empty run"
            );
            let flattened = partitioned
                .iter()
                .flatten()
                .map(|input| input.id().get())
                .collect::<Vec<_>>();
            assert_eq!(
                flattened,
                (0..texts.len() as u64).collect::<Vec<_>>(),
                "{parts} lanes reordered or dropped inputs"
            );
        }
    }

    /// Balancing is by bytes, not by count: the point of the split is that
    /// lanes finish together, and analysis cost tracks input length.
    #[test]
    fn a_partition_balances_bytes_rather_than_input_count() {
        let partitioned = partition_by_bytes(units(&["aaaaaaaa", "b", "c", "d"]), 2);
        assert_eq!(shapes(&partitioned), vec![vec![8], vec![1, 1, 1]]);
    }

    /// Fewer inputs than lanes uses fewer lanes rather than scheduling
    /// tasks with nothing in them.
    #[test]
    fn fewer_inputs_than_lanes_use_fewer_runs() {
        let partitioned = partition_by_bytes(units(&["a", "b"]), 8);
        assert_eq!(shapes(&partitioned), vec![vec![1], vec![1]]);
    }

    /// The property the whole type exists to preserve: what a batch analyzes
    /// to does not depend on how many lanes analyzed it.
    ///
    /// Asserted per analyzer, because the invariant is a property of each
    /// analyzer's session as much as of the split: a session that carried
    /// cross-sentence state would break it, and only the analyzer knows
    /// whether its own does.
    #[test]
    fn lane_count_does_not_change_what_a_vibrato_batch_analyzes_to() {
        let Some(plan) = vibrato_test_plan() else {
            return;
        };
        assert_lanes_preserve_analysis(&plan);
    }

    #[test]
    fn lane_count_does_not_change_what_a_sudachi_batch_analyzes_to() {
        let Some(plan) = sudachi_test_plan() else {
            return;
        };
        assert_lanes_preserve_analysis(&plan);
    }

    fn assert_lanes_preserve_analysis<P: AnalyzerPlan + Sync>(plan: &P) {
        // `upos` rather than a provider-owned key, because this asserts one
        // property over every analyzer and only the portable keys are common
        // to all of their schemas.
        let projection = plan
            .schema()
            .bind_projection([AttributeKey::new("upos").expect("upos is a valid key")])
            .expect("every morphology schema binds portable UPOS");
        let texts = [
            "これはテストです。",
            "短い。",
            "少し長い文章を解析してみます。",
            "国立国語研究所で研究をしています。",
            "はい。",
        ];
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(4)
            .build()
            .expect("a four-thread pool");

        let mut single = plan.create_session();
        let expected = single
            .analyze_bounded_batch(units(&texts), &projection)
            .expect("single-session analysis");

        for lanes in [1_usize, 2, 3, 8] {
            let mut spread = AnalyzerLanes::new(plan, lanes, &pool);
            let actual = spread
                .analyze_bounded_batch(units(&texts), &projection)
                .expect("lane-parallel analysis");
            assert_eq!(actual, expected, "{lanes} lanes changed the analysis");
        }
    }

    /// Available only where a real dictionary is; the invariant is unchanged
    /// without one, so the test reports rather than fails.
    fn fixed_resource(variable: &str) -> Option<String> {
        std::env::var(variable)
            .ok()
            .filter(|path| !path.is_empty() && std::path::Path::new(path).exists())
    }

    fn vibrato_test_plan() -> Option<VibratoPlan> {
        let path = fixed_resource("LINGREP_DICTIONARY_PATH")?;
        let tokenizer = crate::dictionary::create_tokenizer_from_path(&path).ok()?;
        Some(VibratoPlan::with_mode(tokenizer, SegmentationMode::Input))
    }

    fn sudachi_test_plan() -> Option<SudachiPlan> {
        let dictionary = fixed_resource("LINGREP_SUDACHI_DICT_CORE")?;
        let resources = fixed_resource("LINGREP_SUDACHI_RESOURCES")?;
        let (dictionary_digest, resource_digest) =
            SudachiConfig::content_digests(&dictionary, &SudachiResources::directory(&resources))
                .ok()?;
        let config = SudachiConfig::new(
            dictionary,
            SudachiResources::directory(resources),
            dictionary_digest,
            resource_digest,
            SudachiEdition::Core,
            SudachiSplit::C,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            NormalizationPolicy::None,
        )
        .ok()?;
        SudachiPlan::load(config).ok()
    }
}
