#[path = "../support/mod.rs"]
mod support;

mod analyzer_prepare;
mod annotation_export;
mod artifact_fingerprint_golden;
mod attributes;
mod catalog_input;
mod catalog_labels;
mod cli;
mod cli_shape;
mod compare_occurrences_example;
mod completions;
mod contrast;
mod corpus_mode;
mod corpus_render;
mod corpus_sources;
mod dependency_evidence;
mod diagram_export;
mod direct_protocol;
mod disk_protocol;
mod document_reader;
mod examples;
mod explore_characterization;
mod external_gravity_example;
#[cfg(feature = "web")]
mod frequency_view;
mod introspection;
mod kwic_characterization;
mod manpages;
mod near;
mod neighbors;
mod policy_publication;
mod prepared_protocol;
mod protocol;
mod protocol_discovery;
mod prune_protocol;
mod sample_protocol;
#[cfg(unix)]
mod scale_bounds;
mod scope_protocol;
mod scoped_lookaround_protocol;
mod section_protocol;
mod summarize;
mod terminal_safety;
mod tokenize_protocol;
mod variants_protocol;

mod external_analyzer_cli;

mod ending_report;

mod table_document_export;
