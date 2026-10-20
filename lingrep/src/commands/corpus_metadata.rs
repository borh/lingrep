use std::io::Write;

use lingrep_cli_common::SafeText;
use lingrep_corpus::ProvenanceView;
use serde_json::Value;

use crate::error::CliError;
use crate::protocol::sources::{MetadataExample, MetadataField, MetadataGrain, MetadataReport};

const MAX_FIELDS: usize = 256;
const MAX_EXAMPLES: usize = 3;
const MAX_PREVIEW_BYTES: usize = 256;

pub(super) async fn inspect_metadata(
    view: &ProvenanceView,
    document_count: u64,
) -> Result<MetadataReport, CliError> {
    let mut documents = MetadataGrain::default();
    let mut sentences = MetadataGrain {
        units: view.sentence_count(),
        ..MetadataGrain::default()
    };
    for key in view.sentence_metadata_keys().await? {
        field(&mut sentences, &key);
    }
    let mut cursor = 0;
    loop {
        let page = view.document_metadata_page(cursor).await?;
        if page.is_empty() {
            break;
        }
        for row in page {
            cursor = row.first_sentence + row.sentence_count;
            documents.units += 1;
            observe(&mut documents, row.metadata_json.as_deref())?;
        }
    }
    cursor = 0;
    loop {
        let page = view.sentence_metadata_page(cursor).await?;
        if page.is_empty() {
            break;
        }
        for (ordinal, json) in page {
            cursor = ordinal + 1;
            observe(&mut sentences, Some(&json))?;
        }
    }
    let mut divisions = std::collections::BTreeMap::new();
    let kinds = view.division_observation_counts().await?;
    let division_kinds_truncated = kinds.len() > 256;
    for (kind, observed_documents) in kinds.into_iter().take(256) {
        divisions.insert(
            kind,
            crate::protocol::sources::DivisionMetadataReport {
                observed_documents,
                unavailable_documents: document_count.checked_sub(observed_documents).ok_or_else(
                    || CliError::Parse("division observation count exceeds documents".into()),
                )?,
                ..Default::default()
            },
        );
    }
    let mut cursor = 0;
    loop {
        let page = view.division_metadata_page(cursor).await?;
        if page.is_empty() {
            break;
        }
        for row in page {
            cursor = row
                .division
                .id
                .0
                .checked_add(1)
                .ok_or_else(|| CliError::Parse("division ID overflow".into()))?;
            let Some(report) = divisions.get_mut(&row.division.kind) else {
                continue;
            };
            report.metadata.units += 1;
            report.empty_units += u64::from(row.first_sentence == row.end_sentence);
            observe(
                &mut report.metadata,
                Some(
                    &lingrep_core::corpus::divisions::metadata_json(&row.division.metadata)
                        .to_string(),
                ),
            )?;
        }
    }
    for report in divisions.values_mut() {
        for field in report.metadata.fields.values_mut() {
            field.missing = report.metadata.units - field.present;
        }
    }
    for grain in [&mut documents, &mut sentences] {
        for value in grain.fields.values_mut() {
            value.missing = grain.units - value.present;
        }
    }
    Ok(MetadataReport {
        divisions,
        division_kinds_truncated,
        documents,
        sentences,
    })
}

fn field<'a>(grain: &'a mut MetadataGrain, key: &str) -> Option<&'a mut MetadataField> {
    if !grain.fields.contains_key(key) && grain.fields.len() == MAX_FIELDS {
        grain.fields_truncated = true;
        if grain
            .fields
            .last_key_value()
            .is_some_and(|(last, _)| key > last.as_str())
        {
            return None;
        }
        grain.fields.pop_last();
    }
    Some(
        grain
            .fields
            .entry(key.to_owned())
            .or_insert_with(|| MetadataField {
                examples_complete: true,
                ..MetadataField::default()
            }),
    )
}

fn observe(grain: &mut MetadataGrain, json: Option<&str>) -> Result<(), CliError> {
    let Some(json) = json else {
        return Ok(());
    };
    let value: Value =
        serde_json::from_str(json).map_err(|error| CliError::Parse(error.to_string()))?;
    let Value::Object(object) = value else {
        return Err(CliError::Parse("stored metadata must be an object".into()));
    };
    for (key, value) in object {
        let Some(field) = field(grain, &key) else {
            continue;
        };
        field.present += 1;
        let shape = match &value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "list",
            Value::Object(_) => "map",
        };
        *field.shapes.entry(shape.into()).or_default() += 1;
        let mut preview = value.to_string();
        let truncated = preview.len() > MAX_PREVIEW_BYTES;
        if truncated {
            let mut boundary = MAX_PREVIEW_BYTES;
            while !preview.is_char_boundary(boundary) {
                boundary -= 1;
            }
            preview.truncate(boundary);
            field.examples_complete = false;
        }
        let example = MetadataExample { preview, truncated };
        match field.examples.binary_search(&example) {
            Ok(_) => {}
            Err(index) => {
                field.examples.insert(index, example);
                if field.examples.len() > MAX_EXAMPLES {
                    field.examples.pop();
                    field.examples_complete = false;
                }
            }
        }
    }
    Ok(())
}

pub(super) fn write_metadata(
    writer: &mut impl Write,
    report: &MetadataReport,
) -> Result<(), CliError> {
    let grains = std::iter::once(("document", &report.documents))
        .chain(std::iter::once(("sentence", &report.sentences)))
        .chain(
            report
                .divisions
                .iter()
                .map(|(kind, report)| (kind.as_str(), &report.metadata)),
        );
    for (name, grain) in grains {
        writeln!(
            writer,
            "{name} metadata: {} units{}",
            grain.units,
            if grain.fields_truncated {
                " (field list truncated)"
            } else {
                ""
            }
        )?;
        for (key, field) in &grain.fields {
            writeln!(
                writer,
                "  {}: {} present, {} missing; shapes {:?}; examples {}{}",
                SafeText::from_untrusted(key),
                field.present,
                field.missing,
                field.shapes,
                SafeText::from_untrusted(
                    &serde_json::to_string(&field.examples)
                        .map_err(|error| CliError::Parse(error.to_string()))?
                ),
                if field.examples_complete {
                    ""
                } else {
                    " (truncated)"
                }
            )?;
        }
    }
    for (kind, division) in &report.divisions {
        writeln!(
            writer,
            "{}: {} empty units, {} observed documents, {} unavailable documents",
            SafeText::from_untrusted(kind),
            division.empty_units,
            division.observed_documents,
            division.unavailable_documents
        )?;
    }
    if report.division_kinds_truncated {
        writeln!(writer, "division kind inventory truncated")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_shapes_without_inheriting_document_values() {
        let mut grain = MetadataGrain {
            units: 3,
            ..MetadataGrain::default()
        };
        observe(&mut grain, Some(r#"{"speaker":"a","shape":1}"#)).unwrap();
        observe(&mut grain, Some(r#"{"speaker":"b","shape":[1]}"#)).unwrap();
        assert_eq!(grain.fields["speaker"].present, 2);
        assert_eq!(grain.fields["shape"].shapes.len(), 2);
        assert!(grain.fields["speaker"].examples_complete);
    }

    #[test]
    fn cardinality_and_unicode_previews_remain_bounded_and_deterministic() {
        let mut forward = MetadataGrain::default();
        let mut backward = MetadataGrain::default();
        for index in 0..1024 {
            observe(
                &mut forward,
                Some(&format!(
                    r#"{{"key{index:04}":{index},"values":"{}"}}"#,
                    "文".repeat(index)
                )),
            )
            .unwrap();
        }
        for index in (0..1024).rev() {
            observe(
                &mut backward,
                Some(&format!(
                    r#"{{"key{index:04}":{index},"values":"{}"}}"#,
                    "文".repeat(index)
                )),
            )
            .unwrap();
        }
        assert_eq!(forward, backward);
        assert!(forward.fields_truncated);
        assert_eq!(forward.fields.len(), MAX_FIELDS);
        for field in forward.fields.values() {
            assert!(field.examples.len() <= MAX_EXAMPLES);
            assert!(
                field
                    .examples
                    .iter()
                    .all(|example| example.preview.len() <= MAX_PREVIEW_BYTES)
            );
        }
    }
}
