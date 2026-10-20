//! Explicit larger campaigns remain runnable after writer internalization:
//! `cargo test -p lingrep-corpus --lib --release -- provenance::characterization_tests::provenance_shared_api_representative_characterization --ignored --exact --nocapture`

use std::{collections::BTreeMap, fs, sync::Arc, time::Instant};

use super::{ProvenanceView, ProvenanceWriter};
use crate::{
    AnalysisIdentity, DocumentId, MAX_PROVENANCE_PAGE_SIZE, RichSentenceMetadata, SentenceId,
    SentenceLocation,
};

#[derive(Clone, Copy)]
struct Scale {
    name: &'static str,
    documents: u64,
    sentences: u64,
    region_occurrences: u64,
}

const SMOKE: Scale = Scale {
    name: "smoke",
    documents: 100,
    sentences: 1_000,
    region_occurrences: 2_000,
};

const REPRESENTATIVE: Scale = Scale {
    name: "representative",
    documents: 1_551,
    sentences: 400_000,
    region_occurrences: 1_000_000,
};

const STRESS: Scale = Scale {
    name: "stress",
    documents: 1_000_000,
    sentences: 5_000_000,
    region_occurrences: 10_000_000,
};

fn analysis_identity() -> AnalysisIdentity {
    AnalysisIdentity::from_bytes([0x5a; 32])
}

fn document_for(scale: Scale, sentence: u64) -> u64 {
    sentence.saturating_mul(scale.documents) / scale.sentences
}

fn occurrence_count(scale: Scale, sentence: u64) -> u64 {
    let base = scale.region_occurrences / scale.sentences;
    base + u64::from(sentence < scale.region_occurrences % scale.sentences)
}

fn rich(scale: Scale, sentence: u64) -> RichSentenceMetadata {
    let document = document_for(scale, sentence);
    let labels = (0..occurrence_count(scale, sentence))
        .map(|offset| format!("region-{:02}", (sentence + offset) % 64))
        .collect::<Vec<_>>()
        .into_boxed_slice();
    RichSentenceMetadata {
        structure: Default::default(),
        region_labels: labels,
        metadata_json: Some(format!(r#"{{"document":{document}}}"#)),
        sentence_metadata_json: None,
    }
}

fn location(scale: Scale, sentence: u64) -> SentenceLocation {
    let document = document_for(scale, sentence);
    let first = document.saturating_mul(scale.sentences) / scale.documents;
    SentenceLocation {
        source_metadata: None,
        corpus_sentence_ordinal: sentence,
        document_id: DocumentId::new(document.saturating_mul(17).saturating_add(11)),
        document_ordinal: document,
        document_sentence_ordinal: sentence - first,
        source_name: Arc::from(format!("document-{document}.md")),
    }
}

fn percentile(mut values: Vec<u128>, percentile: usize) -> u128 {
    values.sort_unstable();
    values[(values.len() * percentile / 100).min(values.len() - 1)]
}

async fn point_page(view: &ProvenanceView, sentences: &[u64]) -> u128 {
    let started = Instant::now();
    for &sentence in sentences {
        std::hint::black_box(
            view.rich_sentence(SentenceId::new(sentence))
                .await
                .expect("read point page"),
        );
    }
    started.elapsed().as_micros()
}

async fn batch_page(view: &ProvenanceView, sentences: &[SentenceId]) -> u128 {
    let started = Instant::now();
    let page = view
        .rich_sentence_page(sentences)
        .await
        .expect("read bounded page");
    assert!(page.iter().all(Option::is_some));
    std::hint::black_box(page);
    started.elapsed().as_micros()
}

async fn characterize(scale: Scale) {
    let directory = tempfile::tempdir().expect("create characterization directory");
    let path = directory.path().join(format!("{}.sqlite", scale.name));

    let ingest_started = Instant::now();
    let mut writer = ProvenanceWriter::create(&path, &[])
        .await
        .expect("create shared provenance writer");
    let mut generated_occurrences = 0_u64;
    for sentence in 0..scale.sentences {
        let metadata = rich(scale, sentence);
        generated_occurrences += metadata.region_labels.len() as u64;
        writer
            .push_sentence(&location(scale, sentence), &metadata)
            .await
            .expect("stream sentence provenance");
    }
    writer
        .finish(analysis_identity())
        .await
        .expect("finish shared provenance");
    let ingest_ms = ingest_started.elapsed().as_millis();
    assert_eq!(generated_occurrences, scale.region_occurrences);

    let view = ProvenanceView::open(&path, &analysis_identity())
        .await
        .expect("open immutable provenance view");
    assert_eq!(view.sentence_count(), scale.sentences);
    for sentence in [0, scale.sentences / 2, scale.sentences - 1] {
        assert_eq!(
            view.rich_sentence(SentenceId::new(sentence))
                .await
                .expect("read sample"),
            rich(scale, sentence)
        );
    }

    let page_ids = (0..500_u64.min(scale.sentences))
        .map(|offset| offset.saturating_mul(scale.sentences) / 500_u64.min(scale.sentences))
        .collect::<Vec<_>>();
    let batch_page_ids = page_ids
        .iter()
        .copied()
        .map(SentenceId::new)
        .collect::<Vec<_>>();
    let mut point_page_us = Vec::with_capacity(15);
    let mut batch_page_us = Vec::with_capacity(15);
    for iteration in 0..15 {
        if iteration % 2 == 0 {
            point_page_us.push(point_page(&view, &page_ids).await);
            batch_page_us.push(batch_page(&view, &batch_page_ids).await);
        } else {
            batch_page_us.push(batch_page(&view, &batch_page_ids).await);
            point_page_us.push(point_page(&view, &page_ids).await);
        }
    }

    let aggregate_started = Instant::now();
    let mut coverage = BTreeMap::<String, u64>::new();
    let mut aggregate_occurrences = 0_u64;
    for sentence in 0..scale.sentences {
        let metadata = view
            .rich_sentence(SentenceId::new(sentence))
            .await
            .expect("scan aggregate row");
        for label in &metadata.region_labels {
            *coverage.entry(label.to_owned()).or_default() += 1;
            aggregate_occurrences += 1;
        }
    }
    let point_aggregate_ms = aggregate_started.elapsed().as_millis();
    assert_eq!(aggregate_occurrences, scale.region_occurrences);
    assert!(!coverage.is_empty());

    let mut batch_aggregate_us = Vec::with_capacity(15);
    for _ in 0..15 {
        let started = Instant::now();
        let section_coverage = view
            .section_coverage(64)
            .await
            .expect("read bounded section coverage");
        assert_eq!(
            section_coverage
                .iter()
                .map(|section| section.coverage)
                .sum::<u64>(),
            scale.region_occurrences
        );
        std::hint::black_box(section_coverage);
        batch_aggregate_us.push(started.elapsed().as_micros());
    }

    let export_rows = scale.sentences.min(100_000);
    let point_export_started = Instant::now();
    let mut exported = 0_u64;
    for sentence in 0..export_rows {
        std::hint::black_box(
            view.rich_sentence(SentenceId::new(sentence))
                .await
                .expect("stream export row"),
        );
        exported += 1;
    }
    let point_export_ms = point_export_started.elapsed().as_millis().max(1);
    assert_eq!(exported, export_rows);

    let batch_export_started = Instant::now();
    let mut batch_exported = 0_u64;
    for first in (0..export_rows).step_by(MAX_PROVENANCE_PAGE_SIZE) {
        let end = (first + MAX_PROVENANCE_PAGE_SIZE as u64).min(export_rows);
        let ids = (first..end).map(SentenceId::new).collect::<Vec<_>>();
        let rows = view
            .rich_sentence_page(&ids)
            .await
            .expect("stream batched export page");
        assert!(rows.iter().all(Option::is_some));
        batch_exported += rows.len() as u64;
        std::hint::black_box(rows);
    }
    let batch_export_ms = batch_export_started.elapsed().as_millis().max(1);
    assert_eq!(batch_exported, export_rows);
    let batch_export_queries = export_rows.div_ceil(MAX_PROVENANCE_PAGE_SIZE as u64);

    let database_bytes = fs::metadata(&path).expect("stat shared provenance").len();
    eprintln!(
        "phase=comparison scale={} documents={} sentences={} region_occurrences={} database_bytes={} ingest_ms={} page_rows={} point_page_queries={} point_page_p50_us={} point_page_p95_us={} batch_page_queries=1 batch_page_p50_us={} batch_page_p95_us={} point_aggregate_queries={} point_aggregate_ms={} batch_aggregate_queries=1 batch_aggregate_p50_us={} batch_aggregate_p95_us={} export_rows={} point_export_queries={} point_export_ms={} point_export_rows_per_s={} batch_export_queries={} batch_export_ms={} batch_export_rows_per_s={}",
        scale.name,
        scale.documents,
        scale.sentences,
        scale.region_occurrences,
        database_bytes,
        ingest_ms,
        page_ids.len(),
        page_ids.len(),
        percentile(point_page_us.clone(), 50),
        percentile(point_page_us, 95),
        percentile(batch_page_us.clone(), 50),
        percentile(batch_page_us, 95),
        scale.sentences,
        point_aggregate_ms,
        percentile(batch_aggregate_us.clone(), 50),
        percentile(batch_aggregate_us, 95),
        export_rows,
        export_rows,
        point_export_ms,
        u128::from(exported) * 1_000 / point_export_ms,
        batch_export_queries,
        batch_export_ms,
        u128::from(batch_exported) * 1_000 / batch_export_ms,
    );
}

#[tokio::test]
async fn provenance_shared_api_smoke_characterization() {
    characterize(SMOKE).await;
}

#[tokio::test]
#[ignore = "explicit 400k-sentence / 1m-region-occurrence characterization"]
async fn provenance_shared_api_representative_characterization() {
    characterize(REPRESENTATIVE).await;
}

#[tokio::test]
#[ignore = "explicit 5m-sentence / 10m-region-occurrence capacity characterization"]
async fn provenance_shared_api_stress_characterization() {
    characterize(STRESS).await;
}
