//! Keyness, the compare page's query: one pattern's entries ranked by G²
//! between two document scopes of a bound count table, as `ling contrast`
//! ranks them. Unrelated to [`crate::section_contrast`], which orders a
//! streamed result's section columns by signed rate difference.

use std::collections::BTreeMap;

use lingrep_core::execution::CancellationFlag;
use lingrep_corpus::{Direction, ScopeRelation};
use serde::Serialize;

use super::{
    Condition, Dimension, FrequencyPair, MAX_PAGE_ROWS, MAX_QUERY_ITEMS, PAGE_ROWS,
    ProjectionBudget, QueryError, Unsupported, document_clause, group_text, group_value,
    predicates,
};

/// One scope as the page states it: conditions, or a clause written as text
/// in their place.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub conditions: BTreeMap<String, Condition>,
    pub clause: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ContrastQuery {
    pub pattern: String,
    /// The `--where` scope, then the `--versus` scope.
    pub scopes: [Scope; 2],
    pub offset: usize,
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScopeAnswer {
    /// The clause the scope was resolved with; empty for every document.
    pub clause: String,
    pub documents: u64,
    pub tokens: u64,
    /// The pattern's occurrences in the scope and the documents holding them.
    pub occurrences: u64,
    pub works: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Shared {
    pub documents: u64,
    pub tokens: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Side {
    pub count: u64,
    pub works: u64,
    pub per_million: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct SharedCount {
    pub count: u64,
    pub works: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContrastRow {
    pub rank: u64,
    pub group: serde_json::Value,
    pub text: String,
    #[serde(rename = "where")]
    pub where_side: Side,
    pub versus: Side,
    pub shared: SharedCount,
    pub g2: Option<f64>,
    pub min_expected: Option<f64>,
    pub log2_ratio: Option<f64>,
    /// `where`, `versus` or `equal`; `null` with `g2`.
    pub direction: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ContrastAnswer {
    pub command: String,
    #[serde(rename = "where")]
    pub where_scope: ScopeAnswer,
    pub versus: ScopeAnswer,
    pub shared: Shared,
    /// `disjoint`, `overlapping` or `identical`.
    pub relation: &'static str,
    /// Entries with an occurrence in either scope.
    pub entries: u64,
    pub rows: Vec<ContrastRow>,
}

/// A scope whose conditions or clause a count table cannot answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScopeUnsupported {
    /// `where` or `versus`.
    pub scope: &'static str,
    #[serde(flatten)]
    pub unsupported: Unsupported,
}

const SCOPE_NAMES: [&str; 2] = ["where", "versus"];

impl FrequencyPair {
    fn contrast_command(&self, clauses: &[String; 2]) -> String {
        self.ling_command(
            "contrast --table ",
            &[("--where", &clauses[0]), ("--versus", &clauses[1])],
        )
    }

    /// Ranks `query.pattern`'s entries between the two scopes over
    /// `dimensions`, the dimensions of this pair's corpus, and returns the
    /// page of the ranking at `query.offset`.
    ///
    /// # Errors
    ///
    /// An unknown pattern, a table that fails to read, a query above the
    /// resource limits, or a raised `cancellation`. An offset at or past the
    /// last entry is an empty page, not an error.
    pub fn contrast(
        &self,
        dimensions: &[Dimension],
        query: &ContrastQuery,
        cancellation: &CancellationFlag,
    ) -> Result<Result<ContrastAnswer, ScopeUnsupported>, QueryError> {
        let pattern = self.pattern_index(&query.pattern)?;
        let past_end = self.past_last_entry(pattern, query.offset)?;
        let mut clauses = [String::new(), String::new()];
        let mut resolved = [None, None];
        for (index, scope) in query.scopes.iter().enumerate() {
            let unsupported = |unsupported| ScopeUnsupported {
                scope: SCOPE_NAMES[index],
                unsupported,
            };
            clauses[index] = match &scope.clause {
                Some(clause) => clause.trim().to_owned(),
                None => match predicates(dimensions, &scope.conditions) {
                    Ok(written) => written
                        .iter()
                        .map(|(_, predicate)| predicate.as_str())
                        .collect::<Vec<_>>()
                        .join(" & "),
                    Err(error) => return Ok(Err(unsupported(error))),
                },
            };
            resolved[index] = match document_clause(&clauses[index]) {
                Ok(clause) => clause,
                Err(error) => return Ok(Err(unsupported(error))),
            };
        }
        self.admit_query()?;
        let masks = [
            self.mask(&self.admitted(resolved[0].as_ref())?),
            self.mask(&self.admitted(resolved[1].as_ref())?),
        ];
        let scopes = self.table.scopes([&masks[0], &masks[1]])?;
        let limit = if query.limit == 0 {
            PAGE_ROWS
        } else {
            query.limit.min(MAX_PAGE_ROWS)
        };
        // Past the last entry the page is empty, so no row is kept.
        let kept = if past_end { 0 } else { query.offset + limit };
        let ranked = self
            .table
            .contrast_top(pattern, &scopes, kept, cancellation)?;
        let page = ranked.rows.get(query.offset..).unwrap_or_default();
        let mut projection = ProjectionBudget::default();
        let rows = page
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let index = query.offset + index;
                let group = self.table.bounded_group(row.entry, MAX_QUERY_ITEMS)?;
                projection.group(&group)?;
                let side = |scope: usize| Side {
                    count: row.scopes[scope].count,
                    works: row.scopes[scope].works,
                    per_million: row.contrast.rates_per_million[scope],
                };
                Ok(ContrastRow {
                    rank: index as u64 + 1,
                    group: group_value(&group),
                    text: group_text(&group),
                    where_side: side(0),
                    versus: side(1),
                    shared: SharedCount {
                        count: row.shared.count,
                        works: row.shared.works,
                    },
                    g2: row.contrast.log_likelihood,
                    min_expected: row.contrast.min_expected,
                    log2_ratio: row.contrast.log2_ratio,
                    direction: row.contrast.direction.map(|direction| match direction {
                        Direction::First => "where",
                        Direction::Second => "versus",
                        Direction::Equal => "equal",
                    }),
                })
            })
            .collect::<Result<Vec<_>, QueryError>>()?;
        let mut answers = Vec::with_capacity(2);
        for (index, mask) in masks.iter().enumerate() {
            let total = self.table.pattern_totals(mask)?[pattern];
            answers.push(ScopeAnswer {
                clause: clauses[index].clone(),
                documents: scopes.admission()[index].documents_admitted,
                tokens: scopes.admission()[index].tokens_admitted,
                occurrences: total.occurrences,
                works: total.works,
            });
        }
        let versus = answers.pop().expect("two scopes");
        let where_scope = answers.pop().expect("two scopes");
        Ok(Ok(ContrastAnswer {
            command: self.contrast_command(&clauses),
            where_scope,
            versus,
            shared: Shared {
                documents: scopes.shared().documents_admitted,
                tokens: scopes.shared().tokens_admitted,
            },
            relation: match scopes.relation() {
                ScopeRelation::Disjoint => "disjoint",
                ScopeRelation::Overlapping => "overlapping",
                ScopeRelation::Identical => "identical",
            },
            entries: ranked.entries,
            rows,
        }))
    }
}
