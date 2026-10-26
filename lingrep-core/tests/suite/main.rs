//! Consolidated lingrep-core integration suite.
//!
//! One test binary instead of ~45. Tests that observe a process-level property
//! (RSS, mmap residency, wall-clock), that touch OS resources shared by every
//! thread in the process (advisory file locks reaching `fs4`/`flock`, the fd
//! table, fixed ports, env vars, cwd, signal handlers, PID), or that require a
//! gate to build under `--release` keep their own binary; see the plan's
//! isolation rule.

#[global_allocator]
static TEST_ALLOCATOR: std::alloc::System = std::alloc::System;

#[path = "../support/mod.rs"]
mod support;

#[path = "../fixtures/mod.rs"]
mod fixtures;

mod alternation_lx_test;
mod analyzer_contract;
mod anchor_claim_test;
mod and_constraints_test;
mod any_order_test;
mod attribute_key_parsing;
mod bccwj_corpus_test;
mod binding_structural_guards;
mod bound_projection_contract;
mod capture_key_test;
mod catalog_drift_test;
mod catalog_examples;
mod collexeme_lx_test;
mod concord_lx_test;
mod corpus_discovery;
mod corpus_loading_tests;
mod corpus_reader;
mod corpus_source_characterization;
mod corpus_source_contract;
mod dependency_repertoire_test;
mod derived_groups_test;
mod dictionary_binding_test;
mod double_wo_resolve_equivalence_test;
mod edge_cases_consolidated;
mod example_verification_test;
mod execution_adversarial_probe;
mod execution_limits;
mod execution_runner;
mod execution_source;
mod external_protocol_contract;
mod fixtures_test;
mod fsm_kernel_characterization;
mod golden_output_tests;
mod intersection_test;
mod japanese_strategy_tests;
mod lexicon_compile_test;
mod lexicon_matcher_test;
mod lookaround_union_contract;
mod markdown_frontmatter_characterization;
mod markdown_reader_config;
mod matcher_contract;
mod matcher_property_tests;
mod metadata_unset_test;
mod mini_dictionary_test;
mod named_groups_test;
mod named_sets_test;
mod oov_featurization;
mod or_patterns_test;
mod orth_port_differential;
mod pattern_attribution_test;
mod pattern_compiler_contract;
mod pattern_determinism_test;
mod pattern_references_test;
mod policy_compile_test;
mod policy_engine_program_test;
mod policy_engine_test;
mod policy_group_test;
mod policy_helpers;
mod policy_partition_test;
mod policy_program_test;
mod policy_sink_test;
mod policy_trace_test;
mod policy_transform_test;
mod portable_catalog_realization;
mod quantifiers_test;
mod record_transformation_test;
mod reduplication_lx_test;
mod regex_attributes_test;
mod same_as_projection_test;
mod schema_binding_contract;
mod scope_admission;
mod scope_language;
mod section_scope;
mod sentence_explanation_test;
mod sentence_metadata_scope;
mod sf_experimental_lx_test;
mod sfe_catalog_contract;
mod simple_matching_test;
mod span_equality_test;
mod spill_counts;
mod sudachi_contract;
mod testing_api_tests;
mod token_attribute_contract;
mod token_test_algebra_test;
mod tracked_catalogs_test;
