//! Consolidated lingrep-web in-process integration suite.
//!
//! One test binary instead of ~44. Tests that observe a process-level
//! property (RSS, mmap residency, wall-clock timing) or that touch OS
//! resources shared by every thread in the process (advisory file locks
//! reaching `fs4`/`flock`, the fd table, fixed ports, env vars, cwd, signal
//! handlers, PID) keep their own binary; see the plan's isolation rule.
//! `cli.rs` (trycmd) and `tests/browser/` (thirtyfour, chromedriver) also
//! stay separate — see their own module docs / `scripts/run_lingrep_web_layout_e2e.sh`.

mod cache_reader_test;
mod capture_handler_test;
mod capture_protocol_test;
mod capture_snapshot_test;
mod catalog_index_test;
mod catalog_projection_baseline;
mod cli_shape;
mod cold_corpus_start_test;
mod coordinator_integration_test;
mod corpus_context_mapping_test;
mod corpus_context_test;
mod corpus_query_e2e_test;
mod corpus_runtime_test;
mod corpus_set_test;
mod corpus_source_test;
mod detail_revision_test;
mod editor_api_test;
mod editor_asset_test;
mod export_endpoint_test;
mod handlers_test;
mod hit_provenance_render_test;
mod inspect_corruption_teardown_test;
mod inspection_projector_test;
mod integration_test;
mod local_boundary_test;
mod multi_corpus_handlers_test;
mod page_projection_test;
mod pattern_detail_chains_test;
mod pattern_detail_chip_render_test;
mod pattern_detail_definition_test;
mod pattern_detail_section_render_test;
mod permalink_endpoint_test;
mod policy_projection_test;
mod provenance_runtime_test;
mod publish_cleanup_test;
mod reading_test;
mod render_security_support;
mod results_workspace_characterization;
mod revision_state_test;
mod revision_status_test;
mod sample_analysis_test;
mod sample_endpoint_test;
mod sample_render_test;
mod section_span_stream_test;
mod server_context_test;
mod snapshot_catalog_test;
mod snapshot_inspector_test;
mod snapshot_lookup_test;
mod static_asset_test;
mod support;
mod token_inspection_characterization;
mod token_inspection_handler_test;
mod tokenizer_access_test;
mod v6_support;
