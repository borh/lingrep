/// Immutable parent context inherited by an admitted text unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextUnitProvenance {
    source_identity: [u8; 32],
    document_identity: u64,
    paragraph_identity: Option<u64>,
    metadata_digest: [u8; 32],
}

impl TextUnitProvenance {
    #[must_use]
    pub const fn new(
        source_identity: [u8; 32],
        document_identity: u64,
        paragraph_identity: Option<u64>,
        metadata_digest: [u8; 32],
    ) -> Self {
        Self {
            source_identity,
            document_identity,
            paragraph_identity,
            metadata_digest,
        }
    }

    #[must_use]
    pub const fn anonymous() -> Self {
        Self::new([0; 32], 0, None, [0; 32])
    }

    #[must_use]
    pub const fn source_identity(&self) -> &[u8; 32] {
        &self.source_identity
    }

    #[must_use]
    pub const fn document_identity(&self) -> u64 {
        self.document_identity
    }

    #[must_use]
    pub const fn paragraph_identity(&self) -> Option<u64> {
        self.paragraph_identity
    }

    #[must_use]
    pub const fn metadata_digest(&self) -> &[u8; 32] {
        &self.metadata_digest
    }

    pub(crate) fn hash_into(&self, hasher: &mut blake3::Hasher) {
        hasher.update(&self.source_identity);
        hasher.update(&self.document_identity.to_le_bytes());
        match self.paragraph_identity {
            Some(identity) => {
                hasher.update(&[1]);
                hasher.update(&identity.to_le_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        }
        hasher.update(&self.metadata_digest);
    }
}

impl Default for TextUnitProvenance {
    fn default() -> Self {
        Self::anonymous()
    }
}
