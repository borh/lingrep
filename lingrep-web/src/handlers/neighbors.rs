//! The neighbors page: `GET /neighbors`, which lists the values a `near` test
//! with one word and threshold selects from the server's word-vector file.

use askama::Template;
use axum::{
    Router,
    extract::{Query, State},
    response::{IntoResponse as _, Response},
    routing::get,
};
use lingrep_core::fsm::SimilarityThreshold;
use lingrep_corpus::word_vectors::{VectorTable, VectorsBinding, vectors_binding};
use serde::Deserialize;

use crate::state::RequestStore;
use crate::templates::format_count;

const MAX_ROWS: usize = 500;

const DEFAULT_THRESHOLD: &str = "0.6";

#[derive(Deserialize)]
struct Params {
    #[serde(default)]
    word: String,
    #[serde(default)]
    min: Option<String>,
}

pub struct Row {
    pub rank: usize,
    pub value: String,
    /// Similarity truncated to four decimal places.
    pub similarity: String,
    /// The similarity as a percentage of the bar's width.
    pub percent: u32,
}

pub enum Outcome {
    /// No word was submitted.
    Idle,
    /// `min` is not a number from 0 to 1 with at most six decimals.
    InvalidThreshold,
    NoVector,
    Unreadable(String),
    Listed {
        /// The test that selects these values, without its attribute; absent
        /// when the word cannot be spelled as a pattern value.
        test: Option<String>,
        total: String,
        truncated: bool,
        rows: Vec<Row>,
    },
}

pub struct File {
    pub name: String,
    pub declaration: String,
}

#[derive(Template)]
#[template(path = "neighbors.html")]
pub struct NeighborsPage {
    current_page: &'static str,
    frequency_view: bool,
    pub file: Option<File>,
    pub word: String,
    pub threshold: String,
    pub outcome: Outcome,
    max_rows: usize,
}

pub fn page_routes() -> Router<RequestStore> {
    Router::new().route("/neighbors", get(page))
}

async fn page(State(store): State<RequestStore>, Query(params): Query<Params>) -> Response {
    let frequency_view = !store.frequency.is_empty();
    let page = tokio::task::spawn_blocking(move || {
        view(
            crate::patterns::word_vectors(),
            params.word.trim(),
            params.min.as_deref(),
            frequency_view,
        )
    })
    .await;
    match page {
        Ok(page) => crate::handlers::render_or_500(page),
        Err(_) => axum::http::StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub fn view(
    table: Option<&VectorTable>,
    word: &str,
    min: Option<&str>,
    frequency_view: bool,
) -> NeighborsPage {
    let threshold_text = min.map_or(DEFAULT_THRESHOLD, str::trim);
    let outcome = match (table, SimilarityThreshold::parse(threshold_text)) {
        (None, _) => Outcome::Idle,
        (Some(_), _) if word.is_empty() => Outcome::Idle,
        (Some(_), None) => Outcome::InvalidThreshold,
        (Some(table), Some(threshold)) => list(table, word, threshold),
    };
    NeighborsPage {
        current_page: "neighbors",
        frequency_view,
        file: table.map(|table| File {
            name: table
                .path()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            declaration: match vectors_binding(&[], table.path(), table.digest().to_hex().as_str())
            {
                VectorsBinding::Undeclared { declaration } => declaration,
                VectorsBinding::Declared | VectorsBinding::Mismatch { .. } => String::new(),
            },
        }),
        word: word.to_owned(),
        threshold: threshold_text.to_owned(),
        outcome,
        max_rows: MAX_ROWS,
    }
}

fn list(table: &VectorTable, word: &str, threshold: SimilarityThreshold) -> Outcome {
    let vector = match table.unit_vectors(&[word]) {
        Ok(mut vectors) => vectors.pop().flatten(),
        Err(error) => return Outcome::Unreadable(error.to_string()),
    };
    let Some(vector) = vector else {
        return Outcome::NoVector;
    };
    let minimum = i64::from(threshold.millionths());
    let mut hits = match table.scan(&[&vector], |_, similarity| i64::from(similarity) >= minimum) {
        Ok(mut scan) => scan.hits.pop().unwrap_or_default(),
        Err(error) => return Outcome::Unreadable(error.to_string()),
    };
    hits.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    let total = hits.len();
    Outcome::Listed {
        test: lingrep_corpus::word_vectors::pattern_literal(word)
            .map(|word| format!("near {word} >= {threshold}")),
        total: format_count(total as u64),
        truncated: total > MAX_ROWS,
        rows: hits
            .into_iter()
            .take(MAX_ROWS)
            .enumerate()
            .map(|(index, (value, similarity))| {
                let shown = similarity.max(0);
                Row {
                    rank: index + 1,
                    value: value.into(),
                    similarity: format!("{}.{:04}", shown / 1_000_000, shown % 1_000_000 / 100),
                    percent: u32::try_from(shown / 10_000).unwrap_or(0),
                }
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &str) -> (tempfile::TempDir, VectorTable) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Walking Words.vec");
        std::fs::write(&path, rows).unwrap();
        let table = VectorTable::open(&path).unwrap();
        (directory, table)
    }

    const ROWS: &str = "walk 1 0\nstroll 0.8 0.6\nrun 0.6 0.8\ncook 0 1\nback -1 0\n";

    #[test]
    fn a_word_and_threshold_list_the_values_a_near_test_selects_in_rank_order() {
        let (_directory, table) = table(ROWS);
        let page = view(Some(&table), "walk", Some("0.6"), false);
        let Outcome::Listed {
            test,
            total,
            truncated,
            rows,
        } = &page.outcome
        else {
            panic!("not listed");
        };
        assert_eq!(test.as_deref(), Some("near walk >= 0.6"));
        assert_eq!(total, "3");
        assert!(!truncated);
        let listed: Vec<(usize, &str, &str, u32)> = rows
            .iter()
            .map(|row| {
                (
                    row.rank,
                    row.value.as_str(),
                    row.similarity.as_str(),
                    row.percent,
                )
            })
            .collect();
        assert_eq!(
            listed,
            [
                (1, "walk", "1.0000", 100),
                (2, "stroll", "0.8000", 80),
                (3, "run", "0.6000", 60),
            ]
        );
        let near = crate::patterns::tests_support::near_values(&table, "[lemma near walk >= 0.6]");
        assert_eq!(near, ["walk", "stroll", "run"]);

        let html = page.render().unwrap();
        assert!(html.contains("near walk &gt;= 0.6") || html.contains("near walk &#62;= 0.6"));
        assert!(
            html.contains("resource: &quot;walking-words&quot;")
                || html.contains("resource: &#34;walking-words&#34;"),
            "{html}"
        );
    }

    #[test]
    fn missing_words_invalid_thresholds_and_unreadable_files_have_distinct_outcomes() {
        let (_directory, table) = table(ROWS);
        assert!(matches!(
            view(Some(&table), "", None, false).outcome,
            Outcome::Idle
        ));
        assert!(matches!(
            view(Some(&table), "swim", Some("0.6"), false).outcome,
            Outcome::NoVector
        ));
        for threshold in ["1.5", "-0.1", "x", "0.1234567"] {
            assert!(
                matches!(
                    view(Some(&table), "walk", Some(threshold), false).outcome,
                    Outcome::InvalidThreshold
                ),
                "{threshold}"
            );
        }
        let (_ragged_directory, ragged) = self::table("walk 1 0\nstroll 1\n");
        assert!(matches!(
            view(Some(&ragged), "walk", Some("0"), false).outcome,
            Outcome::Unreadable(message) if message.contains("line 2")
        ));
        let unconfigured = view(None, "walk", Some("0.6"), false).render().unwrap();
        assert!(unconfigured.contains("--vectors FILE"), "{unconfigured}");
        assert!(!unconfigured.contains("neighbors-form"), "{unconfigured}");
    }

    #[test]
    fn a_negative_similarity_is_listed_at_threshold_zero_only_when_it_rounds_to_zero() {
        let (_directory, table) = table(ROWS);
        let Outcome::Listed { rows, .. } = view(Some(&table), "walk", Some("0"), false).outcome
        else {
            panic!("not listed");
        };
        let values: Vec<&str> = rows.iter().map(|row| row.value.as_str()).collect();
        assert_eq!(values, ["walk", "stroll", "run", "cook"]);
    }
}
