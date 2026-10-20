//! The BCCWJ half of the corpus seams, shared by everything that walks a
//! distribution.
//!
//! Three callers walk the same inventory — the direct query stream
//! ([`crate::direct_source`]), the prepared build
//! ([`crate::prepared_source`]), and `corpus inventory`
//! ([`crate::commands::corpus`]) — and they must agree on what a discovered
//! source *is*: which zip is ordinal `n`, which target group it belongs to,
//! and what the inventory digest over the whole set comes out as. That
//! agreement is load-bearing: `finish_guarded` refuses a build whose report
//! disagrees with the builder's own derivation, and
//! [`crate::direct_source::validate_ordered_inventory`] refuses a run whose
//! read order disagrees with discovery.
//!
//! [`BccwjCursor`] centralizes the two shared invariants: *ordinal indexes the
//! zip vector* and *each zip is its own target group*.

use std::io;
use std::path::{Path, PathBuf};

use blake3::Hasher;
use lingrep_core::corpus::bccwj::{BccwjInventory, BccwjMetadataLock, BccwjZip};
use lingrep_core::corpus::{CorpusEvent, DiscoveryReport, MarkdownDiagnostics, SourceDocument};
use lingrep_corpus::SourceObservation;

use crate::direct_source::{OrderedInventoryObservation, identity_io_error};

/// One BCCWJ distribution's zips, walked once in inventory order.
///
/// The cursor owns two invariants so the seams do not restate them:
///
/// - a handed-out document's `ordinal` indexes [`Self::zip_for`], which is
///   what lets a caller recover the zip without the producer stashing it for
///   the consumer; and
/// - `target_group_ordinal == ordinal`, because a target is a distinct
///   physical file and the zips are distinct files. See
///   [`bccwj_inventory_summary`].
pub(crate) struct BccwjCursor {
    zips: Vec<BccwjZip>,
    next: usize,
}

impl BccwjCursor {
    pub(crate) fn new(zips: Vec<BccwjZip>) -> Self {
        Self { zips, next: 0 }
    }

    /// The next zip as a discovered source, or `None` once the inventory is
    /// drained.
    pub(crate) fn next_document(&mut self) -> Option<SourceDocument> {
        let zip = self.zips.get(self.next)?;
        let ordinal = self.next as u64;
        self.next += 1;
        Some(SourceDocument {
            ordinal,
            logical_name: zip.logical_name.clone(),
            path: zip.path.clone(),
            target_group_ordinal: ordinal,
            external_target: false,
        })
    }

    /// The zip a handed-out document came from.
    ///
    /// # Errors
    ///
    /// A document this cursor did not produce. Unreachable through the
    /// seams, which pass back what [`Self::next_document`] just returned;
    /// it is an error rather than a panic so a future caller that gets it
    /// wrong fails loudly instead of indexing past the end.
    pub(crate) fn zip_for(&self, document: &SourceDocument) -> io::Result<&BccwjZip> {
        usize::try_from(document.ordinal)
            .ok()
            .and_then(|index| self.zips.get(index))
            .ok_or_else(|| {
                io::Error::other(format!(
                    "bccwj document ordinal {} is outside this inventory",
                    document.ordinal
                ))
            })
    }

    /// The discovery report over the whole inventory, drained or not.
    pub(crate) fn report(&self) -> DiscoveryReport {
        let (inventory, ordered_inventory_digest) =
            bccwj_inventory_summary(self.zips.iter().map(|zip| zip.logical_name.as_str()));
        DiscoveryReport {
            inventory,
            configured_limits: Default::default(),
            observed: Default::default(),
            ordered_inventory_digest,
        }
    }
}

/// The per-run BCCWJ facts every zip of one distribution shares.
///
/// Holding them together is what makes "a BCCWJ stream is open" a single
/// fact. The direct seam used to keep the root and the metadata lock as two
/// independent `Option` fields beside the cursor, so the type admitted a
/// BCCWJ walk with no table to join against and guarded that state at
/// runtime.
pub(crate) struct BccwjStream {
    root: PathBuf,
    /// Built once on the first zip and shared by every later one.
    metadata: BccwjMetadataLock,
    /// The run's `max_input_record_bytes`, applied per C-XML entry.
    record_limit: usize,
}

impl BccwjStream {
    pub(crate) fn new(root: PathBuf, record_limit: usize) -> Self {
        Self {
            root,
            metadata: BccwjMetadataLock::default(),
            record_limit,
        }
    }

    /// Opens one zip's document stream on one handle: stat, hash for
    /// identity, rewind, parse. The zip is the logical *source* (one
    /// `begin_source`, and one ordered-inventory observation when the caller
    /// keeps a live inventory); its entries are the *documents*, one
    /// `DocumentStart` each.
    ///
    /// The zip is the only identifier passed: its path, logical name and
    /// sampling all travel together, so no caller can pair one zip's path
    /// with another's name.
    ///
    /// # Errors
    ///
    /// The handle cannot be opened or stat-ed, the identity observation
    /// rejects the source, or the zip is not a readable container.
    pub(crate) fn open(
        &self,
        source: &SourceObservation,
        inventory: Option<&OrderedInventoryObservation>,
        zip: &BccwjZip,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
        use std::io::Seek;

        let mut file = std::fs::File::open(&zip.path)?;
        let integrity = lingrep_core::corpus::bccwj::zip_file_stat(&file)?;
        if let Some(inventory) = inventory {
            inventory
                .observe(&zip.logical_name)
                .map_err(identity_io_error)?;
        }
        // Drain the same handle that is parsed, for the content observation.
        let mut hasher =
            crate::direct_source::HashingReader::new(&file, source.clone(), &zip.logical_name)
                .map_err(identity_io_error)?;
        io::copy(&mut hasher, &mut io::sink())?;
        file.rewind()?;
        let events = lingrep_core::corpus::bccwj::BccwEvents::new(
            file,
            zip,
            &self.root,
            self.metadata.clone(),
            integrity,
            self.record_limit,
        )?;
        Ok(Box::new(events))
    }

    /// The same open, with the sentence labels fed to the run's unused-label
    /// sink.
    ///
    /// The Markdown dialect feeds that sink from inside its reader; the
    /// BCCWJ reader has no such hook, so the seam observes them on the way
    /// past.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub(crate) fn open_observing_labels(
        &self,
        source: &SourceObservation,
        inventory: Option<&OrderedInventoryObservation>,
        zip: &BccwjZip,
        diagnostics: &MarkdownDiagnostics,
    ) -> io::Result<Box<dyn Iterator<Item = io::Result<CorpusEvent>> + Send>> {
        let events = self.open(source, inventory, zip)?;
        Ok(Box::new(LabelObserving {
            inner: events,
            diagnostics: diagnostics.clone(),
        }))
    }
}

struct LabelObserving<I> {
    inner: I,
    diagnostics: MarkdownDiagnostics,
}

impl<I> Iterator for LabelObserving<I>
where
    I: Iterator<Item = io::Result<CorpusEvent>>,
{
    type Item = io::Result<CorpusEvent>;

    fn next(&mut self) -> Option<Self::Item> {
        let event = self.inner.next()?;
        if let Ok(CorpusEvent::Sentence { active_labels, .. }) = &event {
            for label in active_labels.as_ref() {
                self.diagnostics.observe_label(label);
            }
        }
        Some(event)
    }
}

/// The discovery-level stats and digest one BCCWJ inventory derives: the
/// zips are the discovered sources, each its own target group.
///
/// A *target* is a distinct physical file, and `alias_documents` counts
/// logical documents that resolve onto one — the double-counted content a
/// symlinked Markdown root produces, which is what `summarize` warns about.
/// The zips of a distribution are distinct files with distinct content, so
/// the honest figures are one target each and no aliases; filing them all in
/// group 0 made every multi-zip run report `alias_documents = zips - 1` and
/// draw that warning. Like a JSONL file, one zip carries many admitted
/// documents — that ratio is the builder's *admitted* multiplicity, not an
/// alias.
///
/// The digest is the builder's `discovery_digest` over the same names —
/// each logical name as a little-endian u64 length prefix plus the UTF-8
/// bytes, in registration (byte-sorted zip) order, no other framing. The
/// build's `finish_guarded` checks the report against exactly that value,
/// and `validate_ordered_inventory` checks a run's observed read order
/// against it, so every report over one inventory must use this one shape.
pub(crate) fn bccwj_inventory_summary<'a>(
    zip_names: impl IntoIterator<Item = &'a str>,
) -> (lingrep_core::corpus::InventoryStats, [u8; 32]) {
    let mut hasher = Hasher::new();
    let mut documents = 0_u64;
    for name in zip_names {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        documents = documents.saturating_add(1);
    }
    let digest = *hasher.finalize().as_bytes();
    (
        lingrep_core::corpus::InventoryStats {
            documents,
            distinct_targets: documents,
            alias_documents: 0,
            // One document per target; the generic walker leaves this 0 for
            // an empty inventory, which cannot happen here (`discover`
            // refuses an empty selection) but keeps the shape total.
            max_target_multiplicity: documents.min(1),
            external_targets: 0,
        },
        digest,
    )
}

/// Observe the BCCWJ `DOC/` context files (byte-sorted, the inventory
/// order) through the source observation, before the first zip's
/// `begin_source`. Called by both the naming pass and the build pass — and
/// by direct execution — so the three cannot disagree about what was
/// observed or in what order.
///
/// # Errors
///
/// A context file outside the root, unreadable, or rejected by the
/// observation's phase rules.
pub(crate) fn observe_bccwj_context_files(
    inventory: &BccwjInventory,
    root: &Path,
    observation: &SourceObservation,
) -> io::Result<()> {
    for path in &inventory.context_files {
        let logical_name = path
            .strip_prefix(root)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "bccwj context file {} is not under the root",
                        path.display()
                    ),
                )
            })?
            .to_string_lossy()
            .into_owned();
        let bytes = std::fs::read(path).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("bccwj context file {}: {error}", path.display()),
            )
        })?;
        observation
            .observe_context_file(&logical_name, &bytes)
            .map_err(identity_io_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report a cursor publishes and the order a run actually reads in
    /// must be two independent derivations that agree.
    ///
    /// `validate_ordered_inventory` compares them, so this is what makes
    /// that check meaningful rather than tautological — the direct seam used
    /// to build the report *from* the observation, and the comparison could
    /// not fail. It holds only while both frame a name as
    /// `len_le_u64 + utf8`; this fails if either framing moves.
    #[test]
    fn a_run_that_reads_the_inventory_in_order_validates_against_the_report() {
        let names = [
            "C-XML/FIXED/LB/LB.zip",
            "C-XML/FIXED/PN/PN.zip",
            "C-XML/VARIABLE/OY/OY.zip",
        ];
        let report = BccwjCursor::new(names.iter().copied().map(zip).collect()).report();

        let read_in_order = OrderedInventoryObservation::default();
        for name in names {
            read_in_order.observe(name).expect("observe");
        }
        crate::direct_source::validate_ordered_inventory(&read_in_order, &report)
            .expect("a run that read the inventory in order validates");

        // And the check can actually fail: the same names in a different
        // order must not validate.
        let read_reordered = OrderedInventoryObservation::default();
        for name in [names[1], names[0], names[2]] {
            read_reordered.observe(name).expect("observe");
        }
        crate::direct_source::validate_ordered_inventory(&read_reordered, &report)
            .expect_err("a reordered read must not validate against the discovered inventory");
    }

    fn zip(logical: &str) -> BccwjZip {
        BccwjZip {
            path: PathBuf::from("/dist").join(logical),
            logical_name: logical.to_owned(),
            sampling: lingrep_core::corpus::bccwj::Sampling::Fixed,
            subcorpus: lingrep_core::corpus::bccwj::Subcorpus::LB,
        }
    }

    #[test]
    fn the_cursor_hands_out_each_zip_once_as_its_own_target() {
        let names = ["C-XML/FIXED/LB/LB.zip", "C-XML/FIXED/PN/PN.zip"];
        let mut cursor = BccwjCursor::new(names.iter().copied().map(zip).collect());

        let first = cursor.next_document().expect("first zip");
        let second = cursor.next_document().expect("second zip");
        assert!(cursor.next_document().is_none(), "the inventory is drained");

        for (ordinal, document) in [&first, &second].into_iter().enumerate() {
            assert_eq!(document.ordinal, ordinal as u64);
            // The invariant the seams depend on: a target is a distinct
            // physical file, and each zip is one.
            assert_eq!(document.target_group_ordinal, document.ordinal);
            assert!(!document.external_target);
            assert_eq!(document.logical_name, names[ordinal]);
        }
    }

    #[test]
    fn a_document_recovers_its_zip_without_the_cursor_stashing_it() {
        let mut cursor = BccwjCursor::new(vec![
            zip("C-XML/FIXED/LB/LB.zip"),
            zip("C-XML/FIXED/PN/PN.zip"),
        ]);
        let first = cursor.next_document().expect("first zip");
        // Advance past it: the lookup is by the document's own ordinal, not
        // by where the cursor happens to be standing.
        let _second = cursor.next_document().expect("second zip");
        assert_eq!(
            cursor.zip_for(&first).expect("zip").logical_name,
            "C-XML/FIXED/LB/LB.zip"
        );

        let mut stranger = first.clone();
        stranger.ordinal = 7;
        let error = cursor.zip_for(&stranger).expect_err("not from this cursor");
        assert!(
            error.to_string().contains("outside this inventory"),
            "{error}"
        );
    }

    #[test]
    fn the_report_is_the_same_whenever_it_is_taken() {
        let mut cursor = BccwjCursor::new(vec![
            zip("C-XML/FIXED/LB/LB.zip"),
            zip("C-XML/FIXED/PN/PN.zip"),
        ]);
        let before = cursor.report();
        while cursor.next_document().is_some() {}
        let after = cursor.report();
        assert_eq!(before.inventory, after.inventory);
        assert_eq!(
            before.ordered_inventory_digest,
            after.ordered_inventory_digest
        );
        assert_eq!(after.inventory.documents, 2);
        assert_eq!(after.inventory.distinct_targets, 2);
        assert_eq!(after.inventory.alias_documents, 0);
    }
}
