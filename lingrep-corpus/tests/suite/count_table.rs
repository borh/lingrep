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
