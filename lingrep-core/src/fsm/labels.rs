//! The label sets a catalog declares with `labels:` for its attribute literals.

use std::collections::BTreeMap;

use compact_str::CompactString;

use crate::token_match::{AttributeKey, AttributeValueRef, TokenSequence};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NamedLabelSet {
    name: &'static str,
    attribute: &'static str,
    values: &'static [&'static str],
    admits_subtypes: bool,
}

/// UD v2 universal POS tags (<https://universaldependencies.org/u/pos/>).
const UD_V2_UPOS: &[&str] = &[
    "ADJ", "ADP", "ADV", "AUX", "CCONJ", "DET", "INTJ", "NOUN", "NUM", "PART", "PRON", "PROPN",
    "PUNCT", "SCONJ", "SYM", "VERB", "X",
];

/// UD v2 universal POS tags plus `SPACE`, which spaCy assigns to whitespace
/// tokens.
const SPACY_UPOS: &[&str] = &[
    "ADJ", "ADP", "ADV", "AUX", "CCONJ", "DET", "INTJ", "NOUN", "NUM", "PART", "PRON", "PROPN",
    "PUNCT", "SCONJ", "SPACE", "SYM", "VERB", "X",
];

/// UD v2 universal relations (<https://universaldependencies.org/u/dep/>).
const UD_V2_DEPREL: &[&str] = &[
    "acl",
    "advcl",
    "advmod",
    "amod",
    "appos",
    "aux",
    "case",
    "cc",
    "ccomp",
    "clf",
    "compound",
    "conj",
    "cop",
    "csubj",
    "dep",
    "det",
    "discourse",
    "dislocated",
    "expl",
    "fixed",
    "flat",
    "goeswith",
    "iobj",
    "list",
    "mark",
    "nmod",
    "nsubj",
    "nummod",
    "obj",
    "obl",
    "orphan",
    "parataxis",
    "punct",
    "reparandum",
    "root",
    "vocative",
    "xcomp",
];

/// The ClearNLP relations of spaCy's English parsers (the `parser` labels in
/// `en_core_web_*` model metadata), with the root under both spaCy's `ROOT`
/// and the lowercased `root` that lingrep's spaCy adapter writes.
const CLEARNLP_DEPREL: &[&str] = &[
    "ROOT",
    "acl",
    "acomp",
    "advcl",
    "advmod",
    "agent",
    "amod",
    "appos",
    "attr",
    "aux",
    "auxpass",
    "case",
    "cc",
    "ccomp",
    "compound",
    "conj",
    "csubj",
    "csubjpass",
    "dative",
    "dep",
    "det",
    "dobj",
    "expl",
    "intj",
    "mark",
    "meta",
    "neg",
    "nmod",
    "npadvmod",
    "nsubj",
    "nsubjpass",
    "nummod",
    "oprd",
    "parataxis",
    "pcomp",
    "pobj",
    "poss",
    "preconj",
    "predet",
    "prep",
    "prt",
    "punct",
    "quantmod",
    "relcl",
    "root",
    "xcomp",
];

/// Penn Treebank tags as spaCy's English taggers emit them (the `tagger`
/// labels in `en_core_web_*` model metadata), which add OntoNotes' `ADD`,
/// `AFX`, `HYPH`, `NFP`, `XX` and spaCy's `_SP`.
const PENN_XPOS: &[&str] = &[
    "$", "''", ",", "-LRB-", "-RRB-", ".", ":", "ADD", "AFX", "CC", "CD", "DT", "EX", "FW", "HYPH",
    "IN", "JJ", "JJR", "JJS", "LS", "MD", "NFP", "NN", "NNP", "NNPS", "NNS", "PDT", "POS", "PRP",
    "PRP$", "RB", "RBR", "RBS", "RP", "SYM", "TO", "UH", "VB", "VBD", "VBG", "VBN", "VBP", "VBZ",
    "WDT", "WP", "WP$", "WRB", "XX", "_SP", "``",
];

pub(crate) const NAMED_LABEL_SETS: &[NamedLabelSet] = &[
    NamedLabelSet {
        name: "ud-v2",
        attribute: "upos",
        values: UD_V2_UPOS,
        admits_subtypes: false,
    },
    NamedLabelSet {
        name: "spacy",
        attribute: "upos",
        values: SPACY_UPOS,
        admits_subtypes: false,
    },
    NamedLabelSet {
        name: "ud-v2",
        attribute: "dep.rel",
        values: UD_V2_DEPREL,
        admits_subtypes: true,
    },
    NamedLabelSet {
        name: "clearnlp",
        attribute: "dep.rel",
        values: CLEARNLP_DEPREL,
        admits_subtypes: false,
    },
    NamedLabelSet {
        name: "penn",
        attribute: "xpos",
        values: PENN_XPOS,
        admits_subtypes: false,
    },
];

pub(crate) fn named_label_set(
    name: &str,
    attribute: &AttributeKey,
) -> Result<&'static NamedLabelSet, String> {
    if let Some(set) = NAMED_LABEL_SETS
        .iter()
        .find(|set| set.name == name && set.attribute == attribute.as_str())
    {
        return Ok(set);
    }
    let names: Vec<&str> = NAMED_LABEL_SETS
        .iter()
        .filter(|set| set.attribute == attribute.as_str())
        .map(|set| set.name)
        .collect();
    Err(if names.is_empty() {
        format!(
            "no named label set applies to `{}`; list its values as {{…}}",
            attribute.as_str()
        )
    } else {
        let quoted: Vec<String> = names.iter().map(|name| format!("\"{name}\"")).collect();
        let choices = match quoted.split_last() {
            Some((last, [])) => last.clone(),
            Some((last, rest)) => format!("{} or {last}", rest.join(", ")),
            None => String::new(),
        };
        format!(
            "\"{name}\" is not a label set for `{}`; use {choices}, or list the values as {{…}}",
            attribute.as_str()
        )
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LabelSet {
    /// One named set, or the union of several.
    Named(Box<[&'static NamedLabelSet]>),
    /// Sorted and deduplicated.
    Explicit(Box<[CompactString]>),
}

impl LabelSet {
    pub(crate) fn explicit(mut values: Vec<CompactString>) -> Self {
        values.sort_unstable();
        values.dedup();
        Self::Explicit(values.into_boxed_slice())
    }

    fn admits(&self, value: &str) -> bool {
        match self {
            Self::Named(sets) => sets.iter().any(|set| {
                set.values.binary_search(&value).is_ok()
                    || (set.admits_subtypes
                        && value
                            .split_once(':')
                            .is_some_and(|(base, _)| set.values.binary_search(&base).is_ok()))
            }),
            Self::Explicit(values) => values
                .binary_search_by(|member| member.as_str().cmp(value))
                .is_ok(),
        }
    }
}

/// One field of a `labels:` entry: the set of values that the declaring
/// file's literals and the corpus's tokens may take for one attribute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LabelDeclaration {
    attribute: AttributeKey,
    set: LabelSet,
    source_name: CompactString,
    line: usize,
}

impl LabelDeclaration {
    pub(crate) const fn new(
        attribute: AttributeKey,
        set: LabelSet,
        source_name: CompactString,
        line: usize,
    ) -> Self {
        Self {
            attribute,
            set,
            source_name,
            line,
        }
    }

    pub const fn attribute(&self) -> &AttributeKey {
        &self.attribute
    }

    /// The named sets joined with ` + `, or an explicit list as `{A, B}`,
    /// shortened to its first eight values and `…`.
    pub fn set_name(&self) -> String {
        match &self.set {
            LabelSet::Named(sets) => sets
                .iter()
                .map(|set| set.name)
                .collect::<Vec<_>>()
                .join(" + "),
            LabelSet::Explicit(values) => {
                let mut shown: Vec<&str> =
                    values.iter().take(8).map(CompactString::as_str).collect();
                if values.len() > 8 {
                    shown.push("…");
                }
                format!("{{{}}}", shown.join(", "))
            }
        }
    }

    pub fn source_name(&self) -> &str {
        &self.source_name
    }

    /// 1-indexed line of the `labels:` entry.
    pub const fn line(&self) -> usize {
        self.line
    }

    pub fn admits(&self, value: &str) -> bool {
        self.set.admits(value)
    }
}

const MAX_TRACKED_OUT_OF_SET_VALUES: usize = 32;

/// Token counts of values outside one declaration's set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutOfSetValues {
    /// At most 32 distinct values.
    pub values: BTreeMap<CompactString, u64>,
    /// Tokens whose out-of-set value is not a key of `values`.
    pub untracked_tokens: u64,
}

impl OutOfSetValues {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.untracked_tokens == 0
    }

    fn observe(&mut self, value: &str) {
        if let Some(count) = self.values.get_mut(value) {
            *count = count.saturating_add(1);
        } else if self.values.len() < MAX_TRACKED_OUT_OF_SET_VALUES {
            self.values.insert(value.into(), 1);
        } else {
            self.untracked_tokens = self.untracked_tokens.saturating_add(1);
        }
    }
}

/// Counts, per declaration in catalog order, the token values outside the
/// declared set. A token lacking the attribute is not counted.
pub(crate) fn observe_out_of_set(
    declarations: &[LabelDeclaration],
    sentence: &dyn TokenSequence,
    into: &mut Vec<OutOfSetValues>,
) {
    if declarations.is_empty() {
        return;
    }
    into.resize_with(declarations.len(), OutOfSetValues::default);
    for (declaration, counts) in declarations.iter().zip(into.iter_mut()) {
        for token in 0..sentence.len() {
            match sentence.value_key(token, &declaration.attribute) {
                Some(AttributeValueRef::Scalar(value)) => {
                    if !declaration.admits(value) {
                        counts.observe(value);
                    }
                }
                Some(AttributeValueRef::StringSet(values)) => {
                    for value in values {
                        if !declaration.admits(value) {
                            counts.observe(value);
                        }
                    }
                }
                None => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_sets_are_sorted_for_binary_search() {
        for set in NAMED_LABEL_SETS {
            assert!(
                set.values.windows(2).all(|pair| pair[0] < pair[1]),
                "{} for {}",
                set.name,
                set.attribute
            );
        }
    }

    #[test]
    fn ud_relation_subtypes_are_admitted_by_their_base() {
        let rel = AttributeKey::new("dep.rel").unwrap();
        let ud = named_label_set("ud-v2", &rel).unwrap();
        let clear = named_label_set("clearnlp", &rel).unwrap();
        let (ud, both, clear) = (
            LabelSet::Named(Box::new([ud])),
            LabelSet::Named(Box::new([clear, ud])),
            LabelSet::Named(Box::new([clear])),
        );
        for (set, value, admitted) in [
            (&ud, "nsubj:pass", true),
            (&ud, "obj", true),
            (&ud, "dobj", false),
            (&ud, "dobj:x", false),
            (&clear, "dobj", true),
            (&clear, "nsubj:pass", false),
            (&both, "dobj", true),
            (&both, "nsubj:pass", true),
            (&both, "nsubjpas", false),
        ] {
            assert_eq!(set.admits(value), admitted, "{value}");
        }
    }
}
