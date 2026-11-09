//! Rewrites `skip:` definitions so matching and captures exclude skipped tokens.

use compact_str::CompactString;

use super::model::{
    LookDirection, LookPolarity, LookaroundExpr, ParsedExpr, ParsedSkip, ParsedTest, SourceSpan,
};
use crate::fsm::PositionAssertion;

/// A construct the rewrite does not support, with the span to report.
pub(super) enum Unsupported {
    Reference {
        name: CompactString,
        span: SourceSpan,
    },
    SameAs {
        span: SourceSpan,
    },
}

/// The lookarounds the rewrite introduces. Each is keyed by its own one-byte
/// span inside the `skip` keyword, since compiled lookarounds are identified
/// by span and the body of each kind depends only on the skipped test.
#[derive(Clone, Copy)]
enum Synthesized {
    SentenceStart = 0,
    SentenceEnd = 1,
    LeadingBoundary = 2,
    TrailingBoundary = 3,
}

pub(super) struct SkipRewrite {
    test: CompactString,
    test_span: SourceSpan,
    spans: [SourceSpan; 4],
}

impl SkipRewrite {
    pub(super) fn new(skip: &ParsedSkip) -> Self {
        let start = skip.key_span.start() as usize;
        let byte = |offset: usize| {
            SourceSpan::new(skip.key_span.source(), start + offset..start + offset + 1)
                .expect("the four bytes of `skip` lie inside its own key span")
        };
        Self {
            test: skip.test.clone(),
            test_span: skip.test_span,
            spans: [byte(0), byte(1), byte(2), byte(3)],
        }
    }

    pub(super) fn pattern(&self, expr: ParsedExpr) -> Result<ParsedExpr, Unsupported> {
        let rewritten = self.rewrite(expr, false)?;
        Ok(self.bounded(rewritten))
    }

    fn skipped(&self) -> ParsedTest {
        ParsedTest::Reference {
            name: self.test.clone(),
            span: self.test_span,
        }
    }

    fn not_skipped(&self) -> ParsedTest {
        ParsedTest::Not(Box::new(self.skipped()))
    }

    fn gap(&self) -> ParsedExpr {
        ParsedExpr::Repeat {
            expr: Box::new(ParsedExpr::Token(self.skipped(), self.test_span)),
            min: 0,
            max: None,
        }
    }

    fn is_gap_token(&self, test: &ParsedTest) -> bool {
        matches!(test, ParsedTest::Reference { name, span }
            if *name == self.test && *span == self.test_span)
    }

    fn lookaround(
        &self,
        kind: Synthesized,
        direction: LookDirection,
        polarity: LookPolarity,
        body: ParsedExpr,
    ) -> ParsedExpr {
        ParsedExpr::Lookaround(LookaroundExpr {
            direction,
            polarity,
            scope: None,
            skip_reach: None,
            body: Box::new(body),
            span: self.spans[kind as usize],
        })
    }

    /// Guards `expr` so that a nonempty span it matches neither starts nor
    /// ends on a skipped token; an empty span is not guarded.
    fn bounded(&self, expr: ParsedExpr) -> ParsedExpr {
        let edges = self.edges(&expr);
        if !edges.lead && !edges.trail {
            return expr;
        }
        let mut items = Vec::with_capacity(3);
        if edges.lead {
            items.push(self.lookaround(
                Synthesized::LeadingBoundary,
                LookDirection::FollowedBy,
                LookPolarity::Negative,
                ParsedExpr::Token(self.skipped(), self.test_span),
            ));
        }
        items.push(expr);
        if edges.trail {
            items.push(self.lookaround(
                Synthesized::TrailingBoundary,
                LookDirection::PrecededBy,
                LookPolarity::Negative,
                ParsedExpr::Token(self.skipped(), self.test_span),
            ));
        }
        let guarded = ParsedExpr::Sequence(items);
        if edges.nullable {
            ParsedExpr::Alternation(vec![guarded, ParsedExpr::Sequence(Vec::new())])
        } else {
            guarded
        }
    }

    fn rewrite(&self, expr: ParsedExpr, scoped: bool) -> Result<ParsedExpr, Unsupported> {
        Ok(match expr {
            ParsedExpr::Token(test, span) => {
                ParsedExpr::Token(ParsedTest::And(vec![test, self.not_skipped()]), span)
            }
            ParsedExpr::Assertion(
                assertion @ (PositionAssertion::SentenceStart | PositionAssertion::SentenceEnd),
            ) if scoped => ParsedExpr::Sequence(vec![
                self.gap(),
                ParsedExpr::Assertion(assertion),
                self.gap(),
            ]),
            ParsedExpr::Assertion(PositionAssertion::SentenceStart) => self.lookaround(
                Synthesized::SentenceStart,
                LookDirection::PrecededBy,
                LookPolarity::Positive,
                ParsedExpr::Sequence(vec![
                    ParsedExpr::Assertion(PositionAssertion::SentenceStart),
                    self.gap(),
                ]),
            ),
            ParsedExpr::Assertion(PositionAssertion::SentenceEnd) => self.lookaround(
                Synthesized::SentenceEnd,
                LookDirection::FollowedBy,
                LookPolarity::Positive,
                ParsedExpr::Sequence(vec![
                    self.gap(),
                    ParsedExpr::Assertion(PositionAssertion::SentenceEnd),
                ]),
            ),
            ParsedExpr::Assertion(assertion) => ParsedExpr::Assertion(assertion),
            ParsedExpr::Lookaround(lookaround) => {
                let scoped = lookaround.scope.is_some();
                let skip_reach = if scoped {
                    max_width(&lookaround.body)
                } else {
                    None
                };
                let body = self.rewrite(*lookaround.body, scoped)?;
                let padded = match lookaround.direction {
                    LookDirection::PrecededBy => vec![body, self.gap()],
                    LookDirection::FollowedBy => vec![self.gap(), body],
                };
                ParsedExpr::Lookaround(LookaroundExpr {
                    body: Box::new(ParsedExpr::Sequence(padded)),
                    skip_reach,
                    ..lookaround
                })
            }
            ParsedExpr::Sequence(items) => {
                let mut rewritten = Vec::with_capacity(items.len() * 2);
                let mut consumed_before = false;
                for item in items {
                    let consumes = !zero_width(&item);
                    if consumes && consumed_before {
                        rewritten.push(self.gap());
                    }
                    consumed_before |= consumes;
                    rewritten.push(self.rewrite(item, scoped)?);
                }
                ParsedExpr::Sequence(rewritten)
            }
            ParsedExpr::Alternation(items) => {
                ParsedExpr::Alternation(self.rewrite_all(items, scoped)?)
            }
            ParsedExpr::Intersection { operands, span } => ParsedExpr::Intersection {
                operands: self.rewrite_all(operands, scoped)?,
                span,
            },
            ParsedExpr::AnyOrder { operands, span } => ParsedExpr::AnyOrder {
                operands: self.rewrite_all(operands, scoped)?,
                span,
            },
            ParsedExpr::Repeat { expr, min, max } => self.repeat(*expr, min, max, scoped)?,
            ParsedExpr::Group(expr) => ParsedExpr::Group(Box::new(self.rewrite(*expr, scoped)?)),
            ParsedExpr::NamedGroup { name, expr, span } => {
                let rewritten = self.rewrite(*expr, scoped)?;
                ParsedExpr::NamedGroup {
                    name,
                    expr: Box::new(if scoped {
                        rewritten
                    } else {
                        self.bounded(rewritten)
                    }),
                    span,
                }
            }
            ParsedExpr::SameAs { span, .. } => return Err(Unsupported::SameAs { span }),
            ParsedExpr::Reference { name, span } => {
                return Err(Unsupported::Reference { name, span });
            }
        })
    }

    fn rewrite_all(
        &self,
        items: Vec<ParsedExpr>,
        scoped: bool,
    ) -> Result<Vec<ParsedExpr>, Unsupported> {
        items
            .into_iter()
            .map(|item| self.rewrite(item, scoped))
            .collect()
    }

    /// `E{m,n}` with a gap between consecutive iterations.
    fn repeat(
        &self,
        expr: ParsedExpr,
        min: usize,
        max: Option<usize>,
        scoped: bool,
    ) -> Result<ParsedExpr, Unsupported> {
        let inner = self.rewrite(expr, scoped)?;
        if max == Some(0) {
            return Ok(ParsedExpr::Repeat {
                expr: Box::new(inner),
                min,
                max,
            });
        }
        if contains_capture(&inner) {
            return Ok(ParsedExpr::Repeat {
                expr: Box::new(ParsedExpr::Sequence(vec![self.gap(), inner])),
                min,
                max,
            });
        }
        let rest = max.map(|max| max - 1);
        let mut first = vec![inner.clone()];
        if rest != Some(0) {
            first.push(ParsedExpr::Repeat {
                expr: Box::new(ParsedExpr::Sequence(vec![self.gap(), inner])),
                min: min.max(1) - 1,
                max: rest,
            });
        }
        let iterations = ParsedExpr::Sequence(first);
        Ok(if min == 0 {
            ParsedExpr::Repeat {
                expr: Box::new(iterations),
                min: 0,
                max: Some(1),
            }
        } else {
            iterations
        })
    }

    /// Whether a rewritten expression can match the empty span, and whether
    /// its first or last consumed token can be a gap token.
    fn edges(&self, expr: &ParsedExpr) -> Edges {
        match expr {
            ParsedExpr::Token(test, _) => {
                let gap = self.is_gap_token(test);
                Edges {
                    nullable: false,
                    lead: gap,
                    trail: gap,
                }
            }
            ParsedExpr::Assertion(_) | ParsedExpr::Lookaround(_) => Edges::EMPTY,
            ParsedExpr::Sequence(items) => {
                let edges = items
                    .iter()
                    .map(|item| self.edges(item))
                    .collect::<Vec<_>>();
                let lead = edges
                    .iter()
                    .scan(true, |prefix_nullable, item| {
                        let lead = *prefix_nullable && item.lead;
                        *prefix_nullable &= item.nullable;
                        Some(lead)
                    })
                    .any(|lead| lead);
                let trail = edges
                    .iter()
                    .rev()
                    .scan(true, |suffix_nullable, item| {
                        let trail = *suffix_nullable && item.trail;
                        *suffix_nullable &= item.nullable;
                        Some(trail)
                    })
                    .any(|trail| trail);
                Edges {
                    nullable: edges.iter().all(|item| item.nullable),
                    lead,
                    trail,
                }
            }
            ParsedExpr::Alternation(items) => {
                let edges = items.iter().map(|item| self.edges(item));
                edges.fold(
                    Edges {
                        nullable: false,
                        lead: false,
                        trail: false,
                    },
                    |all, item| Edges {
                        nullable: all.nullable || item.nullable,
                        lead: all.lead || item.lead,
                        trail: all.trail || item.trail,
                    },
                )
            }
            ParsedExpr::Intersection { operands, .. } => operands
                .iter()
                .map(|operand| self.edges(operand))
                .fold(Edges::EMPTY_AND_GAPPED, |all, item| Edges {
                    nullable: all.nullable && item.nullable,
                    lead: all.lead && item.lead,
                    trail: all.trail && item.trail,
                }),
            ParsedExpr::AnyOrder { .. } => Edges {
                nullable: false,
                lead: false,
                trail: false,
            },
            ParsedExpr::Repeat { expr, min, max } => {
                let inner = self.edges(expr);
                let repeats = *max != Some(0);
                Edges {
                    nullable: *min == 0 || inner.nullable,
                    lead: repeats && inner.lead,
                    trail: repeats && inner.trail,
                }
            }
            ParsedExpr::Group(expr) => self.edges(expr),
            ParsedExpr::NamedGroup { expr, .. } => Edges {
                nullable: self.edges(expr).nullable,
                lead: false,
                trail: false,
            },
            ParsedExpr::SameAs { .. } | ParsedExpr::Reference { .. } => Edges {
                nullable: false,
                lead: false,
                trail: false,
            },
        }
    }
}

#[derive(Clone, Copy)]
struct Edges {
    nullable: bool,
    lead: bool,
    trail: bool,
}

impl Edges {
    const EMPTY: Self = Self {
        nullable: true,
        lead: false,
        trail: false,
    };
    const EMPTY_AND_GAPPED: Self = Self {
        nullable: true,
        lead: true,
        trail: true,
    };
}

fn zero_width(expr: &ParsedExpr) -> bool {
    match expr {
        ParsedExpr::Assertion(_) | ParsedExpr::Lookaround(_) => true,
        ParsedExpr::Token(..)
        | ParsedExpr::AnyOrder { .. }
        | ParsedExpr::SameAs { .. }
        | ParsedExpr::Reference { .. } => false,
        ParsedExpr::Sequence(items) | ParsedExpr::Alternation(items) => {
            items.iter().all(zero_width)
        }
        ParsedExpr::Intersection { operands, .. } => operands.iter().any(zero_width),
        ParsedExpr::Repeat { expr, max, .. } => *max == Some(0) || zero_width(expr),
        ParsedExpr::Group(expr) | ParsedExpr::NamedGroup { expr, .. } => zero_width(expr),
    }
}

fn max_width(expr: &ParsedExpr) -> Option<usize> {
    match expr {
        ParsedExpr::Token(..) => Some(1),
        ParsedExpr::Assertion(_) | ParsedExpr::Lookaround(_) => Some(0),
        ParsedExpr::Sequence(items) => items
            .iter()
            .try_fold(0usize, |total, item| total.checked_add(max_width(item)?)),
        ParsedExpr::Alternation(items)
        | ParsedExpr::Intersection {
            operands: items, ..
        } => items
            .iter()
            .try_fold(0usize, |widest, item| Some(widest.max(max_width(item)?))),
        ParsedExpr::Repeat { expr, max, .. } => max_width(expr)?.checked_mul((*max)?),
        ParsedExpr::Group(expr) | ParsedExpr::NamedGroup { expr, .. } => max_width(expr),
        ParsedExpr::AnyOrder { .. } | ParsedExpr::SameAs { .. } | ParsedExpr::Reference { .. } => {
            None
        }
    }
}

fn contains_capture(expr: &ParsedExpr) -> bool {
    match expr {
        ParsedExpr::NamedGroup { .. } => true,
        ParsedExpr::Token(..)
        | ParsedExpr::Assertion(_)
        | ParsedExpr::Lookaround(_)
        | ParsedExpr::SameAs { .. }
        | ParsedExpr::Reference { .. } => false,
        ParsedExpr::Sequence(items)
        | ParsedExpr::Alternation(items)
        | ParsedExpr::Intersection {
            operands: items, ..
        }
        | ParsedExpr::AnyOrder {
            operands: items, ..
        } => items.iter().any(contains_capture),
        ParsedExpr::Repeat { expr, .. } | ParsedExpr::Group(expr) => contains_capture(expr),
    }
}
