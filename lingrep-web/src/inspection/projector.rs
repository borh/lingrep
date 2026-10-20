use std::ops::Range;
use std::sync::LazyLock;

use compact_str::CompactString;
use lingrep_core::token_match::{AttributeKey, TokenInfo, TokenInfoTrait, TokenSequence};

use super::{TOKEN_FIELD_CATALOG, TokenFieldGroupKey, TokenFieldMeta};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenField {
    pub meta: &'static TokenFieldMeta,
    pub value: Option<CompactString>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenFieldGroup {
    pub key: TokenFieldGroupKey,
    pub fields: Box<[TokenField]>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TokenSummary {
    pub orth: Option<CompactString>,
    pub lemma: Option<CompactString>,
    pub pos: Option<CompactString>,
    pub cform: Option<CompactString>,
}

static FORM: LazyLock<AttributeKey> =
    LazyLock::new(|| AttributeKey::new("text").expect("form is canonical"));
static LEMMA: LazyLock<AttributeKey> =
    LazyLock::new(|| AttributeKey::new("lemma").expect("lemma is canonical"));
static UPOS: LazyLock<AttributeKey> =
    LazyLock::new(|| AttributeKey::new("upos").expect("upos is canonical"));
static CFORM: LazyLock<AttributeKey> =
    LazyLock::new(|| AttributeKey::new("unidic.cform").expect("unidic.cform is canonical"));

impl From<&TokenInfo> for TokenSummary {
    fn from(token: &TokenInfo) -> Self {
        Self {
            orth: token.orth().map(CompactString::from),
            lemma: token.lemma().map(CompactString::from),
            pos: token.pos().map(CompactString::from),
            cform: token.cform().map(CompactString::from),
        }
    }
}

impl TokenSummary {
    fn from_sequence<S: TokenSequence + ?Sized>(tokens: &S, index: usize) -> Self {
        Self {
            orth: tokens.scalar_key(index, &FORM).map(CompactString::new),
            lemma: tokens.scalar_key(index, &LEMMA).map(CompactString::new),
            pos: tokens.scalar_key(index, &UPOS).map(CompactString::new),
            cform: tokens.scalar_key(index, &CFORM).map(CompactString::new),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInspection {
    pub summary: TokenSummary,
    pub groups: Box<[TokenFieldGroup]>,
    pub available_fields: usize,
}

impl TokenInspection {
    pub fn field(&self, key: &str) -> Option<&TokenField> {
        self.groups
            .iter()
            .flat_map(|group| group.fields.iter())
            .find(|field| field.meta.key == key)
    }

    fn from_fields(
        summary: TokenSummary,
        fields: Vec<TokenField>,
        available_fields: usize,
    ) -> Self {
        let mut groups = Vec::new();
        for key in [
            TokenFieldGroupKey::Surface,
            TokenFieldGroupKey::Lexeme,
            TokenFieldGroupKey::Pos,
            TokenFieldGroupKey::Forms,
            TokenFieldGroupKey::Reading,
            TokenFieldGroupKey::Variation,
            TokenFieldGroupKey::Accent,
        ] {
            groups.push(TokenFieldGroup {
                key,
                fields: fields
                    .iter()
                    .filter(|field| field.meta.group == key)
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_boxed_slice(),
            });
        }
        Self {
            summary,
            groups: groups.into_boxed_slice(),
            available_fields,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenRole {
    Context,
    Match,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InspectedToken {
    pub sentence_index: usize,
    pub role: TokenRole,
    pub summary: TokenSummary,
    pub capture_membership: Box<[CaptureOrdinal]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaptureOrdinal(usize);

impl CaptureOrdinal {
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    pub const fn index(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCapture {
    pub ordinal: CaptureOrdinal,
    pub name: String,
    pub path: String,
    pub span: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureInspection {
    pub ordinal: CaptureOrdinal,
    pub name: String,
    pub path: String,
    pub span: Range<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureLane {
    pub captures: Box<[CaptureOrdinal]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchInspection {
    pub tokens: Box<[InspectedToken]>,
    pub visible_range: Range<usize>,
    pub captures: Box<[CaptureInspection]>,
    pub capture_lanes: Box<[CaptureLane]>,
}

fn ranges_overlap(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

pub fn assign_capture_lanes(captures: &[ResolvedCapture]) -> Box<[CaptureLane]> {
    let mut ordered = captures.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        (
            left.span.start,
            left.span.end,
            left.path.as_str(),
            left.ordinal,
        )
            .cmp(&(
                right.span.start,
                right.span.end,
                right.path.as_str(),
                right.ordinal,
            ))
    });
    let ranges = captures
        .iter()
        .map(|capture| (capture.ordinal, &capture.span))
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut lanes: Vec<Vec<CaptureOrdinal>> = Vec::new();
    for capture in ordered {
        if let Some(lane) = lanes.iter_mut().find(|lane| {
            lane.iter()
                .all(|ordinal| !ranges_overlap(ranges[ordinal], &capture.span))
        }) {
            lane.push(capture.ordinal);
        } else {
            lanes.push(vec![capture.ordinal]);
        }
    }
    lanes
        .into_iter()
        .map(|captures| CaptureLane {
            captures: captures.into_boxed_slice(),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice()
}

pub struct InspectionProjector;

impl InspectionProjector {
    pub fn project_token(token: &TokenInfo) -> TokenInspection {
        let fields = TOKEN_FIELD_CATALOG
            .iter()
            .map(|meta| TokenField {
                meta,
                value: token.get(meta.attribute).cloned(),
            })
            .collect::<Vec<_>>();
        let available_fields = fields.iter().filter(|field| field.value.is_some()).count();
        TokenInspection::from_fields(TokenSummary::from(token), fields, available_fields)
    }

    pub fn project_window<S: lingrep_core::token_match::TokenSequence + ?Sized>(
        tokens: &S,
        match_span: Range<usize>,
        context: usize,
    ) -> MatchInspection {
        let visible_range = match_span.start.saturating_sub(context)
            ..match_span.end.saturating_add(context).min(tokens.len());
        let projected = visible_range
            .clone()
            .map(|sentence_index| InspectedToken {
                sentence_index,
                role: if match_span.contains(&sentence_index) {
                    TokenRole::Match
                } else {
                    TokenRole::Context
                },
                summary: TokenSummary::from_sequence(tokens, sentence_index),
                capture_membership: Box::default(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        MatchInspection {
            tokens: projected,
            visible_range,
            captures: Box::default(),
            capture_lanes: Box::default(),
        }
    }

    pub fn project_match<S: lingrep_core::token_match::TokenSequence + ?Sized>(
        tokens: &S,
        match_span: Range<usize>,
        captures: &[ResolvedCapture],
        context: usize,
    ) -> MatchInspection {
        let mut inspection = Self::project_window(tokens, match_span, context);
        for token in &mut inspection.tokens {
            token.capture_membership = captures
                .iter()
                .filter(|capture| capture.span.contains(&token.sentence_index))
                .map(|capture| capture.ordinal)
                .collect::<Vec<_>>()
                .into_boxed_slice();
        }
        inspection.captures = captures
            .iter()
            .map(|capture| CaptureInspection {
                ordinal: capture.ordinal,
                name: capture.name.clone(),
                path: capture.path.clone(),
                span: capture.span.clone(),
                text: capture
                    .span
                    .clone()
                    .filter_map(|index| tokens.scalar_key(index, &FORM))
                    .collect(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        inspection.capture_lanes = assign_capture_lanes(captures);
        inspection
    }
}
