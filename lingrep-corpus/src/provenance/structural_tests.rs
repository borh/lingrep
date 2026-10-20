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
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use super::{ProvenanceView, ProvenanceWriter};
use crate::{
    AnalysisIdentity, CorpusError, DocumentId, RichSentenceMetadata, SectionCoverage, SentenceId,
    SentenceLocation,
};
use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};

fn analysis_identity() -> AnalysisIdentity {
    AnalysisIdentity::from_bytes([42; 32])
}

fn location(
    sentence: u64,
    source_name: &str,
    document_ordinal: u64,
    document_sentence_ordinal: u64,
) -> SentenceLocation {
    location_for_document(
        sentence,
        DocumentId::new(11 + document_ordinal),
        source_name,
        document_ordinal,
        document_sentence_ordinal,
    )
}

fn location_for_document(
    sentence: u64,
    document_id: DocumentId,
    source_name: &str,
    document_ordinal: u64,
    document_sentence_ordinal: u64,
) -> SentenceLocation {
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: sentence,
        document_id,
        document_ordinal,
        document_sentence_ordinal,
        source_name: Arc::from(source_name),
    }
}

fn rich() -> RichSentenceMetadata {
    RichSentenceMetadata {
        structure: Default::default(),
        region_labels: vec!["body".into()].into_boxed_slice(),
        metadata_json: Some(r#"{"genre":"paper"}"#.into()),
        sentence_metadata_json: None,
    }
}

#[derive(Clone, Copy)]
struct StorageShape {
    name: &'static str,
    documents: u64,
    sentences_per_document: u64,
}

const SMOKE: StorageShape = StorageShape {
    name: "smoke",
    documents: 10,
    sentences_per_document: 10,
};
const REPRESENTATIVE: StorageShape = StorageShape {
    name: "representative",
    documents: 155,
    sentences_per_document: 32,
};
const STRESS: StorageShape = StorageShape {
    name: "stress",
    documents: 1_024,
    sentences_per_document: 5,
};

async fn connection(path: &Path) -> SqliteConnection {
    SqliteConnection::connect_with(
        &SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false),
    )
    .await
    .unwrap()
}

async fn finished_two_sentence_view(path: &Path) -> ProvenanceView {
    let analysis = analysis_identity();
    let mut writer = ProvenanceWriter::create(path, &[]).await.unwrap();
    writer
        .push_sentence(&location(0, "first.md", 0, 0), &rich())
        .await
        .unwrap();
    writer
        .push_sentence(&location(1, "second.md", 1, 0), &rich())
        .await
        .unwrap();
    writer.finish(analysis).await.unwrap();
    ProvenanceView::open(path, &analysis_identity())
        .await
        .unwrap()
}

fn shape_rich(document: u64, _document_sentence: u64) -> RichSentenceMetadata {
    RichSentenceMetadata {
        structure: Default::default(),
        region_labels: vec![format!("region-{}", document % 8)].into_boxed_slice(),
        metadata_json: Some(format!(r#"{{"document":{document}}}"#)),
        sentence_metadata_json: None,
    }
}

async fn characterize_storage(shape: StorageShape) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join(format!("{}.sqlite", shape.name));
    let total = shape.documents * shape.sentences_per_document;
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    for document in 0..shape.documents {
        let document_id = DocumentId::new(10_000 + document * 17);
        for document_sentence in 0..shape.sentences_per_document {
            let sentence = document * shape.sentences_per_document + document_sentence;
            writer
                .push_sentence(
                    &location_for_document(
                        sentence,
                        document_id,
                        &format!("document-{document}.md"),
                        document,
                        document_sentence,
                    ),
                    &shape_rich(document, document_sentence),
                )
                .await
                .unwrap();
        }
    }
    writer.finish(analysis_identity()).await.unwrap();

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert_eq!(view.sentence_count(), total);
    for sentence in [0, total / 2, total - 1] {
        let document = sentence / shape.sentences_per_document;
        let document_sentence = sentence % shape.sentences_per_document;
        assert_eq!(
            view.rich_sentence(SentenceId::new(sentence)).await.unwrap(),
            shape_rich(document, document_sentence)
        );
    }
    let mut exported = 0_u64;
    for sentence in 0..total {
        let document = sentence / shape.sentences_per_document;
        let document_sentence = sentence % shape.sentences_per_document;
        assert_eq!(
            view.rich_sentence(SentenceId::new(sentence)).await.unwrap(),
            shape_rich(document, document_sentence)
        );
        exported += 1;
    }
    assert_eq!(exported, total);

    let mut database = connection(&path).await;
    let row = sqlx::query(
        "SELECT COUNT(*) AS sentence_count, COUNT(DISTINCT document_id) AS document_count FROM sentence_provenance",
    )
    .fetch_one(&mut database)
    .await
    .unwrap();
    assert_eq!(
        row.try_get::<i64, _>("sentence_count").unwrap(),
        total as i64
    );
    assert_eq!(
        row.try_get::<i64, _>("document_count").unwrap(),
        shape.documents as i64
    );
    database.close().await.unwrap();
    assert!(fs::metadata(&path).unwrap().len() > 0);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            fs::symlink_metadata(PathBuf::from(format!("{}{suffix}", path.display())))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
    }
}

#[tokio::test]
async fn immutable_view_opens_literal_sqlite_path_and_returns_only_rich_metadata() {
    // Would fail if the reader builds a SQLite URL instead of passing the literal
    // path to SqliteConnectOptions: `?` and `#` would then alter its meaning.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("corpus ? #.sqlite");
    let analysis = analysis_identity();
    let location = location(0, "nested/a.md", 0, 0);
    let rich = RichSentenceMetadata {
        structure: Default::default(),
        region_labels: vec!["body".into()].into_boxed_slice(),
        metadata_json: Some(r#"{"genre":"paper"}"#.into()),
        sentence_metadata_json: None,
    };

    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer.push_sentence(&location, &rich).await.unwrap();
    writer.finish(analysis).await.unwrap();

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert_eq!(view.rich_sentence(SentenceId::new(0)).await.unwrap(), rich);
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(!PathBuf::from(format!("{}{suffix}", path.display())).exists());
    }
}

#[tokio::test]
async fn sentence_metadata_and_declared_keys_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sentence-metadata.sqlite");
    let analysis = analysis_identity();
    let with_sent_id = RichSentenceMetadata {
        structure: Default::default(),
        sentence_metadata_json: Some(r#"{"sent_id":"train-s1"}"#.into()),
        ..rich()
    };
    let without = rich();

    let mut writer = ProvenanceWriter::create(&path, &["sent_id".to_owned()])
        .await
        .unwrap();
    writer
        .push_sentence(&location(0, "a.conllu", 0, 0), &with_sent_id)
        .await
        .unwrap();
    writer
        .push_sentence(&location(1, "a.conllu", 0, 1), &without)
        .await
        .unwrap();
    writer.finish(analysis).await.unwrap();

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert_eq!(
        view.sentence_metadata_keys().await.unwrap(),
        std::collections::BTreeSet::from(["sent_id".to_owned()])
    );
    // Only the sentence that carries metadata has a row.
    assert_eq!(
        view.sentence_metadata_page(0).await.unwrap(),
        vec![(0, r#"{"sent_id":"train-s1"}"#.to_owned())]
    );
    // The rich read returns the sentence's own metadata beside the document's.
    assert_eq!(
        view.rich_sentence(SentenceId::new(0)).await.unwrap(),
        with_sent_id
    );
    assert_eq!(
        view.rich_sentence(SentenceId::new(1)).await.unwrap(),
        without
    );
}

#[tokio::test]
async fn a_reader_declaring_no_keys_records_none() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("no-keys.sqlite");
    let analysis = analysis_identity();
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence(&location(0, "a.md", 0, 0), &rich())
        .await
        .unwrap();
    writer.finish(analysis).await.unwrap();

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert!(view.sentence_metadata_keys().await.unwrap().is_empty());
    assert!(view.sentence_metadata_page(0).await.unwrap().is_empty());
}

#[tokio::test]
async fn immutable_view_rejects_a_different_analysis_identity() {
    // Would fail if a reader could reuse rich metadata from a different analysis.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let location = location(0, "source.md", 0, 0);
    let rich = RichSentenceMetadata {
        structure: Default::default(),
        region_labels: Box::new([]),
        metadata_json: None,
        sentence_metadata_json: None,
    };

    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer.push_sentence(&location, &rich).await.unwrap();
    writer.finish(analysis_identity()).await.unwrap();

    let error = match ProvenanceView::open(&path, &AnalysisIdentity::from_bytes([99; 32])).await {
        Ok(_) => panic!("mismatched analysis identity must not open"),
        Err(error) => error,
    };
    assert_eq!(error.code(), "verification_mismatch");
}

#[tokio::test]
async fn immutable_view_exposes_the_committed_contiguous_sentence_count() {
    // Would fail if the writer did not track accepted sentence ordinals and
    // commit their total with the ready state.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");

    let view = finished_two_sentence_view(&path).await;

    assert_eq!(view.sentence_count(), 2);
    assert_eq!(
        view.rich_sentence(SentenceId::new(0)).await.unwrap(),
        rich()
    );
    assert_eq!(
        view.rich_sentence(SentenceId::new(1)).await.unwrap(),
        rich()
    );
}

#[tokio::test]
async fn immutable_view_rejects_malformed_identity_distinctly_from_a_mismatch() {
    // Would fail if corrupt identity bytes were silently treated as a different
    // but well-formed analysis identity.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let _view = finished_two_sentence_view(&path).await;
    let mut database = connection(&path).await;
    sqlx::query("UPDATE provenance_identity SET analysis_identity=x'00'")
        .execute(&mut database)
        .await
        .unwrap();
    database.close().await.unwrap();

    assert!(matches!(
        ProvenanceView::open(&path, &analysis_identity()).await,
        Err(CorpusError::InvalidFormat {
            component: "provenance.sqlite",
            ..
        })
    ));
}

#[tokio::test]
async fn immutable_view_rejects_wrong_schema_before_identity_comparison() {
    // Would fail if an unsupported format could masquerade as an identity miss.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let _view = finished_two_sentence_view(&path).await;
    let mut database = connection(&path).await;
    sqlx::query("UPDATE provenance_identity SET schema_version=99")
        .execute(&mut database)
        .await
        .unwrap();
    database.close().await.unwrap();

    assert!(matches!(
        ProvenanceView::open(&path, &analysis_identity()).await,
        Err(CorpusError::UnsupportedSchema {
            component: "provenance.sqlite",
            version: 99,
        })
    ));
}

#[tokio::test]
async fn immutable_view_rejects_unready_or_count_inconsistent_storage() {
    // Would fail if the reader used a building database or trusted its count
    // without checking the persisted contiguous row relation.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let _view = finished_two_sentence_view(&path).await;
    let mut database = connection(&path).await;
    sqlx::query("UPDATE provenance_identity SET state='building'")
        .execute(&mut database)
        .await
        .unwrap();
    database.close().await.unwrap();
    assert!(matches!(
        ProvenanceView::open(&path, &analysis_identity()).await,
        Err(CorpusError::VerificationMismatch { .. })
    ));

    let mut database = connection(&path).await;
    sqlx::query("UPDATE provenance_identity SET state='ready', sentence_count=3")
        .execute(&mut database)
        .await
        .unwrap();
    database.close().await.unwrap();
    assert!(matches!(
        ProvenanceView::open(&path, &analysis_identity()).await,
        Err(CorpusError::VerificationMismatch { .. })
    ));
}

#[tokio::test]
async fn immutable_view_rejects_noncontiguous_sentence_ids() {
    // Would fail if a hole in the row relation could make metadata attach to a
    // different dense `locations.v1` sentence.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let _view = finished_two_sentence_view(&path).await;
    let mut database = connection(&path).await;
    sqlx::query("UPDATE sentence_provenance SET sentence_id=2 WHERE sentence_id=1")
        .execute(&mut database)
        .await
        .unwrap();
    database.close().await.unwrap();

    assert!(matches!(
        ProvenanceView::open(&path, &analysis_identity()).await,
        Err(CorpusError::VerificationMismatch { .. })
    ));
}

#[tokio::test]
async fn immutable_view_rejects_regular_and_dangling_sqlite_sidecars() {
    // Would fail if `exists` rather than lstat semantics allowed a dangling
    // journal-link, or if immutable open happened before directory validation.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let _view = finished_two_sentence_view(&path).await;
    let wal = PathBuf::from(format!("{}-wal", path.display()));
    fs::write(&wal, b"unexpected").unwrap();
    assert!(matches!(
        ProvenanceView::open(&path, &analysis_identity()).await,
        Err(CorpusError::InvalidFormat {
            component: "provenance.sqlite",
            ..
        })
    ));
    fs::remove_file(&wal).unwrap();

    #[cfg(unix)]
    {
        let journal = PathBuf::from(format!("{}-journal", path.display()));
        std::os::unix::fs::symlink("missing-target", &journal).unwrap();
        assert!(matches!(
            ProvenanceView::open(&path, &analysis_identity()).await,
            Err(CorpusError::InvalidFormat {
                component: "provenance.sqlite",
                ..
            })
        ));
    }
}

#[tokio::test]
async fn writer_refuses_an_existing_database_without_changing_bytes_or_sidecars() {
    // Would fail if SQLite opened the final path before this API claimed it.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("existing.sqlite");
    let original = b"do not replace this artifact";
    fs::write(&path, original).unwrap();
    let sidecars = [
        ("-wal", b"original wal bytes".as_slice()),
        ("-shm", b"original shm bytes".as_slice()),
        ("-journal", b"original journal bytes".as_slice()),
    ];
    for (suffix, bytes) in sidecars {
        fs::write(PathBuf::from(format!("{}{suffix}", path.display())), bytes).unwrap();
    }

    assert!(matches!(
        ProvenanceWriter::create(&path, &[]).await,
        Err(CorpusError::DestinationExists { path: ref found }) if found == &path
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
    for (suffix, bytes) in sidecars {
        let sidecar = PathBuf::from(format!("{}{suffix}", path.display()));
        assert_eq!(fs::read(&sidecar).unwrap(), bytes);
        assert!(fs::symlink_metadata(sidecar).unwrap().file_type().is_file());
    }
}

#[tokio::test]
async fn writer_rejects_sidecars_left_after_close() {
    // Would fail if `finish` claimed a single-file artifact despite a companion
    // directory entry surviving SQLite's DELETE-journal transition.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence(&location(0, "source.md", 0, 0), &rich())
        .await
        .unwrap();
    fs::create_dir(PathBuf::from(format!("{}-journal", path.display()))).unwrap();

    assert!(matches!(
        writer.finish(analysis_identity()).await,
        Err(CorpusError::InvalidFormat {
            component: "provenance.sqlite",
            ..
        })
    ));
}

#[tokio::test]
async fn writer_rejects_a_noncontiguous_sentence_before_it_can_be_committed() {
    // Would fail if a writer accepted a gap that a later immutable view could
    // not safely compose with dense locations.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();

    assert!(matches!(
        writer
            .push_sentence(&location(1, "source.md", 0, 1), &rich())
            .await,
        Err(CorpusError::InvalidFormat {
            component: "provenance.sqlite",
            ..
        })
    ));
    writer
        .push_sentence(&location(0, "source.md", 0, 0), &rich())
        .await
        .unwrap();
    writer.finish(analysis_identity()).await.unwrap();
}

#[tokio::test]
async fn rich_metadata_keeps_multi_document_attribution_and_hostile_text_bound_to_sentence() {
    // Would fail if a multi-row query attributed rich text by result position or
    // interpolated untrusted metadata instead of binding it as data.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let hostile = RichSentenceMetadata {
        structure: Default::default(),
        region_labels: vec!["body".into()].into_boxed_slice(),
        metadata_json: Some(r#"{"note":"<script>alert(1)</script>' OR 1=1 --"}"#.into()),
        sentence_metadata_json: None,
    };
    let second = rich();
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence(&location(0, "first.md", 0, 0), &hostile)
        .await
        .unwrap();
    writer
        .push_sentence(&location(1, "second.md", 1, 0), &second)
        .await
        .unwrap();
    writer.finish(analysis_identity()).await.unwrap();

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert_eq!(view.sentence_count(), 2);
    assert_eq!(
        view.rich_sentence(SentenceId::new(1)).await.unwrap(),
        second
    );
    assert_eq!(
        view.rich_sentence(SentenceId::new(0)).await.unwrap(),
        hostile
    );
}

#[tokio::test]
async fn finished_storage_is_versioned_ready_counted_and_single_file() {
    // Would fail if finalization did not atomically record the ready/count
    // metadata that immutable open validates, or leaked SQLite companions.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let _view = finished_two_sentence_view(&path).await;
    let mut database = connection(&path).await;
    let row = sqlx::query(
        "SELECT schema_version, state, sentence_count FROM provenance_identity WHERE singleton=1",
    )
    .fetch_one(&mut database)
    .await
    .unwrap();
    assert_eq!(
        row.try_get::<i64, _>("schema_version").unwrap(),
        i64::from(super::SCHEMA_VERSION)
    );
    assert_eq!(row.try_get::<String, _>("state").unwrap(), "ready");
    assert_eq!(row.try_get::<i64, _>("sentence_count").unwrap(), 2);
    database.close().await.unwrap();
    for suffix in ["-wal", "-shm", "-journal"] {
        assert!(
            fs::symlink_metadata(PathBuf::from(format!("{}{suffix}", path.display())))
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        );
    }
}

#[tokio::test]
async fn provenance_storage_shapes_preserve_counts_and_samples() {
    for shape in [SMOKE, REPRESENTATIVE, STRESS] {
        characterize_storage(shape).await;
    }
}

#[tokio::test]
async fn private_rows_keep_distinct_document_ids_and_their_metadata_attribution() {
    // Would fail if the writer mixed locations or rich metadata between two
    // documents. The internal query proves the derived rows; no location field
    // is added to ProvenanceView's public surface.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("multi-document.sqlite");
    let first_id = DocumentId::new(41);
    let second_id = DocumentId::new(9_007);
    let first = RichSentenceMetadata {
        structure: Default::default(),
        region_labels: vec!["intro".into()].into_boxed_slice(),
        metadata_json: Some(r#"{"owner":"first"}"#.into()),
        sentence_metadata_json: None,
    };
    let second = RichSentenceMetadata {
        structure: Default::default(),
        region_labels: vec!["appendix".into()].into_boxed_slice(),
        metadata_json: Some(r#"{"owner":"second"}"#.into()),
        sentence_metadata_json: None,
    };
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence(
            &location_for_document(0, first_id, "first.md", 0, 0),
            &first,
        )
        .await
        .unwrap();
    writer
        .push_sentence(
            &location_for_document(1, second_id, "second.md", 1, 0),
            &second,
        )
        .await
        .unwrap();
    writer.finish(analysis_identity()).await.unwrap();

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert_eq!(view.sentence_count(), 2);
    assert_eq!(view.rich_sentence(SentenceId::new(0)).await.unwrap(), first);
    assert_eq!(
        view.rich_sentence(SentenceId::new(1)).await.unwrap(),
        second
    );

    let mut database = connection(&path).await;
    let rows = sqlx::query(
        "SELECT sentence_id, document_id, source_name, document_ordinal, document_sentence_ordinal, dm.metadata_json FROM sentence_provenance sp JOIN document_metadata dm USING(document_id) ORDER BY sentence_id",
    )
    .fetch_all(&mut database)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].try_get::<i64, _>("document_id").unwrap(), 41);
    assert_eq!(
        rows[0].try_get::<String, _>("source_name").unwrap(),
        "first.md"
    );
    assert_eq!(rows[0].try_get::<i64, _>("document_ordinal").unwrap(), 0);
    assert_eq!(
        rows[0]
            .try_get::<i64, _>("document_sentence_ordinal")
            .unwrap(),
        0
    );
    assert_eq!(
        rows[0].try_get::<String, _>("metadata_json").unwrap(),
        r#"{"owner":"first"}"#
    );
    assert_eq!(rows[1].try_get::<i64, _>("document_id").unwrap(), 9_007);
    assert_eq!(
        rows[1].try_get::<String, _>("source_name").unwrap(),
        "second.md"
    );
    assert_eq!(rows[1].try_get::<i64, _>("document_ordinal").unwrap(), 1);
    assert_eq!(
        rows[1]
            .try_get::<i64, _>("document_sentence_ordinal")
            .unwrap(),
        0
    );
    assert_eq!(
        rows[1].try_get::<String, _>("metadata_json").unwrap(),
        r#"{"owner":"second"}"#
    );
    database.close().await.unwrap();
}

async fn coverage_view(path: &Path) -> ProvenanceView {
    let mut writer = ProvenanceWriter::create(path, &[]).await.unwrap();
    for (sentence, region) in [(0, "b,m,a"), (1, "b,m,n"), (2, "b,n")] {
        let mut metadata = rich();
        metadata.region_labels = region
            .split(',')
            .map(str::to_owned)
            .collect::<Vec<_>>()
            .into_boxed_slice();
        writer
            .push_sentence(&location(sentence, "paper.md", 0, sentence), &metadata)
            .await
            .unwrap();
    }
    writer.finish(analysis_identity()).await.unwrap();
    ProvenanceView::open(path, &analysis_identity())
        .await
        .unwrap()
}

#[tokio::test]
async fn section_coverage_uses_spans_and_orders_coverage_then_label() {
    let directory = tempfile::tempdir().unwrap();
    let view = coverage_view(&directory.path().join("coverage.sqlite")).await;

    assert_eq!(
        view.section_coverage(4).await.unwrap(),
        [
            SectionCoverage {
                label: "b".into(),
                coverage: 3,
            },
            SectionCoverage {
                label: "m".into(),
                coverage: 2,
            },
            SectionCoverage {
                label: "n".into(),
                coverage: 2,
            },
            SectionCoverage {
                label: "a".into(),
                coverage: 1,
            },
        ]
    );
}

#[tokio::test]
async fn section_coverage_enforces_the_explicit_label_capacity() {
    let directory = tempfile::tempdir().unwrap();
    let view = coverage_view(&directory.path().join("coverage.sqlite")).await;

    assert_eq!(
        view.section_coverage(3).await.unwrap_err().code(),
        "section_label_capacity"
    );
    assert_eq!(
        view.section_coverage(0).await.unwrap_err().code(),
        "section_label_capacity"
    );
}

#[tokio::test]
async fn section_coverage_is_empty_when_the_artifact_has_no_regions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("empty-regions.sqlite");
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    let mut metadata = rich();
    metadata.region_labels = Box::new([]);
    writer
        .push_sentence(&location(0, "paper.md", 0, 0), &metadata)
        .await
        .unwrap();
    writer.finish(analysis_identity()).await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();

    assert!(view.section_coverage(8).await.unwrap().is_empty());
}

#[tokio::test]
async fn document_metadata_returns_one_row_per_document_with_its_sentence_span() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let analysis = analysis_identity();
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    // Two documents, three sentences then two, with frontmatter on the first
    // sentence of each — the shape the writer produces, since metadata is
    // duplicated across a document's rows.
    for sentence in 0..3 {
        writer
            .push_sentence(
                &location_for_document(sentence, DocumentId::new(0), "a.md", 0, sentence),
                &rich(),
            )
            .await
            .unwrap();
    }
    for sentence in 0..2 {
        writer
            .push_sentence(
                &location_for_document(3 + sentence, DocumentId::new(1), "b.md", 1, sentence),
                &rich(),
            )
            .await
            .unwrap();
    }
    writer.finish(analysis).await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();

    let rows = view.document_metadata_page(0).await.unwrap();

    assert_eq!(rows.len(), 2, "one row per document, not per sentence");
    assert_eq!(rows[0].document_id, DocumentId::new(0));
    assert_eq!(rows[0].first_sentence, 0);
    assert_eq!(rows[0].sentence_count, 3);
    assert_eq!(rows[1].document_id, DocumentId::new(1));
    assert_eq!(rows[1].first_sentence, 3);
    assert_eq!(rows[1].sentence_count, 2);
    assert_eq!(
        rows.iter().map(|row| row.sentence_count).sum::<u64>(),
        view.sentence_count(),
        "the spans tile the artifact"
    );
    assert_eq!(
        rows[0].metadata_json.as_deref(),
        Some(r#"{"genre":"paper"}"#),
        "frontmatter is returned as stored text, not as parsed metadata"
    );
}

#[tokio::test]
async fn document_metadata_returns_none_for_a_document_without_frontmatter() {
    // The writer emits NULL for an empty `Metadata`, so this is a state the
    // artifact produces legitimately — data, not corruption.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let analysis = analysis_identity();
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence(
            &location_for_document(0, DocumentId::new(0), "a.md", 0, 0),
            &RichSentenceMetadata {
                structure: Default::default(),
                region_labels: Box::new([]),
                metadata_json: None,
                sentence_metadata_json: None,
            },
        )
        .await
        .unwrap();
    writer.finish(analysis).await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();

    let rows = view.document_metadata_page(0).await.unwrap();

    assert_eq!(rows.len(), 1);
    assert!(rows[0].metadata_json.is_none());
}

/// An analyzed sentence followed by two that no analysis claims: all three
/// reach storage, and only the analyzed one carries analyzer columns.
///
/// The writer holds each row until that sentence's fate is known, so this pins
/// both exits from the hold — the analysis completing the row, and the next
/// sentence forcing the one before it out as provisional. The *second*
/// unclaimed sentence is what makes this a test rather than a restatement:
/// with only one, `finish` would flush it whether or not the arrival of a new
/// sentence also flushes, and a lost row would go unnoticed.
#[tokio::test]
async fn an_analyzed_and_two_unanalyzed_sentences_all_reach_storage() {
    let tagged = |label: &str| RichSentenceMetadata {
        sentence_metadata_json: Some(serde_json::json!({"label":label}).to_string()),
        ..rich()
    };

    use lingrep_core::analysis::{
        AnalyzerPlan, AnalyzerSession, InputUnitId, NormalizationPolicy, TextUnit,
    };

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let plan =
        lingrep_core::analysis::VibratoPlan::new(lingrep_core::testing::test_tokenizer().clone());
    let projection = plan
        .schema()
        .bind_projection(plan.schema().keys().iter().cloned())
        .unwrap();
    let mut session = plan.create_session();

    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence_with_text(&location(0, "paper.md", 0, 0), &tagged("first"), "研究")
        .await
        .unwrap();
    let analyzed = session
        .analyze_bounded_batch(
            vec![
                TextUnit::sentence(InputUnitId::new(0), "研究", NormalizationPolicy::Nfc).unwrap(),
            ],
            &projection,
        )
        .unwrap()
        .into_inputs()
        .into_vec()
        .pop()
        .unwrap();
    writer.attach_analyzed_sentence(0, &analyzed).await.unwrap();
    writer
        .push_sentence_with_text(&location(1, "paper.md", 0, 1), &tagged("second"), "調査")
        .await
        .unwrap();
    writer
        .push_sentence_with_text(&location(2, "paper.md", 0, 2), &tagged("third"), "分析")
        .await
        .unwrap();
    writer.finish(analysis_identity()).await.unwrap();

    let mut connection = connection(&path).await;
    let rows = sqlx::query(
        "SELECT sp.sentence_id, json_extract(sm.metadata_json,'$.label') AS label, input_unit_id, normalization, analyzer_sentence_ordinal FROM sentence_provenance sp LEFT JOIN sentence_metadata sm ON sm.sentence_id=sp.sentence_id ORDER BY sp.sentence_id",
    )
    .fetch_all(&mut connection)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3, "every sentence is stored");
    assert_eq!(rows[0].get::<i64, _>("sentence_id"), 0);
    assert_eq!(rows[0].get::<String, _>("label"), "first");
    assert_eq!(rows[0].get::<Option<i64>, _>("input_unit_id"), Some(0));
    assert_eq!(
        rows[0].get::<Option<String>, _>("normalization").as_deref(),
        Some("nfc")
    );
    assert_eq!(
        rows[0].get::<Option<i64>, _>("analyzer_sentence_ordinal"),
        Some(0)
    );
    assert_eq!(rows[1].get::<i64, _>("sentence_id"), 1);
    assert_eq!(rows[1].get::<String, _>("label"), "second");
    assert_eq!(
        rows[1].get::<Option<i64>, _>("input_unit_id"),
        None,
        "a sentence no analysis claimed carries no analyzer columns"
    );
    assert_eq!(rows[1].get::<Option<String>, _>("normalization"), None);
    assert_eq!(
        rows[1].get::<Option<i64>, _>("analyzer_sentence_ordinal"),
        None
    );
    assert_eq!(rows[2].get::<i64, _>("sentence_id"), 2);
    assert_eq!(rows[2].get::<String, _>("label"), "third");
    assert_eq!(rows[2].get::<Option<i64>, _>("input_unit_id"), None);
    connection.close().await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    assert_eq!(
        view.sentence_analysis(SentenceId::new(0))
            .await
            .unwrap()
            .text,
        analyzed.input().text()
    );
}

#[tokio::test]
async fn immutable_view_rejects_obsolete_schema_and_invalid_document_ranges() {
    for mutation in [
        "UPDATE provenance_identity SET schema_version=1",
        "UPDATE provenance_identity SET schema_version=2",
        "UPDATE provenance_identity SET schema_version=3",
        "DELETE FROM document_metadata WHERE first_sentence=0",
        "UPDATE document_metadata SET sentence_count=2 WHERE first_sentence=0",
        "UPDATE document_metadata SET sentence_count=2 WHERE first_sentence=1",
        "INSERT INTO document_metadata VALUES(999, 2, 1, NULL)",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provenance.sqlite");
        let view = finished_two_sentence_view(&path).await;
        drop(view);
        let mut database = connection(&path).await;
        sqlx::query(mutation).execute(&mut database).await.unwrap();
        database.close().await.unwrap();
        assert!(
            ProvenanceView::open(&path, &analysis_identity())
                .await
                .is_err(),
            "{mutation}"
        );
    }
}

#[tokio::test]
async fn document_metadata_is_stored_once_and_cannot_change_within_a_document() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    for ordinal in 0..3 {
        writer
            .push_sentence(&location(ordinal, "paper.md", 0, ordinal), &rich())
            .await
            .unwrap();
    }
    writer.finish(analysis_identity()).await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    let rows = view.document_metadata_page(0).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].first_sentence, rows[0].sentence_count), (0, 3));
    for ordinal in 0..3 {
        assert_eq!(
            view.rich_sentence(SentenceId::new(ordinal))
                .await
                .unwrap()
                .metadata_json,
            rows[0].metadata_json
        );
    }
    let path = directory.path().join("changed.sqlite");
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    writer
        .push_sentence(&location(0, "paper.md", 0, 0), &rich())
        .await
        .unwrap();
    let mut changed = rich();
    changed.metadata_json = None;
    assert!(
        writer
            .push_sentence(&location(1, "paper.md", 0, 1), &changed)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn structural_corruption_cannot_be_interpreted_as_absence() {
    for mutation in [
        "UPDATE sentence_provenance SET division_ids_json='[]' WHERE sentence_id=0",
        "UPDATE sentence_provenance SET division_ids_json='[0,0]' WHERE sentence_id=0",
        "UPDATE sentence_provenance SET division_ids_json='[99]' WHERE sentence_id=0",
        "UPDATE divisions SET end_sentence=2 WHERE division_id=0",
        "UPDATE divisions SET document_id=12 WHERE division_id=0",
        "UPDATE divisions SET source_id='' WHERE division_id=0",
        "UPDATE divisions SET parent_id=99 WHERE division_id=0",
        "UPDATE divisions SET kind='bad kind' WHERE division_id=0",
        "UPDATE divisions SET metadata_json='[]' WHERE division_id=0",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("provenance.sqlite");
        drop(finished_two_sentence_view(&path).await);
        let mut database = connection(&path).await;
        sqlx::query("INSERT INTO divisions VALUES(0,11,'section','authored',NULL,0,1,'{\"role\":\"methods\"}')").execute(&mut database).await.unwrap();
        sqlx::query(
            "INSERT INTO division_observations VALUES(11,'section','{\"state\":\"observed\"}')",
        )
        .execute(&mut database)
        .await
        .unwrap();
        sqlx::query("UPDATE sentence_provenance SET division_ids_json='[0]' WHERE sentence_id=0")
            .execute(&mut database)
            .await
            .unwrap();
        database.close().await.unwrap();
        drop(
            ProvenanceView::open(&path, &analysis_identity())
                .await
                .unwrap(),
        );
        let mut database = connection(&path).await;
        sqlx::query("PRAGMA foreign_keys=OFF")
            .execute(&mut database)
            .await
            .unwrap();
        sqlx::query(mutation).execute(&mut database).await.unwrap();
        database.close().await.unwrap();
        assert!(
            ProvenanceView::open(&path, &analysis_identity())
                .await
                .is_err(),
            "{mutation}"
        );
    }
}

#[tokio::test]
async fn metadata_pages_bound_bytes_as_well_as_row_counts() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("provenance.sqlite");
    let metadata = (0..10)
        .map(|key| {
            (
                format!("field-{key}"),
                serde_json::Value::String("x".repeat(60000)),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let json = serde_json::Value::Object(metadata).to_string();
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    for sentence in 0..10 {
        writer
            .push_sentence(
                &location(sentence, "large.jsonl", sentence, 0),
                &RichSentenceMetadata {
                    metadata_json: Some(json.clone()),
                    sentence_metadata_json: Some(json.clone()),
                    ..rich()
                },
            )
            .await
            .unwrap();
    }
    writer.finish(analysis_identity()).await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    let documents = view.document_metadata_page(0).await.unwrap();
    assert_eq!(documents.len(), 6);
    assert!(
        documents
            .iter()
            .map(|row| row.metadata_json.as_ref().unwrap().len())
            .sum::<usize>()
            <= 4 * 1024 * 1024
    );
    assert_eq!(view.document_metadata_page(6).await.unwrap().len(), 4);
    assert_eq!(view.sentence_metadata_page(0).await.unwrap().len(), 6);
    assert_eq!(view.scope_metadata_page(0, false).await.unwrap().len(), 6);
    let error = view
        .rich_sentence_page(&vec![SentenceId::new(0); 100])
        .await
        .unwrap_err();
    assert!(error.to_string().contains("byte budget"), "{error}");
}

#[tokio::test]
async fn derived_labels_participate_in_recorded_row_byte_bounds() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("labels.sqlite");
    let labels = (0..512)
        .map(|index| format!("label-{index}-{}", "x".repeat(1024)))
        .collect::<Box<[_]>>();
    let metadata = RichSentenceMetadata {
        region_labels: labels,
        ..rich()
    };
    let mut writer = ProvenanceWriter::create(&path, &[]).await.unwrap();
    for ordinal in 0..8 {
        writer
            .push_sentence(&location(ordinal, "labels.md", 0, ordinal), &metadata)
            .await
            .unwrap();
        let retained = writer
            .batch
            .iter()
            .map(|row| row.region_labels_json.len())
            .sum::<usize>();
        assert!(retained < 1024 * 1024, "retained label bytes: {retained}");
    }
    writer.finish(analysis_identity()).await.unwrap();
    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .unwrap();
    let page = view
        .rich_sentence_page(&[SentenceId::new(0)])
        .await
        .unwrap();
    assert_eq!(page[0].as_ref().unwrap().active_regions.len(), 512);
    assert!(
        view.rich_sentence_page(&vec![SentenceId::new(0); 100])
            .await
            .unwrap_err()
            .to_string()
            .contains("byte budget")
    );
}
