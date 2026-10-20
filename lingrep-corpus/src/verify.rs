use std::collections::HashMap;

use futures::TryStreamExt as _;
use lingrep_core::corpus::InventoryStats;
use sqlx::{Connection, Row, SqliteConnection, sqlite::SqliteConnectOptions};

use crate::{
    CorpusArtifact, CorpusError,
    enhanced::ENHANCED_NAME,
    locations::MappedLocations,
    manifest::{
        AdmittedInventoryHasher, LOCATIONS_NAME, PROVENANCE_NAME, TOKENS_NAME, component_fact,
    },
    tokens::TokenReader,
};

/// One manifest/component integrity comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComponentVerification {
    pub name: String,
    pub length_matches: bool,
    pub blake3_matches: bool,
}

/// Result of a complete opt-in artifact integrity scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerificationReport {
    pub components: Vec<ComponentVerification>,
    pub compared_location_rows: u64,
}

impl CorpusArtifact {
    /// Hashes every component and cross-checks every dense/SQLite location tuple.
    pub async fn verify_integrity(&self) -> Result<VerificationReport, CorpusError> {
        let mut components = Vec::with_capacity(self.manifest.components.len());
        for (name, expected) in &self.manifest.components {
            let actual = component_fact(&self.root.join(name))?;
            let length_matches = actual.length == expected.length;
            let blake3_matches = actual.blake3 == expected.blake3;
            components.push(ComponentVerification {
                name: name.clone(),
                length_matches,
                blake3_matches,
            });
            if !length_matches {
                return Err(CorpusError::ComponentLengthMismatch {
                    component: component_name(name),
                });
            }
            if !blake3_matches {
                return Err(CorpusError::ComponentChecksumMismatch {
                    component: component_name(name),
                });
            }
        }

        let reader = TokenReader::open(
            &self.root.join(TOKENS_NAME),
            &self.analysis_identity,
            &self.physical_token_schema,
        )?;
        reader
            .verify_integrity()
            .map_err(|_| CorpusError::ComponentChecksumMismatch {
                component: TOKENS_NAME,
            })?;
        if let Some(enhanced) = &self.enhanced {
            enhanced
                .verify_integrity()
                .map_err(|_| CorpusError::ComponentChecksumMismatch {
                    component: ENHANCED_NAME,
                })?;
        }
        if let Some(postings) = &self.postings {
            postings.verify()?;
        }
        let locations =
            MappedLocations::open(&self.root.join(LOCATIONS_NAME), &self.analysis_identity)?;
        locations
            .verify_integrity()
            .map_err(|_| CorpusError::ComponentChecksumMismatch {
                component: LOCATIONS_NAME,
            })?;
        self.verify_admitted_document_inventory(&locations)?;
        let provenance = self.provenance_view().await?;
        drop(provenance);
        self.verify_location_rows(&locations).await?;
        Ok(VerificationReport {
            components,
            compared_location_rows: locations.sentence_count(),
        })
    }

    fn verify_admitted_document_inventory(
        &self,
        locations: &MappedLocations,
    ) -> Result<(), CorpusError> {
        // locations.v1 remains the sole per-document location authority. The
        // manifest retains only an independently produced logical summary and
        // digest; physical discovery inventory remains a separate contract.
        let mut groups = HashMap::<u64, u64>::new();
        let mut external_targets = 0_u64;
        let mut digest = AdmittedInventoryHasher::new();
        for ordinal in 0..locations.document_count() {
            let document = locations.document(ordinal)?;
            if !groups.contains_key(&document.target_group_ordinal)
                && document.target_group_ordinal != groups.len() as u64
            {
                return Err(CorpusError::DiscoveryReportMismatch);
            }
            *groups.entry(document.target_group_ordinal).or_default() += 1;
            external_targets = external_targets
                .checked_add(u64::from(document.external_target))
                .ok_or(CorpusError::DiscoveryReportMismatch)?;
            digest.push(
                &document.source_name,
                document.target_group_ordinal,
                document.external_target,
            )?;
        }
        for ordinal in 0..locations.document_count() {
            let document = locations.document(ordinal)?;
            if groups.get(&document.target_group_ordinal).copied()
                != Some(document.target_multiplicity)
            {
                return Err(CorpusError::DiscoveryReportMismatch);
            }
        }
        let documents = locations.document_count();
        let distinct_targets = groups.len() as u64;
        let actual = InventoryStats {
            documents,
            distinct_targets,
            alias_documents: documents
                .checked_sub(distinct_targets)
                .ok_or(CorpusError::DiscoveryReportMismatch)?,
            max_target_multiplicity: groups.values().copied().max().unwrap_or(0),
            external_targets,
        };
        if actual != self.manifest.admitted_inventory()
            || digest.finish() != self.manifest.admitted_inventory_digest()?
        {
            return Err(CorpusError::DiscoveryReportMismatch);
        }
        Ok(())
    }

    async fn verify_location_rows(&self, locations: &MappedLocations) -> Result<(), CorpusError> {
        let options = SqliteConnectOptions::new()
            .filename(self.root.join(PROVENANCE_NAME))
            .read_only(true)
            .immutable(true);
        let mut connection = SqliteConnection::connect_with(&options)
            .await
            .map_err(database)?;
        let mut rows = sqlx::query(
            "SELECT sentence_id, source_name, document_id, document_ordinal,
                    document_sentence_ordinal
             FROM sentence_provenance
             ORDER BY sentence_id",
        )
        .fetch(&mut connection);
        let mut ordinal = 0_u64;
        while let Some(row) = rows.try_next().await.map_err(database)? {
            let location = locations.sentence(ordinal)?;
            let sqlite_sentence = sqlite_u64(&row, "sentence_id")?;
            let sqlite_source: String = row.try_get("source_name").map_err(database)?;
            let sqlite_document = sqlite_u64(&row, "document_id")?;
            let sqlite_document_ordinal = sqlite_u64(&row, "document_ordinal")?;
            let sqlite_local_ordinal = sqlite_u64(&row, "document_sentence_ordinal")?;
            if sqlite_sentence != ordinal
                || sqlite_source != location.source_name.as_ref()
                || sqlite_document != location.document_id.get()
                || sqlite_document_ordinal != location.document_ordinal
                || sqlite_local_ordinal != location.document_sentence_ordinal
            {
                return Err(CorpusError::VerificationMismatch {
                    component: "locations.v1/provenance.sqlite",
                    sentence_ordinal: Some(ordinal),
                    message: "dense and SQLite sentence-location tuples differ".into(),
                });
            }
            ordinal = ordinal
                .checked_add(1)
                .ok_or_else(|| CorpusError::VerificationMismatch {
                    component: "locations.v1/provenance.sqlite",
                    sentence_ordinal: None,
                    message: "location row count overflow".into(),
                })?;
        }
        drop(rows);
        connection.close().await.map_err(database)?;
        if ordinal != locations.sentence_count() {
            return Err(CorpusError::VerificationMismatch {
                component: "locations.v1/provenance.sqlite",
                sentence_ordinal: Some(ordinal),
                message: "dense and SQLite sentence counts differ".into(),
            });
        }
        Ok(())
    }
}

fn sqlite_u64(row: &sqlx::sqlite::SqliteRow, field: &str) -> Result<u64, CorpusError> {
    let value: i64 = row.try_get(field).map_err(database)?;
    u64::try_from(value).map_err(|_| CorpusError::VerificationMismatch {
        component: "locations.v1/provenance.sqlite",
        sentence_ordinal: None,
        message: format!("{field} is negative"),
    })
}

fn database(error: sqlx::Error) -> CorpusError {
    CorpusError::InvalidFormat {
        component: PROVENANCE_NAME,
        message: error.to_string(),
    }
}

fn component_name(name: &str) -> &'static str {
    match name {
        TOKENS_NAME => TOKENS_NAME,
        LOCATIONS_NAME => LOCATIONS_NAME,
        PROVENANCE_NAME => PROVENANCE_NAME,
        ENHANCED_NAME => ENHANCED_NAME,
        crate::postings::NAME => crate::postings::NAME,
        _ => "artifact",
    }
}
