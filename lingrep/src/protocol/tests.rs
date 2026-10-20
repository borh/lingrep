// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]

use crate::{
    protocol::{
        ProtocolError, capabilities,
        check::{
            CheckData, CheckEnvelope, CheckEvent, CompileStats, CompileWarning,
            DefinitionDescriptor, DriftBodyData, DriftCarrierData, DriftData, SourceLocation,
        },
        completions,
        diagnostic::{
            DiagnosticData, DiagnosticEnvelope, DiagnosticEvent, DiagnosticLocation,
            DiagnosticSeverity, DiagnosticStage, RetryPolicy,
        },
        doctor, invocation,
        query::{
            AnalyzerRun, Capture, CommittedPrefix, CompleteCorpus, Coverage, DefinitionBody,
            DefinitionData, DirectCompletion, DirectRunState, ExecutionLimits, GraphBinding,
            GraphNodeAttribute, GraphNodeId, InventoryStats, MatchData, MatchOrder,
            ProtocolKeyField, QueryComplete, QueryEnvelope, QueryEvent, QueryMode, QueryRun,
            ResourceHighWater, RunCorpus, RunCounters, SourceSpan, SpanConvention, SummaryData,
            SummaryGroupData, TagField, TemporaryStorageLimits, Termination, TokenEvidence,
            TraversalLimits,
        },
        sentence_final::{
            CompleteData as SentenceFinalComplete, RowData as SentenceFinalRow,
            RunData as SentenceFinalRun, SentenceFinalEnvelope, SentenceFinalEvent,
        },
        sources::{
            AggregateState, SourceData, SourcesComplete, SourcesEnvelope, SourcesEvent,
            SourcesKind, SourcesRun, TraversalHighWater,
        },
        status,
        tokenize::{SentenceData, TokenizeComplete, TokenizeEnvelope, TokenizeEvent, TokenizeRun},
        validate_check_jsonl, validate_diagnostic_jsonl, validate_query_jsonl,
        validate_query_stream, validate_sentence_final_stream, validate_sources_jsonl,
        validate_tokenize_jsonl,
    },
    write_jsonl,
};

#[test]
fn progressive_contract_schema_names_are_pinned() {
    assert_eq!(crate::protocol::query::SCHEMA, "lingrep.query/v2");
    assert_eq!(crate::protocol::check::SCHEMA, "lingrep.check/v1");
    assert_eq!(crate::protocol::tokenize::SCHEMA, "lingrep.tokenize/v1");
    assert_eq!(crate::protocol::sources::SCHEMA, "lingrep.sources/v1");
    assert_eq!(crate::protocol::diagnostic::SCHEMA, "lingrep.diagnostic/v1");
    assert_eq!(status::SCHEMA, "lingrep.status/v1");
    assert_eq!(capabilities::SCHEMA, "lingrep.capabilities/v1");
    assert_eq!(invocation::SCHEMA, "lingrep.invocation/v1");
    assert_eq!(doctor::SCHEMA, "lingrep.doctor/v1");
    assert_eq!(completions::SCHEMA, "lingrep.completions/v1");
    assert_eq!(
        crate::protocol::sentence_final::SCHEMA,
        "lingrep.sentence-final/v1"
    );
}

fn sentence_final_run() -> SentenceFinalEnvelope {
    SentenceFinalEnvelope::new(SentenceFinalEvent::Run(SentenceFinalRun {
        run_id: "run-1".into(),
        dictionary_identity: "dict".into(),
        catalog_fingerprint: "catalog".into(),
        input_kind: "direct".into(),
        limits: serde_json::json!({"workers": 8}),
    }))
}

fn sentence_final_row() -> SentenceFinalEnvelope {
    SentenceFinalEnvelope::new(SentenceFinalEvent::Row(SentenceFinalRow {
        pattern: "p".into(),
        surface: "猫".into(),
        count: 2,
    }))
}

fn sentence_final_complete() -> SentenceFinalEnvelope {
    SentenceFinalEnvelope::new(SentenceFinalEvent::Complete(SentenceFinalComplete {
        coverage: "full".into(),
        rows: 1,
        matches_found: 2,
        elapsed_millis: 3,
        resources: serde_json::json!({"temporary_storage_bytes": 0}),
    }))
}

#[test]
fn sentence_final_v1_requires_run_rows_then_complete() {
    validate_sentence_final_stream([
        sentence_final_run(),
        sentence_final_row(),
        sentence_final_complete(),
    ])
    .unwrap();
    assert!(validate_sentence_final_stream([sentence_final_row()]).is_err());
    assert!(validate_sentence_final_stream([sentence_final_run(), sentence_final_run()]).is_err());
    assert!(
        validate_sentence_final_stream([
            sentence_final_run(),
            sentence_final_complete(),
            sentence_final_row(),
        ])
        .is_err()
    );
}

#[test]
fn query_v5_round_trips_typed_keys_and_summary_groups() {
    let field = ProtocolKeyField::Scalar {
        name: "pos1".into(),
        value: "名詞".into(),
    };
    assert_eq!(
        serde_json::to_value(&field).unwrap(),
        serde_json::json!({"kind": "scalar", "name": "pos1", "value": "名詞"})
    );
    for group in [
        SummaryGroupData::Surface {
            value: "猫".into()
        },
        SummaryGroupData::Unit {
            value: "猫だ".into(),
        },
        SummaryGroupData::Term {
            value: "大学".into(),
        },
        SummaryGroupData::Key {
            fields: vec![field.clone()],
        },
    ] {
        let value = serde_json::to_value(&group).unwrap();
        assert_eq!(
            serde_json::from_value::<SummaryGroupData>(value).unwrap(),
            group
        );
    }
    assert_eq!(
        serde_json::to_value(SummaryGroupData::Key {
            fields: vec![field]
        })
        .unwrap(),
        serde_json::json!({
            "kind": "key",
            "fields": [{"kind": "scalar", "name": "pos1", "value": "名詞"}]
        })
    );
}

#[test]
fn typed_query_stream_requires_run_and_one_terminal_complete() {
    assert!(matches!(
        validate_query_stream([query_match(), query_complete()]),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_query_stream([query_run(), query_complete(), query_complete()]),
        Err(ProtocolError::DuplicateComplete)
    ));
}

#[test]
fn raw_jsonl_query_validation_enforces_schema_and_full_event_grammar() {
    let run = jsonl(query_run());
    let pattern = jsonl(query_pattern());
    let matched = jsonl(query_match());
    let complete = jsonl(query_complete());
    let summary = jsonl(QueryEnvelope::new(QueryEvent::Summary(SummaryData {
        pattern: "p".into(),
        group: SummaryGroupData::Surface {
            value: "猫".into()
        },
        count: 2,
        rank: 1,
    })));
    assert!(
        validate_query_jsonl(
            &[
                run.as_slice(),
                pattern.as_slice(),
                matched.as_slice(),
                complete.as_slice()
            ]
            .concat()
        )
        .is_ok()
    );
    assert!(validate_query_jsonl(&[run.as_slice(), complete.as_slice()].concat()).is_ok());
    assert!(matches!(
        validate_query_jsonl(&[matched.as_slice(), complete.as_slice()].concat()),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_query_jsonl(&[run.as_slice(), run.as_slice(), complete.as_slice()].concat()),
        Err(ProtocolError::DuplicateRun)
    ));
    assert!(matches!(
        validate_query_jsonl(&[run.as_slice(), matched.as_slice(), run.as_slice()].concat()),
        Err(ProtocolError::RunAfterResult)
    ));
    assert!(matches!(
        validate_query_jsonl(
            &[
                run.as_slice(),
                matched.as_slice(),
                summary.as_slice(),
                complete.as_slice()
            ]
            .concat()
        ),
        Err(ProtocolError::ResultModeMismatch { .. })
    ));
    assert!(matches!(
        validate_query_jsonl(
            &[
                run.as_slice(),
                matched.as_slice(),
                pattern.as_slice(),
                complete.as_slice()
            ]
            .concat()
        ),
        Err(ProtocolError::DefinitionAfterResult)
    ));
    assert!(matches!(
        validate_query_jsonl(&[run.as_slice(), complete.as_slice(), matched.as_slice()].concat()),
        Err(ProtocolError::EventAfterComplete)
    ));
    assert!(matches!(
        validate_query_jsonl(&[run.as_slice(), complete.as_slice(), complete.as_slice()].concat()),
        Err(ProtocolError::DuplicateComplete)
    ));
    assert!(matches!(
        validate_query_jsonl(b"{\"schema\":\"lingrep.query/v0\",\"type\":\"run\",\"data\":{}}\n"),
        Err(ProtocolError::WrongSchema { .. })
    ));
    assert!(matches!(
        validate_query_jsonl(
            b"{\"schema\":\"lingrep.query/v2\",\"type\":\"future\",\"data\":{}}\n"
        ),
        Err(ProtocolError::UnknownEventType { .. })
    ));
    assert!(matches!(
        validate_query_jsonl(
            b"{\"schema\":\"lingrep.query/v2\",\"type\":\"run\",\"data\":{\"mode\":\"future\"}}\n"
        ),
        Err(ProtocolError::MalformedJson { .. })
    ));
    assert!(matches!(
        validate_query_jsonl(b"{\"schema\":\"lingrep.query/v2\""),
        Err(ProtocolError::TruncatedLine { .. })
    ));
}

#[test]
fn raw_namespace_validators_enforce_run_record_complete_and_exact_schema() {
    let tokenize = ["tokenize-run", "tokenize-sentence", "tokenize-complete"]
        .into_iter()
        .map(|name| std::fs::read(format!("tests/golden/{name}.jsonl")).unwrap())
        .collect::<Vec<_>>();
    assert!(validate_tokenize_jsonl(&tokenize.concat()).is_ok());
    let sources = ["sources-run", "sources-source", "sources-complete"]
        .into_iter()
        .map(|name| std::fs::read(format!("tests/golden/{name}.jsonl")).unwrap())
        .collect::<Vec<_>>();
    assert!(validate_sources_jsonl(&sources.concat()).is_ok());
    assert!(validate_check_jsonl(&std::fs::read("tests/golden/check.jsonl").unwrap()).is_ok());
    assert!(matches!(
        validate_tokenize_jsonl(b""),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_sources_jsonl(b""),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_check_jsonl(b"{\"schema\":\"lingrep.check/v0\",\"type\":\"check\",\"data\":{}}\n"),
        Err(ProtocolError::WrongSchema { .. })
    ));
}

#[test]
fn query_validator_rejects_a_contradictory_run_corpus_descriptor() {
    let mut run = serde_json::to_value(query_run()).unwrap();
    run["data"]["corpus_kind"] = "prepared".into();
    assert!(validate_query_jsonl(&[jsonl(run), jsonl(query_complete())].concat()).is_err());
}

#[test]
fn tokenize_validator_retains_the_direct_run_state_for_completion_validation() {
    let run = std::fs::read("tests/golden/tokenize-run.jsonl").unwrap();
    let sentence = std::fs::read("tests/golden/tokenize-sentence.jsonl").unwrap();
    let mut complete = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/tokenize-complete.jsonl").unwrap(),
    )
    .unwrap();
    complete["data"]["state"] = "prefix_unavailable".into();
    complete["data"]
        .as_object_mut()
        .unwrap()
        .remove("source_identity");
    complete["data"]
        .as_object_mut()
        .unwrap()
        .remove("analysis_identity");
    assert!(validate_tokenize_jsonl(&[run, sentence, jsonl(complete)].concat()).is_err());
}

#[test]
fn query_validator_rejects_a_direct_full_eof_complete_without_identities() {
    let mut complete = serde_json::to_value(query_complete()).unwrap();
    complete["data"]["state"] = "prefix_unavailable".into();
    complete["data"]
        .as_object_mut()
        .unwrap()
        .remove("source_identity");
    complete["data"]
        .as_object_mut()
        .unwrap()
        .remove("analysis_identity");
    assert!(validate_query_jsonl(&[jsonl(query_run()), jsonl(complete)].concat()).is_err());
}

#[test]
fn query_validator_accepts_a_direct_prefix_completion_only_when_unavailable() {
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run()),
                jsonl(valid_query_complete_with(
                    CompleteCorpus::Direct {
                        completion: DirectCompletion::PrefixUnavailable,
                    },
                    Coverage::Prefix,
                    Termination::ResultLimit,
                )),
            ]
            .concat()
        )
        .is_ok()
    );
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run()),
                jsonl(query_complete_with(
                    complete_corpus(),
                    Coverage::Prefix,
                    Termination::ResultLimit,
                )),
            ]
            .concat()
        )
        .is_err()
    );

    let mut direct_prefix = valid_query_complete_with(
        CompleteCorpus::Direct {
            completion: DirectCompletion::PrefixUnavailable,
        },
        Coverage::Prefix,
        Termination::ResultLimit,
    );
    let QueryEvent::Complete(direct_prefix) = &mut direct_prefix.event else {
        unreachable!("valid_query_complete_with always builds a complete event");
    };
    direct_prefix.inventory = Some(inventory());
    assert!(validate_query_jsonl(&[jsonl(query_run()), jsonl(direct_prefix)].concat()).is_err());
}

#[test]
fn query_validator_rejects_an_invalid_prepared_coverage_termination_pair() {
    let corpus = RunCorpus::Prepared {
        source_identity: "source".into(),
        analysis_identity: "analysis".into(),
    };
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(corpus)),
                jsonl(query_complete_with(
                    CompleteCorpus::Prepared {
                        source_identity: "source".into(),
                        analysis_identity: "analysis".into(),
                    },
                    Coverage::Full,
                    Termination::ResultLimit,
                )),
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn tokenize_validator_rejects_an_invalid_prepared_coverage_termination_pair() {
    let mut run = prepared_tokenize_event("tokenize-run");
    let mut complete = prepared_tokenize_event("tokenize-complete");
    run["data"]["source_identity"] = "source".into();
    run["data"]["analysis_identity"] = "analysis".into();
    complete["data"]["source_identity"] = "source".into();
    complete["data"]["analysis_identity"] = "analysis".into();
    complete["data"]["coverage"] = "prefix".into();
    complete["data"]["termination"] = "eof".into();
    let sentence = std::fs::read("tests/golden/tokenize-sentence.jsonl").unwrap();
    assert!(validate_tokenize_jsonl(&[jsonl(run), sentence, jsonl(complete)].concat()).is_err());
}

#[test]
fn query_validator_requires_committed_prefix_for_prefix_result_limit() {
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run()),
                jsonl(query_complete_with(
                    CompleteCorpus::Direct {
                        completion: DirectCompletion::PrefixUnavailable,
                    },
                    Coverage::Prefix,
                    Termination::ResultLimit,
                )),
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_rejects_a_committed_prefix_on_a_full_eof_complete() {
    let mut envelope = query_complete();
    let QueryEvent::Complete(complete) = &mut envelope.event else {
        unreachable!("query_complete always builds a complete event");
    };
    complete.committed_prefix = Some(CommittedPrefix {
        corpus_sentence_ordinal: 9,
        sequence: 4,
    });
    assert!(validate_query_jsonl(&[jsonl(query_run()), jsonl(envelope)].concat()).is_err());
}

#[test]
fn query_validator_requires_inventory_availability_by_corpus_lifecycle() {
    let prepared_run = RunCorpus::Prepared {
        source_identity: "source".into(),
        analysis_identity: "analysis".into(),
    };
    let mut prepared_envelope = query_complete_with(
        CompleteCorpus::Prepared {
            source_identity: "source".into(),
            analysis_identity: "analysis".into(),
        },
        Coverage::Full,
        Termination::Eof,
    );
    let QueryEvent::Complete(prepared_complete) = &mut prepared_envelope.event else {
        unreachable!("query_complete_with always builds a complete event");
    };
    prepared_complete.inventory = None;
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(prepared_run)),
                jsonl(prepared_envelope)
            ]
            .concat()
        )
        .is_err()
    );

    let mut direct_full = query_complete();
    let QueryEvent::Complete(direct_full) = &mut direct_full.event else {
        unreachable!("query_complete always builds a complete event");
    };
    direct_full.inventory = None;
    assert!(validate_query_jsonl(&[jsonl(query_run()), jsonl(direct_full)].concat()).is_err());

    let stdin_run = RunCorpus::Stdin {
        stream_id: "stdin-1".into(),
    };
    let stdin_complete = query_complete_with(
        CompleteCorpus::Stdin {
            stream_id: "stdin-1".into(),
            observed_content_digest: Some("blake3:input".into()),
        },
        Coverage::Full,
        Termination::Eof,
    );
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(stdin_run)),
                jsonl(stdin_complete)
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_requires_known_inventory_for_a_prepared_run() {
    let mut envelope = query_run_with_corpus(RunCorpus::Prepared {
        source_identity: "source".into(),
        analysis_identity: "analysis".into(),
    });
    let QueryEvent::Run(run) = &mut envelope.event else {
        unreachable!("query_run_with_corpus always builds a run event");
    };
    run.inventory = None;
    let complete = valid_query_complete_with(
        CompleteCorpus::Prepared {
            source_identity: "source".into(),
            analysis_identity: "analysis".into(),
        },
        Coverage::Full,
        Termination::Eof,
    );
    assert!(validate_query_jsonl(&[jsonl(envelope), jsonl(complete)].concat()).is_err());
}

#[test]
fn query_validator_rejects_inventory_on_a_direct_run() {
    let mut envelope = query_run();
    let QueryEvent::Run(run) = &mut envelope.event else {
        unreachable!("query_run always builds a run event");
    };
    run.inventory = Some(inventory());
    assert!(validate_query_jsonl(&[jsonl(envelope), jsonl(query_complete())].concat()).is_err());
}

#[test]
fn query_validator_rejects_inventory_on_a_stdin_run() {
    let mut envelope = query_run_with_corpus(RunCorpus::Stdin {
        stream_id: "stdin-1".into(),
    });
    let QueryEvent::Run(run) = &mut envelope.event else {
        unreachable!("query_run_with_corpus always builds a run event");
    };
    run.inventory = Some(inventory());
    let complete = valid_query_complete_with(
        CompleteCorpus::Stdin {
            stream_id: "stdin-1".into(),
            observed_content_digest: Some("blake3:input".into()),
        },
        Coverage::Full,
        Termination::Eof,
    );
    assert!(validate_query_jsonl(&[jsonl(envelope), jsonl(complete)].concat()).is_err());
}

#[test]
fn tokenize_runs_do_not_expose_query_inventory() {
    let tokenize_run = fixtures()
        .into_iter()
        .find_map(|(name, bytes)| (name == "tokenize-run").then_some(bytes))
        .unwrap();
    let value = serde_json::from_slice::<serde_json::Value>(&tokenize_run).unwrap();
    assert!(value["data"].get("inventory").is_none());
}

#[test]
fn tokenize_validator_requires_full_eof_and_honest_completion_inventory() {
    let run = std::fs::read("tests/golden/tokenize-run.jsonl").unwrap();
    let mut complete = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/tokenize-complete.jsonl").unwrap(),
    )
    .unwrap();
    complete["data"]
        .as_object_mut()
        .unwrap()
        .remove("inventory");
    assert!(validate_tokenize_jsonl(&[run.clone(), jsonl(complete)].concat()).is_err());

    let mut prefix = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/tokenize-complete.jsonl").unwrap(),
    )
    .unwrap();
    prefix["data"]["coverage"] = "prefix".into();
    prefix["data"]["termination"] = "result_limit".into();
    assert!(validate_tokenize_jsonl(&[run, jsonl(prefix)].concat()).is_err());
}

#[test]
fn tokenize_validator_correlates_sentence_stream_ids_with_stdin_runs_only() {
    let direct_run = std::fs::read("tests/golden/tokenize-run.jsonl").unwrap();
    let direct_complete = std::fs::read("tests/golden/tokenize-complete.jsonl").unwrap();
    let mut direct_sentence = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/tokenize-sentence.jsonl").unwrap(),
    )
    .unwrap();
    direct_sentence["data"]["stream_id"] = "stdin-not-valid-for-filesystem".into();
    assert!(
        validate_tokenize_jsonl(
            &[
                direct_run.clone(),
                jsonl(direct_sentence),
                direct_complete.clone()
            ]
            .concat()
        )
        .is_err()
    );

    let mut stdin_run = serde_json::from_slice::<serde_json::Value>(&direct_run).unwrap();
    stdin_run["data"]["corpus_kind"] = "stdin".into();
    stdin_run["data"].as_object_mut().unwrap().remove("state");
    stdin_run["data"]["stream_id"] = "stdin-1".into();
    let mut stdin_sentence = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/tokenize-sentence.jsonl").unwrap(),
    )
    .unwrap();
    stdin_sentence["data"]["stream_id"] = "stdin-2".into();
    let mut stdin_complete = serde_json::from_slice::<serde_json::Value>(&direct_complete).unwrap();
    stdin_complete["data"]["corpus_kind"] = "stdin".into();
    for field in ["state", "source_identity", "analysis_identity", "inventory"] {
        stdin_complete["data"]
            .as_object_mut()
            .unwrap()
            .remove(field);
    }
    stdin_complete["data"]["stream_id"] = "stdin-1".into();
    stdin_complete["data"]["observed_content_digest"] = "digest".into();

    assert!(
        validate_tokenize_jsonl(
            &[
                jsonl(stdin_run),
                jsonl(stdin_sentence),
                jsonl(stdin_complete)
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_requires_prepared_completion_identities_to_match_the_run() {
    let corpus = RunCorpus::Prepared {
        source_identity: "source".into(),
        analysis_identity: "analysis".into(),
    };
    let matching = CompleteCorpus::Prepared {
        source_identity: "source".into(),
        analysis_identity: "analysis".into(),
    };
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(corpus)),
                jsonl(query_complete_with(
                    matching,
                    Coverage::Full,
                    Termination::Eof,
                )),
            ]
            .concat()
        )
        .is_ok()
    );
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(RunCorpus::Prepared {
                    source_identity: "source".into(),
                    analysis_identity: "analysis".into(),
                })),
                jsonl(query_complete_with(
                    CompleteCorpus::Prepared {
                        source_identity: "source".into(),
                        analysis_identity: "other-analysis".into(),
                    },
                    Coverage::Full,
                    Termination::Eof,
                )),
            ]
            .concat()
        )
        .is_err()
    );
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(RunCorpus::Stdin {
                    stream_id: "stdin-1".into(),
                })),
                jsonl(valid_query_complete_with(
                    CompleteCorpus::Stdin {
                        stream_id: "stdin-2".into(),
                        observed_content_digest: Some("blake3:input".into()),
                    },
                    Coverage::Full,
                    Termination::Eof,
                )),
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_requires_a_digest_for_a_full_stdin_eof() {
    let run = RunCorpus::Stdin {
        stream_id: "stdin-1".into(),
    };
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(run.clone())),
                jsonl(valid_query_complete_with(
                    CompleteCorpus::Stdin {
                        stream_id: "stdin-1".into(),
                        observed_content_digest: Some("blake3:input".into()),
                    },
                    Coverage::Full,
                    Termination::Eof,
                )),
            ]
            .concat()
        )
        .is_ok()
    );
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(run)),
                jsonl(valid_query_complete_with(
                    CompleteCorpus::Stdin {
                        stream_id: "stdin-1".into(),
                        observed_content_digest: None,
                    },
                    Coverage::Full,
                    Termination::Eof,
                )),
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_requires_stdin_prefix_completion_to_omit_the_digest() {
    let run = RunCorpus::Stdin {
        stream_id: "stdin-1".into(),
    };
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(run.clone())),
                jsonl(valid_query_complete_with(
                    CompleteCorpus::Stdin {
                        stream_id: "stdin-1".into(),
                        observed_content_digest: None,
                    },
                    Coverage::Prefix,
                    Termination::ResultLimit,
                )),
            ]
            .concat()
        )
        .is_ok()
    );
    assert!(
        validate_query_jsonl(
            &[
                jsonl(query_run_with_corpus(run)),
                jsonl(query_complete_with(
                    CompleteCorpus::Stdin {
                        stream_id: "stdin-1".into(),
                        observed_content_digest: Some("blake3:staged".into()),
                    },
                    Coverage::Prefix,
                    Termination::ResultLimit,
                )),
            ]
            .concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_rejects_any_direct_run_state_other_than_pending() {
    let mut run = serde_json::to_value(query_run()).unwrap();
    run["data"]["state"] = "known".into();
    assert!(validate_query_jsonl(&[jsonl(run), jsonl(query_complete())].concat()).is_err());
}

#[test]
fn query_validator_rejects_matches_when_run_mode_is_summarize() {
    let mut run = serde_json::to_value(query_run()).unwrap();
    run["data"]["mode"] = "summarize".into();
    assert!(
        validate_query_jsonl(&[jsonl(run), jsonl(query_match()), jsonl(query_complete())].concat())
            .is_err()
    );
}

#[test]
fn query_validator_rejects_summaries_when_run_mode_is_matches() {
    let summary = QueryEnvelope::new(QueryEvent::Summary(SummaryData {
        pattern: "p".into(),
        group: SummaryGroupData::Surface {
            value: "猫".into()
        },
        count: 2,
        rank: 1,
    }));
    assert!(
        validate_query_jsonl(
            &[jsonl(query_run()), jsonl(summary), jsonl(query_complete())].concat()
        )
        .is_err()
    );
}

#[test]
fn query_validator_allows_an_empty_summarize_result_stream() {
    let mut envelope = query_run();
    let QueryEvent::Run(run) = &mut envelope.event else {
        unreachable!("query_run always builds a run event");
    };
    run.mode = QueryMode::Summarize;
    assert!(validate_query_jsonl(&[jsonl(envelope), jsonl(query_complete())].concat()).is_ok());
}

#[test]
fn corpus_descriptors_are_flattened_and_expose_corpus_kind() {
    let run = serde_json::to_value(query_run()).unwrap();
    assert_eq!(run["data"]["corpus_kind"], "direct");
    assert!(run["data"].get("identities").is_none());
}

#[test]
fn diagnostic_data_has_an_explicit_observed_input_digest_field() {
    let diagnostic = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/diagnostic.jsonl").unwrap(),
    )
    .unwrap();
    assert_eq!(
        diagnostic["data"].get("observed_input_digest"),
        Some(&serde_json::Value::Null)
    );
}

#[test]
fn sources_complete_flattens_inventory_counts() {
    let complete = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read("tests/golden/sources-complete.jsonl").unwrap(),
    )
    .unwrap();
    assert_eq!(complete["data"]["alias_documents"], 0);
    assert_eq!(complete["data"]["max_target_multiplicity"], 1);
    assert!(complete["data"].get("inventory").is_none());
}

#[test]
fn raw_jsonl_requires_lf_for_every_nonempty_event_line() {
    for (name, validator) in [
        ("query-complete", validate_query_jsonl as fn(&[u8]) -> _),
        ("tokenize-complete", validate_tokenize_jsonl),
        ("sources-complete", validate_sources_jsonl),
        ("check", validate_check_jsonl),
        ("diagnostic", validate_diagnostic_jsonl),
    ] {
        let mut line = std::fs::read(format!("tests/golden/{name}.jsonl")).unwrap();
        assert_eq!(line.pop(), Some(b'\n'));
        assert!(
            matches!(validator(&line), Err(ProtocolError::TruncatedLine { .. })),
            "{name}"
        );
    }
    assert!(matches!(
        validate_query_jsonl(b""),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_tokenize_jsonl(b""),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_sources_jsonl(b""),
        Err(ProtocolError::MissingRun)
    ));
    assert!(matches!(
        validate_check_jsonl(b""),
        Err(ProtocolError::MissingRun)
    ));
    assert!(validate_diagnostic_jsonl(b"").is_ok());
}

#[test]
fn diagnostic_validator_has_its_own_exact_framed_protocol() {
    let diagnostic = std::fs::read("tests/golden/diagnostic.jsonl").unwrap();
    assert!(validate_diagnostic_jsonl(&diagnostic).is_ok());
    assert!(matches!(
        validate_diagnostic_jsonl(
            b"{\"schema\":\"lingrep.diagnostic/v0\",\"type\":\"diagnostic\",\"data\":{}}\n"
        ),
        Err(ProtocolError::WrongSchema { .. })
    ));
    assert!(
        matches!(validate_diagnostic_jsonl(b"{\"schema\":\"lingrep.diagnostic/v1\",\"type\":\"future\",\"data\":{}}\n"), Err(ProtocolError::UnknownEventType { event_type, .. }) if event_type == "future")
    );
}

#[test]
fn diagnostic_v3_separates_actionable_fields_and_omits_retry_for_warnings() {
    let error = DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
        run_id: None,
        observed_input_digest: None,
        stage: DiagnosticStage::Invocation,
        severity: DiagnosticSeverity::Error,
        code: "invalid_invocation".into(),
        message: "unknown command".into(),
        suggestions: vec!["matches".into()],
        usage: Some("ling matches [OPTIONS] <ROOT>".into()),
        retry: Some(RetryPolicy::AfterChange),
        location: None,
        related_locations: Vec::new(),
    }));
    let value = serde_json::to_value(error).unwrap();
    assert_eq!(value["schema"], "lingrep.diagnostic/v1");
    assert_eq!(value["data"]["message"], "unknown command");
    assert_eq!(value["data"]["suggestions"][0], "matches");
    assert_eq!(value["data"]["retry"], "after_change");

    let warning = DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(DiagnosticData {
        run_id: None,
        observed_input_digest: None,
        stage: DiagnosticStage::Catalog,
        severity: DiagnosticSeverity::Warning,
        code: "catalog_warning".into(),
        message: "warning".into(),
        suggestions: Vec::new(),
        usage: None,
        retry: None,
        location: None,
        related_locations: Vec::new(),
    }));
    assert!(
        serde_json::to_value(warning).unwrap()["data"]
            .get("retry")
            .is_none()
    );
}

#[test]
fn v2_allows_additive_data_fields_but_exactly_enforces_schema_and_event_tags() {
    let mut run = serde_json::to_value(query_run()).unwrap();
    run["data"]["future_field"] = 17.into();
    assert!(serde_json::from_value::<QueryEnvelope>(run).is_ok());
    assert!(
        serde_json::from_str::<QueryEnvelope>(
            r#"{"schema":"lingrep.query/v2","type":"run","data":{}}"#
        )
        .is_err()
    );
    assert!(
        serde_json::from_str::<QueryEnvelope>(
            r#"{"schema":"lingrep.query/v2","type":"future","data":{}}"#
        )
        .is_err()
    );
}

#[test]
fn corpus_descriptors_are_tagged_and_only_express_honest_stage_availability() {
    let stdin = CompleteCorpus::Stdin {
        stream_id: "stdin-7".into(),
        observed_content_digest: Some("blake3:abc".into()),
    };
    assert_eq!(
        serde_json::to_value(stdin).unwrap(),
        serde_json::json!({
            "corpus_kind": "stdin",
            "stream_id": "stdin-7",
            "observed_content_digest": "blake3:abc",
        })
    );
    assert_eq!(
        serde_json::to_value(direct_pending()).unwrap(),
        serde_json::json!({"corpus_kind": "direct", "state": "pending"})
    );
}

#[test]
fn sources_aggregate_state_distinguishes_known_inventory_from_pending() {
    assert_eq!(
        serde_json::to_value(AggregateState::Known {
            inventory: inventory()
        })
        .unwrap(),
        serde_json::json!({"state": "known", "inventory": {
            "documents": 1,
            "distinct_targets": 1,
            "alias_documents": 0,
            "max_target_multiplicity": 1,
            "external_targets": 0,
        }})
    );
}

#[test]
fn check_drift_round_trips_and_keeps_the_v2_schema_string() {
    let envelope = CheckEnvelope::new(CheckEvent::Check(CheckData {
        comparison: Vec::new(),
        examples: None,
        catalog_fingerprint: "catalog".into(),
        definitions: Vec::new(),
        compile_stats: CompileStats {
            source_bytes: 0,
            definitions: 0,
            lexicon_members: 0,
            expression_nodes: 0,
            max_depth: 0,
            metadata_bytes: 0,
            regex_bytes: 0,
            reference_edges: 0,
            expansion_work: 0,
            generated_owned_bytes: 0,
            nfa_states: 0,
            transitions: 0,
        },
        warnings: Vec::new(),
        drift: vec![DriftData {
            name: "ARU".into(),
            bodies: vec![
                DriftBodyData {
                    carriers: vec![DriftCarrierData {
                        catalog: "sfe.lx".into(),
                        kind: "fragment".into(),
                    }],
                },
                DriftBodyData {
                    carriers: vec![DriftCarrierData {
                        catalog: "patterns/sfe.lx".into(),
                        kind: "pattern".into(),
                    }],
                },
            ],
        }],
        binding: None,
    }));

    let bytes = jsonl(&envelope);
    let value: serde_json::Value = serde_json::from_slice(bytes.trim_ascii_end()).unwrap();
    // Additive fields keep the schema string.
    assert_eq!(value["schema"], "lingrep.check/v1");
    // Each body is an object with a `carriers` key, so the node can absorb a
    // future additive field without turning an array into an object.
    assert!(value["data"]["drift"][0]["bodies"][0]["carriers"].is_array());

    assert_eq!(
        serde_json::from_slice::<CheckEnvelope>(bytes.trim_ascii_end()).unwrap(),
        envelope
    );
}

#[test]
fn every_current_event_is_pinned_by_a_full_byte_golden() {
    for (name, actual) in fixtures() {
        let expected = std::fs::read(format!("tests/golden/{name}.jsonl")).unwrap();
        assert_eq!(reserialize_fixture(name, &expected), expected, "{name}");
        assert_eq!(actual, expected, "{name}");
    }
}

#[test]
fn prepared_query_run_with_known_inventory_is_pinned_by_a_byte_golden() {
    let expected = std::fs::read("tests/golden/query-run-prepared.jsonl").unwrap();
    let actual = jsonl(query_run_with_corpus(RunCorpus::Prepared {
        source_identity: "source".into(),
        analysis_identity: "analysis".into(),
    }));
    assert_eq!(actual, expected);
    assert!(validate_query_jsonl(&[actual, jsonl(prepared_query_complete())].concat()).is_ok());
}

fn fixtures() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("query-run", jsonl(query_run())),
        ("query-pattern", jsonl(query_pattern())),
        ("query-match", jsonl(query_match())),
        ("query-match-derived", jsonl(query_match_derived())),
        (
            "query-summary",
            jsonl(QueryEnvelope::new(QueryEvent::Summary(SummaryData {
                pattern: "p".into(),
                group: SummaryGroupData::Surface {
                    value: "猫".into()
                },
                count: 2,
                rank: 1,
            }))),
        ),
        ("query-complete", jsonl(query_complete())),
        (
            "check",
            jsonl(CheckEnvelope::new(CheckEvent::Check(CheckData {
                comparison: Vec::new(),
                examples: None,
                catalog_fingerprint: "catalog".into(),
                definitions: vec![DefinitionDescriptor {
                    name: "p".into(),
                    catalog_ordinal: 0,
                    body: DefinitionBody::Pattern {
                        expression: "[text=猫]".into(),
                    },
                    metadata: serde_json::json!({"nested": ["x"]}),
                    source_file: "patterns.lx".into(),
                    source_span: span(0, 8),
                }],
                compile_stats: CompileStats {
                    source_bytes: 20,
                    definitions: 1,
                    lexicon_members: 0,
                    expression_nodes: 1,
                    max_depth: 1,
                    metadata_bytes: 16,
                    regex_bytes: 0,
                    reference_edges: 0,
                    expansion_work: 1,
                    generated_owned_bytes: 8,
                    nfa_states: 2,
                    transitions: 1,
                },
                warnings: vec![CompileWarning {
                    code: "unused_metadata".into(),
                    message: "metadata is unused".into(),
                    location: Some(SourceLocation {
                        source_file: "patterns.lx".into(),
                        source_span: span(0, 8),
                    }),
                }],
                // Empty, so the byte golden is unchanged by the additive
                // fields — that byte-compatibility is the point of
                // `skip_serializing_if` here. The populated shapes are
                // round-tripped by `check_drift_round_trips_...` below and
                // the check-command tests.
                drift: Vec::new(),
                binding: None,
            }))),
        ),
        (
            "tokenize-run",
            jsonl(TokenizeEnvelope::new(TokenizeEvent::Run(Box::new(
                TokenizeRun {
                    run_id: "run-1".into(),
                    analyzer: fixture_analyzer_run(),
                    corpus: direct_pending(),
                    dictionary_identity: "dict".into(),
                    reader_identity: "reader".into(),
                    token_schema_identity: "token/v1".into(),
                    execution_limits: execution_limits(),
                    temporary_storage_limits: temp_limits(),
                    traversal_limits: traversal_limits(),
                    worker_count: 4,
                },
            )))),
        ),
        (
            "tokenize-sentence",
            jsonl(TokenizeEnvelope::new(TokenizeEvent::Sentence(
                SentenceData {
                    corpus_sentence_ordinal: 9,
                    source_name: "a.md".into(),
                    stream_id: None,
                    document_ordinal: 3,
                    document_sentence_ordinal: 2,
                    tokens: vec![token()],
                },
            ))),
        ),
        (
            "tokenize-complete",
            jsonl(TokenizeEnvelope::new(TokenizeEvent::Complete(
                TokenizeComplete {
                    coverage: Coverage::Full,
                    corpus: complete_corpus(),
                    sentences_emitted: 1,
                    tokens_emitted: 1,
                    elapsed_millis: 12,
                    resources: resources(),
                    inventory: Some(inventory()),
                    termination: Termination::Eof,
                },
            ))),
        ),
        (
            "sources-run",
            jsonl(SourcesEnvelope::new(SourcesEvent::Run(SourcesRun {
                run_id: "run-1".into(),
                source_kind: SourcesKind::Live,
                reader_identity: "reader".into(),
                admission_policy_identity: "admission/v1".into(),
                traversal_limits: traversal_limits(),
                aggregate_state: AggregateState::Pending,
            }))),
        ),
        (
            "sources-source",
            jsonl(SourcesEnvelope::new(SourcesEvent::Source(SourceData {
                ordinal: 3,
                logical_name: "a.md".into(),
                target_group_ordinal: 1,
                external_target: false,
                target_multiplicity: None,
            }))),
        ),
        (
            "sources-complete",
            jsonl(SourcesEnvelope::new(SourcesEvent::Complete(
                SourcesComplete {
                    metadata: None,
                    coverage: Coverage::Full,
                    inventory: inventory(),
                    traversal_high_water: TraversalHighWater {
                        max_depth: 1,
                        examined_entries: 1,
                        max_directory_entries: 1,
                        index_bytes: 1,
                    },
                    ordered_inventory_digest: "inventory".into(),
                    elapsed_millis: 12,
                    termination: Termination::Eof,
                    verification: None,
                },
            ))),
        ),
        (
            "diagnostic",
            jsonl(DiagnosticEnvelope::new(DiagnosticEvent::Diagnostic(
                DiagnosticData {
                    run_id: Some("run-1".into()),
                    observed_input_digest: None,
                    stage: DiagnosticStage::Matching,
                    severity: DiagnosticSeverity::Error,
                    code: "input_failed".into(),
                    message: "could not read".into(),
                    suggestions: vec!["check the input path".into()],
                    usage: None,
                    retry: Some(RetryPolicy::AfterChange),
                    location: Some(DiagnosticLocation {
                        source_name: Some("a.md".into()),
                        source_span: Some(span(2, 3)),
                        line: None,
                        column: None,
                        corpus_sentence_ordinal: Some(9),
                        token_index: Some(1),
                    }),
                    related_locations: Vec::new(),
                },
            ))),
        ),
    ]
}

fn reserialize_fixture(name: &str, bytes: &[u8]) -> Vec<u8> {
    match name {
        "query-run"
        | "query-pattern"
        | "query-match"
        | "query-match-derived"
        | "query-summary"
        | "query-complete" => jsonl(serde_json::from_slice::<QueryEnvelope>(bytes).unwrap()),
        "check" => jsonl(serde_json::from_slice::<CheckEnvelope>(bytes).unwrap()),
        "tokenize-run" | "tokenize-sentence" | "tokenize-complete" => {
            jsonl(serde_json::from_slice::<TokenizeEnvelope>(bytes).unwrap())
        }
        "sources-run" | "sources-source" | "sources-complete" => {
            jsonl(serde_json::from_slice::<SourcesEnvelope>(bytes).unwrap())
        }
        "diagnostic" => jsonl(serde_json::from_slice::<DiagnosticEnvelope>(bytes).unwrap()),
        unexpected => panic!("unknown fixture {unexpected}"),
    }
}
fn jsonl<E: serde::Serialize>(event: E) -> Vec<u8> {
    let mut actual = Vec::new();
    write_jsonl(&mut actual, &event).unwrap();
    actual
}

fn fixture_analyzer_run() -> AnalyzerRun {
    AnalyzerRun::new(
        &lingrep_corpus::AnalyzerRecord::vibrato_fixture(),
        "locally-executed",
        &lingrep_core::token_match::TokenSchema::from_keys(["text"]).unwrap(),
    )
}
fn prepared_tokenize_event(name: &str) -> serde_json::Value {
    let mut event = serde_json::from_slice::<serde_json::Value>(
        &std::fs::read(format!("tests/golden/{name}.jsonl")).unwrap(),
    )
    .unwrap();
    event["data"]["corpus_kind"] = "prepared".into();
    event["data"].as_object_mut().unwrap().remove("state");
    event
}
fn span(start: u64, end: u64) -> SourceSpan {
    SourceSpan { start, end }
}
/// The wire format uses canonical V1 keys in lexicographic order.
#[test]
fn token_attributes_serialize_with_canonical_v8_keys() {
    use std::collections::BTreeMap;

    use lingrep_core::token_match::{TokenAttributeIndex, TokenInfo};

    let mut token = TokenInfo::default();
    for (attribute, index) in TokenAttributeIndex::ALL.into_iter().zip(0..) {
        token.set(attribute, format!("値-{index}").into());
    }

    let previous: BTreeMap<String, String> = TokenAttributeIndex::ALL
        .into_iter()
        .filter_map(|attribute| {
            lingrep_core::token_match::TokenInfoTrait::get(&token, attribute).map(|value| {
                (
                    lingrep_core::token_match::AttributeKey::from(attribute).to_string(),
                    value.to_string(),
                )
            })
        })
        .collect();
    let current = crate::protocol::query::TokenAttributes::from_token(&token);

    assert_eq!(
        serde_json::to_string(&current).unwrap(),
        serde_json::to_string(&previous).unwrap(),
    );
    assert_eq!(current.len(), TokenAttributeIndex::COUNT);

    let round_tripped: crate::protocol::query::TokenAttributes =
        serde_json::from_str(&serde_json::to_string(&current).unwrap()).unwrap();
    assert_eq!(round_tripped, current);
}

/// A token carrying nothing serializes as an empty object, not as `null`.
#[test]
fn absent_attributes_serialize_as_an_empty_map() {
    let attributes = crate::protocol::query::TokenAttributes::from_token(
        &lingrep_core::token_match::TokenInfo::default(),
    );

    assert!(attributes.is_empty());
    assert_eq!(serde_json::to_string(&attributes).unwrap(), "{}");
}

/// Provider keys remain open while malformed keys are refused.
#[test]
fn provider_attribute_keys_round_trip_and_malformed_keys_fail() {
    let attributes =
        serde_json::from_str::<crate::protocol::query::TokenAttributes>(r#"{"vendor.extra":"x"}"#)
            .unwrap();
    assert_eq!(attributes.get("vendor.extra"), Some("x"));
    let error =
        serde_json::from_str::<crate::protocol::query::TokenAttributes>(r#"{"bad key":"x"}"#)
            .unwrap_err();

    assert!(
        error.to_string().contains("invalid token attribute key"),
        "{error}"
    );
}

fn token() -> TokenEvidence {
    TokenEvidence {
        index: 1,
        attributes: crate::protocol::query::TokenAttributes::from_pairs([("text", "猫")]),
    }
}
fn complete_corpus() -> CompleteCorpus {
    CompleteCorpus::Direct {
        completion: DirectCompletion::Known {
            source_identity: "source".into(),
            analysis_identity: "analysis".into(),
        },
    }
}
fn direct_pending() -> RunCorpus {
    RunCorpus::Direct {
        state: DirectRunState::Pending,
    }
}
fn execution_limits() -> ExecutionLimits {
    ExecutionLimits {
        max_sentence_bytes: 65_536,
        max_stage_sentences: 512,
        max_stage_input_bytes: 131_072,
        max_tokenized_bytes: 33_554_432,
    }
}
fn temp_limits() -> TemporaryStorageLimits {
    TemporaryStorageLimits {
        max_temp_bytes: 1024,
    }
}
fn traversal_limits() -> TraversalLimits {
    TraversalLimits {
        max_depth: 64,
        max_source_entries: 100,
        max_directory_entries: 10,
        max_documents: 10,
        max_index_bytes: 1024,
    }
}
fn inventory() -> InventoryStats {
    InventoryStats {
        documents: 1,
        distinct_targets: 1,
        alias_documents: 0,
        max_target_multiplicity: 1,
        external_targets: 0,
    }
}
fn resources() -> ResourceHighWater {
    ResourceHighWater {
        staged_raw_bytes: 12,
        tokenized_bytes: 34,
        temporary_storage_bytes: 56,
        execution_limits: execution_limits(),
        temporary_storage_limits: temp_limits(),
    }
}
fn query_run() -> QueryEnvelope {
    QueryEnvelope::new(QueryEvent::Run(Box::new(QueryRun {
        run_id: "run-1".into(),
        analyzer: fixture_analyzer_run(),
        mode: QueryMode::Matches,
        catalog_fingerprint: "catalog".into(),
        definition_count: 1,
        corpus: direct_pending(),
        dictionary_identity: "dict".into(),
        reader_identity: "reader".into(),
        token_schema_identity: "token/v1".into(),
        published_attributes: vec!["text".to_owned()],
        published_source_metadata: Vec::new(),
        ordering: MatchOrder::CorpusSentenceStartEndPatternCapturesGraphBindingsTerm,
        span_convention: SpanConvention::HalfOpenToken,
        char_offsets: false,
        requested_context: 2,
        requested_limit: Some(10),
        requested_top: None,
        execution_limits: execution_limits(),
        temporary_storage_limits: temp_limits(),
        traversal_limits: traversal_limits(),
        inventory: None,
        worker_count: 4,
        sample: None,
    })))
}
fn query_run_with_corpus(corpus: RunCorpus) -> QueryEnvelope {
    let mut envelope = query_run();
    let QueryEvent::Run(run) = &mut envelope.event else {
        unreachable!("query_run always builds a run event");
    };
    run.inventory = matches!(&corpus, RunCorpus::Prepared { .. }).then(inventory);
    run.corpus = corpus;
    envelope
}
fn query_pattern() -> QueryEnvelope {
    QueryEnvelope::new(QueryEvent::Definition(DefinitionData {
        name: "p".into(),
        catalog_ordinal: 0,
        body: DefinitionBody::Pattern {
            expression: "[text=猫]".into(),
        },
        metadata: serde_json::json!({"nested": ["x"]}),
        source_file: "patterns.lx".into(),
        source_span: span(0, 8),
    }))
}
fn query_match() -> QueryEnvelope {
    QueryEnvelope::new(QueryEvent::Match(MatchData {
        source_metadata: None,
        sequence: 4,
        pattern: "p".into(),
        span: span(1, 3),
        char_span: None,
        captures: vec![Capture {
            name: "target".into(),
            hygienic_path: "p.target".into(),
            span: span(1, 3),
            char_span: None,
        }],
        graph_bindings: Vec::new(),
        corpus_sentence_ordinal: 9,
        source_name: "a.md".into(),
        document_ordinal: 3,
        document_sentence_ordinal: 2,
        surface: "猫".into(),
        tokens: vec![token()],
        left_context: Some(vec![token()]),
        right_context: None,
        unit: None,
        key: None,
        lexical: None,
        tags: Vec::new(),
    }))
}

/// The same event once policy derived a unit, a key, and a tag from it.
///
/// A separate fixture rather than fields on `query_match`: the additive rule
/// only holds if a record with no derived fields retains its original wire
/// shape, and one fixture cannot pin both halves.
fn query_match_derived() -> QueryEnvelope {
    let mut envelope = query_match();
    let QueryEvent::Match(matched) = &mut envelope.event else {
        unreachable!("query_match always builds a match event");
    };
    matched.unit = Some("猫".into());
    matched.key = Some(vec![ProtocolKeyField::Scalar {
        name: "$target.norm".into(),
        value: "猫".into(),
    }]);
    matched.tags = vec![TagField {
        name: "aspect".into(),
        value: "stative".into(),
    }];
    envelope
}

#[test]
fn query_v7_round_trips_tagged_graph_assignments() {
    let mut envelope = query_match();
    let QueryEvent::Match(matched) = &mut envelope.event else {
        unreachable!();
    };
    matched.graph_bindings = vec![
        GraphBinding::Node {
            name: "predicate".into(),
            node: GraphNodeId::Empty {
                boundary: 2,
                subindex: 1,
            },
            attributes: vec![GraphNodeAttribute {
                name: "lemma".into(),
                value: "have".into(),
            }],
        },
        GraphBinding::Edge {
            name: "role".into(),
            head: GraphNodeId::Empty {
                boundary: 2,
                subindex: 1,
            },
            dependent: GraphNodeId::Word { token_index: 0 },
            relation: "nsubj".into(),
        },
    ];

    let json = serde_json::to_vec(&envelope).unwrap();
    let decoded: QueryEnvelope = serde_json::from_slice(&json).unwrap();
    assert_eq!(decoded, envelope);
    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["schema"], "lingrep.query/v2");
    assert_eq!(value["data"]["graph_bindings"][0]["binding_kind"], "node");
    assert_eq!(value["data"]["graph_bindings"][1]["binding_kind"], "edge");
}

fn query_complete() -> QueryEnvelope {
    QueryEnvelope::new(QueryEvent::Complete(QueryComplete {
        retained_matches: None,
        coverage: Coverage::Full,
        corpus: complete_corpus(),
        counters: RunCounters {
            offsets_omitted: 0,
            documents_admitted: 1,
            sentences_admitted: 1,
            sentences_proven_absent: 0,
            sentences_matched: 1,
            tokens_matched: 1,
            matches_found: 1,
            records_emitted: 1,
            records_unkeyed: 0,
            captures_dropped: 0,
        },
        elapsed_millis: 12,
        resources: resources(),
        committed_prefix: None,
        inventory: Some(inventory()),
        termination: Termination::Eof,
        scope: None,
    }))
}
fn prepared_query_complete() -> QueryEnvelope {
    valid_query_complete_with(
        CompleteCorpus::Prepared {
            source_identity: "source".into(),
            analysis_identity: "analysis".into(),
        },
        Coverage::Full,
        Termination::Eof,
    )
}
fn query_complete_with(
    corpus: CompleteCorpus,
    coverage: Coverage,
    termination: Termination,
) -> QueryEnvelope {
    let mut envelope = query_complete();
    let QueryEvent::Complete(complete) = &mut envelope.event else {
        unreachable!("query_complete always builds a complete event");
    };
    complete.corpus = corpus;
    complete.coverage = coverage;
    complete.termination = termination;
    envelope
}
fn valid_query_complete_with(
    corpus: CompleteCorpus,
    coverage: Coverage,
    termination: Termination,
) -> QueryEnvelope {
    let is_prepared = matches!(&corpus, CompleteCorpus::Prepared { .. });
    let is_direct = matches!(&corpus, CompleteCorpus::Direct { .. });
    let mut envelope = query_complete_with(corpus, coverage, termination);
    let QueryEvent::Complete(complete) = &mut envelope.event else {
        unreachable!("query_complete_with always builds a complete event");
    };
    complete.committed_prefix = match coverage {
        Coverage::Full => None,
        Coverage::Prefix => Some(CommittedPrefix {
            corpus_sentence_ordinal: 9,
            sequence: 4,
        }),
    };
    complete.inventory = match (is_prepared, is_direct, coverage) {
        (true, _, _) | (_, true, Coverage::Full) => Some(inventory()),
        _ => None,
    };
    envelope
}

/// Both contracts must be reachable by id, or `schema` and `describe`
/// cannot discover them and the command's output is undocumented.
#[test]
fn both_attribute_contracts_are_registered() {
    use crate::protocol::ContractId;
    use crate::protocol::registry::{get, summaries};

    for (id, expected) in [
        (ContractId::Attributes, "lingrep.attributes/v1"),
        (ContractId::AttributesTsv, "lingrep.attributes-tsv/v1"),
    ] {
        assert_eq!(id.as_str(), expected);
        assert!(
            summaries().iter().any(|summary| summary.id == id),
            "{expected} must appear in the index"
        );
        assert_eq!(get(id).summary().owners, &["coverage"]);
    }
}

/// The JSONL stream is four phases in a fixed order: one scope record,
/// zero or more lexicon projection records, one record for every declared
/// schema attribute, then zero or more exclusions. The schema-sized phase is
/// what makes "every attribute is reported" a contract rather than an
/// implementation accident.
#[test]
fn the_attributes_stream_declares_one_scope_and_a_complete_schema_phase() {
    use crate::protocol::ContractId;
    use crate::protocol::discovery::{ContractSpec, StreamShape};
    use crate::protocol::registry::get;

    let ContractSpec::Jsonl(spec) = get(ContractId::Attributes) else {
        panic!("lingrep.attributes/v1 is a JSONL contract");
    };
    let StreamShape::Phased { phases } = spec.stream else {
        panic!("the attributes stream is phased");
    };

    let shape: Vec<(&str, u64, Option<u64>)> = phases
        .iter()
        .map(|phase| (phase.name, phase.min, phase.max))
        .collect();
    assert_eq!(
        shape,
        vec![
            ("scope", 1, Some(1)),
            ("projections", 0, None),
            ("coverage", 1, Some(u64::from(u8::MAX))),
            ("exclusions", 0, None),
        ]
    );
}

/// `envelope_schema` rewrites `/properties/schema` and fails without it,
/// so the envelope must be a struct carrying `schema`, not an alias for
/// the event enum. Building the contract is what proves it.
#[test]
fn the_attributes_envelope_carries_its_schema_property() {
    use crate::protocol::ContractId;
    use crate::protocol::discovery::ContractSpec;
    use crate::protocol::registry::get;

    let ContractSpec::Jsonl(spec) = get(ContractId::Attributes) else {
        panic!("lingrep.attributes/v1 is a JSONL contract");
    };
    let schema = (spec.schema)(ContractId::Attributes).expect("schema builds");
    assert_eq!(
        schema.pointer("/properties/schema/const"),
        Some(&serde_json::json!("lingrep.attributes/v1"))
    );
}

/// A closed pipe must stay an I/O failure, not a serialization one.
///
/// The distinction is not cosmetic: [`crate::protocol::OutputError::is_broken_pipe`]
/// is what selects the typed `broken_pipe` diagnostic over a reported
/// serialization defect, and a run that ends on a closed stdout is the ordinary
/// case of piping JSONL into `head`. Serializing the record straight into the
/// writer instead of through the scratch buffer hands `serde_json` the error
/// first, which wraps it and loses the distinction — this is what reports that.
#[test]
fn a_closed_pipe_during_record_serialization_stays_a_broken_pipe() {
    use crate::protocol::{OutputError, write_jsonl_buffered};
    use std::io::{self, Write};

    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut scratch = Vec::new();
    let error = write_jsonl_buffered(&mut ClosedPipe, &mut scratch, &serde_json::json!({"a": 1}))
        .expect_err("a closed pipe fails the write");
    assert!(
        matches!(error, OutputError::Io(_)),
        "an I/O failure must not be reported as a serialization error: {error:?}"
    );
    assert!(error.is_broken_pipe());
}

#[test]
fn query_metadata_projection_requires_a_matching_run_declaration() {
    let mut run = serde_json::to_value(query_run()).unwrap();
    run["data"]["published_source_metadata"] = serde_json::json!(["@document.id"]);
    let mut matched = serde_json::to_value(query_match()).unwrap();
    matched["data"]["source_metadata"] = serde_json::json!({"document":{"id":0}});
    let bytes = |run: &serde_json::Value, matched: &serde_json::Value| {
        [jsonl(run), jsonl(matched), jsonl(query_complete())].concat()
    };
    assert!(validate_query_jsonl(&bytes(&run, &matched)).is_ok());
    for metadata in [
        serde_json::json!({"private":"leak"}),
        serde_json::Value::Null,
        serde_json::json!({"id":"x".repeat(65536)}),
    ] {
        let mut invalid = matched.clone();
        invalid["data"]["source_metadata"] = metadata;
        assert!(validate_query_jsonl(&bytes(&run, &invalid)).is_err());
    }
    let undeclared = serde_json::to_value(query_run()).unwrap();
    assert!(validate_query_jsonl(&bytes(&undeclared, &matched)).is_err());
    run["data"]["published_source_metadata"] = serde_json::json!(["@document.id", "@document.id"]);
    assert!(validate_query_jsonl(&bytes(&run, &matched)).is_err());
}

#[test]
fn block_sample_reports_require_the_physical_component_identity() {
    use super::query::SampleReport;
    use lingrep_core::execution::CorpusSample;
    let sample = CorpusSample::new(10_000, 7).unwrap();
    let sentence = serde_json::to_value(SampleReport::of(sample)).unwrap();
    assert_eq!(
        sentence,
        serde_json::json!({"unit":"sentence", "rate_per_million":10000,"seed":7})
    );
    let block = SampleReport::token_block(sample, "ab".repeat(32));
    let json = serde_json::to_value(&block).unwrap();
    assert_eq!(
        serde_json::from_value::<SampleReport>(json.clone()).unwrap(),
        block
    );
    let mut missing_identity = json;
    missing_identity
        .as_object_mut()
        .unwrap()
        .remove("token_component_blake3");
    assert!(serde_json::from_value::<SampleReport>(missing_identity).is_err());
}
