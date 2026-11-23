#![allow(clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use hegel::generators;
use lingrep_core::execution::CancellationFlag;
use lingrep_core::spill::{SpillPolicy, TempBudget};
use lingrep_corpus::{
    CountTable, CountTableError, CountTableKey, CountTableWriter, DocumentMask, DocumentTotals,
    FieldValue, Group, GroupKind, GroupSchema, KeyEquals, KeyFieldError, KeySubset, PatternTotal,
    TopRow, ValueSlots,
};

fn key() -> CountTableKey {
    CountTableKey {
        analysis_identity: "analysis".into(),
        catalog_fingerprint: "catalog".into(),
        matcher_revision: "matcher".into(),
    }
}

fn surface() -> GroupSchema {
    GroupSchema {
        kind: GroupKind::Surface,
        names: Vec::new(),
    }
}

type Pairs = Vec<Vec<Vec<(u32, u64)>>>;

fn sentences_of(document: u32, count: u64) -> Vec<(u64, u64)> {
    let spread = count.min(3);
    (0..spread)
        .map(|index| {
            (
                1000 * u64::from(document) + index,
                if index + 1 == spread {
                    count - index
                } else {
                    1
                },
            )
        })
        .collect()
}

fn writer_for(path: &Path, documents: &[DocumentTotals], pairs: &Pairs) -> CountTableWriter {
    let widest = pairs.iter().map(Vec::len).max().unwrap_or(0);
    let values = (0..widest)
        .map(|value| format!("v{value:06}"))
        .collect::<Vec<_>>();
    let mut writer = CountTableWriter::create(
        path,
        key(),
        (0..pairs.len())
            .map(|pattern| format!("p{pattern}"))
            .collect(),
        vec![surface()],
        &values,
        documents,
    )
    .expect("create");
    for (pattern, entries) in pairs.iter().enumerate() {
        for (entry, entry_pairs) in entries.iter().enumerate() {
            writer
                .begin_entry(pattern, 0, &[entry as u32])
                .expect("begin entry");
            for (document, count) in entry_pairs {
                for (sentence, count) in sentences_of(*document, *count) {
                    writer
                        .add_sentence(*document, sentence, count)
                        .expect("add sentence");
                }
            }
        }
    }
    writer
}

fn publish(writer: CountTableWriter) {
    writer
        .finish(&CancellationFlag::new())
        .expect("finish")
        .publish()
        .expect("publish");
}

fn write_table(path: &Path, documents: &[DocumentTotals], pairs: &Pairs) -> CountTable {
    publish(writer_for(path, documents, pairs));
    CountTable::open(path, &key()).expect("open")
}

fn streamed(
    table: &CountTable,
    mask: &DocumentMask,
    policy: SpillPolicy,
    budget: Arc<TempBudget>,
) -> Result<Vec<Vec<TopRow>>, CountTableError> {
    let mut patterns = vec![Vec::new(); table.pattern_names().len()];
    for row in table.rows(mask, policy, budget)? {
        let (pattern, row) = row?;
        patterns[pattern].push(row);
    }
    Ok(patterns)
}

fn spill_policy(max_pending_keys: usize) -> SpillPolicy {
    SpillPolicy {
        max_pending_bytes: usize::MAX,
        max_pending_keys,
        merge_fan_in: 2,
    }
}

fn expected_rows(
    pairs: &Pairs,
    admitted: &[bool],
    limit: Option<usize>,
) -> Vec<Vec<(String, u64, u64)>> {
    pairs
        .iter()
        .map(|entries| {
            let mut rows = entries
                .iter()
                .enumerate()
                .map(|(entry, entry_pairs)| {
                    let admitted = entry_pairs
                        .iter()
                        .filter(|(document, _)| admitted[*document as usize]);
                    let (count, works) =
                        admitted.fold((0, 0), |(count, works), (_, n)| (count + n, works + 1));
                    (entry, count, works)
                })
                .filter(|(_, count, _)| *count > 0)
                .collect::<Vec<_>>();
            rows.sort_by(|left, right| right.1.cmp(&left.1).then(left.0.cmp(&right.0)));
            rows.truncate(limit.unwrap_or(usize::MAX));
            rows.into_iter()
                .map(|(entry, count, works)| (format!("v{entry:06}"), count, works))
                .collect()
        })
        .collect()
}

fn expected_totals(pairs: &Pairs, admitted: &[bool]) -> Vec<PatternTotal> {
    pairs
        .iter()
        .enumerate()
        .map(|(pattern, entries)| {
            let admitted_pairs = entries
                .iter()
                .flatten()
                .filter(|(document, _)| admitted[*document as usize]);
            let mut documents = admitted_pairs
                .clone()
                .map(|(document, _)| *document)
                .collect::<Vec<_>>();
            documents.sort_unstable();
            documents.dedup();
            PatternTotal {
                pattern,
                occurrences: admitted_pairs.map(|(_, count)| count).sum(),
                works: documents.len() as u64,
            }
        })
        .collect()
}

fn surfaces(table: &CountTable, rows: &[TopRow]) -> Vec<(String, u64, u64)> {
    rows.iter()
        .map(|row| match table.group(row.entry).expect("group") {
            Group::Surface(value) => (value.to_owned(), row.count, row.works),
            other => panic!("unexpected group {other:?}"),
        })
        .collect()
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn top_and_rows_equal_a_direct_count_under_every_strategy(tc: hegel::TestCase) {
    let document_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(6));
    let pattern_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(3));
    let mut pairs: Pairs = Vec::new();
    for _ in 0..pattern_count {
        let entry_count = tc.draw(generators::integers::<usize>().max_value(300));
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let present = tc.draw(
                generators::integers::<u32>()
                    .min_value(1)
                    .max_value((1 << document_count) - 1),
            );
            entries.push(
                (0..document_count as u32)
                    .filter(|document| present & (1 << document) != 0)
                    .map(|document| {
                        let count =
                            tc.draw(generators::integers::<u64>().min_value(1).max_value(3));
                        (document, count)
                    })
                    .collect(),
            );
        }
        pairs.push(entries);
    }
    let admitted = tc.draw(
        generators::vecs(generators::booleans())
            .min_size(document_count)
            .max_size(document_count),
    );
    let limit = match tc.draw(generators::integers::<u8>().max_value(3)) {
        0 => None,
        1 => Some(tc.draw(generators::integers::<usize>().min_value(1).max_value(5))),
        2 => Some(
            tc.draw(
                generators::integers::<usize>()
                    .min_value(301)
                    .max_value(1000),
            ),
        ),
        _ => Some(usize::MAX),
    };
    let resident_rows = tc.draw(generators::integers::<usize>().min_value(1).max_value(64));
    let documents = (0..document_count as u64)
        .map(|document| DocumentTotals {
            document_id: 10 + 2 * document,
            sentences: document + 1,
            tokens: 7 * (document + 1),
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(&directory.path().join("t.lingtable"), &documents, &pairs);
    table.verify().expect("verify");

    let ids = documents
        .iter()
        .zip(&admitted)
        .filter(|(_, admitted)| **admitted)
        .map(|(document, _)| document.document_id)
        .collect::<Vec<_>>();
    let mask = table.mask(|id| ids.contains(&id));
    let expected = expected_rows(&pairs, &admitted, limit);
    let admitted_documents = documents.iter().zip(&admitted).filter(|(_, a)| **a);
    let expected_admission = lingrep_corpus::Admission {
        documents_admitted: ids.len() as u64,
        sentences_admitted: admitted_documents.clone().map(|(d, _)| d.sentences).sum(),
        tokens_admitted: admitted_documents.map(|(d, _)| d.tokens).sum(),
    };
    assert_eq!(
        table.pattern_totals(&mask).expect("pattern totals"),
        expected_totals(&pairs, &admitted)
    );
    let every_row = (0..pairs.len())
        .map(|pattern| {
            surfaces(
                &table,
                &table.pattern_entries(pattern, &mask).expect("rows"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(every_row, expected_rows(&pairs, &admitted, None));
    for (pattern, expected) in every_row.iter().enumerate() {
        let rows = table
            .ranked_pattern_rows(
                pattern,
                &mask,
                spill_policy(resident_rows),
                Arc::new(TempBudget::new(u64::MAX)),
                &CancellationFlag::new(),
            )
            .expect("ranked rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        assert_eq!(surfaces(&table, &rows), *expected);
        if let Some(row) = rows.first() {
            assert!(matches!(
                table.bounded_group(row.entry, 0),
                Err(lingrep_corpus::CountTableError::GroupLimit(0))
            ));
            assert_eq!(
                table
                    .bounded_group(row.entry, usize::MAX)
                    .expect("bounded group"),
                table.group(row.entry).expect("group")
            );
            assert_eq!(
                table
                    .find_entry(pattern, &table.group(row.entry).expect("group"))
                    .expect("find entry"),
                Some(row.entry)
            );
        }
    }
    for (entry, entry_pairs) in pairs.iter().flatten().enumerate() {
        let (documents, counts) = table.entry_documents(entry as u32).expect("pairs");
        assert_eq!(
            documents
                .iter()
                .zip(counts)
                .map(|(document, count)| (*document, u64::from(*count)))
                .collect::<Vec<_>>(),
            *entry_pairs
        );
        let (sentences, sentence_counts) = table.entry_sentences(entry as u32).expect("sentences");
        assert_eq!(
            sentences
                .iter()
                .zip(sentence_counts)
                .map(|(sentence, count)| (u64::from(*sentence), u64::from(*count)))
                .collect::<Vec<_>>(),
            entry_pairs
                .iter()
                .flat_map(|(document, count)| sentences_of(*document, *count))
                .collect::<Vec<_>>()
        );
    }
    for (pattern, entries) in pairs.iter().enumerate() {
        let mut expected = vec![0u64; document_count];
        for (document, count) in entries.iter().flatten() {
            expected[*document as usize] += count;
        }
        assert_eq!(
            table.pattern_document_counts(pattern).expect("counts"),
            expected,
            "pattern {pattern}"
        );
        assert_eq!(
            table
                .pattern_document_counts_iter(pattern)
                .expect("counts iterator")
                .collect::<Result<Vec<_>, _>>()
                .expect("counts"),
            expected
        );
    }
    let Some(limit) = limit else {
        let budget = Arc::new(TempBudget::new(u64::MAX));
        let rows = table
            .rows(&mask, spill_policy(resident_rows), Arc::clone(&budget))
            .expect("rows");
        assert_eq!(rows.admission(), expected_admission);
        let actual = streamed(&table, &mask, spill_policy(resident_rows), budget)
            .expect("rows")
            .iter()
            .map(|rows| surfaces(&table, rows))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected);
        return;
    };
    for crossover in [0, u64::MAX] {
        let result = table
            .top_with_crossover(&mask, limit, crossover)
            .expect("top");
        let actual = result
            .patterns
            .iter()
            .map(|pattern| surfaces(&table, &pattern.rows))
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "crossover {crossover}");
        assert_eq!(
            lingrep_corpus::Admission {
                documents_admitted: result.documents_admitted,
                sentences_admitted: result.sentences_admitted,
                tokens_admitted: result.tokens_admitted,
            },
            expected_admission
        );
    }
}

#[test]
fn rows_are_ordered_through_temporary_runs_charged_to_the_budget() {
    let documents = [DocumentTotals {
        document_id: 0,
        sentences: 1,
        tokens: 1,
    }];
    let entries = (0..200u64).map(|entry| vec![(0, entry % 7 + 1)]).collect();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![entries],
    );
    let mask = table.mask(|_| true);

    let budget = Arc::new(TempBudget::new(u64::MAX));
    let rows = streamed(&table, &mask, spill_policy(8), Arc::clone(&budget)).expect("rows");
    assert_eq!(rows[0].len(), 200);
    assert!(
        budget.peak_used_bytes() > 0,
        "200 rows with 8 resident are ordered on disk"
    );

    let error = streamed(&table, &mask, spill_policy(8), Arc::new(TempBudget::new(1)))
        .expect_err("a one-byte budget cannot hold a run");
    assert!(
        matches!(&error, CountTableError::Io(error) if error.kind() == std::io::ErrorKind::StorageFull),
        "{error:?}"
    );
}

#[test]
fn top_accepts_the_largest_limit_on_a_one_entry_table_under_either_strategy() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    one_entry_table(&path);
    let table = CountTable::open(&path, &key()).expect("open");
    let mask = table.mask(|_| true);
    for crossover in [0, u64::MAX] {
        let result = table
            .top_with_crossover(&mask, usize::MAX, crossover)
            .expect("top");
        assert_eq!(
            surfaces(&table, &result.patterns[0].rows),
            [("v000000".to_owned(), 1, 1)],
            "crossover {crossover}"
        );
    }
}

#[test]
fn verify_rejects_document_offsets_that_move_a_pair_into_another_document() {
    let documents = [
        DocumentTotals {
            document_id: 0,
            sentences: 1,
            tokens: 1,
        },
        DocumentTotals {
            document_id: 1,
            sentences: 1,
            tokens: 1,
        },
    ];
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    let table = write_table(&path, &documents, &vec![vec![vec![(0, 1), (1, 1)]]]);
    table.verify().expect("verify");
    drop(table);
    let offsets = [0u64, 0, 2]
        .iter()
        .flat_map(|offset| offset.to_le_bytes())
        .collect::<Vec<_>>();
    std::fs::write(path.join("document_pair_offsets.u64"), offsets).expect("rewrite offsets");
    let table = CountTable::open(&path, &key()).expect("open");
    assert!(matches!(table.verify(), Err(CountTableError::Corrupt(_))));
}

#[test]
fn verify_rejects_document_pattern_totals_that_differ_from_the_pairs() {
    let documents = [DocumentTotals {
        document_id: 0,
        sentences: 1,
        tokens: 1,
    }];
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    let table = write_table(&path, &documents, &vec![vec![vec![(0, 2)], vec![(0, 3)]]]);
    table.verify().expect("verify");
    drop(table);
    std::fs::write(path.join("document_pattern_counts.u64"), 4u64.to_le_bytes())
        .expect("rewrite totals");
    let table = CountTable::open(&path, &key()).expect("open");
    assert!(matches!(table.verify(), Err(CountTableError::Corrupt(_))));
}

fn is_corrupt<T>(result: Result<T, CountTableError>) -> bool {
    matches!(result, Err(CountTableError::Corrupt(_)))
}

#[test]
fn document_totals_whose_sum_exceeds_u64_are_corruption_errors() {
    for (sentences, tokens) in [(u64::MAX, 1), (1, u64::MAX)] {
        let documents = (0..2)
            .map(|document_id| DocumentTotals {
                document_id,
                sentences,
                tokens,
            })
            .collect::<Vec<_>>();
        let directory = tempfile::tempdir().expect("tempdir");
        let table = write_table(
            &directory.path().join("t.lingtable"),
            &documents,
            &vec![vec![vec![(0, 1), (1, 1)]]],
        );
        let all = table.mask(|_| true);
        let first = table.mask(|id| id == 0);
        let budget = || Arc::new(TempBudget::new(u64::MAX));
        let slots =
            ValueSlots::number([vec!["a".to_owned()], vec!["a".to_owned()]]).expect("slots");
        assert!(is_corrupt(table.verify()));
        assert!(is_corrupt(table.top(&all, 10)));
        assert!(is_corrupt(table.top_with_crossover(&all, 10, 0)));
        assert!(is_corrupt(table.rows(&all, spill_policy(4), budget())));
        assert!(is_corrupt(table.ranked_pattern_rows(
            0,
            &all,
            spill_policy(4),
            budget(),
            &CancellationFlag::new(),
        )));
        assert!(is_corrupt(table.scopes([&all, &first])));
        assert!(is_corrupt(table.scopes([&first, &all])));
        assert!(is_corrupt(table.value_counts(
            &slots,
            &all,
            [(0, 1), (1, 1)]
        )));
        table.top(&first, 10).expect("one document's totals fit");
    }
}

#[test]
fn document_pattern_counts_whose_sum_exceeds_u64_are_corruption_errors() {
    let documents = (0..2)
        .map(|document_id| DocumentTotals {
            document_id,
            sentences: 1,
            tokens: 1,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    write_table(&path, &documents, &vec![vec![vec![(0, 1), (1, 1)]]]);
    let counts = [u64::MAX, 1]
        .iter()
        .flat_map(|count| count.to_le_bytes())
        .collect::<Vec<_>>();
    std::fs::write(path.join("document_pattern_counts.u64"), counts).expect("rewrite totals");
    let table = CountTable::open(&path, &key()).expect("open");
    let all = table.mask(|_| true);
    let slots = ValueSlots::number([vec!["a".to_owned()], vec!["a".to_owned()]]).expect("slots");
    assert!(is_corrupt(table.verify()));
    assert!(is_corrupt(table.pattern_totals(&all)));
    let per_document = table
        .pattern_document_counts(0)
        .expect("per-document counts");
    assert_eq!(per_document, [u64::MAX, 1]);
    assert!(is_corrupt(table.value_counts(
        &slots,
        &all,
        per_document.into_iter().enumerate(),
    )));
}

#[test]
fn a_raised_cancellation_or_an_unpublished_table_leaves_no_directory() {
    let documents = [DocumentTotals {
        document_id: 0,
        sentences: 1,
        tokens: 1,
    }];
    let pairs = vec![vec![vec![(0, 1)]]];
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    let left = || {
        std::fs::read_dir(directory.path())
            .expect("read dir")
            .count()
    };

    let cancellation = CancellationFlag::new();
    cancellation.cancel();
    assert!(matches!(
        writer_for(&path, &documents, &pairs).finish(&cancellation),
        Err(CountTableError::Cancelled)
    ));
    assert_eq!(
        left(),
        0,
        "a cancelled finish removes its staging directory"
    );

    let staged = writer_for(&path, &documents, &pairs)
        .finish(&CancellationFlag::new())
        .expect("finish");
    assert_eq!(
        left(),
        2,
        "a staged table and its lock file are beside its destination"
    );
    assert!(!path.exists());
    drop(staged);
    assert_eq!(left(), 0, "an unpublished staged table is removed");
}

fn one_entry_table(path: &Path) {
    let documents = [DocumentTotals {
        document_id: 0,
        sentences: 1,
        tokens: 1,
    }];
    write_table(path, &documents, &vec![vec![vec![(0, 1)]]]);
}

#[test]
fn open_rejects_a_table_whose_key_differs_in_any_field() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    one_entry_table(&path);
    let cases: [(&str, fn(&mut CountTableKey)); 3] = [
        ("analysis identity", |key| key.analysis_identity.push('x')),
        ("catalog fingerprint", |key| {
            key.catalog_fingerprint.push('x')
        }),
        ("matcher revision", |key| key.matcher_revision.push('x')),
    ];
    for (field, change) in cases {
        let mut current = key();
        change(&mut current);
        match CountTable::open(&path, &current) {
            Err(CountTableError::KeyMismatch {
                field: rejected, ..
            }) => assert_eq!(rejected, field),
            other => panic!("{field}: expected a key mismatch, got {:?}", other.err()),
        }
    }
}

/// The component set removal deletes is every file the writer publishes, so
/// `CountTable::remove` deletes a published table completely.
#[test]
fn a_published_table_holds_only_its_components_and_removal_deletes_them_all() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    one_entry_table(&path);
    assert_eq!(
        CountTable::foreign_entries(&path).expect("listed"),
        Vec::<String>::new()
    );
    CountTable::remove(&path, &key().analysis_identity).expect("removed");
    assert_eq!(
        std::fs::read_dir(directory.path())
            .expect("listing")
            .count(),
        0
    );
}

/// A table renamed by an interrupted `corpus prune` is not served.
#[test]
fn open_rejects_a_table_under_a_prune_leftover_name() {
    let directory = tempfile::tempdir().expect("tempdir");
    let published = directory.path().join("t.lingtable");
    one_entry_table(&published);
    let path = directory.path().join(".t.lingtable.pruning-1-0");
    std::fs::rename(&published, &path).expect("renamed");
    assert!(matches!(
        CountTable::open(&path, &key()),
        Err(CountTableError::PruneLeftover(rejected)) if rejected == path
    ));
}

#[test]
fn read_key_returns_the_key_a_table_was_written_for() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    one_entry_table(&path);
    assert_eq!(CountTable::read_key(&path).expect("key"), key());
    assert!(matches!(
        CountTable::read_key(&directory.path().join("absent")),
        Err(CountTableError::Io(_))
    ));
}

#[test]
fn open_rejects_another_format_version() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    one_entry_table(&path);
    let header = std::fs::read_to_string(path.join("header.json")).expect("header");
    std::fs::write(
        path.join("header.json"),
        header.replace("\"version\": 4", "\"version\": 3"),
    )
    .expect("rewrite header");
    assert!(matches!(
        CountTable::read_key(&path),
        Err(CountTableError::UnsupportedFormat { version: 3, .. })
    ));
    let error = CountTable::open(&path, &key()).err();
    assert!(
        matches!(
            error,
            Some(CountTableError::UnsupportedFormat { version: 3, .. })
        ),
        "{error:?}"
    );
    assert!(
        error
            .expect("error")
            .to_string()
            .contains("rebuild it with `ling summarize --write-table`")
    );
}

#[test]
fn writer_rejects_input_out_of_order_and_publishes_nothing() {
    let documents = [
        DocumentTotals {
            document_id: 1,
            sentences: 1,
            tokens: 1,
        },
        DocumentTotals {
            document_id: 2,
            sentences: 1,
            tokens: 1,
        },
    ];
    type Step = fn(&mut CountTableWriter) -> Result<(), CountTableError>;
    let cases: [(&str, Step); 11] = [
        ("pattern goes back", |writer| {
            writer.begin_entry(1, 0, &[0])?;
            writer.add_sentence(0, 0, 1)?;
            writer.begin_entry(0, 0, &[0])
        }),
        ("sentence repeats", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(1, 5, 1)?;
            writer.add_sentence(1, 5, 1)
        }),
        ("document goes back", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(1, 5, 1)?;
            writer.add_sentence(0, 6, 1)
        }),
        ("document is undeclared", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(2, 0, 1)
        }),
        ("count is zero", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(0, 0, 0)
        }),
        ("sentence count is beyond 2^32-1", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(0, 0, 1 << 32)
        }),
        ("sentence is beyond 2^32-1", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(0, 1 << 32, 1)
        }),
        ("pair count is beyond 2^32-1", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(0, 0, u64::from(u32::MAX))?;
            writer.add_sentence(0, 1, 1)?;
            writer.begin_entry(0, 0, &[0])
        }),
        ("entry has no sentence", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.begin_entry(0, 0, &[0])
        }),
        ("entry is out of key order", |writer| {
            writer.begin_entry(0, 0, &[0])?;
            writer.add_sentence(0, 0, 1)?;
            writer.begin_entry(0, 0, &[0])
        }),
        ("key rank is outside the vocabulary", |writer| {
            writer.begin_entry(0, 0, &[1])
        }),
    ];
    for (case, step) in cases {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("t.lingtable");
        let mut writer = CountTableWriter::create(
            &path,
            key(),
            vec!["a".into(), "b".into()],
            vec![surface()],
            &["only"],
            &documents,
        )
        .expect("create");
        assert!(
            matches!(step(&mut writer), Err(CountTableError::Writer(_))),
            "{case}"
        );
        drop(writer);
        let left = std::fs::read_dir(directory.path())
            .expect("read dir")
            .count();
        assert_eq!(left, 0, "{case}: the staging directory remains");
    }
    let directory = tempfile::tempdir().expect("tempdir");
    assert!(matches!(
        CountTableWriter::create(
            &directory.path().join("t.lingtable"),
            key(),
            vec!["a".into()],
            vec![surface()],
            &["b", "a"],
            &documents,
        ),
        Err(CountTableError::Writer(_))
    ));
}

#[test]
fn writer_does_not_replace_an_existing_destination() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    CountTableWriter::check_destination(&path).expect("preflight");
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    std::fs::create_dir(&path).expect("occupy destination");
    assert!(matches!(
        CountTableWriter::create(&path, key(), Vec::new(), Vec::new(), &[] as &[&str], &[]),
        Err(CountTableError::DestinationExists(_))
    ));
}

#[test]
fn keys_of_every_kind_decode_to_their_values_and_patterns_without_entries_stay_empty() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    let schemas = vec![
        GroupSchema {
            kind: GroupKind::Term,
            names: Vec::new(),
        },
        GroupSchema {
            kind: GroupKind::Key,
            names: vec!["text".into(), "upos".into()],
        },
        GroupSchema {
            kind: GroupKind::LexicalKey,
            names: vec!["lemma".into(), "parts".into()],
        },
    ];
    let documents = [DocumentTotals {
        document_id: 3,
        sentences: 2,
        tokens: 9,
    }];
    let mut writer = CountTableWriter::create(
        &path,
        key(),
        vec!["empty".into(), "keys".into(), "also-empty".into()],
        schemas,
        &["a", "b", "c"],
        &documents,
    )
    .expect("create");
    for (schema, words) in [(0, vec![2]), (1, vec![0, 1]), (2, vec![3, 0, 1, 0, 1])] {
        writer.begin_entry(1, schema, &words).expect("begin");
        writer
            .add_sentence(0, 0, 4 - u64::from(schema))
            .expect("sentence");
    }
    publish(writer);
    let table = CountTable::open(&path, &key()).expect("open");
    table.verify().expect("verify");
    assert_eq!(
        table.pattern_names().collect::<Vec<_>>(),
        ["empty", "keys", "also-empty"]
    );
    let result = table.top(&table.mask(|_| true), usize::MAX).expect("top");
    assert!(result.patterns[0].rows.is_empty() && result.patterns[2].rows.is_empty());
    let groups = result.patterns[1]
        .rows
        .iter()
        .map(|row| table.group(row.entry).expect("group"))
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        [
            Group::Term("c"),
            Group::Key(vec![
                ("text", FieldValue::Scalar("a")),
                ("upos", FieldValue::Scalar("b"))
            ]),
            Group::Key(vec![
                ("lemma", FieldValue::List(vec!["a", "b"])),
                ("parts", FieldValue::Scalar("b"))
            ]),
        ]
    );
    for (row, group) in result.patterns[1].rows.iter().zip(&groups) {
        assert_eq!(table.find_entry(1, group).expect("find"), Some(row.entry));
        assert_eq!(table.find_entry(0, group).expect("find"), None);
    }
    for absent in [
        Group::Term("a"),
        Group::Surface("c"),
        Group::Term("z"),
        Group::Key(vec![("text", FieldValue::Scalar("a"))]),
        Group::Key(vec![
            ("lemma", FieldValue::Scalar("a")),
            ("parts", FieldValue::Scalar("b")),
        ]),
    ] {
        assert_eq!(
            table.find_entry(1, &absent).expect("find"),
            None,
            "{absent:?}"
        );
    }
}

#[test]
fn a_lexical_key_of_scalars_is_found_by_the_ordinary_key_of_its_values() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    let names = vec!["lemma".to_owned()];
    let documents = [DocumentTotals {
        document_id: 1,
        sentences: 1,
        tokens: 1,
    }];
    let mut writer = CountTableWriter::create(
        &path,
        key(),
        vec!["p".into()],
        vec![
            GroupSchema {
                kind: GroupKind::Key,
                names: names.clone(),
            },
            GroupSchema {
                kind: GroupKind::LexicalKey,
                names,
            },
        ],
        &["a", "b", "c"],
        &documents,
    )
    .expect("create");
    for (schema, words) in [(0, vec![0]), (1, vec![0, 1]), (1, vec![2, 2])] {
        writer.begin_entry(0, schema, &words).expect("begin");
        writer.add_sentence(0, 0, 1).expect("sentence");
    }
    publish(writer);
    let table = CountTable::open(&path, &key()).expect("open");
    table.verify().expect("verify");
    for (entry, group) in [
        Group::Key(vec![("lemma", FieldValue::Scalar("a"))]),
        Group::Key(vec![("lemma", FieldValue::Scalar("b"))]),
        Group::Key(vec![("lemma", FieldValue::List(vec!["c"]))]),
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(
            table.find_entry(0, group).expect("find"),
            Some(entry as u32)
        );
    }
    assert_eq!(
        table
            .find_entry(0, &Group::Key(vec![("lemma", FieldValue::Scalar("c"))]))
            .expect("find"),
        None
    );
}

#[test]
fn ranked_scan_reads_past_the_floor_into_entries_whose_total_equals_it() {
    let documents = [
        DocumentTotals {
            document_id: 0,
            sentences: 1,
            tokens: 1,
        },
        DocumentTotals {
            document_id: 1,
            sentences: 1,
            tokens: 1,
        },
    ];
    let mut entries = vec![vec![(0, 2)]];
    entries.extend((0..64).map(|_| vec![(0, 2), (1, 1)]));
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![entries],
    );
    let mask = table.mask(|id| id == 0);
    let result = table.top_with_crossover(&mask, 1, 0).expect("top");
    assert_eq!(
        surfaces(&table, &result.patterns[0].rows),
        [("v000000".to_owned(), 2, 1)]
    );
}

#[test]
fn overflowing_header_dimensions_return_corruption_errors() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("table");
    write_table(&path, &[], &vec![]);
    let header_path = path.join("header.json");
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&header_path).expect("read header"))
            .expect("header JSON");
    for field in [
        "values",
        "entries",
        "documents",
        "key_words",
        "pairs",
        "document_patterns",
    ] {
        for dimension in [u64::MAX, u64::MAX / 4 + 1, u64::MAX / 8 + 1] {
            let mut header = original.clone();
            header[field] = dimension.into();
            std::fs::write(&header_path, serde_json::to_vec(&header).expect("JSON"))
                .expect("write header");
            assert!(
                matches!(
                    CountTable::open(&path, &key()),
                    Err(CountTableError::Corrupt(_))
                ),
                "{field} = {dimension}"
            );
        }
    }
    std::fs::write(&header_path, serde_json::to_vec(&original).expect("JSON"))
        .expect("restore header");
    CountTable::open(&path, &key()).expect("empty table");
}

fn relative(actual: f64, expected: f64) -> f64 {
    if expected == 0.0 {
        actual.abs()
    } else {
        ((actual - expected) / expected).abs()
    }
}

#[test]
#[allow(clippy::excessive_precision)]
fn contrast_statistics_equal_an_independent_reference() {
    // (a, N1, b, N2) and the reference G², min E, rates per million and log2
    // ratio, evaluated with 50-digit decimals from UCREL's formulas
    // (https://ucrel.lancs.ac.uk/llwizard.html). The first row checks by hand:
    // E1 = E2 = 15, G² = 2 (10 ln(10/15) + 20 ln(20/15)).
    let cases: [((u64, u64, u64, u64), f64, f64, f64, f64, Option<f64>); 7] = [
        (
            (10, 1000, 20, 1000),
            3.397980735907949,
            15.0,
            1.0e4,
            2.0e4,
            Some(-1.0),
        ),
        (
            (52, 50_000, 7, 120_000),
            89.17160904682299,
            17.35294117647059,
            1040.0,
            58.33333333333333,
            Some(4.156119201917282),
        ),
        (
            (0, 1000, 15, 3000),
            8.630462173553428,
            3.75,
            0.0,
            5000.0,
            None,
        ),
        (
            (15, 3000, 0, 1000),
            8.630462173553428,
            3.75,
            5000.0,
            0.0,
            None,
        ),
        (
            (1, 154_059_223, 1, 154_059_223),
            0.0,
            1.0,
            6.491010278560213e-3,
            6.491010278560213e-3,
            Some(0.0),
        ),
        (
            (123_456, 80_000_000, 4321, 74_059_223),
            130_371.1137199899,
            61_424.85437091293,
            1543.2,
            58.34519759949412,
            Some(4.725167325864036),
        ),
        (
            (3, 17, 9, 400),
            6.452383940038862,
            0.4892086330935252,
            176_470.5882352941,
            22_500.0,
            Some(2.971430847803229),
        ),
    ];
    for ((a, n1, b, n2), g2, min_expected, rate1, rate2, log2_ratio) in cases {
        let contrast = lingrep_corpus::Contrast::of([a, b], [n1, n2]);
        let case = (a, n1, b, n2);
        assert!(
            relative(contrast.log_likelihood.expect("G²"), g2) < 1e-12,
            "{case:?}: {contrast:?}"
        );
        assert!(
            relative(contrast.min_expected.expect("min E"), min_expected) < 1e-12,
            "{case:?}"
        );
        assert!(
            relative(contrast.rates_per_million[0].expect("rate"), rate1) < 1e-12,
            "{case:?}"
        );
        assert!(
            relative(contrast.rates_per_million[1].expect("rate"), rate2) < 1e-12,
            "{case:?}"
        );
        match (contrast.log2_ratio, log2_ratio) {
            (Some(actual), Some(expected)) => {
                assert!((actual - expected).abs() < 1e-12, "{case:?}")
            }
            (actual, expected) => assert_eq!(actual, expected, "{case:?}"),
        }
    }
}

#[test]
fn a_scope_without_tokens_yields_no_rate_score_or_direction() {
    use lingrep_corpus::Contrast;
    for (counts, tokens, rates) in [
        ([0, 4], [0, 100], [None, Some(40_000.0)]),
        ([4, 0], [100, 0], [Some(40_000.0), None]),
        ([0, 0], [0, 0], [None, None]),
    ] {
        assert_eq!(
            Contrast::of(counts, tokens),
            Contrast {
                rates_per_million: rates,
                log_likelihood: None,
                min_expected: None,
                log2_ratio: None,
                direction: None,
            }
        );
    }
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn contrast_is_symmetric_in_its_scopes_and_zero_at_equal_rates(tc: hegel::TestCase) {
    use lingrep_corpus::{Contrast, Direction};
    let count = || generators::integers::<u64>().max_value(1 << 40);
    let tokens = || {
        generators::integers::<u64>()
            .min_value(1)
            .max_value(1 << 40)
    };
    let (a, b) = (tc.draw(count()), tc.draw(count()));
    let (n1, n2) = (tc.draw(tokens()), tc.draw(tokens()));
    let forward = Contrast::of([a, b], [n1, n2]);
    let backward = Contrast::of([b, a], [n2, n1]);
    let g2 = forward.log_likelihood.expect("G²");
    assert!(g2 >= 0.0);
    assert!(relative(backward.log_likelihood.expect("G²"), g2) < 1e-9);
    assert_eq!(forward.log2_ratio.map(|ratio| -ratio), backward.log2_ratio);
    let flipped = |direction| match direction {
        Direction::First => Direction::Second,
        Direction::Second => Direction::First,
        Direction::Equal => Direction::Equal,
    };
    assert_eq!(forward.direction.map(flipped), backward.direction);
    let equal_rates = u128::from(a) * u128::from(n2) == u128::from(b) * u128::from(n1);
    assert_eq!(forward.direction == Some(Direction::Equal), equal_rates);
    if equal_rates {
        assert_eq!(g2, 0.0);
    }
}

#[hegel::test(test_cases = 200, derandomize = true)]
fn contrast_g2_scales_with_the_counts_and_tokens(tc: hegel::TestCase) {
    use lingrep_corpus::Contrast;
    let draw = |min| tc.draw(generators::integers::<u64>().min_value(min));
    let (a, b, n1, n2) = (draw(0), draw(0), draw(1), draw(1));
    let k = tc.draw(
        generators::integers::<u64>()
            .min_value(1)
            .max_value(u64::from(u32::MAX)),
    );
    // Draws whose scaled values do not fit a u64 are discarded.
    let scale = |value: u64| value.checked_mul(k);
    let (Some(ka), Some(kb), Some(kn1), Some(kn2)) = (scale(a), scale(b), scale(n1), scale(n2))
    else {
        tc.reject();
    };
    let once = Contrast::of([a, b], [n1, n2]);
    let scaled = Contrast::of([ka, kb], [kn1, kn2]);
    let g2 = once.log_likelihood.expect("G²");
    assert!(
        (scaled.log_likelihood.expect("G²") - k as f64 * g2).abs()
            <= 1e-9 * (k as f64 * g2).max(1.0)
    );
    assert_eq!(scaled.direction, once.direction);
}

#[test]
fn contrast_of_counts_near_the_integer_limit_does_not_overflow() {
    let contrast = lingrep_corpus::Contrast::of([u64::MAX, 1], [u64::MAX, u64::MAX]);
    assert!(contrast.log_likelihood.expect("G²") > 0.0);
    assert_eq!(contrast.direction, Some(lingrep_corpus::Direction::First));
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn contrast_ranks_every_entry_by_g2_as_a_direct_count_does(tc: hegel::TestCase) {
    use lingrep_corpus::{Contrast, ScopeCount, ScopeRelation};
    let document_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(6));
    let pattern_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(2));
    let pairs: Pairs = (0..pattern_count)
        .map(|_| {
            let entry_count = tc.draw(generators::integers::<usize>().max_value(200));
            (0..entry_count)
                .map(|_| {
                    let present = tc.draw(
                        generators::integers::<u32>()
                            .min_value(1)
                            .max_value((1 << document_count) - 1),
                    );
                    (0..document_count as u32)
                        .filter(|document| present & (1 << document) != 0)
                        .map(|document| {
                            (
                                document,
                                tc.draw(generators::integers::<u64>().min_value(1).max_value(4)),
                            )
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    let draw_admitted = || {
        tc.draw(
            generators::vecs(generators::booleans())
                .min_size(document_count)
                .max_size(document_count),
        )
    };
    let first = draw_admitted();
    let second = if tc.draw(generators::booleans()) {
        first.clone()
    } else {
        draw_admitted()
    };
    let limit = tc.draw(generators::integers::<usize>().max_value(250));
    let resident_rows = tc.draw(generators::integers::<usize>().min_value(1).max_value(64));
    let documents = (0..document_count as u64)
        .map(|document| DocumentTotals {
            document_id: 10 + 2 * document,
            sentences: document + 1,
            tokens: 7 * (document + 1),
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(&directory.path().join("t.lingtable"), &documents, &pairs);
    let mask = |admitted: &[bool]| {
        table.mask(|id| {
            documents
                .iter()
                .zip(admitted)
                .any(|(document, admitted)| *admitted && document.document_id == id)
        })
    };
    let (first_mask, second_mask) = (mask(&first), mask(&second));
    let scopes = table.scopes([&first_mask, &second_mask]).expect("scopes");

    let tokens = |admitted: &[bool]| -> u64 {
        documents
            .iter()
            .zip(admitted)
            .filter(|(_, admitted)| **admitted)
            .map(|(document, _)| document.tokens)
            .sum()
    };
    let both = first
        .iter()
        .zip(&second)
        .map(|(first, second)| *first && *second)
        .collect::<Vec<_>>();
    assert_eq!(
        scopes
            .admission()
            .map(|admission| admission.tokens_admitted),
        [tokens(&first), tokens(&second)]
    );
    assert_eq!(scopes.shared().tokens_admitted, tokens(&both));
    assert_eq!(
        scopes.shared().documents_admitted,
        both.iter().filter(|both| **both).count() as u64
    );
    let relation = if first == second {
        ScopeRelation::Identical
    } else if both.contains(&true) {
        ScopeRelation::Overlapping
    } else {
        ScopeRelation::Disjoint
    };
    assert_eq!(scopes.relation(), relation);

    let counted = |entry_pairs: &Vec<(u32, u64)>, admitted: &[bool]| {
        let mut scope = ScopeCount::default();
        for (document, count) in entry_pairs {
            if admitted[*document as usize] {
                scope.count += count;
                scope.works += 1;
            }
        }
        scope
    };
    let mut entry = 0u32;
    for (pattern, entries) in pairs.iter().enumerate() {
        let mut expected = Vec::new();
        for entry_pairs in entries {
            let counts = [counted(entry_pairs, &first), counted(entry_pairs, &second)];
            if counts[0].count + counts[1].count > 0 {
                let contrast = Contrast::of(
                    counts.map(|scope| scope.count),
                    [tokens(&first), tokens(&second)],
                );
                expected.push((entry, counts, counted(entry_pairs, &both), contrast));
            }
            entry += 1;
        }
        expected.sort_by(|left, right| {
            let score = |row: &(u32, [ScopeCount; 2], ScopeCount, Contrast)| {
                row.3.log_likelihood.unwrap_or(-1.0)
            };
            score(right)
                .total_cmp(&score(left))
                .then(
                    (right.1[0].count + right.1[1].count).cmp(&(left.1[0].count + left.1[1].count)),
                )
                .then(left.0.cmp(&right.0))
        });
        let as_tuples = |rows: Vec<lingrep_corpus::ContrastRow>| {
            rows.into_iter()
                .map(|row| (row.entry, row.scopes, row.shared, row.contrast))
                .collect::<Vec<_>>()
        };
        let ranked = table
            .ranked_contrast_rows(
                pattern,
                &scopes,
                spill_policy(resident_rows),
                Arc::new(TempBudget::new(u64::MAX)),
                &CancellationFlag::new(),
            )
            .expect("ranked rows");
        assert_eq!(ranked.entries, expected.len() as u64);
        assert_eq!(
            as_tuples(ranked.collect::<Result<_, _>>().expect("rows")),
            expected
        );
        let top = table
            .contrast_top(pattern, &scopes, limit, &CancellationFlag::new())
            .expect("top");
        assert_eq!(top.entries, expected.len() as u64);
        expected.truncate(limit);
        assert_eq!(as_tuples(top.rows), expected);
        if relation == ScopeRelation::Identical {
            assert!(
                expected
                    .iter()
                    .all(|row| row.3.log_likelihood.is_none_or(|g2| g2 == 0.0))
            );
        }
    }
}

#[hegel::test(test_cases = 300, derandomize = true)]
fn contrast_g2_and_ratio_follow_the_exact_rate_comparison(tc: hegel::TestCase) {
    use lingrep_corpus::{Contrast, Direction};
    // Rates c1/(c1 m) and c2/(c2 m) are equal; `step` moves the second scope's
    // tokens off equality by one. Token totals beyond 2^53 are not exact in f64.
    let draw =
        |min: u64, max: u64| tc.draw(generators::integers::<u64>().min_value(min).max_value(max));
    let (c1, c2) = (draw(1, 1000), draw(1, 1000));
    let m = draw(1 << 40, 1 << 53);
    let step = draw(0, 2);
    let tokens = [c1 * m, c2 * m + step - 1];
    let contrast = Contrast::of([c1, c2], tokens);
    let first = u128::from(c1) * u128::from(tokens[1]);
    let second = u128::from(c2) * u128::from(tokens[0]);
    let g2 = contrast.log_likelihood.expect("G²");
    let ratio = contrast.log2_ratio.expect("ratio");
    let (first_f, second_f) = (first as f64, second as f64);
    match first.cmp(&second) {
        std::cmp::Ordering::Equal => {
            assert_eq!(contrast.direction, Some(Direction::Equal));
            assert_eq!((g2, ratio), (0.0, 0.0), "{tokens:?}");
            return;
        }
        std::cmp::Ordering::Greater => {
            assert_eq!(contrast.direction, Some(Direction::First));
            assert!(ratio >= 0.0, "{tokens:?}: {ratio}");
        }
        std::cmp::Ordering::Less => {
            assert_eq!(contrast.direction, Some(Direction::Second));
            assert!(ratio <= 0.0, "{tokens:?}: {ratio}");
        }
    }
    // The rate ratio is first / second; the difference of the f64 logarithms
    // carries about 2^-46 of absolute error at these magnitudes.
    let expected = first_f.log2() - second_f.log2();
    assert!(
        (ratio - expected).abs() <= 1e-12,
        "{tokens:?}: {ratio} vs {expected}"
    );
    assert_eq!(
        ratio.partial_cmp(&0.0),
        Some(first.cmp(&second)),
        "{tokens:?}: the sign survives products that round to the same f64: {first_f} {second_f}"
    );
    assert!(g2.is_finite() && g2 >= 0.0, "{tokens:?}: {g2}");
}

#[test]
fn contrast_top_is_the_head_of_the_full_ranking_when_most_entries_are_rare() {
    // 6,000 entries whose table totals fall from 6,001 to 2: few entries can
    // reach a high score, so a ranking that stops early has rows to skip.
    let documents = (0..8u64)
        .map(|document| DocumentTotals {
            document_id: document,
            sentences: 1,
            tokens: 50_000 + 9_000 * document,
        })
        .collect::<Vec<_>>();
    let entries = (0..6_000u32)
        .map(|entry| {
            let total = 6_000 / (u64::from(entry) + 1) + 1;
            let home = entry % 8;
            let away = (entry / 8 + home + 1) % 8;
            if entry % 3 == 0 || away == home || total < 2 {
                vec![(home, total)]
            } else {
                let mut pairs = vec![(home, total - total / 3), (away, total / 3)];
                pairs.retain(|(_, count)| *count > 0);
                pairs.sort_unstable();
                pairs
            }
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![entries],
    );
    assert_contrast_top_is_the_head_of_the_full_ranking(
        &table,
        &[
            (&[0, 1, 2], &[3, 4, 5]),
            (&[0, 1, 2, 3], &[2, 3, 4]),
            (&[0], &[0, 1, 2, 3, 4, 5, 6, 7]),
            (&[6], &[7]),
        ],
        &[0, 1, 10, 100, 3_000, 7_000],
    );
}

#[test]
fn contrast_top_keeps_a_rare_entry_that_outscores_every_common_one() {
    // 3,072 common entries lean 3:1 towards the first document; the rarer
    // entries after them occur only there, and score higher than any common
    // entry although each occurs a fifth as often.
    let documents = (0..2u64)
        .map(|document| DocumentTotals {
            document_id: document,
            sentences: 1,
            tokens: 100_000_000,
        })
        .collect::<Vec<_>>();
    let entries = (0..3_100u64)
        .map(|entry| {
            if entry < 3_072 {
                let total = 5_000 - entry;
                vec![(0, total - total / 4), (1, total / 4)]
            } else {
                vec![(0, 1_000 - (entry - 3_072))]
            }
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![entries],
    );
    let top = table
        .contrast_top(
            0,
            &table
                .scopes([&table.mask(|id| id == 0), &table.mask(|id| id == 1)])
                .expect("scopes"),
            10,
            &CancellationFlag::new(),
        )
        .expect("top");
    assert_eq!(
        top.rows.iter().map(|row| row.entry).collect::<Vec<_>>(),
        (3_072..3_082).collect::<Vec<_>>()
    );
    assert_contrast_top_is_the_head_of_the_full_ranking(&table, &[(&[0], &[1])], &[10, 40]);
}

fn assert_contrast_top_is_the_head_of_the_full_ranking(
    table: &CountTable,
    scopes: &[(&[u64], &[u64])],
    limits: &[usize],
) {
    for (first, second) in scopes {
        let (first, second) = (
            table.mask(|id| first.contains(&id)),
            table.mask(|id| second.contains(&id)),
        );
        let scopes = table.scopes([&first, &second]).expect("scopes");
        let ranked = table
            .ranked_contrast_rows(
                0,
                &scopes,
                spill_policy(1 << 20),
                Arc::new(TempBudget::new(u64::MAX)),
                &CancellationFlag::new(),
            )
            .expect("ranked rows");
        let entries = ranked.entries;
        let full = ranked.collect::<Result<Vec<_>, _>>().expect("rows");
        for limit in limits {
            let top = table
                .contrast_top(0, &scopes, *limit, &CancellationFlag::new())
                .expect("top");
            assert_eq!(top.entries, entries);
            assert_eq!(top.rows, full[..(*limit).min(full.len())], "limit {limit}");
        }
    }
}

#[test]
fn masks_and_scopes_of_another_table_are_rejected() {
    let documents = (0..3u64)
        .map(|document| DocumentTotals {
            document_id: 10 + document,
            sentences: 1,
            tokens: 100,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("t.lingtable");
    let pairs: Pairs = vec![vec![vec![(0, 3), (1, 1)], vec![(2, 5)]]];
    let table = write_table(&path, &documents, &pairs);
    let other = write_table(
        &directory.path().join("u.lingtable"),
        &documents,
        &vec![vec![vec![(2, 1)]]],
    );
    let reopened = CountTable::open(&path, &key()).expect("open");
    let mask = table.mask(|id| id != 11);
    let scopes = table.scopes([&mask, &mask]).expect("scopes");
    for foreign in [&other, &reopened] {
        assert!(foreign.scopes([&mask, &mask]).is_err());
        assert!(foreign.pattern_totals(&mask).is_err());
        assert!(
            foreign
                .contrast_top(0, &scopes, 10, &CancellationFlag::new())
                .is_err()
        );
        assert!(
            foreign
                .ranked_contrast_rows(
                    0,
                    &scopes,
                    spill_policy(4),
                    Arc::new(TempBudget::new(u64::MAX)),
                    &CancellationFlag::new(),
                )
                .is_err()
        );
    }
    assert_eq!(
        table
            .contrast_top(0, &scopes, 10, &CancellationFlag::new())
            .expect("top")
            .entries,
        2
    );
}

#[test]
fn a_raised_cancellation_stops_contrast_ranking() {
    let documents = (0..2u64)
        .map(|document| DocumentTotals {
            document_id: document,
            sentences: 1,
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![vec![vec![(0, 2)], vec![(1, 3)]]],
    );
    let mask = table.mask(|_| true);
    let scopes = table.scopes([&mask, &mask]).expect("scopes");
    let cancellation = CancellationFlag::new();
    assert_eq!(
        table
            .contrast_top(0, &scopes, 10, &cancellation)
            .expect("top")
            .entries,
        2
    );
    cancellation.cancel();
    assert!(matches!(
        table.contrast_top(0, &scopes, 10, &cancellation),
        Err(CountTableError::Cancelled)
    ));
    assert!(matches!(
        table.ranked_contrast_rows(
            0,
            &scopes,
            spill_policy(4),
            Arc::new(TempBudget::new(u64::MAX)),
            &cancellation,
        ),
        Err(CountTableError::Cancelled)
    ));

    let draining = CancellationFlag::new();
    let mut rows = table
        .ranked_contrast_rows(
            0,
            &scopes,
            spill_policy(1),
            Arc::new(TempBudget::new(u64::MAX)),
            &draining,
        )
        .expect("rows");
    assert!(rows.next().expect("first row").is_ok());
    draining.cancel();
    assert!(matches!(rows.next(), Some(Err(CountTableError::Cancelled))));
    assert!(rows.next().is_none());
}

#[hegel::test(test_cases = 100, derandomize = true)]
fn unranked_pattern_rows_are_the_ranked_rows_in_entry_order(tc: hegel::TestCase) {
    let document_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(6));
    let pattern_count = tc.draw(generators::integers::<usize>().min_value(1).max_value(3));
    let pairs: Pairs = (0..pattern_count)
        .map(|_| {
            let entry_count = tc.draw(generators::integers::<usize>().max_value(200));
            (0..entry_count)
                .map(|_| {
                    let present = tc.draw(
                        generators::integers::<u32>()
                            .min_value(1)
                            .max_value((1 << document_count) - 1),
                    );
                    (0..document_count as u32)
                        .filter(|document| present & (1 << document) != 0)
                        .map(|document| {
                            let count = generators::integers::<u64>().min_value(1).max_value(4);
                            (document, tc.draw(count))
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    let admitted = tc.draw(
        generators::vecs(generators::booleans())
            .min_size(document_count)
            .max_size(document_count),
    );
    let documents = (0..document_count as u64)
        .map(|document| DocumentTotals {
            document_id: document,
            sentences: 1,
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(&directory.path().join("t.lingtable"), &documents, &pairs);
    let mask = table.mask(|id| admitted[id as usize]);
    let never = CancellationFlag::new();
    for pattern in 0..pattern_count {
        let unranked = table
            .unranked_pattern_rows(pattern, &mask, &never)
            .expect("unranked")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        let mut ranked = table
            .ranked_pattern_rows(
                pattern,
                &mask,
                spill_policy(4),
                Arc::new(TempBudget::new(u64::MAX)),
                &never,
            )
            .expect("ranked")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        ranked.sort_by_key(|row| row.entry);
        assert_eq!(unranked, ranked, "pattern {pattern}");
    }
}

#[test]
fn unranked_pattern_rows_stop_when_cancelled_and_reject_a_foreign_mask() {
    let documents = (0..2u64)
        .map(|document| DocumentTotals {
            document_id: document,
            sentences: 1,
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let pairs: Pairs = vec![vec![vec![(0, 2)], vec![(1, 3)]]];
    let table = write_table(&directory.path().join("t.lingtable"), &documents, &pairs);
    let other = write_table(&directory.path().join("u.lingtable"), &documents, &pairs);
    let mask = table.mask(|_| true);
    let raised = CancellationFlag::new();
    raised.cancel();
    let mut rows = table
        .unranked_pattern_rows(0, &mask, &raised)
        .expect("rows");
    assert!(matches!(rows.next(), Some(Err(CountTableError::Cancelled))));
    assert!(rows.next().is_none());
    assert!(
        other
            .unranked_pattern_rows(0, &mask, &CancellationFlag::new())
            .is_err()
    );
}

#[test]
fn a_raised_cancellation_stops_ranked_pattern_rows() {
    let documents = (0..2u64)
        .map(|document| DocumentTotals {
            document_id: document,
            sentences: 1,
            tokens: 10,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![vec![vec![(0, 2)], vec![(1, 3)], vec![(0, 1), (1, 1)]]],
    );
    let mask = table.mask(|_| true);
    let rows = |cancellation: &CancellationFlag| {
        table.ranked_pattern_rows(
            0,
            &mask,
            spill_policy(1),
            Arc::new(TempBudget::new(u64::MAX)),
            cancellation,
        )
    };
    let raised = CancellationFlag::new();
    raised.cancel();
    assert!(matches!(rows(&raised), Err(CountTableError::Cancelled)));

    let draining = CancellationFlag::new();
    let mut drain = rows(&draining).expect("rows");
    assert!(drain.next().expect("first row").is_ok());
    draining.cancel();
    assert!(matches!(
        drain.next(),
        Some(Err(CountTableError::Cancelled))
    ));
    assert!(drain.next().is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn sparse_document_indexes_open_under_a_small_heap_limit() {
    const CHILD: &str = "LINGREP_SPARSE_COUNT_INDEX";
    let documents = 1u64 << 26;
    if let Some(path) = std::env::var_os(CHILD) {
        let table = CountTable::open(Path::new(&path), &key()).expect("open sparse table");
        assert_eq!(table.document_ids().len() as u64, documents);
        assert_eq!(table.document_ids().last(), Some(&0));
        return;
    }
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("table");
    write_table(&path, &[], &vec![]);
    let header_path = path.join("header.json");
    let mut header: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&header_path).expect("read")).expect("JSON");
    header["documents"] = documents.into();
    std::fs::write(header_path, serde_json::to_vec(&header).expect("JSON")).expect("header");
    for name in [
        "document_ids.u64",
        "document_sentences.u64",
        "document_tokens.u64",
        "document_pair_offsets.u64",
        "document_pattern_offsets.u64",
    ] {
        let sentinel = u64::from(name.contains("offsets"));
        std::fs::OpenOptions::new()
            .write(true)
            .open(path.join(name))
            .expect("index")
            .set_len((documents + sentinel) * 8)
            .expect("sparse length");
    }
    let output = std::process::Command::new("sh")
        .args(["-c", "ulimit -c 0; ulimit -d 131072; exec \"$1\" --exact count_table::sparse_document_indexes_open_under_a_small_heap_limit --test-threads=1", "sh"])
        .arg(std::env::current_exe().expect("test executable"))
        .env(CHILD, &path).output().expect("child");
    assert!(
        output.status.success(),
        "{:?}: {} {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn verification_rejects_repeated_ranks_and_inconsistent_document_major_cells() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("table");
    let documents = [DocumentTotals {
        document_id: 0,
        sentences: 2,
        tokens: 2,
    }];
    drop(write_table(
        &path,
        &documents,
        &vec![vec![vec![(0, 1)], vec![(0, 2)]]],
    ));
    for component in [
        "ranked_entries.u32",
        "ranked_totals.u64",
        "document_entries.u32",
        "document_counts.u32",
    ] {
        let file = path.join(component);
        let original = std::fs::read(&file).expect("read");
        let mut changed = original.clone();
        if component.ends_with("u64") {
            changed[..8].copy_from_slice(&99u64.to_le_bytes());
        } else if component == "document_counts.u32" {
            changed[..4].copy_from_slice(&99u32.to_le_bytes());
        } else {
            let first = changed[..4].to_vec();
            changed[4..8].copy_from_slice(&first);
        }
        std::fs::write(&file, changed).expect("corrupt");
        {
            let table = CountTable::open(&path, &key()).expect("open");
            assert!(is_corrupt(table.verify()), "{component}");
        }
        std::fs::write(file, original).expect("restore");
    }
    CountTable::open(&path, &key())
        .expect("open")
        .verify()
        .expect("valid table");
}

#[test]
fn cancellation_during_value_aggregation_stops_before_the_remaining_documents() {
    use lingrep_corpus::DocumentSlots;
    use std::cell::Cell;

    struct CancellingSlots {
        visits: Cell<usize>,
        cancellation: CancellationFlag,
        cancel_after: usize,
    }
    impl DocumentSlots for CancellingSlots {
        fn document_count(&self) -> usize {
            3
        }
        fn slot_count(&self) -> usize {
            1
        }
        fn slots(&self, _: usize) -> &[u32] {
            let visits = self.visits.get() + 1;
            self.visits.set(visits);
            if visits == self.cancel_after {
                self.cancellation.cancel();
            }
            &[0]
        }
    }
    let documents = (0..3)
        .map(|document_id| DocumentTotals {
            document_id,
            sentences: 1,
            tokens: 1,
        })
        .collect::<Vec<_>>();
    let directory = tempfile::tempdir().expect("tempdir");
    let table = write_table(
        &directory.path().join("t.lingtable"),
        &documents,
        &vec![vec![vec![(0, 1), (1, 1), (2, 1)]]],
    );
    let mask = table.mask(|_| true);
    for cancel_after in [1, 3, 4, 6] {
        let cancellation = CancellationFlag::new();
        let slots = CancellingSlots {
            visits: Cell::new(0),
            cancellation: cancellation.clone(),
            cancel_after,
        };
        assert!(matches!(
            table.value_counts_cancellable(&slots, &mask, [(0, 1), (1, 1), (2, 1)], &cancellation),
            Err(CountTableError::Cancelled)
        ));
        assert_eq!(slots.visits.get(), cancel_after);
    }
}

fn npv_table(path: &Path, entries: &[([&str; 3], &[(u32, u64)])]) -> CountTable {
    let mut values = entries
        .iter()
        .flat_map(|(key, _)| key.iter().copied())
        .collect::<Vec<_>>();
    values.sort_unstable();
    values.dedup();
    let rank = |value: &str| values.binary_search(&value).expect("value") as u32;
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|(key, _)| key.map(rank));
    let documents = [0, 1].map(|document_id| DocumentTotals {
        document_id,
        sentences: 10,
        tokens: 100,
    });
    let mut writer = CountTableWriter::create(
        path,
        key(),
        vec!["npv".into(), "nouns".into()],
        vec![
            GroupSchema {
                kind: GroupKind::Key,
                names: vec!["$n.lemma".into(), "$p.lemma".into(), "$v.lemma".into()],
            },
            surface(),
        ],
        &values,
        &documents,
    )
    .expect("create");
    for (key, pairs) in &sorted {
        writer
            .begin_entry(0, 0, &key.map(rank))
            .expect("begin entry");
        for (document, count) in *pairs {
            writer
                .add_sentence(*document, u64::from(*document) * 100, *count)
                .expect("add sentence");
        }
    }
    writer.begin_entry(1, 1, &[rank("坂")]).expect("begin");
    writer.add_sentence(0, 0, 1).expect("sentence");
    publish(writer);
    CountTable::open(path, &key()).expect("open")
}

fn equals(fields: &[(&str, &str)]) -> KeyEquals {
    KeyEquals::new(
        fields
            .iter()
            .map(|(field, value)| ((*field).to_owned(), (*value).to_owned()))
            .collect(),
    )
}

#[test]
fn a_key_subset_sums_exactly_equal_fields_before_any_limit() {
    let directory = tempfile::tempdir().expect("tempdir");
    let table = npv_table(
        &directory.path().join("t.lingtable"),
        &[
            (["坂", "を", "上る"], &[(0, 6), (1, 4)]),
            (["坂", "を", "下りる"], &[(1, 4)]),
            (["坂", "を", "照らす"], &[(0, 1)]),
            (["寺坂", "を", "上る"], &[(0, 9)]),
            (["坂", "が", "ある"], &[(0, 3)]),
            (["山", "を", "上る"], &[(1, 2)]),
        ],
    );
    let cancellation = CancellationFlag::new();
    let saka_wo = equals(&[("$n.lemma", "坂"), ("$p.lemma", "を")]);
    let all = table.mask(|_| true);
    assert_eq!(
        table
            .key_subset(0, &all, &saka_wo, &cancellation)
            .expect("table")
            .expect("fields"),
        KeySubset {
            occurrences: 15,
            works: 2,
            entries: 3
        }
    );
    let first = table.mask(|id| id == 0);
    assert_eq!(
        table
            .key_subset(0, &first, &saka_wo, &cancellation)
            .expect("table")
            .expect("fields"),
        KeySubset {
            occurrences: 7,
            works: 1,
            entries: 2
        }
    );
    let admitted = table
        .unranked_pattern_rows(0, &all, &cancellation)
        .expect("rows")
        .map(|row| row.expect("row"))
        .filter(|row| {
            table
                .key_admits(0, row.entry, &saka_wo)
                .expect("table")
                .expect("fields")
        })
        .map(|row| table.group(row.entry).expect("group"))
        .collect::<Vec<_>>();
    let mut keys = admitted
        .iter()
        .map(|group| match group {
            Group::Key(fields) => fields
                .iter()
                .map(|(_, value)| match value {
                    FieldValue::Scalar(value) => *value,
                    FieldValue::List(_) => panic!("{group:?}"),
                })
                .collect::<Vec<_>>(),
            other => panic!("{other:?}"),
        })
        .collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            ["坂", "を", "上る"],
            ["坂", "を", "下りる"],
            ["坂", "を", "照らす"]
        ]
    );
    assert_eq!(
        table
            .key_subset(0, &table.mask(|_| false), &saka_wo, &cancellation)
            .expect("table")
            .expect("fields"),
        KeySubset::default()
    );
    assert_eq!(
        table
            .key_subset(0, &all, &equals(&[("$n.lemma", "谷")]), &cancellation)
            .expect("table")
            .expect("fields"),
        KeySubset::default()
    );
    assert_eq!(
        table
            .key_subset(0, &all, &KeyEquals::default(), &cancellation)
            .expect("table")
            .expect("fields"),
        KeySubset {
            occurrences: 29,
            works: 2,
            entries: 6
        }
    );
}

#[test]
fn a_key_subset_names_a_field_the_pattern_lacks() {
    let directory = tempfile::tempdir().expect("tempdir");
    let table = npv_table(
        &directory.path().join("t.lingtable"),
        &[(["坂", "を", "上る"], &[(0, 1)])],
    );
    let all = table.mask(|_| true);
    let cancellation = CancellationFlag::new();
    assert_eq!(
        table
            .key_subset(0, &all, &equals(&[("$v.norm", "上る")]), &cancellation)
            .expect("table"),
        Err(KeyFieldError::UnknownField {
            pattern: "npv".into(),
            field: "$v.norm".into(),
            fields: vec!["$n.lemma".into(), "$p.lemma".into(), "$v.lemma".into()],
        })
    );
    assert_eq!(
        table
            .key_subset(1, &all, &equals(&[("$n.lemma", "坂")]), &cancellation)
            .expect("table"),
        Err(KeyFieldError::UnknownField {
            pattern: "nouns".into(),
            field: "$n.lemma".into(),
            fields: Vec::new(),
        })
    );
}
