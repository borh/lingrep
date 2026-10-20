// Exempt test code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(clippy::panic, clippy::unwrap_used)]

use std::collections::BTreeSet;

use clap::CommandFactory;
use lingrep::protocol::ContractId;

#[test]
fn contract_ids_are_complete_unique_and_back_existing_schema_constants() {
    use lingrep::output::{
        MATCHES_CONTRACT, SENTENCE_FINAL_CONTRACT, SOURCES_CONTRACT, SUMMARY_CONTRACT,
        TOKENIZE_CONTRACT,
    };
    use lingrep::protocol::{
        ContractId, capabilities, check, completions, diagnostic, doctor, invocation, query,
        sentence_final, sources, status, tokenize, variants,
    };

    assert_eq!(ContractId::ALL.len(), 21);
    let mut ids = ContractId::ALL
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 21);
    assert_eq!(query::SCHEMA, ContractId::Query.as_str());
    assert_eq!(check::SCHEMA, ContractId::Check.as_str());
    assert_eq!(tokenize::SCHEMA, ContractId::Tokenize.as_str());
    assert_eq!(sources::SCHEMA, ContractId::Sources.as_str());
    assert_eq!(diagnostic::SCHEMA, ContractId::Diagnostic.as_str());
    assert_eq!(status::SCHEMA, ContractId::Status.as_str());
    assert_eq!(capabilities::SCHEMA, ContractId::Capabilities.as_str());
    assert_eq!(invocation::SCHEMA, ContractId::Invocation.as_str());
    assert_eq!(doctor::SCHEMA, ContractId::Doctor.as_str());
    assert_eq!(completions::SCHEMA, ContractId::Completions.as_str());
    assert_eq!(sentence_final::SCHEMA, ContractId::SentenceFinal.as_str());
    assert_eq!(MATCHES_CONTRACT, ContractId::MatchesTsv.as_str());
    assert_eq!(SUMMARY_CONTRACT, ContractId::SummarizeTsv.as_str());
    assert_eq!(
        SENTENCE_FINAL_CONTRACT,
        ContractId::SentenceFinalTsv.as_str()
    );
    assert_eq!(TOKENIZE_CONTRACT, ContractId::TokenizeTsv.as_str());
    assert_eq!(SOURCES_CONTRACT, ContractId::SourcesTsv.as_str());
    assert_eq!(variants::SCHEMA, ContractId::Variants.as_str());
}

#[test]
fn registry_is_complete_unique_and_summary_sorted() {
    let summaries = lingrep::protocol::registry::summaries();
    assert_eq!(summaries.len(), 21);
    assert!(
        summaries
            .windows(2)
            .all(|pair| pair[0].id.as_str() < pair[1].id.as_str())
    );
    for id in lingrep::protocol::ContractId::ALL {
        assert_eq!(lingrep::protocol::registry::get(*id).id(), *id);
    }
}

#[test]
fn every_contract_owner_is_a_public_parser_command() {
    fn collect(command: &clap::Command, prefix: &str, paths: &mut Vec<String>) {
        for child in command
            .get_subcommands()
            .filter(|child| !child.is_hide_set() && child.get_name() != "help")
        {
            let path = if prefix.is_empty() {
                child.get_name().to_owned()
            } else {
                format!("{prefix} {}", child.get_name())
            };
            paths.push(path.clone());
            collect(child, &path, paths);
        }
    }

    let mut command = lingrep::Cli::command();
    command.build();
    let mut paths = vec!["ling".to_owned()];
    collect(&command, "", &mut paths);
    for summary in lingrep::protocol::registry::summaries() {
        for owner in summary.owners {
            assert!(
                paths.iter().any(|path| path == owner),
                "{} names missing owner {owner}",
                summary.id.as_str()
            );
        }
    }
}

#[test]
fn every_contract_example_id_resolves() {
    let example_ids = lingrep::examples::all_examples()
        .map(|example| example.id)
        .collect::<BTreeSet<_>>();

    for id in lingrep::protocol::ContractId::ALL {
        let contract = lingrep::protocol::registry::get(*id);
        let referenced = match contract {
            lingrep::protocol::discovery::ContractSpec::Jsonl(spec) => spec.example_ids,
            lingrep::protocol::discovery::ContractSpec::Tsv(spec) => spec.example_ids,
        };
        for example_id in referenced {
            assert!(
                example_ids.contains(example_id),
                "{} references missing example {example_id}",
                id.as_str()
            );
        }
    }
}

#[test]
fn stream_shapes_name_real_protocol_lifecycles() {
    use lingrep::protocol::ContractId::*;
    use lingrep::protocol::discovery::StreamShape;
    use lingrep::protocol::registry;

    for id in [Query, Tokenize, Sources, SentenceFinal] {
        assert!(
            matches!(registry::get(id).stream(), Some(StreamShape::Phased { .. })),
            "{id:?}"
        );
    }
    for id in [
        Check,
        Doctor,
        Completions,
        Invocation,
        Manpages,
        ProtocolContract,
    ] {
        assert!(
            matches!(registry::get(id).stream(), Some(StreamShape::Single { .. })),
            "{id:?}"
        );
    }
    for id in [Capabilities, Diagnostic, Status] {
        assert!(
            matches!(
                registry::get(id).stream(),
                Some(StreamShape::Repeated { .. })
            ),
            "{id:?}"
        );
    }
    for id in [
        MatchesTsv,
        SummarizeTsv,
        SentenceFinalTsv,
        TokenizeTsv,
        SourcesTsv,
    ] {
        assert!(registry::get(id).stream().is_none(), "{id:?}");
    }
}

fn record_validator(id: lingrep::protocol::ContractId) -> jsonschema::Validator {
    let detail = lingrep::protocol::registry::get(id).to_detail().unwrap();
    jsonschema::validator_for(detail.json_schema.as_ref().unwrap()).unwrap()
}

#[test]
fn every_json_contract_builds_a_draft_2020_12_schema_with_its_exact_wire_id() {
    use lingrep::protocol::discovery::ContractKind;

    for summary in lingrep::protocol::registry::summaries()
        .into_iter()
        .filter(|summary| summary.kind == ContractKind::Jsonl)
    {
        let detail = lingrep::protocol::registry::get(summary.id)
            .to_detail()
            .unwrap();
        let schema = detail.json_schema.unwrap();
        assert_eq!(
            schema["$schema"],
            "https://json-schema.org/draft/2020-12/schema"
        );
        assert_eq!(schema["properties"]["schema"]["const"], summary.id.as_str());
    }
}

#[test]
fn query_v5_schema_contains_lexical_definition_and_key_shapes() {
    let detail = lingrep::protocol::registry::get(ContractId::Query)
        .to_detail()
        .unwrap();
    let schema = serde_json::to_string(detail.json_schema.as_ref().unwrap()).unwrap();
    for current in ["lexicon", "lexical", "term", "list"] {
        assert!(
            schema.contains(&format!("\"{current}\"")),
            "missing `{current}` in v5"
        );
    }
    assert!(schema.contains("scalar"));
    assert!(schema.contains("SummaryGroupData"));
}

#[test]
fn doctor_v3_accepts_historical_optional_and_additive_probe_fields() {
    use lingrep::protocol::{
        ContractId,
        doctor::{DoctorEnvelope, ProbeEvidence, ProbeKind},
    };

    let historical = serde_json::json!({
        "schema": "lingrep.doctor/v1",
        "type": "doctor",
        "data": {
            "ready": true,
            "probes": [{"name": "dictionary", "ok": true, "message": "ready"}]
        }
    });
    let validator = record_validator(ContractId::Doctor);
    assert!(validator.is_valid(&historical));
    let decoded: DoctorEnvelope = serde_json::from_value(historical).unwrap();
    let serialized = serde_json::to_value(decoded).unwrap();
    assert!(serialized["data"]["probes"][0].get("kind").is_none());

    let additive = serde_json::json!({
        "schema": "lingrep.doctor/v1",
        "type": "doctor",
        "data": {
            "ready": true,
            "future_top_level": "ignored",
            "probes": [{
                "name": "dictionary",
                "ok": true,
                "message": "loaded",
                "kind": ProbeKind::Dictionary,
                "target": "/dictionary",
                "evidence": ProbeEvidence::Dictionary { blake3: "00".repeat(32) },
                "future_probe_field": 1
            }]
        }
    });
    assert!(validator.is_valid(&additive));
    serde_json::from_value::<DoctorEnvelope>(additive).unwrap();
}

#[test]
fn record_schema_accepts_structure_but_stream_validator_rejects_order() {
    use lingrep::protocol::{ContractId, query::QueryEnvelope, validate_query_stream};

    let parse = |name: &str| {
        serde_json::from_slice::<QueryEnvelope>(
            std::fs::read(format!("tests/golden/{name}.jsonl"))
                .unwrap()
                .strip_suffix(b"\n")
                .unwrap(),
        )
        .unwrap()
    };
    let events = [
        parse("query-run"),
        parse("query-complete"),
        parse("query-match"),
    ];
    let validator = record_validator(ContractId::Query);
    for event in &events {
        assert!(validator.is_valid(&serde_json::to_value(event).unwrap()));
    }
    assert!(validate_query_stream(events).is_err());
}

#[test]
fn derived_schema_rejects_the_wrong_wire_id() {
    let validator = record_validator(lingrep::protocol::ContractId::Query);
    let bytes = std::fs::read("tests/golden/query-run.jsonl").unwrap();
    let mut record: serde_json::Value =
        serde_json::from_slice(bytes.strip_suffix(b"\n").unwrap()).unwrap();
    record["schema"] = "lingrep.wrong/v1".into();
    assert!(!validator.is_valid(&record));
}

/// V1 binds the run to the complete analyzer descriptor rather than scattering
/// partial identity fields across the record.
#[test]
fn run_record_publishes_one_complete_analyzer_descriptor() {
    let bytes = std::fs::read("tests/golden/query-run.jsonl").unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(bytes.strip_suffix(b"\n").unwrap()).unwrap();
    let data = record["data"].as_object().expect("run data is an object");

    assert!(data.contains_key("analyzer"));
    for field in [
        "analyzer_descriptor",
        "schema_digest",
        "trust_class",
        "profile",
        "segmentation_mode",
    ] {
        assert!(
            !data.contains_key(field),
            "run record unexpectedly duplicates analyzer field {field}"
        );
    }
    assert_eq!(data["analyzer"]["family"], "vibrato");
    assert_eq!(data["analyzer"]["schema"][0]["key"], "text");
}

#[test]
fn every_json_contract_accepts_a_production_serialized_record() {
    use lingrep::protocol::{
        ContractId,
        capabilities::{ArgumentData, CapabilityData, CapabilityEnvelope},
        completions::{CompletionData, CompletionEnvelope},
        contract::{ContractEnvelope, ContractEvent, ContractIndexData},
        doctor::{DoctorData, DoctorEnvelope, ProbeData},
        invocation::{InvocationData, InvocationEnvelope},
        manpages::{ManpagesData, ManpagesEnvelope},
        sentence_final::{RunData, SentenceFinalEnvelope, SentenceFinalEvent},
        status::{StatusData, StatusEnvelope},
    };

    let golden = |name: &str| {
        let bytes = std::fs::read(format!("tests/golden/{name}")).unwrap();
        serde_json::from_slice::<serde_json::Value>(bytes.strip_suffix(b"\n").unwrap()).unwrap()
    };
    let records = vec![
        (ContractId::Query, golden("query-run.jsonl")),
        (ContractId::Check, golden("check.jsonl")),
        (ContractId::Tokenize, golden("tokenize-run.jsonl")),
        (ContractId::Sources, golden("sources-run.jsonl")),
        (ContractId::Diagnostic, golden("diagnostic.jsonl")),
        (
            ContractId::Status,
            serde_json::to_value(StatusEnvelope::new(StatusData {
                stage: "scan".into(),
                current: 1,
                total: Some(2),
                message: "ready".into(),
            }))
            .unwrap(),
        ),
        (
            ContractId::Capabilities,
            serde_json::to_value(CapabilityEnvelope::new(CapabilityData {
                command: "matches".into(),
                purpose: "search".into(),
                output_modes: vec!["human".into()],
                schemas: vec![],
                tsv_contracts: vec![],
                input_modes: vec!["direct".into()],
                arguments: vec![ArgumentData {
                    name: "root".into(),
                    required: true,
                    values: vec![],
                    conflicts: vec![],
                }],
                environment: vec![],
                defaults: vec![],
                exit_codes: vec!["0 complete".into()],
                examples: vec![],
            }))
            .unwrap(),
        ),
        (
            ContractId::Invocation,
            serde_json::to_value(InvocationEnvelope::new(InvocationData {
                command: "matches".into(),
                catalog_source: None,
                catalog_fingerprint: None,
                dictionary_path: None,
                dictionary_source: None,
                input_kind: "direct".into(),
                output_mode: "human".into(),
                color: "auto".into(),
                progress: "auto".into(),
                limits: serde_json::json!({}),
                schemas: vec![],
            }))
            .unwrap(),
        ),
        (
            ContractId::Doctor,
            serde_json::to_value(DoctorEnvelope::new(DoctorData {
                ready: true,
                probes: vec![ProbeData {
                    name: "dictionary".into(),
                    ok: true,
                    message: "ready".into(),
                    kind: None,
                    target: None,
                    code: None,
                    evidence: None,
                }],
            }))
            .unwrap(),
        ),
        (
            ContractId::Completions,
            serde_json::to_value(CompletionEnvelope::new(CompletionData {
                shell: "bash".into(),
                script: "complete".into(),
            }))
            .unwrap(),
        ),
        (
            ContractId::SentenceFinal,
            serde_json::to_value(SentenceFinalEnvelope::new(SentenceFinalEvent::Run(
                RunData {
                    run_id: "run".into(),
                    dictionary_identity: "dictionary".into(),
                    catalog_fingerprint: "catalog".into(),
                    input_kind: "direct".into(),
                    limits: serde_json::json!({}),
                },
            )))
            .unwrap(),
        ),
        (
            ContractId::ProtocolContract,
            serde_json::to_value(ContractEnvelope::new(ContractEvent::ContractIndex(
                ContractIndexData { contracts: vec![] },
            )))
            .unwrap(),
        ),
        (
            ContractId::Manpages,
            serde_json::to_value(ManpagesEnvelope::new(ManpagesData {
                output: "pages".into(),
                pages: vec![],
            }))
            .unwrap(),
        ),
    ];
    assert_eq!(records.len(), 13);
    for (id, record) in records {
        assert!(record_validator(id).is_valid(&record), "{id:?}: {record}");
    }
}

#[test]
fn tsv_descriptors_publish_the_writer_column_order() {
    use lingrep::protocol::ContractId::*;
    use lingrep::protocol::discovery::ContractSpec;

    let cases: &[(lingrep::protocol::ContractId, &[&str])] = &[
        (
            MatchesTsv,
            &[
                "sequence",
                "pattern",
                "source_name",
                "document_ordinal",
                "document_sentence_ordinal",
                "corpus_sentence_ordinal",
                "match_start",
                "match_end",
                "left_surface",
                "surface",
                "right_surface",
            ],
        ),
        (SummarizeTsv, &["pattern", "rank", "group", "count"]),
        (SentenceFinalTsv, &["pattern", "surface", "count"]),
        (
            TokenizeTsv,
            &[
                "corpus_sentence_ordinal",
                "source_name",
                "document_ordinal",
                "document_sentence_ordinal",
                "token_index",
                "orth",
                "lemma",
                "pos",
                "cform",
            ],
        ),
        (
            SourcesTsv,
            &[
                "ordinal",
                "logical_name",
                "target_group_ordinal",
                "external_target",
                "target_multiplicity",
            ],
        ),
    ];
    for (id, expected) in cases {
        let ContractSpec::Tsv(spec) = lingrep::protocol::registry::get(*id) else {
            panic!("{id:?} was not TSV");
        };
        assert_eq!(
            spec.columns
                .iter()
                .map(|column| column.name)
                .collect::<Vec<_>>(),
            *expected
        );
    }
}

#[test]
fn summarize_tsv_describes_its_canonical_typed_group_order() {
    use lingrep::protocol::discovery::ContractSpec;

    let ContractSpec::Tsv(spec) = lingrep::protocol::registry::get(ContractId::SummarizeTsv) else {
        panic!("summarize/v2 was not TSV");
    };
    assert_eq!(
        spec.ordering,
        "Authored definition; count descending; group kind surface, rendered unit, lexical term, projected key; \
         keyed field-name vector in authored order; keyed value vector."
    );
}

#[test]
fn tsv_reference_matches_registered_contracts() {
    let document = std::fs::read_to_string("../docs/lingrep-tsv-contracts.md").unwrap();
    let generated = lingrep::protocol::registry::render_tsv_markdown();
    let begin = document
        .find("<!-- BEGIN GENERATED TSV CONTRACTS -->")
        .unwrap();
    let end = document
        .find("<!-- END GENERATED TSV CONTRACTS -->")
        .unwrap();
    let body_start = document.get(begin..).unwrap().find('\n').unwrap() + begin + 1;
    assert_eq!(
        document.get(body_start..end).unwrap().trim_end(),
        generated.trim_end()
    );
}

#[test]
fn schema_json_distinguishes_compact_index_from_one_detail() {
    let index = crate::support::lingrep_without_dictionary()
        .args(["contracts", "--json"])
        .output()
        .unwrap();
    assert!(index.status.success());
    let records = crate::support::parse_jsonl(&index.stdout);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["schema"], "lingrep.protocol-contract/v1");
    assert_eq!(records[0]["type"], "contract_index");
    let summaries = records[0]["data"]["contracts"].as_array().unwrap();
    assert_eq!(summaries.len(), 21);
    assert!(
        summaries
            .iter()
            .all(|summary| summary.get("json_schema").is_none())
    );

    let detail = crate::support::lingrep_without_dictionary()
        .args(["contracts", "lingrep.query/v2", "--json"])
        .output()
        .unwrap();
    assert!(detail.status.success());
    let records = crate::support::parse_jsonl(&detail.stdout);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["type"], "contract");
    assert_eq!(records[0]["data"]["id"], "lingrep.query/v2");
    assert_eq!(records[0]["data"]["stream"]["shape"], "phased");
    assert_eq!(
        records[0]["data"]["json_schema"]["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
}

#[test]
fn unknown_contract_is_structured_usage_with_clap_suggestion() {
    let output = crate::support::lingrep_without_dictionary()
        .args(["contracts", "lingrep.quer/v1", "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let records = crate::support::parse_jsonl(&output.stderr);
    assert_eq!(records[0]["schema"], "lingrep.diagnostic/v1");
    assert!(
        !records[0]["data"]["message"]
            .as_str()
            .unwrap()
            .contains("Usage:")
    );
    assert!(
        records[0]["data"]["suggestions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "lingrep.query/v2")
    );
}
