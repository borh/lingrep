use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use compact_str::CompactString;
use unicode_segmentation::UnicodeSegmentation;

use super::{
    AnalysisError, AnalyzerSession, NormalizationPolicy, TextUnit, TextUnitKind,
    ValidatedAnalysisBatch, analyzer::sealed,
};
use crate::token_match::{
    AttributeKey, AttributeKind, AttributeValueRef, BoundProjection, BoundTokenSentence,
    ProjectedAttributeValue, TokenSchema, TokenSequence,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextAnnotation {
    /// Nonempty UTF-8 byte range in the owning annotated text.
    pub range: Range<usize>,
    pub attribute: AttributeKey,
    pub values: Box<[CompactString]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnotatedText {
    text: String,
    annotations: Box<[TextAnnotation]>,
}

impl AnnotatedText {
    pub fn new(text: String, mut annotations: Vec<TextAnnotation>) -> Result<Self, AnalysisError> {
        for annotation in &mut annotations {
            if annotation.range.is_empty() || text.get(annotation.range.clone()).is_none() {
                return Err(AnalysisError::InvalidSourceRange);
            }
            annotation.values.sort();
            let values = annotation.values.to_vec();
            annotation.values = values
                .into_iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        annotations.sort_by(|a, b| {
            (&a.range.start, &a.range.end, &a.attribute, &a.values).cmp(&(
                &b.range.start,
                &b.range.end,
                &b.attribute,
                &b.values,
            ))
        });
        Ok(Self {
            text,
            annotations: annotations.into_boxed_slice(),
        })
    }

    pub fn annotation_bytes(annotations: &[TextAnnotation]) -> usize {
        std::mem::size_of_val(annotations)
            + annotations
                .iter()
                .map(|a| {
                    a.attribute.as_str().len()
                        + std::mem::size_of_val(a.values.as_ref())
                        + a.values.iter().map(CompactString::len).sum::<usize>()
                })
                .sum::<usize>()
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn annotations(&self) -> &[TextAnnotation] {
        &self.annotations
    }
    pub fn into_parts(self) -> (String, Box<[TextAnnotation]>) {
        (self.text, self.annotations)
    }
    pub fn resident_bytes(&self) -> usize {
        self.text.capacity()
            + std::mem::size_of_val(self.annotations.as_ref())
            + self
                .annotations
                .iter()
                .map(|a| {
                    a.attribute.as_str().len()
                        + std::mem::size_of_val(a.values.as_ref())
                        + a.values.iter().map(CompactString::len).sum::<usize>()
                })
                .sum::<usize>()
    }

    /// Rejects annotation boundaries that cannot be preserved through normalization.
    pub fn normalize(self, policy: NormalizationPolicy) -> Result<Self, AnalysisError> {
        if policy == NormalizationPolicy::None
            || self.annotations.is_empty()
            || policy.normalizes(&self.text, &self.text)
        {
            return Ok(Self {
                text: policy.apply(self.text),
                annotations: self.annotations,
            });
        }
        let mut boundaries = BTreeMap::new();
        for annotation in &self.annotations {
            boundaries.insert(annotation.range.start, 0);
            boundaries.insert(annotation.range.end, 0);
        }
        let mut normalized = String::new();
        let mut mapping_work = 0usize;
        for (start, cluster) in self.text.grapheme_indices(true) {
            let end = start + cluster.len();
            let projected = policy.apply(cluster.to_owned());
            mapping_work = mapping_work.saturating_add(
                cluster
                    .len()
                    .saturating_mul(boundaries.range(start..end).count()),
            );
            if mapping_work > 64 * 1024 * 1024 {
                return Err(AnalysisError::ProviderFailure(
                    "source annotation normalization exceeds its mapping-work budget".into(),
                ));
            }
            for (&boundary, mapped) in boundaries.range_mut(start..end) {
                let prefix = policy.apply(
                    cluster
                        .get(..boundary - start)
                        .ok_or(AnalysisError::InvalidAlignment)?
                        .to_owned(),
                );
                let suffix = policy.apply(
                    cluster
                        .get(boundary - start..)
                        .ok_or(AnalysisError::InvalidAlignment)?
                        .to_owned(),
                );
                if prefix.len() + suffix.len() != projected.len()
                    || !projected.starts_with(&prefix)
                    || !projected.ends_with(&suffix)
                {
                    return Err(AnalysisError::InvalidAlignment);
                }
                *mapped = normalized.len() + prefix.len();
            }
            normalized.push_str(&projected);
        }
        if let Some(last) = boundaries.get_mut(&self.text.len()) {
            *last = normalized.len();
        }
        let mut annotations = self.annotations;
        for annotation in &mut annotations {
            annotation.range =
                boundaries[&annotation.range.start]..boundaries[&annotation.range.end];
        }
        Ok(Self {
            text: normalized,
            annotations,
        })
    }
}

/// Applies source sets to complete analyzer surfaces; annotation keys must be absent from the base schema.
pub struct AnnotatedAnalysisSession<A> {
    inner: A,
    schema: TokenSchema,
    max_output_bytes: usize,
}
impl<A> AnnotatedAnalysisSession<A> {
    pub fn new(inner: A, schema: TokenSchema, max_output_bytes: usize) -> Self {
        Self {
            inner,
            schema,
            max_output_bytes,
        }
    }
}
impl<A: AnalyzerSession> sealed::Session for AnnotatedAnalysisSession<A> {}
impl<A: AnalyzerSession> AnalyzerSession for AnnotatedAnalysisSession<A> {
    fn schema(&self) -> &TokenSchema {
        &self.schema
    }
    fn finish(&mut self) -> Result<(), AnalysisError> {
        self.inner.finish()
    }
    fn analyze_bounded_batch(
        &mut self,
        mut inputs: Vec<TextUnit>,
        projection: &BoundProjection,
    ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
        let base = self.inner.schema();
        for annotation in inputs.iter().flat_map(TextUnit::annotations) {
            if base.keys().contains(&annotation.attribute)
                || self.schema.kind_for_key(&annotation.attribute) != Some(AttributeKind::StringSet)
            {
                return Err(AnalysisError::UnsupportedProjection);
            }
        }
        let base_projection = base
            .bind_projection(
                projection
                    .keys()
                    .iter()
                    .filter(|key| base.keys().contains(key))
                    .cloned(),
            )
            .map_err(|_| AnalysisError::UnsupportedProjection)?;
        let annotations = inputs
            .iter_mut()
            .map(|input| {
                (
                    input.id(),
                    input.identity(),
                    std::mem::take(&mut input.annotations),
                )
            })
            .collect::<Vec<_>>();
        let batch = self.inner.analyze_bounded_batch(inputs, &base_projection)?;
        let mut annotation_output_bytes = 0usize;
        if batch.inputs().len() != annotations.len() {
            return Err(AnalysisError::InvalidAlignment);
        }
        let mut inputs = Vec::new();
        for (input, (id, identity, annotations)) in
            batch.into_inputs().into_vec().into_iter().zip(annotations)
        {
            let (mut input, sentences) = input.into_parts();
            if input.id() != id || input.identity() != identity {
                return Err(AnalysisError::InvalidAlignment);
            }
            input.annotations = annotations;
            let mut rebuilt = Vec::new();
            for mut sentence in sentences {
                let words = &sentence.words;
                let mut projected: BTreeMap<(usize, usize), BTreeSet<CompactString>> =
                    BTreeMap::new();
                for annotation in input.annotations() {
                    let Some(ordinal) = projection.ordinal_for_key(&annotation.attribute) else {
                        continue;
                    };
                    let field = ordinal.index();
                    if projection.kinds()[field] != AttributeKind::StringSet {
                        return Err(AnalysisError::UnsupportedProjection);
                    }
                    let surfaces = &sentence.surface_tokens;
                    let first = surfaces.partition_point(|s| {
                        (s.source_range().start() as usize) < annotation.range.start
                    });
                    for surface in &surfaces[first..] {
                        if surface.source_range().end() as usize > annotation.range.end {
                            break;
                        }
                        for word in surface.word_range() {
                            let target = projected.entry((word as usize, field)).or_default();
                            for value in &annotation.values {
                                if !target.contains(value) {
                                    annotation_output_bytes = annotation_output_bytes
                                        .saturating_add(
                                            value.len() + std::mem::size_of::<CompactString>() + 64,
                                        );
                                    if annotation_output_bytes > self.max_output_bytes {
                                        return Err(AnalysisError::ProviderFailure("projected source annotations exceed tokenized-memory limit".into()));
                                    }
                                    target.insert(value.clone());
                                }
                            }
                        }
                    }
                }
                let projected = &projected;
                let values = (0..words.len())
                    .flat_map(|word| {
                        projection
                            .keys()
                            .iter()
                            .enumerate()
                            .map(move |(field, key)| {
                                if let Some(set) = projected.get(&(word, field)) {
                                    return ProjectedAttributeValue::string_set(
                                        set.iter().cloned(),
                                    );
                                }
                                match words.value_key(word, key) {
                                    Some(AttributeValueRef::Scalar(value)) => {
                                        ProjectedAttributeValue::scalar(value)
                                    }
                                    Some(AttributeValueRef::StringSet(value)) => {
                                        ProjectedAttributeValue::string_set(value.iter().cloned())
                                    }
                                    None if projection.kinds()[field]
                                        == AttributeKind::StringSet =>
                                    {
                                        ProjectedAttributeValue::string_set(std::iter::empty::<
                                            CompactString,
                                        >(
                                        ))
                                    }
                                    None => ProjectedAttributeValue::missing_scalar(),
                                }
                            })
                    })
                    .collect();
                sentence.words = BoundTokenSentence::from_projected_attribute_values(
                    projection,
                    words.len(),
                    values,
                )
                .map_err(|_| AnalysisError::UnsupportedProjection)?;
                rebuilt.push(sentence);
            }
            let mode = match input.kind() {
                TextUnitKind::Sentence => super::SegmentationMode::Input,
                TextUnitKind::Segment => super::SegmentationMode::Analyzer,
            };
            inputs.push(super::AnalyzedInput::new(input, rebuilt, mode)?);
        }
        ValidatedAnalysisBatch::new(inputs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{
        AnalyzedInput, AnalyzedSentence, InputUnitId, SegmentationMode, SurfaceToken,
    };
    fn annotated(text: &str, range: Range<usize>) -> AnnotatedText {
        AnnotatedText::new(
            text.into(),
            vec![TextAnnotation {
                range,
                attribute: AttributeKey::new("tei.ruby").unwrap(),
                values: vec!["ねこいぬ".into()].into_boxed_slice(),
            }],
        )
        .unwrap()
    }
    #[test]
    fn nfc_maps_exact_annotation_boundaries_and_rejects_split_compositions() {
        let value = annotated("e\u{301}猫", 0..3)
            .normalize(NormalizationPolicy::Nfc)
            .unwrap();
        assert_eq!(value.text(), "é猫");
        assert_eq!(value.annotations()[0].range, 0..2);
        assert_eq!(
            annotated("e\u{301}", 0..1).normalize(NormalizationPolicy::Nfc),
            Err(AnalysisError::InvalidAlignment)
        );
        let value = annotated("\u{1100}\u{1161}猫", 0..6)
            .normalize(NormalizationPolicy::Nfc)
            .unwrap();
        assert_eq!(value.text(), "가猫");
        assert_eq!(value.annotations()[0].range, 0..3);
    }
    struct TwoWords {
        schema: TokenSchema,
        multiword: bool,
    }
    impl sealed::Session for TwoWords {}
    impl AnalyzerSession for TwoWords {
        fn schema(&self) -> &TokenSchema {
            &self.schema
        }
        fn analyze_bounded_batch(
            &mut self,
            inputs: Vec<TextUnit>,
            projection: &BoundProjection,
        ) -> Result<ValidatedAnalysisBatch, AnalysisError> {
            let inputs = inputs
                .into_iter()
                .map(|input| {
                    let words = BoundTokenSentence::from_projected_attribute_values(
                        projection,
                        2,
                        ["猫猫", "犬"]
                            .into_iter()
                            .flat_map(|text| {
                                projection
                                    .keys()
                                    .iter()
                                    .map(move |_| ProjectedAttributeValue::scalar(text))
                            })
                            .collect(),
                    )
                    .unwrap();
                    let spans = if self.multiword {
                        vec![SurfaceToken::new(input.source_range(0, 9).unwrap(), 0, 2)]
                    } else {
                        vec![
                            SurfaceToken::new(input.source_range(0, 6).unwrap(), 0, 1),
                            SurfaceToken::new(input.source_range(6, 9).unwrap(), 1, 2),
                        ]
                    }
                    .into_boxed_slice();
                    let sentence =
                        AnalyzedSentence::new(0, input.source_range(0, 9).unwrap(), spans, words)?;
                    AnalyzedInput::new(input, vec![sentence], SegmentationMode::Input)
                })
                .collect::<Result<Vec<_>, _>>()?;
            ValidatedAnalysisBatch::new(inputs)
        }
    }
    #[test]
    fn complete_surface_containment_controls_annotation_membership() {
        let base = TokenSchema::from_keys(["text"]).unwrap();
        let schema = crate::corpus::tei::annotated_schema(&base).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let mut analyzer = AnnotatedAnalysisSession::new(
            TwoWords {
                schema: base,
                multiword: false,
            },
            schema,
            32 * 1024 * 1024,
        );
        let input = TextUnit::annotated(
            InputUnitId::new(0),
            TextUnitKind::Sentence,
            annotated("猫猫犬", 3..9),
            NormalizationPolicy::None,
        )
        .unwrap();
        let result = analyzer
            .analyze_bounded_batch(vec![input], &projection)
            .unwrap();
        let words = result.inputs()[0].sentences()[0].words();
        assert_eq!(
            words.value_key(0, &AttributeKey::new("tei.ruby").unwrap()),
            None
        );
        assert_eq!(
            words.value_key(1, &AttributeKey::new("tei.ruby").unwrap()),
            Some(AttributeValueRef::StringSet(&["ねこいぬ".into()]))
        );
        assert_eq!(
            words.char_span(0),
            Some(crate::token_match::CharSpan { start: 0, end: 2 })
        );
        assert_eq!(
            words.char_span(1),
            Some(crate::token_match::CharSpan { start: 2, end: 3 })
        );
    }
    #[hegel::test(test_cases = 300, derandomize = true)]
    fn normalized_annotation_ranges_match_independent_prefix_and_suffix_normalization(
        tc: hegel::TestCase,
    ) {
        use hegel::generators as g;
        use unicode_normalization::UnicodeNormalization;
        let parts = tc.draw(
            g::vecs(g::sampled_from(vec![
                "猫",
                "e\u{301}",
                "é",
                "\u{1100}\u{1161}",
                "가",
                "𝒜",
                "A\u{308}\u{323}",
                "\u{301}",
            ]))
            .min_size(1)
            .max_size(12),
        );
        let raw = parts.concat();
        let mut boundaries = raw.char_indices().map(|(i, _)| i).collect::<Vec<_>>();
        boundaries.push(raw.len());
        let a = tc.draw(g::integers::<usize>().max_value(boundaries.len() - 2));
        let b = tc.draw(
            g::integers::<usize>()
                .min_value(a + 1)
                .max_value(boundaries.len() - 1),
        );
        let (a, b) = (boundaries[a], boundaries[b]);
        let normalized = raw.nfc().collect::<String>();
        let before = raw.get(..a).unwrap().nfc().collect::<String>();
        let selected = raw.get(a..b).unwrap().nfc().collect::<String>();
        let after = raw.get(b..).unwrap().nfc().collect::<String>();
        let result = annotated(&raw, a..b).normalize(NormalizationPolicy::Nfc);
        if format!("{before}{selected}{after}") == normalized {
            let value = result.unwrap();
            assert_eq!(value.text(), normalized);
            assert_eq!(
                value.annotations()[0].range,
                before.len()..before.len() + selected.len()
            );
        } else {
            assert_eq!(result, Err(AnalysisError::InvalidAlignment));
        }
    }

    #[hegel::test(test_cases = 200, derandomize = true)]
    fn source_field_projection_matches_surface_containment_across_schema_orders(
        tc: hegel::TestCase,
    ) {
        use hegel::generators as g;
        let start = tc.draw(g::integers::<usize>().max_value(2));
        let end = tc.draw(g::integers::<usize>().min_value(start + 1).max_value(3));
        let key = tc.draw(g::sampled_from(
            crate::corpus::tei::ANNOTATION_KEYS.to_vec(),
        ));
        let value = tc.draw(g::text().min_size(1).max_size(64));
        let base = if tc.draw(g::booleans()) {
            TokenSchema::from_keys(["text"]).unwrap()
        } else {
            TokenSchema::from_keys(["text", "lemma", "norm", "upos", "xpos"]).unwrap()
        };
        let schema = crate::corpus::tei::annotated_schema(&base).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        let mut analyzer = AnnotatedAnalysisSession::new(
            TwoWords {
                schema: base,
                multiword: false,
            },
            schema,
            1024 * 1024,
        );
        let text = AnnotatedText::new(
            "猫猫犬".into(),
            vec![TextAnnotation {
                range: start * 3..end * 3,
                attribute: AttributeKey::new(key).unwrap(),
                values: vec![value.clone().into()].into_boxed_slice(),
            }],
        )
        .unwrap();
        let input = TextUnit::annotated(
            InputUnitId::new(0),
            TextUnitKind::Sentence,
            text,
            NormalizationPolicy::None,
        )
        .unwrap();
        let output = analyzer
            .analyze_bounded_batch(vec![input], &projection)
            .unwrap();
        let words = output.inputs()[0].sentences()[0].words();
        for (word, (a, b)) in [(0, 2), (2, 3)].into_iter().enumerate() {
            let expected = if start <= a && b <= end {
                Some(AttributeValueRef::StringSet(&[CompactString::new(&value)]))
            } else {
                None
            };
            assert_eq!(
                words.value_key(word, &AttributeKey::new(key).unwrap()),
                expected
            );
        }
        assert_eq!(
            words
                .get_key(0, &AttributeKey::new("text").unwrap())
                .map(CompactString::as_str),
            Some("猫猫")
        );
        assert_eq!(
            words
                .get_key(1, &AttributeKey::new("text").unwrap())
                .map(CompactString::as_str),
            Some("犬")
        );
    }

    #[test]
    fn multiword_annotation_projection_preserves_values_and_enforces_the_budget() {
        let base = TokenSchema::from_keys(["text"]).unwrap();
        let schema = crate::corpus::tei::annotated_schema(&base).unwrap();
        let projection = schema
            .bind_projection(schema.keys().iter().cloned())
            .unwrap();
        for (range, budget, expected) in [
            (0..9, 1024, Some(true)),
            (3..9, 1024, Some(false)),
            (0..9, 1, None),
        ] {
            let input = TextUnit::annotated(
                InputUnitId::new(0),
                TextUnitKind::Sentence,
                annotated("猫猫犬", range),
                NormalizationPolicy::None,
            )
            .unwrap();
            let mut analyzer = AnnotatedAnalysisSession::new(
                TwoWords {
                    schema: base.clone(),
                    multiword: true,
                },
                schema.clone(),
                budget,
            );
            let output = analyzer.analyze_bounded_batch(vec![input], &projection);
            if let Some(present) = expected {
                let output = output.unwrap();
                let words = output.inputs()[0].sentences()[0].words();
                for word in 0..2 {
                    let expected_values = [CompactString::new("ねこいぬ")];
                    assert_eq!(
                        words.value_key(word, &AttributeKey::new("tei.ruby").unwrap()),
                        if present {
                            Some(AttributeValueRef::StringSet(&expected_values))
                        } else {
                            None
                        }
                    );
                    assert_eq!(
                        words.char_span(word),
                        Some(crate::token_match::CharSpan { start: 0, end: 3 })
                    );
                }
            } else {
                assert!(matches!(output, Err(AnalysisError::ProviderFailure(_))));
            }
        }
    }
}
