#![allow(clippy::panic)]

use std::path::Path;
use std::sync::Arc;

use hegel::generators;
use lingrep_core::execution::CancellationFlag;
use lingrep_core::spill::{SpillPolicy, TempBudget};
use lingrep_corpus::{
    CountTable, CountTableError, CountTableKey, CountTableWriter, DocumentMask, DocumentTotals,
    FieldValue, Group, GroupKind, GroupSchema, PatternTotal, TopRow,
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

/// The sentences `writer_for` writes for `count` occurrences in `document`:
/// up to three ordinals from `1000 * document`, with the remainder of the
/// count on the last.
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
    assert_eq!(left(), 1, "a staged table is beside its destination");
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
        header.replace("\"version\": 3", "\"version\": 2"),
    )
    .expect("rewrite header");
    assert!(matches!(
        CountTable::read_key(&path),
        Err(CountTableError::UnsupportedFormat { version: 2, .. })
    ));
    let error = CountTable::open(&path, &key()).err();
    assert!(
        matches!(
            error,
            Some(CountTableError::UnsupportedFormat { version: 2, .. })
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

/// Relative difference, for comparing a double with a reference value.
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
fn unranked_pattern_rows_stop_when_cancelled_and_refuse_a_foreign_mask() {
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
