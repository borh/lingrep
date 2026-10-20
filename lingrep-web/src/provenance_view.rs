//! Pure display projection over provenance composed by `lingrep-corpus`.

use std::collections::BTreeSet;

use lingrep_corpus::{ActiveRegion, SentenceProvenance};

/// One metadata key and its ordered display values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceField {
    pub key: String,
    pub values: Box<[String]>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceDivision {
    pub source_id: Option<String>,
    pub id: u64,
    pub kind: String,
    pub parent_id: Option<u64>,
    pub fields: Box<[ProvenanceField]>,
}

/// Everything the hit-detail panel shows for one hit's sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitProvenance {
    pub source: String,
    pub document: Box<[ProvenanceField]>,
    pub divisions: Box<[ProvenanceDivision]>,
    pub sentence: Box<[ProvenanceField]>,
    pub sections: Box<[String]>,
}

impl HitProvenance {
    #[must_use]
    pub fn from_sentence(provenance: &SentenceProvenance) -> Self {
        Self {
            source: basename(provenance.location.source_name.as_ref()),
            document: metadata_fields(provenance.rich.metadata_json.as_deref()),
            divisions: provenance
                .rich
                .structure
                .divisions
                .iter()
                .map(|division| ProvenanceDivision {
                    source_id: division.source_id.clone(),
                    id: division.id.0,
                    kind: division.kind.clone(),
                    parent_id: division.parent_id.map(|id| id.0),
                    fields: metadata_fields_value(lingrep_core::corpus::divisions::metadata_json(
                        &division.metadata,
                    )),
                })
                .collect(),
            sentence: metadata_fields(provenance.rich.sentence_metadata_json.as_deref()),
            sections: section_labels(&provenance.active_regions),
        }
    }
}

#[must_use]
pub fn section_labels(regions: &[ActiveRegion]) -> Box<[String]> {
    let mut seen = BTreeSet::new();
    regions
        .iter()
        .map(|region| region.label.as_str())
        .filter(|label| seen.insert((*label).to_owned()))
        .map(str::to_owned)
        .collect()
}

#[must_use]
pub fn metadata_fields(metadata_json: Option<&str>) -> Box<[ProvenanceField]> {
    let Some(metadata_json) = metadata_json else {
        return Box::new([]);
    };
    serde_json::from_str(metadata_json)
        .map(metadata_fields_value)
        .unwrap_or_default()
}

fn metadata_fields_value(value: serde_json::Value) -> Box<[ProvenanceField]> {
    let serde_json::Value::Object(object) = value else {
        return Box::new([]);
    };
    let mut fields = object
        .into_iter()
        .map(|(key, value)| ProvenanceField {
            key,
            values: metadata_values(value),
        })
        .collect::<Vec<_>>();
    fields.sort_unstable_by(|left, right| left.key.cmp(&right.key));
    fields.into_boxed_slice()
}

fn metadata_values(value: serde_json::Value) -> Box<[String]> {
    match value {
        serde_json::Value::Array(values) => values
            .into_iter()
            .map(metadata_display_value)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        value => Box::new([metadata_display_value(value)]),
    }
}

fn metadata_display_value(value: serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value,
        value => serde_json::to_string(&value).expect("JSON value serialization is infallible"),
    }
}

/// The final path component of a stored source name. Splits on both path
/// separators and never returns the original string when no component is usable.
fn basename(source_name: &str) -> String {
    source_name
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty() && *component != "." && *component != "..")
        .map_or_else(|| "(unknown source)".to_owned(), str::to_owned)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use lingrep_core::{corpus::DocumentId, execution::SentenceLocation};
    use lingrep_corpus::{ActiveRegion, RichSentenceMetadata, SentenceProvenance};

    use super::{HitProvenance, metadata_fields, section_labels};

    fn sentence_provenance() -> SentenceProvenance {
        SentenceProvenance {
            location: SentenceLocation {
                source_metadata: None,
                corpus_sentence_ordinal: 0,
                document_id: DocumentId::new(0),
                document_ordinal: 0,
                document_sentence_ordinal: 0,
                source_name: Arc::from("/corpus/00042.md"),
            },
            rich: RichSentenceMetadata {
            structure: Arc::new(lingrep_core::corpus::StructuralContext {
                observations:Default::default(),
                divisions:Box::new([Arc::new(lingrep_core::corpus::Division {id:lingrep_core::corpus::DivisionId(7),source_id:Some("authored-7".into()),document_id:DocumentId::new(0),kind:"paragraph".into(),parent_id:Some(lingrep_core::corpus::DivisionId(2)),metadata:lingrep_core::corpus::metadata_from_json(serde_json::json!({"speaker":"division"}),"test").unwrap()})])
            }),
                region_labels: vec!["methods".into(), "quote".into(), "methods".into()]
                    .into_boxed_slice(),
                metadata_json: Some(
                    r#"{"hostile":"<script>alert(1)</script>' OR 1=1 --","keywords":["社会学","農学"],"year":"2013"}"#.into(),
                ),
                sentence_metadata_json: Some(r#"{"speaker":"sentence"}"#.into()),
            },
            active_regions: vec![
                ActiveRegion {
                    label: "methods".into(),
                    first_sentence: 0,
                    end_sentence: 10,
                },
                ActiveRegion {
                    label: "quote".into(),
                    first_sentence: 2,
                    end_sentence: 4,
                },
                ActiveRegion {
                    label: "methods".into(),
                    first_sentence: 3,
                    end_sentence: 4,
                },
            ]
            .into_boxed_slice(),
        }
    }

    #[test]
    fn composed_sentence_becomes_web_display_fields() {
        let view = HitProvenance::from_sentence(&sentence_provenance());
        assert_eq!(view.source, "00042.md");
        assert_eq!(view.document[0].key, "hostile");
        assert_eq!(
            view.document[0].values[0],
            "<script>alert(1)</script>' OR 1=1 --"
        );
        assert_eq!(&*view.document[1].values, &["社会学", "農学"]);
        assert_eq!(view.document[2].values[0], "2013");
        assert_eq!(view.divisions.len(), 1);
        assert_eq!(view.divisions[0].id, 7);
        assert_eq!(view.divisions[0].source_id.as_deref(), Some("authored-7"));
        assert_eq!(view.divisions[0].parent_id, Some(2));
        assert_eq!(view.divisions[0].fields[0].values[0], "division");
        assert_eq!(view.sentence[0].values[0], "sentence");
        assert_eq!(&*view.sections, &["methods", "quote"]);
    }

    #[test]
    fn metadata_keeps_numeric_and_opaque_values_without_lossy_projection() {
        let fields = metadata_fields(Some(
            r#"{"float":"3.14","integer":"2016","opaque":{"x":1}}"#,
        ));
        assert_eq!(fields[0].values[0], "3.14");
        assert_eq!(fields[1].values[0], "2016");
        assert_eq!(fields[2].values[0], r#"{"x":1}"#);
    }

    #[test]
    fn malformed_or_absent_metadata_degrades_to_no_display_fields() {
        assert!(metadata_fields(None).is_empty());
        assert!(metadata_fields(Some("not JSON")).is_empty());
        assert!(metadata_fields(Some("[]")).is_empty());
    }

    #[test]
    fn section_labels_preserve_authored_order_and_remove_duplicates() {
        let regions = [
            ActiveRegion {
                label: "b".into(),
                first_sentence: 0,
                end_sentence: 3,
            },
            ActiveRegion {
                label: "a".into(),
                first_sentence: 1,
                end_sentence: 2,
            },
            ActiveRegion {
                label: "b".into(),
                first_sentence: 1,
                end_sentence: 2,
            },
        ];
        assert_eq!(&*section_labels(&regions), &["b", "a"]);
        assert!(section_labels(&[]).is_empty());
    }

    #[test]
    fn basename_never_leaks_a_path() {
        let mut provenance = sentence_provenance();
        for (source, expected) in [
            ("/corpus/00042.md", "00042.md"),
            ("00042.md", "00042.md"),
            ("/corpus/", "corpus"),
            ("/", "(unknown source)"),
            ("", "(unknown source)"),
            ("..", "(unknown source)"),
            ("C:\\corpus\\01.md", "01.md"),
        ] {
            provenance.location.source_name = Arc::from(source);
            assert_eq!(HitProvenance::from_sentence(&provenance).source, expected);
        }
    }
}
