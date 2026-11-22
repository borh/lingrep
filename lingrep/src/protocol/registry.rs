use super::discovery::{ContractKind, ContractSpec, ContractSummary};
use super::{
    ContractId, attributes, capabilities, check, completions, contract, contrast, diagnostic,
    dictionary, disk, doctor, invocation, manpages, neighbors, prune, query, sources, status,
    table, tokenize, variants, window_counts,
};

pub fn get(id: ContractId) -> ContractSpec {
    match id {
        ContractId::Conllu => ContractSpec::Conllu(ContractSummary {
            id,
            kind: ContractKind::Conllu,
            owners: &["tokenize"],
            summary: "CoNLL-U corpus export selected by --output-format conllu.",
        }),
        ContractId::Attributes => attributes::contract(),
        ContractId::AttributesTsv => crate::output::attributes_contract(),
        ContractId::Query => query::contract(),
        ContractId::Check => check::contract(),
        ContractId::Tokenize => tokenize::contract(),
        ContractId::Sources => sources::contract(),
        ContractId::Diagnostic => diagnostic::contract(),
        ContractId::Disk => disk::contract(),
        ContractId::Status => status::contract(),
        ContractId::Capabilities => capabilities::contract(),
        ContractId::Invocation => invocation::contract(),
        ContractId::Doctor => doctor::contract(),
        ContractId::Dictionary => dictionary::contract(),
        ContractId::Completions => completions::contract(),
        ContractId::ProtocolContract => contract::contract(),
        ContractId::Prune => prune::contract(),
        ContractId::Manpages => manpages::contract(),
        ContractId::MatchesTsv => crate::output::matches_contract(),
        ContractId::EvidenceTsv => crate::output::evidence_contract(),
        ContractId::SummarizeTsv => crate::output::summarize_contract(),
        ContractId::TokenizeTsv => crate::output::tokenize_contract(),
        ContractId::SourcesTsv => crate::output::sources_contract(),
        ContractId::Variants => variants::contract(),
        ContractId::Table => table::contract(),
        ContractId::Contrast => contrast::contract(),
        ContractId::Neighbors => neighbors::contract(),
        ContractId::WindowCounts => window_counts::contract(),
    }
}

pub fn summaries() -> Vec<ContractSummary<'static>> {
    let mut values = ContractId::ALL
        .iter()
        .map(|id| get(*id).summary())
        .collect::<Vec<_>>();
    values.sort_by_key(|value| value.id.as_str());
    values
}

pub fn owned_by(path: &str) -> Vec<ContractSummary<'static>> {
    summaries()
        .into_iter()
        .filter(|summary| summary.owners.contains(&path))
        .collect()
}

/// Renders the checked reference block for every registered TSV projection.
#[must_use]
pub fn render_tsv_markdown() -> String {
    use super::discovery::ContractSpec;

    let mut output = String::from(
        "| Contract | Command | Columns in order | Ordering | Coverage |\n\
         |---|---|---|---|---|\n",
    );
    for summary in summaries()
        .into_iter()
        .filter(|summary| summary.kind == super::discovery::ContractKind::Tsv)
    {
        let ContractSpec::Tsv(spec) = get(summary.id) else {
            continue;
        };
        let columns = spec
            .columns
            .iter()
            .map(|column| format!("`{}`", column.name))
            .collect::<Vec<_>>()
            .join(", ");
        output.push_str(&format!(
            "| `{}` | `{}` | {} | {} | {} |\n",
            spec.summary.id.as_str(),
            spec.summary.owners.join("`, `"),
            columns,
            spec.ordering,
            spec.coverage
        ));
    }
    output
}
