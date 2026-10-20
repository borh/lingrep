//! The declared corpora and their per-corpus state.
//!
//! Per-corpus state is deliberately narrow: a runtime and a context memo. Every
//! expensive resource lives on `ServerContext`, which is what makes a slot cheap
//! enough to construct for every declared corpus at startup and build lazily.

use std::sync::Arc;

use indexmap::IndexMap;
use tokio::sync::Mutex;

use crate::corpus_context::CorpusContextView;
use crate::corpus_runtime::{CorpusIdentity, CorpusRuntime};

/// Longest accepted corpus key.
const MAX_NAME_LEN: usize = 64;

/// A corpus's protocol key: what `k=` carries and what registry lookups use.
///
/// Constrained to `[A-Za-z0-9_-]` so it passes the strict permalink parser
/// without relaxing it and needs no URL escaping. This is *not* the name shown
/// to a researcher — see `CorpusSlot::display_name` — because a corpus file may
/// be called `日本語コーパス.jsonl`, which is a fine filename and an impossible
/// key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CorpusName(String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameError {
    Empty,
    TooLong,
    InvalidChar(char),
    Reserved,
}

impl std::fmt::Display for NameError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(formatter, "a corpus name must not be empty"),
            Self::TooLong => {
                write!(
                    formatter,
                    "a corpus name must be at most {MAX_NAME_LEN} characters"
                )
            }
            Self::InvalidChar(found) => write!(
                formatter,
                "a corpus name may contain only ASCII letters, digits, '-' and '_'; found {found:?}"
            ),
            Self::Reserved => write!(
                formatter,
                "'{}' is reserved for the --input corpus and may not be declared",
                CorpusName::DEFAULT
            ),
        }
    }
}

impl CorpusName {
    /// Key of the corpus supplied by `--input`.
    pub const DEFAULT: &'static str = "default";

    /// Validate a key arriving over the wire (the permalink's `k`).
    ///
    /// Admits `default`, because a v2 link for the default corpus carries
    /// `k=default` and must be readable. Reserved-ness is a property of
    /// *declaration*, not of the name — see [`CorpusName::parse_declared`].
    pub fn parse(raw: &str) -> Result<Self, NameError> {
        if raw.is_empty() {
            return Err(NameError::Empty);
        }
        if raw.chars().count() > MAX_NAME_LEN {
            return Err(NameError::TooLong);
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
        {
            return Err(NameError::InvalidChar(bad));
        }
        Ok(Self(raw.to_owned()))
    }

    /// Validate a key an *operator* supplied via `--corpus NAME=PATH`.
    ///
    /// Everything [`CorpusName::parse`] enforces, plus a refusal of the reserved
    /// `default` key, which `--input` already occupies.
    pub fn parse_declared(raw: &str) -> Result<Self, NameError> {
        let parsed = Self::parse(raw)?;
        if parsed.as_str() == Self::DEFAULT {
            return Err(NameError::Reserved);
        }
        Ok(parsed)
    }

    /// The default key, for the corpus `--input` declares.
    #[must_use]
    pub fn default_key() -> Self {
        Self(Self::DEFAULT.to_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One declared corpus. Everything expensive is on `ServerContext`.
///
/// A slot carries its own `name`, which the set's key also holds. The
/// duplication is deliberate: it makes a resolved slot a *complete* answer to
/// "which corpus", so no caller has to pass a key and a slot as two arguments
/// that could be mismatched.
pub struct CorpusSlot {
    pub name: CorpusName,
    pub runtime: Arc<CorpusRuntime>,
    /// Shown to a researcher; any UTF-8, never the path.
    pub display_name: String,
    context: Mutex<Option<(CorpusIdentity, Arc<CorpusContextView>)>>,
}

impl CorpusSlot {
    #[must_use]
    pub fn new(name: CorpusName, runtime: Arc<CorpusRuntime>, display_name: String) -> Self {
        Self {
            name,
            runtime,
            display_name,
            context: Mutex::new(None),
        }
    }

    /// The per-corpus context memo slot, in the shape `identity_memo` expects.
    #[must_use]
    pub fn context_slot(&self) -> &Mutex<Option<(CorpusIdentity, Arc<CorpusContextView>)>> {
        &self.context
    }
}

/// The declared corpora, in declaration order, with the default first-resolved.
pub struct CorpusSet {
    slots: IndexMap<CorpusName, CorpusSlot>,
    default_name: CorpusName,
}

impl CorpusSet {
    /// Build from declaration-ordered pairs. The first entry is the default.
    ///
    /// # Panics
    /// If `slots` is empty; a server with no corpus cannot start, which
    /// `Config::validate` rejects before this is reached.
    #[must_use]
    pub fn new(slots: IndexMap<CorpusName, CorpusSlot>) -> Self {
        let default_name = slots
            .keys()
            .next()
            .expect("a validated configuration declares at least one corpus")
            .clone();
        Self {
            slots,
            default_name,
        }
    }

    /// Resolve a requested key, or the default when none is given.
    /// `None` means the key was supplied and is not declared.
    #[must_use]
    pub fn resolve(&self, name: Option<&str>) -> Option<&CorpusSlot> {
        match name {
            None => self.slots.get(&self.default_name),
            // The one place `CorpusName`'s validator is bypassed, and soundly:
            // this constructs a LOOKUP KEY, never a stored name. Every name in
            // `slots` was validated by `parse`/`parse_declared` at construction,
            // so an unvalidated key cannot equal one — an invalid key simply
            // misses the map. That is the behaviour the handlers want: `resolve`
            // is deliberately two-valued, so a malformed key and an undeclared
            // key are one answer, `None`, and startup owns validation.
            //
            // Validating here would buy nothing and cost a scan of client data
            // on every request. Do not "fix" this into a `parse` call, and do
            // not copy the pattern anywhere the name is retained.
            Some(key) => self.slots.get(&CorpusName(key.to_owned())),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&CorpusName, &CorpusSlot)> {
        self.slots.iter()
    }
}
