use serde::{Deserialize, Serialize};
use std::io::Write;
use std::ops::Range;
use std::sync::Arc;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Connection, QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use tempfile::TempDir;

use crate::snapshot::{MatchRef, PatternKey};
use lingrep_core::token_match::AttributeKey;

const DISK_BUDGET: u64 = 128 * 1024 * 1024;
const RECORD_BYTES: usize = 1024 * 1024;
const BUFFER_BYTES: usize = 4 * 1024 * 1024;
const BUFFER_ROWS: usize = 256;
const PAGE_BYTES: u64 = 4096;

#[derive(Debug, thiserror::Error)]
pub enum ResultIndexError {
    #[error(
        "Too many results to retain. Narrow the pattern or document conditions and search again."
    )]
    BudgetExceeded,
    #[error("The complete results could not be stored or read: {0}")]
    Storage(String),
    #[error("The result position is invalid.")]
    InvalidPosition,
}

fn database(error: sqlx::Error) -> ResultIndexError {
    if error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == "13")
    {
        ResultIndexError::BudgetExceeded
    } else {
        ResultIndexError::Storage(error.to_string())
    }
}

fn integer(value: u64) -> Result<i64, ResultIndexError> {
    i64::try_from(value).map_err(|_| ResultIndexError::InvalidPosition)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultOccurrence {
    pub sequence: u64,
    pub document_ordinal: u64,
    pub reference: MatchRef,
    pub document_sentence_ordinal: u64,
    pub source_name: Arc<str>,
    pub source_metadata: Option<Arc<serde_json::Map<String, serde_json::Value>>>,
    pub char_span: Option<Range<u32>>,
    pub capture_char_spans: Box<[Option<Range<u32>>]>,
    pub unit: Option<String>,
    pub key: Option<Vec<lingrep_core::fsm::ProjectedKeyField>>,
    pub lexical_key: Option<Vec<lingrep_core::fsm::LexicalProjectedKeyField>>,
    pub tags: Vec<(compact_str::CompactString, String)>,
}
impl ResultOccurrence {
    pub(crate) fn from_reference(document: u64, reference: MatchRef) -> Self {
        Self {
            sequence: 0,
            document_ordinal: document,
            reference,
            document_sentence_ordinal: 0,
            source_name: Arc::from(""),
            source_metadata: None,
            char_span: None,
            capture_char_spans: Box::default(),
            unit: None,
            key: None,
            lexical_key: None,
            tags: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub struct ResultPage {
    pub rows: Vec<ResultOccurrence>,
    pub matches_total: u64,
    pub start: u64,
    pub previous: Option<i64>,
    pub next: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultDocument {
    pub ordinal: u64,
    pub first_sequence: u64,
    pub first_sentence: u64,
    pub matches: u64,
}

#[derive(Debug)]
pub struct DocumentPage {
    pub documents: Vec<ResultDocument>,
    pub total: u64,
}

#[derive(Debug)]
pub struct ResultIndex {
    pool: SqlitePool,
    directory: TempDir,
    publication_attributes: Arc<[AttributeKey]>,
}

impl PartialEq for ResultIndex {
    fn eq(&self, other: &Self) -> bool {
        self.directory.path() == other.directory.path()
    }
}
impl Eq for ResultIndex {}

impl ResultIndex {
    pub fn publication_attributes(&self) -> &[AttributeKey] {
        &self.publication_attributes
    }

    /// Returns occurrences after the exclusive global sequence; -1 starts at the beginning.
    pub async fn page(
        &self,
        pattern: PatternKey,
        document: Option<u64>,
        after: i64,
        limit: usize,
    ) -> Result<ResultPage, ResultIndexError> {
        if after < -1 {
            return Err(ResultIndexError::InvalidPosition);
        }
        let limit = limit.max(1);
        let matches_total = self.count(pattern, document).await?;
        let limit = limit.min(usize::try_from(matches_total).unwrap_or(usize::MAX).max(1));
        let mut query = QueryBuilder::new("SELECT COUNT(*) FROM matches WHERE ");
        scope(&mut query, pattern, document)?;
        query.push(" AND sequence <= ").push_bind(after);
        let start: i64 = query
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(database)?;
        let mut query = QueryBuilder::new("SELECT sequence, document, payload FROM matches WHERE ");
        scope(&mut query, pattern, document)?;
        query
            .push(" AND sequence > ")
            .push_bind(after)
            .push(" ORDER BY sequence LIMIT ")
            .push_bind((limit + 1) as i64);
        let mut rows = query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(database)?
            .into_iter()
            .map(occurrence)
            .collect::<Result<Vec<_>, _>>()?;
        let next = if rows.len() > limit {
            rows.truncate(limit);
            rows.last().map(|row| row.sequence)
        } else {
            None
        };
        let previous = if start == 0 {
            None
        } else {
            let mut query = QueryBuilder::new("SELECT sequence FROM matches WHERE ");
            scope(&mut query, pattern, document)?;
            query
                .push(" AND sequence <= ")
                .push_bind(after)
                .push(" ORDER BY sequence DESC LIMIT 1 OFFSET ")
                .push_bind(limit as i64);
            Some(
                query
                    .build_query_scalar::<i64>()
                    .fetch_optional(&self.pool)
                    .await
                    .map_err(database)?
                    .unwrap_or(-1),
            )
        };
        Ok(ResultPage {
            rows,
            matches_total,
            start: start as u64,
            previous,
            next,
        })
    }

    /// The occurrences of `pattern` in corpus sentence `sentence`, in sequence order.
    pub async fn sentence_occurrences(
        &self,
        pattern: PatternKey,
        sentence: u64,
    ) -> Result<Vec<ResultOccurrence>, ResultIndexError> {
        let mut query = QueryBuilder::new("SELECT sequence, document, payload FROM matches WHERE ");
        scope(&mut query, pattern, None)?;
        query
            .push(" AND sentence = ")
            .push_bind(integer(sentence)?)
            .push(" ORDER BY sequence");
        query
            .build()
            .fetch_all(&self.pool)
            .await
            .map_err(database)?
            .into_iter()
            .map(occurrence)
            .collect()
    }

    pub async fn count(
        &self,
        pattern: PatternKey,
        document: Option<u64>,
    ) -> Result<u64, ResultIndexError> {
        let mut query = QueryBuilder::new("SELECT COUNT(*) FROM matches WHERE ");
        scope(&mut query, pattern, document)?;
        let count: i64 = query
            .build_query_scalar()
            .fetch_one(&self.pool)
            .await
            .map_err(database)?;
        Ok(count as u64)
    }

    pub async fn reference(
        &self,
        sequence: u64,
    ) -> Result<Option<ResultOccurrence>, ResultIndexError> {
        sqlx::query("SELECT sequence, document, payload FROM matches WHERE sequence = ?")
            .bind(integer(sequence)?)
            .fetch_optional(&self.pool)
            .await
            .map_err(database)?
            .map(occurrence)
            .transpose()
    }

    /// Documents are ordered by their first occurrence; first_sentence is the first matching sentence, not the document's start.
    pub async fn documents(
        &self,
        pattern: PatternKey,
        offset: u64,
        limit: usize,
    ) -> Result<DocumentPage, ResultIndexError> {
        let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM documents WHERE pattern = ?")
            .bind(pattern.index() as i64)
            .fetch_one(&self.pool)
            .await
            .map_err(database)?;
        let rows = sqlx::query("SELECT document, first_sequence, first_sentence, matches FROM documents WHERE pattern = ? ORDER BY first_sequence LIMIT ? OFFSET ?").bind(pattern.index() as i64).bind(limit.clamp(1, 100) as i64).bind(integer(offset)?).fetch_all(&self.pool).await.map_err(database)?;
        let documents = rows
            .into_iter()
            .map(|row| ResultDocument {
                ordinal: row.get::<i64, _>("document") as u64,
                first_sequence: row.get::<i64, _>("first_sequence") as u64,
                first_sentence: row.get::<i64, _>("first_sentence") as u64,
                matches: row.get::<i64, _>("matches") as u64,
            })
            .collect();
        Ok(DocumentPage {
            documents,
            total: total as u64,
        })
    }

    /// Returns at most 256 complete references after the exclusive global sequence.
    pub async fn export_page(
        &self,
        after: i64,
        limit: usize,
    ) -> Result<Vec<ResultOccurrence>, ResultIndexError> {
        if after < -1 {
            return Err(ResultIndexError::InvalidPosition);
        }
        sqlx::query("SELECT sequence, document, payload FROM matches WHERE sequence > ? ORDER BY sequence LIMIT ?").bind(after).bind(limit.clamp(1, 256) as i64).fetch_all(&self.pool).await.map_err(database)?.into_iter().map(occurrence).collect()
    }
}

fn scope(
    query: &mut QueryBuilder<Sqlite>,
    pattern: PatternKey,
    document: Option<u64>,
) -> Result<(), ResultIndexError> {
    query.push("pattern = ").push_bind(pattern.index() as i64);
    if let Some(document) = document {
        query.push(" AND document = ").push_bind(integer(document)?);
    }
    Ok(())
}

fn occurrence(row: sqlx::sqlite::SqliteRow) -> Result<ResultOccurrence, ResultIndexError> {
    serde_json::from_slice(row.get::<&[u8], _>("payload"))
        .map_err(|error| ResultIndexError::Storage(error.to_string()))
}

#[derive(Debug)]
struct BufferedRow {
    sequence: i64,
    document: i64,
    sentence: i64,
    pattern: i64,
    payload: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct ResultIndexWriter {
    connection: SqliteConnection,
    directory: TempDir,
    runtime: tokio::runtime::Handle,
    sequence: u64,
    buffered: Vec<BufferedRow>,
    buffered_bytes: usize,
    publication_attributes: Arc<[AttributeKey]>,
}

impl ResultIndexWriter {
    pub(crate) fn new(
        publication_attributes: Arc<[AttributeKey]>,
    ) -> Result<Self, ResultIndexError> {
        Self::with_budget(DISK_BUDGET, publication_attributes)
    }

    fn with_budget(
        bytes: u64,
        publication_attributes: Arc<[AttributeKey]>,
    ) -> Result<Self, ResultIndexError> {
        let runtime = tokio::runtime::Handle::current();
        let directory =
            tempfile::tempdir().map_err(|error| ResultIndexError::Storage(error.to_string()))?;
        let options = SqliteConnectOptions::new()
            .filename(directory.path().join("results.sqlite3"))
            .create_if_missing(true)
            .page_size(PAGE_BYTES as u32)
            .journal_mode(SqliteJournalMode::Off)
            .synchronous(SqliteSynchronous::Off)
            .pragma("cache_size", "-2048")
            .pragma("temp_store", "FILE");
        let mut connection = runtime
            .block_on(SqliteConnection::connect_with(&options))
            .map_err(database)?;
        runtime.block_on(async {
            let mut limit = QueryBuilder::<Sqlite>::new("PRAGMA max_page_count = ");
            limit.push((bytes / PAGE_BYTES).to_string());
            limit.build().execute(&mut connection).await?;
            sqlx::raw_sql("CREATE TABLE matches (sequence INTEGER PRIMARY KEY, pattern INTEGER NOT NULL, document INTEGER NOT NULL, sentence INTEGER NOT NULL, payload BLOB NOT NULL); CREATE INDEX match_pattern_order ON matches(pattern, sequence); CREATE INDEX match_document_order ON matches(pattern, document, sequence); CREATE TABLE documents (pattern INTEGER NOT NULL, document INTEGER NOT NULL, first_sequence INTEGER NOT NULL, first_sentence INTEGER NOT NULL, matches INTEGER NOT NULL, PRIMARY KEY(pattern, document)) WITHOUT ROWID; CREATE INDEX document_order ON documents(pattern, first_sequence); BEGIN IMMEDIATE;").execute(&mut connection).await
        }).map_err(database)?;
        Ok(Self {
            connection,
            directory,
            runtime,
            sequence: 0,
            buffered: Vec::with_capacity(BUFFER_ROWS),
            buffered_bytes: 0,
            publication_attributes,
        })
    }

    #[cfg(test)]
    fn append(&mut self, document: u64, reference: &MatchRef) -> Result<u64, ResultIndexError> {
        let mut row = ResultOccurrence::from_reference(document, reference.clone());
        self.append_occurrence(&mut row)?;
        Ok(row.sequence)
    }

    pub(crate) fn append_occurrence(
        &mut self,
        row: &mut ResultOccurrence,
    ) -> Result<(), ResultIndexError> {
        row.sequence = self.sequence;
        let mut bytes = LimitedBytes(Vec::new());
        serde_json::to_writer(&mut bytes, &row).map_err(|_| ResultIndexError::BudgetExceeded)?;
        if self.buffered.len() >= BUFFER_ROWS || self.buffered_bytes + bytes.0.len() > BUFFER_BYTES
        {
            self.flush()?;
        }
        let sequence = integer(self.sequence)?;
        self.buffered_bytes += bytes.0.len();
        self.buffered.push(BufferedRow {
            sequence,
            document: integer(row.document_ordinal)?,
            sentence: integer(row.reference.sentence_index)?,
            pattern: row.reference.pattern_key.index() as i64,
            payload: bytes.0,
        });
        self.sequence += 1;
        Ok(())
    }

    fn flush(&mut self) -> Result<(), ResultIndexError> {
        if self.buffered.is_empty() {
            return Ok(());
        }
        let mut matches = QueryBuilder::<Sqlite>::new(
            "INSERT INTO matches (sequence, pattern, document, sentence, payload) ",
        );
        matches.push_values(&self.buffered, |mut values, row| {
            values
                .push_bind(row.sequence)
                .push_bind(row.pattern)
                .push_bind(row.document)
                .push_bind(row.sentence)
                .push_bind(&row.payload);
        });
        let mut documents = QueryBuilder::<Sqlite>::new(
            "INSERT INTO documents (pattern, document, first_sequence, first_sentence, matches) ",
        );
        documents.push_values(&self.buffered, |mut values, row| {
            values
                .push_bind(row.pattern)
                .push_bind(row.document)
                .push_bind(row.sequence)
                .push_bind(row.sentence)
                .push("1");
        });
        documents
            .push(" ON CONFLICT(pattern, document) DO UPDATE SET matches = documents.matches + 1");
        self.runtime
            .block_on(async {
                matches
                    .build()
                    .persistent(false)
                    .execute(&mut self.connection)
                    .await?;
                documents
                    .build()
                    .persistent(false)
                    .execute(&mut self.connection)
                    .await
            })
            .map_err(database)?;
        self.buffered.clear();
        self.buffered_bytes = 0;
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<Arc<ResultIndex>, ResultIndexError> {
        self.flush()?;
        self.runtime
            .block_on(async { sqlx::query("COMMIT").execute(&mut self.connection).await })
            .map_err(database)?;
        self.runtime
            .block_on(self.connection.close())
            .map_err(database)?;
        let options = SqliteConnectOptions::new()
            .filename(self.directory.path().join("results.sqlite3"))
            .read_only(true)
            .pragma("cache_size", "-2048")
            .pragma("query_only", "ON");
        let pool = self
            .runtime
            .block_on(
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(options),
            )
            .map_err(database)?;
        Ok(Arc::new(ResultIndex {
            pool,
            directory: self.directory,
            publication_attributes: self.publication_attributes,
        }))
    }
}

struct LimitedBytes(Vec<u8>);
impl Write for LimitedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > RECORD_BYTES {
            return Err(std::io::Error::other("result record exceeds budget"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(crate) mod bindings {
    use lingrep_core::corpus::conllu::EnhancedNodeId;
    use lingrep_core::fsm::{GraphBinding, GraphNodeAttribute};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize, Deserialize)]
    enum Node {
        Word(usize),
        Empty(usize, usize),
    }
    impl From<EnhancedNodeId> for Node {
        fn from(value: EnhancedNodeId) -> Self {
            match value {
                EnhancedNodeId::Word { token_index } => Self::Word(token_index),
                EnhancedNodeId::Empty { boundary, subindex } => Self::Empty(boundary, subindex),
            }
        }
    }
    impl From<Node> for EnhancedNodeId {
        fn from(value: Node) -> Self {
            match value {
                Node::Word(token_index) => Self::Word { token_index },
                Node::Empty(boundary, subindex) => Self::Empty { boundary, subindex },
            }
        }
    }
    #[derive(Serialize, Deserialize)]
    enum Binding {
        Node {
            name: compact_str::CompactString,
            node: Node,
            attributes: Vec<(
                lingrep_core::token_match::AttributeKey,
                compact_str::CompactString,
            )>,
        },
        Edge {
            name: compact_str::CompactString,
            head: Node,
            dependent: Node,
            relation: compact_str::CompactString,
        },
    }
    #[derive(Serialize)]
    enum BorrowedBinding<'a> {
        Node {
            name: &'a compact_str::CompactString,
            node: Node,
            #[serde(serialize_with = "serialize_attributes")]
            attributes: &'a [GraphNodeAttribute],
        },
        Edge {
            name: &'a compact_str::CompactString,
            head: Node,
            dependent: Node,
            relation: &'a compact_str::CompactString,
        },
    }
    fn serialize_attributes<S: Serializer>(
        values: &&[GraphNodeAttribute],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(
            values
                .iter()
                .map(|attribute| (&attribute.name, &attribute.value)),
        )
    }
    pub fn serialize<S: Serializer>(
        values: &[GraphBinding],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(values.iter().map(|value| match value {
            GraphBinding::Node {
                name,
                node,
                attributes,
            } => BorrowedBinding::Node {
                name,
                node: (*node).into(),
                attributes,
            },
            GraphBinding::Edge {
                name,
                head,
                dependent,
                relation,
            } => BorrowedBinding::Edge {
                name,
                head: (*head).into(),
                dependent: (*dependent).into(),
                relation,
            },
        }))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Box<[GraphBinding]>, D::Error> {
        Ok(Vec::<Binding>::deserialize(deserializer)?
            .into_iter()
            .map(|value| match value {
                Binding::Node {
                    name,
                    node,
                    attributes,
                } => GraphBinding::Node {
                    name,
                    node: node.into(),
                    attributes: attributes
                        .into_iter()
                        .map(|(name, value)| GraphNodeAttribute { name, value })
                        .collect(),
                },
                Binding::Edge {
                    name,
                    head,
                    dependent,
                    relation,
                } => GraphBinding::Edge {
                    name,
                    head: head.into(),
                    dependent: dependent.into(),
                    relation,
                },
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{CaptureKey, CaptureSpanRef, LexicalMatchRef};
    use lingrep_core::corpus::conllu::EnhancedNodeId;
    use lingrep_core::fsm::{GraphBinding, GraphNodeAttribute};

    fn reference(sentence: u64, pattern: usize) -> MatchRef {
        MatchRef {
            sentence_index: sentence,
            span: 2..4,
            pattern_key: PatternKey::new(pattern).unwrap(),
            captures: vec![CaptureSpanRef {
                capture_key: CaptureKey::new(0).unwrap(),
                span: 2..3,
            }]
            .into(),
            graph_bindings: vec![
                GraphBinding::Node {
                    name: "n".into(),
                    node: EnhancedNodeId::Empty {
                        boundary: 2,
                        subindex: 1,
                    },
                    attributes: vec![GraphNodeAttribute {
                        name: lingrep_core::token_match::AttributeKey::new("text").unwrap(),
                        value: "語".into(),
                    }]
                    .into(),
                },
                GraphBinding::Edge {
                    name: "e".into(),
                    head: EnhancedNodeId::Word { token_index: 1 },
                    dependent: EnhancedNodeId::Empty {
                        boundary: 2,
                        subindex: 1,
                    },
                    relation: "nsubj".into(),
                },
            ]
            .into(),
            lexical: Some(LexicalMatchRef {
                term: "語彙".into(),
                matched_by: vec!["lemma".into(), "norm".into()].into(),
            }),
            in_chain: true,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pages_preserve_complete_references_and_global_occurrences_across_documents() {
        let index = tokio::task::spawn_blocking(|| {
            let mut writer = ResultIndexWriter::new(Arc::from([])).unwrap();
            for sequence in 0..7 {
                writer
                    .append(sequence / 3, &reference(sequence, (sequence % 2) as usize))
                    .unwrap();
            }
            writer.finish().unwrap()
        })
        .await
        .unwrap();
        let p = PatternKey::new(0).unwrap();
        let page = index.page(p, None, -1, 2).await.unwrap();
        assert_eq!(
            page.rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [0, 2]
        );
        assert_eq!(
            (page.matches_total, page.start, page.previous, page.next),
            (4, 0, None, Some(2))
        );
        let last = index.page(p, None, 2, 2).await.unwrap();
        assert_eq!(
            last.rows.iter().map(|r| r.sequence).collect::<Vec<_>>(),
            [4, 6]
        );
        assert_eq!((last.start, last.previous, last.next), (2, Some(-1), None));
        assert_eq!(
            index.reference(4).await.unwrap().unwrap(),
            ResultOccurrence {
                sequence: 4,
                ..ResultOccurrence::from_reference(1, reference(4, 0))
            }
        );
        let scoped = index.page(p, Some(1), -1, 30).await.unwrap();
        assert_eq!(
            scoped.rows,
            vec![index.reference(4).await.unwrap().unwrap()]
        );
        assert_eq!(index.count(p, Some(1)).await.unwrap(), 1);
        let docs = index.documents(p, 0, 2).await.unwrap();
        assert_eq!(docs.total, 3);
        assert_eq!(
            docs.documents,
            vec![
                ResultDocument {
                    ordinal: 0,
                    first_sequence: 0,
                    first_sentence: 0,
                    matches: 2
                },
                ResultDocument {
                    ordinal: 1,
                    first_sequence: 4,
                    first_sentence: 4,
                    matches: 1
                }
            ]
        );
        assert_eq!(
            index.documents(p, 2, 100).await.unwrap().documents[0].ordinal,
            2
        );
        let mut all = vec![];
        let mut after = -1;
        loop {
            let rows = index.export_page(after, 2).await.unwrap();
            if rows.is_empty() {
                break;
            }
            after = rows.last().unwrap().sequence as i64;
            all.extend(rows);
        }
        assert_eq!(
            all,
            (0..7)
                .map(|sequence| ResultOccurrence {
                    sequence,
                    ..ResultOccurrence::from_reference(
                        sequence / 3,
                        reference(sequence, (sequence % 2) as usize)
                    )
                })
                .collect::<Vec<_>>()
        );
        let path = index.directory.path().to_owned();
        assert!(path.exists());
        drop(index);
        assert!(!path.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn response_limits_and_writer_batch_boundaries_preserve_exact_totals() {
        let index = tokio::task::spawn_blocking(|| {
            let mut writer = ResultIndexWriter::new(Arc::from([])).unwrap();
            for sequence in 0..600 {
                writer.append(sequence, &reference(sequence, 0)).unwrap();
            }
            writer.finish().unwrap()
        })
        .await
        .unwrap();
        let p = PatternKey::new(0).unwrap();
        let page = index.page(p, None, 255, usize::MAX).await.unwrap();
        assert_eq!(page.rows.len(), 344);
        assert_eq!(page.rows.first().unwrap().sequence, 256);
        assert_eq!(
            (page.start, page.matches_total, page.previous, page.next),
            (256, 600, Some(-1), None)
        );
        assert_eq!(
            index
                .documents(p, 0, usize::MAX)
                .await
                .unwrap()
                .documents
                .len(),
            100
        );
        assert_eq!(index.documents(p, 0, 1).await.unwrap().total, 600);
        assert_eq!(index.export_page(-1, usize::MAX).await.unwrap().len(), 256);
        assert!(matches!(
            index.page(p, None, -2, 30).await,
            Err(ResultIndexError::InvalidPosition)
        ));
        assert!(index.reference(600).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn journal_budget_exhaustion_drops_incomplete_files() {
        let path = tokio::task::spawn_blocking(|| {
            let mut writer = ResultIndexWriter::with_budget(32 * 1024, Arc::from([])).unwrap();
            let path = writer.directory.path().to_owned();
            let mut large = reference(0, 0);
            large.lexical.as_mut().unwrap().term = "x".repeat(20 * 1024);
            writer.append(0, &large).unwrap();
            assert!(matches!(
                writer.finish(),
                Err(ResultIndexError::BudgetExceeded)
            ));
            path
        })
        .await
        .unwrap();
        assert!(!path.exists());
    }
}
