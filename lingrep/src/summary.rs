//! Exact bounded aggregation for `(authored pattern, document, typed group)` keys,
//! and for per-pattern counts under each section label or document metadata value.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::io;
use std::sync::Arc;
use std::thread::JoinHandle;

use compact_str::CompactString;
use crossbeam_channel::{Receiver, Sender};
use indexmap::IndexSet;
use lingrep_core::execution::CancellationFlag;
use lingrep_core::fsm::{LexicalProjectedKeyField, LexicalProjectedKeyValue, ProjectedKeyField};
use lingrep_core::spill::{SortedDrain, SortedRunCounts, SpillPolicy, TempBudget};
use serde::{Deserialize, Serialize};

use crate::protocol::query::{MetadataValueData, PatternCount, SectionData};

/// Keys held in the scan's sorted buffer before it spills a run.
///
/// This is a memory-for-passes trade: a bigger buffer merges more duplicates
/// before anything is written, and leaves fewer runs to compact, so each record
/// crosses the codec and the compressor fewer times. It was worth almost
/// nothing while a key owned its strings — the buffer was expensive per key and
/// deepened every insert into a `BTreeMap` of string tuples. Interning made a
/// key an integer tuple and changed that answer: 16× this buffer buys 13% of
/// the n-gram run for 35% more peak RSS on the catalog that spills most, which
/// is 0.72× the prepared artifact it reads. Larger still keeps paying, but
/// less.
const DEFAULT_RESIDENT_KEYS: usize = 65_536;
const DEFAULT_MERGE_FAN_IN: usize = 16;

/// Keys carried per message from the scan to a counting shard.
///
/// Big enough that channel synchronization is amortized to noise, small
/// enough that a batch is a bounded allocation and backpressure engages
/// within a fraction of a second of a shard falling behind.
const AGGREGATION_BATCH_KEYS: usize = 4096;

/// Batches a shard's queue holds before the scan blocks on it.
const SHARD_QUEUE_BATCHES: usize = 4;

/// A projected key value, held as a dense index rather than as its text.
///
/// The scan phase assigns these in first-encounter order, so their numeric
/// order is arbitrary and the scan's sort is a grouping, not a presentation.
/// [`SummaryAggregator::finalize`] rewrites every one into its lexicographic
/// rank before the ordering phase, which is what keeps the published order
/// identical to comparing the strings themselves.
type ValueId = u32;

/// The values a catalog's keys are drawn from, each held once.
///
/// A summary key is a tuple of token attribute values, and those values come
/// from a corpus vocabulary orders of magnitude smaller than the record count:
/// `patterns/ngrams.lx` over `sample-science` projects 34 million values drawn
/// from 24 thousand distinct ones. Interning turns every comparison, spill
/// encode and spill decode of a key into integer work.
///
/// The hasher is the one `lingrep_corpus::tokens` already interns these same corpus
/// strings with.
#[derive(Default)]
struct ValueInterner {
    values: IndexSet<Box<str>, BuildHasherDefault<rustc_hash::FxHasher>>,
}

impl ValueInterner {
    /// Borrows, so a repeat sighting — overwhelmingly the common case — costs
    /// a hash and a probe and nothing else, and only a first sighting
    /// allocates. This is why [`AggregationInput`] hands over the record's own
    /// fields rather than a clone of them.
    fn intern(&mut self, value: &str) -> io::Result<ValueId> {
        let index = match self.values.get_index_of(value) {
            Some(index) => index,
            None => self.values.insert_full(Box::from(value)).0,
        };
        ValueId::try_from(index).map_err(|_| value_count_overflow())
    }
}

fn value_count_overflow() -> io::Error {
    io::Error::other("summary interned value count exceeds u32")
}

fn remap_value(value: ValueId, ranks: &[ValueId]) -> io::Result<ValueId> {
    ranks
        .get(value as usize)
        .copied()
        .ok_or_else(|| io::Error::other("summary key value is absent from the canonical table"))
}

fn resolve_value(value: ValueId, values: &[Box<str>]) -> io::Result<String> {
    values
        .get(value as usize)
        .map(|value| value.as_ref().to_owned())
        .ok_or_else(|| io::Error::other("summary key value is out of range"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SummaryPartition {
    pub pattern_ordinal: u64,
    pub document_ordinal: Option<u64>,
    pub sentence_ordinal: Option<u64>,
}

impl SummaryPartition {
    /// The partition of a summary that is not split by document.
    #[must_use]
    pub const fn pattern(pattern_ordinal: u64) -> Self {
        Self {
            pattern_ordinal,
            document_ordinal: None,
            sentence_ordinal: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SummaryRow {
    pub partition: SummaryPartition,
    pub rank: u64,
    pub group: AggregationGroup,
    pub count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct SummaryKey {
    partition: SummaryPartition,
    group: StoredGroup,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct SummaryOrder {
    partition: SummaryPartition,
    count: usize,
    group: StoredGroup,
}

/// Spill representation. Neither a key's field names nor its values are stored
/// as text: names are a catalog-level schema and values are drawn from a corpus
/// vocabulary, so both are small integer identities here and a spilled record is
/// proportional to the key's width rather than to the length of its strings.
#[derive(Clone, Debug, Serialize, Deserialize)]
enum StoredGroup {
    Surface(ValueId),
    Unit(ValueId),
    Term(ValueId),
    Key {
        schema: u32,
        values: Vec<ValueId>,
    },
    LexicalKey {
        schema: u32,
        values: Vec<StoredLexicalValue>,
    },
}

/// [`LexicalProjectedKeyValue`] with its strings interned.
///
/// `Scalar` precedes `Segments`, which is both the derived order of the value
/// this mirrors and what [`cmp_mixed_key`] assumes of an ordinary key value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum StoredLexicalValue {
    Scalar(ValueId),
    Segments(Vec<ValueId>),
}

impl PartialEq for StoredGroup {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for StoredGroup {}

impl PartialOrd for StoredGroup {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for StoredGroup {
    fn cmp(&self, other: &Self) -> Ordering {
        fn rank(group: &StoredGroup) -> u8 {
            match group {
                StoredGroup::Surface(_) => 0,
                StoredGroup::Unit(_) => 1,
                StoredGroup::Term(_) => 2,
                StoredGroup::Key { .. } | StoredGroup::LexicalKey { .. } => 3,
            }
        }
        rank(self)
            .cmp(&rank(other))
            .then_with(|| match (self, other) {
                (Self::Surface(a), Self::Surface(b))
                | (Self::Unit(a), Self::Unit(b))
                | (Self::Term(a), Self::Term(b)) => a.cmp(b),
                (
                    Self::Key {
                        schema: a_schema,
                        values: a,
                    },
                    Self::Key {
                        schema: b_schema,
                        values: b,
                    },
                ) => a_schema.cmp(b_schema).then_with(|| a.cmp(b)),
                (
                    Self::LexicalKey {
                        schema: a_schema,
                        values: a,
                    },
                    Self::LexicalKey {
                        schema: b_schema,
                        values: b,
                    },
                ) => a_schema.cmp(b_schema).then_with(|| a.cmp(b)),
                (
                    Self::Key {
                        schema: a_schema,
                        values: a,
                    },
                    Self::LexicalKey {
                        schema: b_schema,
                        values: b,
                    },
                ) => a_schema.cmp(b_schema).then_with(|| cmp_mixed_key(a, b)),
                (
                    Self::LexicalKey {
                        schema: a_schema,
                        values: a,
                    },
                    Self::Key {
                        schema: b_schema,
                        values: b,
                    },
                ) => a_schema
                    .cmp(b_schema)
                    .then_with(|| cmp_mixed_key(b, a).reverse()),
                _ => Ordering::Equal,
            })
    }
}

fn cmp_mixed_key(ordinary: &[ValueId], lexical: &[StoredLexicalValue]) -> Ordering {
    for (left, right) in ordinary.iter().zip(lexical) {
        let order = match right {
            StoredLexicalValue::Scalar(right) => left.cmp(right),
            StoredLexicalValue::Segments(_) => Ordering::Less,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    ordinary.len().cmp(&lexical.len())
}

impl StoredGroup {
    /// Rewrites both identities into their canonical form for the ordering
    /// phase: the schema into its deduplicated index, and every value into its
    /// lexicographic rank.
    ///
    /// The rank rewrite is what makes the ordering phase's integer comparison
    /// mean string comparison, so it must happen to every value on the way in —
    /// a group left in scan ids would sort against ranked ones as if its
    /// strings were different.
    fn canonicalize(&self, schemas: &[u32], ranks: &[ValueId]) -> io::Result<Self> {
        let schema_of = |schema: &u32| -> io::Result<u32> {
            schemas.get(*schema as usize).copied().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "summary key schema is absent from the canonical remap",
                )
            })
        };
        match self {
            Self::Surface(value) => Ok(Self::Surface(remap_value(*value, ranks)?)),
            Self::Unit(value) => Ok(Self::Unit(remap_value(*value, ranks)?)),
            Self::Term(value) => Ok(Self::Term(remap_value(*value, ranks)?)),
            Self::Key { schema, values } => Ok(Self::Key {
                schema: schema_of(schema)?,
                values: values
                    .iter()
                    .map(|value| remap_value(*value, ranks))
                    .collect::<io::Result<Vec<_>>>()?,
            }),
            Self::LexicalKey { schema, values } => Ok(Self::LexicalKey {
                schema: schema_of(schema)?,
                values: values
                    .iter()
                    .map(|value| match value {
                        StoredLexicalValue::Scalar(value) => {
                            Ok(StoredLexicalValue::Scalar(remap_value(*value, ranks)?))
                        }
                        StoredLexicalValue::Segments(values) => Ok(StoredLexicalValue::Segments(
                            values
                                .iter()
                                .map(|value| remap_value(*value, ranks))
                                .collect::<io::Result<Vec<_>>>()?,
                        )),
                    })
                    .collect::<io::Result<Vec<_>>>()?,
            }),
        }
    }
}

/// A group on its way *in* to the aggregator.
///
/// This is not [`AggregationGroup`], which is a published row's group on the
/// way out. The distinction is ownership: a match record already owns its
/// projected key, and the aggregator only ever reads a value before interning
/// it, so handing over the record's own slice costs nothing while cloning it
/// into an owned group allocated a `String` per projected value per record and
/// freed it again immediately.
///
/// `Surface` is owned because it is the one group with no counterpart in the
/// record — it is rendered from the sentence span, so somebody has to hold it.
#[derive(Clone, Debug)]
pub enum AggregationInput<'a> {
    Surface(String),
    Unit(&'a str),
    Term(&'a str),
    Key(&'a [ProjectedKeyField]),
    LexicalKey(&'a [LexicalProjectedKeyField]),
}

/// An inbound group whose record crossed a partition-work boundary.
///
/// This is the aggregation key in its string form: the inline tally is a map
/// over it, the shard channel carries it, and a shard interns it on arrival.
/// Its equality — hand-written below — must therefore mean exactly what
/// [`StoredGroup`]'s does after interning, including the cross-variant rule
/// that an ordinary key equals an all-scalar lexical key with the same field
/// names and values; a divergence would split one group's count across two
/// map entries or two shards.
#[derive(Clone, Debug)]
pub(crate) enum OwnedAggregationInput {
    Surface(String),
    Unit(String),
    Term(CompactString),
    Key(Vec<ProjectedKeyField>),
    LexicalKey(Vec<LexicalProjectedKeyField>),
}

impl AggregationInput<'_> {
    /// The owned form of this group, for the inline tally or a shard channel.
    ///
    /// The parallel publication path never calls this — its workers build
    /// owned groups by moving fields out of the record — so the clone here is
    /// paid only by the serial ingress: traced runs, the pre-engagement
    /// window, and tests.
    fn into_owned(self) -> OwnedAggregationInput {
        match self {
            Self::Surface(value) => OwnedAggregationInput::Surface(value),
            Self::Unit(value) => OwnedAggregationInput::Unit(value.to_owned()),
            Self::Term(value) => OwnedAggregationInput::Term(CompactString::from(value)),
            Self::Key(fields) => OwnedAggregationInput::Key(fields.to_vec()),
            Self::LexicalKey(fields) => OwnedAggregationInput::LexicalKey(fields.to_vec()),
        }
    }
}

impl OwnedAggregationInput {
    /// Borrows this group back as the reference-shaped input a shard's
    /// [`GroupStore`] interns from.
    fn as_input(&self) -> AggregationInput<'_> {
        match self {
            Self::Surface(value) => AggregationInput::Surface(value.clone()),
            Self::Unit(value) => AggregationInput::Unit(value),
            Self::Term(value) => AggregationInput::Term(value.as_str()),
            Self::Key(fields) => AggregationInput::Key(fields),
            Self::LexicalKey(fields) => AggregationInput::LexicalKey(fields),
        }
    }

    fn variant_rank(&self) -> u8 {
        match self {
            Self::Surface(_) => 0,
            Self::Unit(_) => 1,
            Self::Term(_) => 2,
            Self::Key(_) | Self::LexicalKey(_) => 3,
        }
    }
}

/// String-level counterpart of [`cmp_mixed_key`]: an ordinary value compares
/// to a lexical scalar as the strings compare, and precedes any segments list.
fn cmp_owned_mixed(
    ordinary: &[ProjectedKeyField],
    lexical: &[LexicalProjectedKeyField],
) -> Ordering {
    for (left, right) in ordinary.iter().zip(lexical) {
        let order = match &right.value {
            LexicalProjectedKeyValue::Scalar(value) => left.value.as_str().cmp(value.as_str()),
            LexicalProjectedKeyValue::Segments(_) => Ordering::Less,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    ordinary.len().cmp(&lexical.len())
}

fn cmp_names<'a>(a: impl Iterator<Item = &'a str>, b: impl Iterator<Item = &'a str>) -> Ordering {
    a.cmp(b)
}

impl PartialEq for OwnedAggregationInput {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for OwnedAggregationInput {}

impl PartialOrd for OwnedAggregationInput {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OwnedAggregationInput {
    fn cmp(&self, other: &Self) -> Ordering {
        self.variant_rank()
            .cmp(&other.variant_rank())
            .then_with(|| match (self, other) {
                (Self::Surface(a), Self::Surface(b)) | (Self::Unit(a), Self::Unit(b)) => a.cmp(b),
                (Self::Term(a), Self::Term(b)) => a.cmp(b),
                (Self::Key(a), Self::Key(b)) => cmp_names(
                    a.iter().map(|field| field.name.as_str()),
                    b.iter().map(|field| field.name.as_str()),
                )
                .then_with(|| {
                    a.iter()
                        .map(|field| field.value.as_str())
                        .cmp(b.iter().map(|field| field.value.as_str()))
                }),
                (Self::LexicalKey(a), Self::LexicalKey(b)) => cmp_names(
                    a.iter().map(|field| field.name.as_str()),
                    b.iter().map(|field| field.name.as_str()),
                )
                .then_with(|| {
                    a.iter()
                        .map(|field| &field.value)
                        .cmp(b.iter().map(|field| &field.value))
                }),
                (Self::Key(a), Self::LexicalKey(b)) => cmp_names(
                    a.iter().map(|field| field.name.as_str()),
                    b.iter().map(|field| field.name.as_str()),
                )
                .then_with(|| cmp_owned_mixed(a, b)),
                (Self::LexicalKey(a), Self::Key(b)) => cmp_names(
                    a.iter().map(|field| field.name.as_str()),
                    b.iter().map(|field| field.name.as_str()),
                )
                .then_with(|| cmp_owned_mixed(b, a).reverse()),
                _ => Ordering::Equal,
            })
    }
}

/// The routing hash of a group, over its strings.
///
/// Written by hand for the same reason [`OwnedAggregationInput`]'s equality
/// is: equal groups must land on one shard, so a lexical scalar hashes
/// exactly as an ordinary key value does and both key variants share a tag.
/// Field names stand in for the interned schema id — equality of names is
/// equality of schema. The hash never leaves the process.
pub(crate) fn shard_hash(partition: SummaryPartition, group: &OwnedAggregationInput) -> u64 {
    let mut hasher = rustc_hash::FxHasher::default();
    partition.hash(&mut hasher);
    fn hash_key_parts<'a>(
        hasher: &mut rustc_hash::FxHasher,
        names: impl Iterator<Item = &'a str>,
        values: impl Iterator<Item = &'a LexicalProjectedKeyValue>,
    ) {
        3_u8.hash(hasher);
        for name in names {
            name.hash(hasher);
        }
        for value in values {
            match value {
                LexicalProjectedKeyValue::Scalar(value) => {
                    0_u8.hash(hasher);
                    value.hash(hasher);
                }
                LexicalProjectedKeyValue::Segments(segments) => {
                    1_u8.hash(hasher);
                    segments.hash(hasher);
                }
            }
        }
    }
    match group {
        OwnedAggregationInput::Surface(value) => {
            0_u8.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        OwnedAggregationInput::Unit(value) => {
            1_u8.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        OwnedAggregationInput::Term(value) => {
            2_u8.hash(&mut hasher);
            value.as_str().hash(&mut hasher);
        }
        OwnedAggregationInput::Key(fields) => {
            3_u8.hash(&mut hasher);
            for field in fields {
                field.name.as_str().hash(&mut hasher);
            }
            for field in fields {
                0_u8.hash(&mut hasher);
                field.value.hash(&mut hasher);
            }
        }
        OwnedAggregationInput::LexicalKey(fields) => hash_key_parts(
            &mut hasher,
            fields.iter().map(|field| field.name.as_str()),
            fields.iter().map(|field| &field.value),
        ),
    }
    hasher.finish()
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum AggregationGroup {
    Surface(String),
    Unit(String),
    Term(String),
    Key(Vec<ProjectedKeyField>),
    LexicalKey(Vec<LexicalProjectedKeyField>),
}

impl PartialEq for SummaryOrder {
    fn eq(&self, other: &Self) -> bool {
        self.partition == other.partition && self.count == other.count && self.group == other.group
    }
}

impl Eq for SummaryOrder {}

impl PartialOrd for SummaryOrder {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SummaryOrder {
    fn cmp(&self, other: &Self) -> Ordering {
        self.partition
            .cmp(&other.partition)
            .then_with(|| other.count.cmp(&self.count))
            .then_with(|| self.group.cmp(&other.group))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
struct GroupOrder {
    pattern_ordinal: u64,
    group: StoredGroup,
    document_ordinal: u64,
    sentence_ordinal: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OrderBy {
    Count,
    Group,
}

enum OrderedShard {
    Count(SortedRunCounts<SummaryOrder>),
    Group(SortedRunCounts<GroupOrder>),
}

/// What the scan sends a counting shard.
enum ShardRequest {
    /// Groups to count with the tally each already carries: one for a scanned
    /// record, more for an entry migrated from the inline buffer. Groups
    /// arrive as strings; the shard interns them into its own local domain.
    Batch(Vec<(SummaryPartition, OwnedAggregationInput, usize)>),
    /// The end of the scan's additions: the shard answers on its dedicated
    /// snapshot channel with its local identity domain — vocabulary and key
    /// schemas — so the scan can compute the canonical global ranks the
    /// following [`ShardRequest::Order`] carries. The answer channel's sender
    /// lives on the shard thread itself, never inside a queued message, so a
    /// shard that dies with this request still buffered disconnects the
    /// waiting scan instead of deadlocking it.
    Snapshot,
    /// The canonical identities to order by, indexed by this shard's local
    /// ids. The shard answers by returning its ordered half and exiting.
    Order {
        schema_remap: Arc<Vec<u32>>,
        value_ranks: Arc<Vec<ValueId>>,
        by: OrderBy,
        cancellation: Option<CancellationFlag>,
    },
}

/// One shard's identity domain, surrendered at the snapshot handshake.
///
/// `values[id]` is the string local `ValueId` `id` interned; `key_schemas[id]`
/// likewise for local schema ids. Local ids are schedule-dependent and never
/// observable: finalize rewrites both into canonical global identities before
/// anything is ordered or published.
struct ShardSnapshot {
    values: Vec<Box<str>>,
    key_schemas: Vec<Vec<CompactString>>,
}

struct Shard {
    sender: Option<Sender<ShardRequest>>,
    /// The thread's answer to [`ShardRequest::Snapshot`]. Its sender is owned
    /// by the shard thread's stack, so thread exit — orderly or on error —
    /// disconnects this receiver rather than leaving a reply sender alive in
    /// an undrained request queue.
    snapshots: Receiver<ShardSnapshot>,
    thread: Option<JoinHandle<io::Result<OrderedShard>>>,
    /// Groups awaiting dispatch to this shard's thread.
    batch: Vec<(SummaryPartition, OwnedAggregationInput, usize)>,
}

fn join_shard_error(shard: &mut Shard) -> io::Error {
    shard.sender = None;
    match shard.thread.take() {
        Some(thread) => match thread.join() {
            Ok(Ok(_)) => io::Error::other("summary shard exited without an error"),
            Ok(Err(error)) => error,
            Err(_) => io::Error::other("summary shard panicked"),
        },
        None => io::Error::other("summary shard joined twice"),
    }
}

fn dispatch_shard(shard: &mut Shard) -> io::Result<()> {
    let batch = std::mem::replace(&mut shard.batch, Vec::with_capacity(AGGREGATION_BATCH_KEYS));
    let sender = shard
        .sender
        .as_ref()
        .expect("shard accepts input until finalize");
    if sender.send(ShardRequest::Batch(batch)).is_err() {
        return Err(join_shard_error(shard));
    }
    Ok(())
}

/// Owns the shard threads so that dropping an aggregator mid-scan — an
/// execution error, a cancelled run — closes every channel and joins every
/// thread instead of leaking them. Kept separate from [`SummaryAggregator`]
/// so `finalize` can still move the aggregator's other fields out.
struct ShardPool {
    shards: Vec<Shard>,
}

impl Drop for ShardPool {
    fn drop(&mut self) {
        for shard in &mut self.shards {
            shard.sender = None;
            if let Some(thread) = shard.thread.take() {
                let _ = thread.join();
            }
        }
    }
}

impl ShardPool {
    /// Flushes every pending batch and collects each shard's identity domain.
    ///
    /// After this returns, no further additions are legal: the snapshot is
    /// the moment a shard's local ids stop growing, which is what makes the
    /// rank tables finalize computes from it complete.
    fn snapshot(&mut self) -> io::Result<Vec<ShardSnapshot>> {
        for shard in &mut self.shards {
            if !shard.batch.is_empty() {
                dispatch_shard(shard)?;
            }
            let Some(sender) = shard.sender.as_ref() else {
                return Err(io::Error::other("summary shard closed before its snapshot"));
            };
            if sender.send(ShardRequest::Snapshot).is_err() {
                return Err(join_shard_error(shard));
            }
        }
        let mut snapshots = Vec::with_capacity(self.shards.len());
        for shard in &mut self.shards {
            match shard.snapshots.recv() {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(_) => return Err(join_shard_error(shard)),
            }
        }
        Ok(snapshots)
    }

    /// Hands each shard its ordering identities and collects the ordered
    /// halves. Every thread is joined before this returns, error or not.
    fn shutdown(
        &mut self,
        orders: Vec<(Arc<Vec<u32>>, Arc<Vec<ValueId>>)>,
        by: OrderBy,
        cancellation: Option<&CancellationFlag>,
    ) -> io::Result<Vec<OrderedShard>> {
        for (shard, (schema_remap, value_ranks)) in self.shards.iter_mut().zip(orders) {
            let Some(sender) = shard.sender.take() else {
                continue;
            };
            let _ = sender.send(ShardRequest::Order {
                schema_remap,
                value_ranks,
                by,
                cancellation: cancellation.cloned(),
            });
        }
        // Every sender is gone, so every shard's loop has an end and joining
        // cannot hang.
        let mut ordered = Vec::with_capacity(self.shards.len());
        let mut first_error = None;
        for shard in &mut self.shards {
            let Some(thread) = shard.thread.take() else {
                continue;
            };
            match thread.join() {
                Ok(Ok(counts)) => ordered.push(counts),
                Ok(Err(error)) => {
                    first_error.get_or_insert(error);
                }
                Err(_) => {
                    first_error.get_or_insert(io::Error::other("summary shard panicked"));
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(ordered),
        }
    }
}

/// One counting shard: phase-1 exact counts over its partition of the key
/// space during the scan, then its own phase-2 ordering sort once the scan
/// closes with the canonical identities.
/// The identity state one shard interns its groups against.
///
/// The aggregation front end, off the scan thread: value interning,
/// key-schema registration, and the per-pattern schema candidates. Each
/// shard owns one, so its ids are local to the shard and schedule-dependent —
/// which is fine, because the snapshot handshake maps
/// them all into one canonical global domain before anything is ordered.
#[derive(Default)]
struct GroupStore {
    values: ValueInterner,
    key_schemas: Vec<Vec<CompactString>>,
    pattern_schemas: BTreeMap<u64, Vec<u32>>,
}

impl GroupStore {
    fn store(
        &mut self,
        pattern_ordinal: u64,
        group: AggregationInput<'_>,
    ) -> io::Result<StoredGroup> {
        match group {
            AggregationInput::Surface(value) => {
                Ok(StoredGroup::Surface(self.values.intern(&value)?))
            }
            AggregationInput::Unit(value) => Ok(StoredGroup::Unit(self.values.intern(value)?)),
            AggregationInput::Term(value) => Ok(StoredGroup::Term(self.values.intern(value)?)),
            AggregationInput::Key(fields) => {
                let candidates = self.pattern_schemas.entry(pattern_ordinal).or_default();
                let existing = candidates.iter().copied().find(|schema| {
                    self.key_schemas[*schema as usize]
                        .iter()
                        .map(CompactString::as_str)
                        .eq(fields.iter().map(|field| field.name.as_str()))
                });
                let schema = if let Some(schema) = existing {
                    schema
                } else {
                    let schema = u32::try_from(self.key_schemas.len())
                        .map_err(|_| io::Error::other("summary key schema count exceeds u32"))?;
                    self.key_schemas
                        .push(fields.iter().map(|field| field.name.clone()).collect());
                    candidates.push(schema);
                    schema
                };
                let mut values = Vec::with_capacity(fields.len());
                for field in fields {
                    values.push(self.values.intern(&field.value)?);
                }
                Ok(StoredGroup::Key { schema, values })
            }
            AggregationInput::LexicalKey(fields) => {
                let candidates = self.pattern_schemas.entry(pattern_ordinal).or_default();
                let names = fields
                    .iter()
                    .map(|field| field.name.clone())
                    .collect::<Vec<_>>();
                let existing = candidates
                    .iter()
                    .copied()
                    .find(|schema| self.key_schemas[*schema as usize] == names);
                let schema = if let Some(schema) = existing {
                    schema
                } else {
                    let schema = u32::try_from(self.key_schemas.len())
                        .map_err(|_| io::Error::other("summary key schema count exceeds u32"))?;
                    self.key_schemas.push(names);
                    candidates.push(schema);
                    schema
                };
                let mut values = Vec::with_capacity(fields.len());
                for field in fields {
                    values.push(match &field.value {
                        LexicalProjectedKeyValue::Scalar(value) => {
                            StoredLexicalValue::Scalar(self.values.intern(value)?)
                        }
                        LexicalProjectedKeyValue::Segments(segments) => {
                            let mut interned = Vec::with_capacity(segments.len());
                            for segment in segments {
                                interned.push(self.values.intern(segment)?);
                            }
                            StoredLexicalValue::Segments(interned)
                        }
                    });
                }
                Ok(StoredGroup::LexicalKey { schema, values })
            }
        }
    }

    fn into_snapshot(self) -> ShardSnapshot {
        ShardSnapshot {
            values: self.values.values.into_iter().collect(),
            key_schemas: self.key_schemas,
        }
    }
}

fn shard_thread(
    receiver: Receiver<ShardRequest>,
    snapshots: Sender<ShardSnapshot>,
    resident_key_limit: usize,
    merge_fan_in: usize,
    budget: Arc<TempBudget>,
) -> io::Result<OrderedShard> {
    let mut counts = SortedRunCounts::<SummaryKey>::with_policy(
        SpillPolicy {
            max_pending_bytes: usize::MAX,
            max_pending_keys: resident_key_limit,
            merge_fan_in,
        },
        Arc::clone(&budget),
    )?;
    let mut store = GroupStore::default();
    for request in &receiver {
        match request {
            ShardRequest::Batch(batch) => {
                for (partition, group, count) in batch {
                    let group = store.store(partition.pattern_ordinal, group.as_input())?;
                    counts.add(SummaryKey { partition, group }, count)?;
                }
            }
            ShardRequest::Snapshot => {
                // Every batch precedes the snapshot, so the local id domain
                // is complete here and surrendering it closes it.
                let snapshot = std::mem::take(&mut store).into_snapshot();
                if snapshots.send(snapshot).is_err() {
                    return Err(io::Error::other(
                        "summary scan dropped its snapshot receiver",
                    ));
                }
            }
            ShardRequest::Order {
                schema_remap,
                value_ranks,
                by,
                cancellation,
            } => {
                if let Some(cancellation) = &cancellation {
                    counts.set_cancellation(cancellation.clone());
                }
                // Drain rather than finalize-and-visit: the drain flushes and
                // merges only if something spilled, and it releases each
                // counted entry as the ordered buffer grows, so the phase
                // transition holds one buffer's worth of tallies, not two.
                let mut drain = counts.into_sorted_drain()?;
                return match by {
                    OrderBy::Count => {
                        let mut ordered = SortedRunCounts::with_policy(
                            SpillPolicy {
                                max_pending_bytes: usize::MAX,
                                max_pending_keys: resident_key_limit,
                                merge_fan_in,
                            },
                            Arc::clone(&budget),
                        )?;
                        while let Some((key, count)) = drain.next_entry()? {
                            ordered.add(
                                SummaryOrder {
                                    partition: key.partition,
                                    count,
                                    group: key.group.canonicalize(&schema_remap, &value_ranks)?,
                                },
                                1,
                            )?;
                        }
                        ordered.finalize()?;
                        Ok(OrderedShard::Count(ordered))
                    }
                    OrderBy::Group => {
                        let mut ordered = SortedRunCounts::with_policy(
                            SpillPolicy {
                                max_pending_bytes: usize::MAX,
                                max_pending_keys: resident_key_limit,
                                merge_fan_in,
                            },
                            Arc::clone(&budget),
                        )?;
                        if let Some(cancellation) = &cancellation {
                            ordered.set_cancellation(cancellation.clone());
                        }
                        while let Some((key, count)) = drain.next_entry()? {
                            if cancellation
                                .as_ref()
                                .is_some_and(CancellationFlag::is_cancelled)
                            {
                                return Err(io::Error::new(
                                    io::ErrorKind::Interrupted,
                                    "summary ordering cancelled",
                                ));
                            }
                            let (Some(document_ordinal), Some(sentence_ordinal)) = (
                                key.partition.document_ordinal,
                                key.partition.sentence_ordinal,
                            ) else {
                                return Err(io::Error::other(
                                    "a count table requires per-sentence partitions",
                                ));
                            };
                            ordered.add(
                                GroupOrder {
                                    pattern_ordinal: key.partition.pattern_ordinal,
                                    group: key.group.canonicalize(&schema_remap, &value_ranks)?,
                                    document_ordinal,
                                    sentence_ordinal,
                                },
                                count,
                            )?;
                        }
                        ordered.finalize()?;
                        Ok(OrderedShard::Group(ordered))
                    }
                };
            }
        }
    }
    // The scan dropped its sender without closing with `Order`: the run was
    // abandoned, so there is nothing to return and nobody to read it.
    Err(io::Error::other("summary scan ended without ordering"))
}

/// How the scan is currently counting.
///
/// Every run starts [`ScanMode::Inline`]: a bounded in-memory tally with no
/// threads, no channels, and no per-record cost beyond the map entry. The
/// moment the map reaches the resident-key limit — the same moment the
/// unsharded design would have spilled its first run — the workload has
/// demonstrated real aggregation cost, the shards are spawned, the tallies
/// migrate to them by key hash, and the scan continues sharded. A run whose
/// keys fit in memory therefore never pays for the machinery; on the
/// catalogs that motivated sharding the inline phase is a fraction of a
/// percent of the scan.
enum ScanMode {
    Inline(BTreeMap<(SummaryPartition, OwnedAggregationInput), usize>),
    Sharded(ShardPool),
}

/// Scan-phase exact counts, sharded by group hash across a small pool of
/// counting threads once the workload spills. No caller-visible row is
/// available until [`SummaryAggregator::finalize`] completes both external
/// phases.
///
/// Nothing identity-bearing lives on the scan thread: groups travel as their
/// strings and each shard interns them into its own local domain, so
/// interning, schema registration, hashing after the first, counting,
/// spilling and ordering all run off the caller. Local ids are
/// schedule-dependent and never observable — finalize's snapshot handshake
/// rewrites them into one canonical global domain (lexicographic value ranks,
/// deduplicated schemas) before anything is ordered. The partition is exact,
/// because the routing hash is equality-consistent, so a group's whole count
/// lives on one shard and publication is a cheap ordered merge of disjoint
/// streams.
pub struct SummaryAggregator {
    mode: ScanMode,
    resident_key_limit: usize,
    merge_fan_in: usize,
    shard_count: usize,
    budget: Arc<TempBudget>,
}

impl SummaryAggregator {
    pub fn new(max_temp_bytes: u64, shards: usize) -> io::Result<Self> {
        Self::with_limits(
            DEFAULT_RESIDENT_KEYS,
            DEFAULT_MERGE_FAN_IN,
            max_temp_bytes,
            shards,
        )
    }

    pub fn with_limits(
        resident_key_limit: usize,
        merge_fan_in: usize,
        max_temp_bytes: u64,
        shards: usize,
    ) -> io::Result<Self> {
        Ok(Self {
            mode: ScanMode::Inline(BTreeMap::new()),
            resident_key_limit,
            merge_fan_in,
            shard_count: shards.max(1),
            budget: Arc::new(TempBudget::new(max_temp_bytes)),
        })
    }

    /// Spawns the shard pool and migrates the inline tallies to it by key
    /// hash. Called at the moment the inline buffer fills, and by `finalize`
    /// so both external phases always run in the shards.
    fn engage_shards(&mut self) -> io::Result<()> {
        let ScanMode::Inline(tallies) = &mut self.mode else {
            return Ok(());
        };
        let tallies = std::mem::take(tallies);
        // The limit is a total, not a per-shard grant: dividing it keeps the
        // scan's buffered-key footprint what it was unsharded, at the price
        // of each shard spilling proportionally earlier.
        let per_shard_limit = self.resident_key_limit.div_ceil(self.shard_count);
        let merge_fan_in = self.merge_fan_in;
        let mut pool = ShardPool { shards: Vec::new() };
        for _ in 0..self.shard_count {
            let (sender, receiver) = crossbeam_channel::bounded(SHARD_QUEUE_BATCHES);
            let (snapshot_sender, snapshots) = crossbeam_channel::bounded(1);
            let shard_budget = Arc::clone(&self.budget);
            let thread = std::thread::Builder::new()
                .name("summary-shard".into())
                .spawn(move || {
                    shard_thread(
                        receiver,
                        snapshot_sender,
                        per_shard_limit,
                        merge_fan_in,
                        shard_budget,
                    )
                })?;
            pool.shards.push(Shard {
                sender: Some(sender),
                snapshots,
                thread: Some(thread),
                batch: Vec::with_capacity(AGGREGATION_BATCH_KEYS),
            });
        }
        for ((partition, group), count) in tallies {
            let hash = shard_hash(partition, &group);
            let index = (hash % pool.shards.len() as u64) as usize;
            let shard = &mut pool.shards[index];
            shard.batch.push((partition, group, count));
            if shard.batch.len() >= AGGREGATION_BATCH_KEYS {
                dispatch_shard(shard)?;
            }
        }
        self.mode = ScanMode::Sharded(pool);
        Ok(())
    }

    pub fn add(
        &mut self,
        partition: SummaryPartition,
        group: AggregationInput<'_>,
    ) -> io::Result<()> {
        self.add_group(partition, group.into_owned(), None)
    }

    /// [`Self::add`] with the group already owned and its routing hash
    /// already computed — by a partition worker, off the scan thread — so the
    /// sharded arm hashes nothing here. The hash must be [`shard_hash`] of
    /// exactly this group.
    pub(crate) fn add_owned_prehashed(
        &mut self,
        partition: SummaryPartition,
        group: OwnedAggregationInput,
        hash: u64,
    ) -> io::Result<()> {
        self.add_group(partition, group, Some(hash))
    }

    pub fn temp_budget(&self) -> Arc<TempBudget> {
        Arc::clone(&self.budget)
    }

    fn add_group(
        &mut self,
        partition: SummaryPartition,
        group: OwnedAggregationInput,
        hash: Option<u64>,
    ) -> io::Result<()> {
        match &mut self.mode {
            ScanMode::Inline(tallies) => {
                let count = tallies.entry((partition, group)).or_insert(0);
                *count = count
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("summary count overflow"))?;
                if tallies.len() >= self.resident_key_limit {
                    self.engage_shards()?;
                }
            }
            ScanMode::Sharded(pool) => {
                let hash = hash.unwrap_or_else(|| shard_hash(partition, &group));
                let index = (hash % pool.shards.len() as u64) as usize;
                let shard = &mut pool.shards[index];
                shard.batch.push((partition, group, 1));
                if shard.batch.len() >= AGGREGATION_BATCH_KEYS {
                    dispatch_shard(shard)?;
                }
            }
        }
        Ok(())
    }

    pub fn finalize(self) -> io::Result<FinalizedSummary> {
        let (shards, budget, key_schemas, values) = self.finalize_by(OrderBy::Count, None)?;
        let shards = shards
            .into_iter()
            .map(|shard| match shard {
                OrderedShard::Count(counts) => Ok(counts),
                OrderedShard::Group(_) => Err(io::Error::other("shard ordered by group")),
            })
            .collect::<io::Result<_>>()?;
        Ok(FinalizedSummary {
            shards,
            budget,
            key_schemas,
            values,
        })
    }

    /// Finalizes a `--by-document` aggregation for a count table: each
    /// pattern's groups in ling's key order, each group's documents in order.
    /// Once `cancellation` is raised, this and the returned groups' visit
    /// fail with [`io::ErrorKind::Interrupted`] within a bounded number of
    /// records.
    pub fn finalize_by_group(self, cancellation: &CancellationFlag) -> io::Result<FinalizedGroups> {
        let (shards, budget, key_schemas, values) =
            self.finalize_by(OrderBy::Group, Some(cancellation))?;
        let shards = shards
            .into_iter()
            .map(|shard| match shard {
                OrderedShard::Group(counts) => Ok(counts),
                OrderedShard::Count(_) => Err(io::Error::other("shard ordered by count")),
            })
            .collect::<io::Result<_>>()?;
        Ok(FinalizedGroups {
            shards,
            budget,
            key_schemas,
            values,
        })
    }

    #[allow(clippy::type_complexity)]
    fn finalize_by(
        mut self,
        by: OrderBy,
        cancellation: Option<&CancellationFlag>,
    ) -> io::Result<(
        Vec<OrderedShard>,
        Arc<TempBudget>,
        Vec<Vec<CompactString>>,
        Vec<Box<str>>,
    )> {
        // A run that never filled the inline buffer runs both external phases
        // in the shards it now spawns; migrating at most a buffer's worth of
        // tallies is cheaper than keeping a second, inline ordering path.
        self.engage_shards()?;
        let ScanMode::Sharded(pool) = &mut self.mode else {
            return Err(io::Error::other("summary scan failed to engage its shards"));
        };
        // The snapshot closes every shard's local id domain; the union of
        // those domains is the canonical one. Value rank is lexicographic, so
        // comparing two ranks answers exactly what comparing the two strings
        // would, wherever the values were first interned.
        let snapshots = pool.snapshot()?;
        let mut values: Vec<Box<str>> = snapshots
            .iter()
            .flat_map(|snapshot| snapshot.values.iter().cloned())
            .collect();
        values.sort_unstable();
        values.dedup();
        if u32::try_from(values.len()).is_err() {
            return Err(value_count_overflow());
        }
        let mut key_schemas: Vec<Vec<CompactString>> = snapshots
            .iter()
            .flat_map(|snapshot| snapshot.key_schemas.iter().cloned())
            .collect();
        key_schemas.sort_unstable();
        key_schemas.dedup();
        let mut orders = Vec::with_capacity(snapshots.len());
        for snapshot in &snapshots {
            let value_ranks = snapshot
                .values
                .iter()
                .map(|value| {
                    values
                        .binary_search(value)
                        .map_err(|_| {
                            io::Error::other("summary value is absent from the canonical table")
                        })
                        .and_then(|rank| {
                            ValueId::try_from(rank).map_err(|_| value_count_overflow())
                        })
                })
                .collect::<io::Result<Vec<_>>>()?;
            let schema_remap = snapshot
                .key_schemas
                .iter()
                .map(|schema| {
                    key_schemas
                        .binary_search(schema)
                        .map_err(|_| {
                            io::Error::other(
                                "summary key schema is absent from the canonical table",
                            )
                        })
                        .and_then(|index| {
                            u32::try_from(index).map_err(|_| {
                                io::Error::other("canonical summary key schema count exceeds u32")
                            })
                        })
                })
                .collect::<io::Result<Vec<_>>>()?;
            orders.push((Arc::new(schema_remap), Arc::new(value_ranks)));
        }
        let shards = pool.shutdown(orders, by, cancellation)?;
        Ok((shards, Arc::clone(&self.budget), key_schemas, values))
    }
}

/// Fully scanned, merged, and ordered exact summary ready for publication.
pub struct FinalizedSummary {
    /// Each shard's ordered half. The partition is disjoint by key and every
    /// stream is sorted by [`SummaryOrder`]'s own order, so the published
    /// order is their plain merge.
    shards: Vec<SortedRunCounts<SummaryOrder>>,
    budget: Arc<TempBudget>,
    key_schemas: Vec<Vec<CompactString>>,
    /// Every distinct projected value, indexed by lexicographic rank — which is
    /// the identity the ordering phase's records carry.
    values: Vec<Box<str>>,
}

impl FinalizedSummary {
    pub fn peak_temp_bytes(&self) -> u64 {
        self.budget.peak_used_bytes()
    }

    /// Merges the shards' ordered streams and walks the global order once.
    /// Consumes the streams: a second call sees nothing.
    pub fn visit(
        &mut self,
        top: Option<u64>,
        mut visitor: impl FnMut(&SummaryRow) -> io::Result<()>,
    ) -> io::Result<u64> {
        let mut drains = Vec::with_capacity(self.shards.len());
        for counts in self.shards.drain(..) {
            drains.push(counts.into_sorted_drain()?);
        }
        let mut heads = Vec::with_capacity(drains.len());
        for drain in &mut drains {
            heads.push(drain.next_entry()?.map(|(entry, _count)| entry));
        }
        let mut current_partition = None;
        let mut rank = 0_u64;
        let mut emitted = 0_u64;
        // The selection is a scan over at most `shards` heads. Two heads never
        // tie: a tie would need one (partition, count, group) on two shards,
        // and a group lives on exactly one.
        while let Some(next) = heads
            .iter()
            .enumerate()
            .filter_map(|(index, head)| head.as_ref().map(|entry| (index, entry)))
            .min_by(|(_, left), (_, right)| left.cmp(right))
            .map(|(index, _)| index)
        {
            let entry = heads[next].take().expect("selected head exists");
            heads[next] = drains[next].next_entry()?.map(|(entry, _count)| entry);
            if current_partition != Some(entry.partition) {
                current_partition = Some(entry.partition);
                rank = 0;
            }
            rank = rank
                .checked_add(1)
                .ok_or_else(|| io::Error::other("summary rank overflow"))?;
            if top.is_some_and(|limit| rank > limit) {
                continue;
            }
            let count = u64::try_from(entry.count)
                .map_err(|_| io::Error::other("summary count exceeds u64"))?;
            visitor(&SummaryRow {
                partition: entry.partition,
                rank,
                group: materialize_group(&entry.group, &self.key_schemas, &self.values)?,
                count,
            })?;
            emitted = emitted
                .checked_add(1)
                .ok_or_else(|| io::Error::other("summary row count overflow"))?;
        }
        Ok(emitted)
    }
}

fn materialize_group(
    group: &StoredGroup,
    key_schemas: &[Vec<CompactString>],
    interned: &[Box<str>],
) -> io::Result<AggregationGroup> {
    match group {
        StoredGroup::Surface(value) => {
            Ok(AggregationGroup::Surface(resolve_value(*value, interned)?))
        }
        StoredGroup::Unit(value) => Ok(AggregationGroup::Unit(resolve_value(*value, interned)?)),
        StoredGroup::Term(value) => Ok(AggregationGroup::Term(resolve_value(*value, interned)?)),
        StoredGroup::Key { schema, values } => {
            let names = key_schemas
                .get(*schema as usize)
                .ok_or_else(|| io::Error::other("summary key schema is out of range"))?;
            if names.len() != values.len() {
                return Err(io::Error::other("summary key schema width mismatch"));
            }
            let mut fields = Vec::with_capacity(values.len());
            for (name, value) in names.iter().zip(values) {
                fields.push(ProjectedKeyField {
                    name: name.clone(),
                    value: resolve_value(*value, interned)?,
                });
            }
            Ok(AggregationGroup::Key(fields))
        }
        StoredGroup::LexicalKey { schema, values } => {
            let names = key_schemas
                .get(*schema as usize)
                .ok_or_else(|| io::Error::other("summary lexical key schema is out of range"))?;
            if names.len() != values.len() {
                return Err(io::Error::other(
                    "summary lexical key schema width mismatch",
                ));
            }
            let mut fields = Vec::with_capacity(values.len());
            for (name, value) in names.iter().zip(values) {
                fields.push(LexicalProjectedKeyField {
                    name: name.clone(),
                    value: match value {
                        StoredLexicalValue::Scalar(value) => {
                            LexicalProjectedKeyValue::Scalar(resolve_value(*value, interned)?)
                        }
                        StoredLexicalValue::Segments(segments) => {
                            let mut resolved = Vec::with_capacity(segments.len());
                            for segment in segments {
                                resolved.push(resolve_value(*segment, interned)?);
                            }
                            LexicalProjectedKeyValue::Segments(resolved)
                        }
                    },
                });
            }
            Ok(AggregationGroup::LexicalKey(fields))
        }
    }
}

pub enum GroupEvent<'a> {
    /// A new entry. `schema` indexes [`FinalizedGroups::schemas`]; `words`
    /// encode the key as [`lingrep_corpus::CountTableWriter::begin_entry`]
    /// takes it.
    Entry {
        pattern_ordinal: u64,
        schema: u32,
        words: &'a [u32],
    },
    /// The entry's occurrences in one sentence; sentences arrive in
    /// ascending order.
    Sentence {
        document_ordinal: u64,
        sentence_ordinal: u64,
        count: u64,
    },
}

pub struct FinalizedGroups {
    shards: Vec<SortedRunCounts<GroupOrder>>,
    budget: Arc<TempBudget>,
    key_schemas: Vec<Vec<CompactString>>,
    values: Vec<Box<str>>,
}

impl FinalizedGroups {
    pub fn peak_temp_bytes(&self) -> u64 {
        self.budget.peak_used_bytes()
    }

    /// The vocabulary key words index, in lexicographic order.
    pub fn values(&self) -> &[Box<str>] {
        &self.values
    }

    /// Surface, unit and term, then each key schema as a key, then each as
    /// a lexical key.
    pub fn schemas(&self) -> Vec<lingrep_corpus::GroupSchema> {
        use lingrep_corpus::{GroupKind, GroupSchema};
        let named = |kind| {
            self.key_schemas.iter().map(move |names| GroupSchema {
                kind,
                names: names.iter().map(ToString::to_string).collect(),
            })
        };
        [GroupKind::Surface, GroupKind::Unit, GroupKind::Term]
            .into_iter()
            .map(|kind| GroupSchema {
                kind,
                names: Vec::new(),
            })
            .chain(named(GroupKind::Key))
            .chain(named(GroupKind::LexicalKey))
            .collect()
    }

    fn encode(&self, group: &StoredGroup, words: &mut Vec<u32>) -> io::Result<u32> {
        let schemas = u32::try_from(self.key_schemas.len())
            .map_err(|_| io::Error::other("key schema count exceeds u32"))?;
        words.clear();
        Ok(match group {
            StoredGroup::Surface(value) => {
                words.push(*value);
                0
            }
            StoredGroup::Unit(value) => {
                words.push(*value);
                1
            }
            StoredGroup::Term(value) => {
                words.push(*value);
                2
            }
            StoredGroup::Key { schema, values } => {
                words.extend_from_slice(values);
                3 + schema
            }
            StoredGroup::LexicalKey { schema, values } => {
                for value in values {
                    match value {
                        StoredLexicalValue::Scalar(value) => words.extend([0, *value]),
                        StoredLexicalValue::Segments(segments) => {
                            let marker = u32::try_from(segments.len() + 1)
                                .map_err(|_| io::Error::other("list field exceeds u32"))?;
                            words.push(marker);
                            words.extend_from_slice(segments);
                        }
                    }
                }
                3 + schemas + schema
            }
        })
    }

    /// Merges the shards' streams and reports each entry once, before its
    /// documents. Groups that compare equal are one entry, whichever key
    /// form each occurrence had. Consumes the streams.
    pub fn visit(
        &mut self,
        mut visitor: impl FnMut(GroupEvent<'_>) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut drains = Vec::with_capacity(self.shards.len());
        for counts in self.shards.drain(..) {
            drains.push(counts.into_sorted_drain()?);
        }
        let mut heads = Vec::with_capacity(drains.len());
        for drain in &mut drains {
            heads.push(drain.next_entry()?);
        }
        let mut current: Option<(u64, StoredGroup)> = None;
        let mut words = Vec::new();
        while let Some(next) = heads
            .iter()
            .enumerate()
            .filter_map(|(index, head)| head.as_ref().map(|(entry, _)| (index, entry)))
            .min_by(|(_, left), (_, right)| left.cmp(right))
            .map(|(index, _)| index)
        {
            let (entry, count) = heads[next].take().expect("selected head exists");
            heads[next] = drains[next].next_entry()?;
            let same = current.as_ref().is_some_and(|(pattern, group)| {
                *pattern == entry.pattern_ordinal && *group == entry.group
            });
            if !same {
                let schema = self.encode(&entry.group, &mut words)?;
                visitor(GroupEvent::Entry {
                    pattern_ordinal: entry.pattern_ordinal,
                    schema,
                    words: &words,
                })?;
                current = Some((entry.pattern_ordinal, entry.group));
            }
            visitor(GroupEvent::Sentence {
                document_ordinal: entry.document_ordinal,
                sentence_ordinal: entry.sentence_ordinal,
                count: count as u64,
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
enum SlotCount {
    Matches,
    SentencesMatched,
}

type SlotCountKey = (u32, u64, SlotCount);

#[derive(Clone, Copy, Debug, Default)]
struct SlotTotals {
    documents: u64,
    sentences: u64,
    tokens: u64,
}

struct SlotTally {
    totals: Vec<SlotTotals>,
    last_document: Vec<Option<u64>>,
    counts: SortedRunCounts<SlotCountKey>,
    matched: Vec<u64>,
}

impl SlotTally {
    fn new(
        slots: usize,
        resident_key_limit: usize,
        merge_fan_in: usize,
        budget: Arc<TempBudget>,
    ) -> io::Result<Self> {
        if u32::try_from(slots).is_err() {
            return Err(io::Error::other("slot count exceeds u32"));
        }
        Ok(Self {
            totals: vec![SlotTotals::default(); slots],
            last_document: vec![None; slots],
            counts: SortedRunCounts::with_policy(
                SpillPolicy {
                    max_pending_bytes: usize::MAX,
                    max_pending_keys: resident_key_limit,
                    merge_fan_in,
                },
                budget,
            )?,
            matched: Vec::new(),
        })
    }

    fn observe(
        &mut self,
        slots: &[u32],
        document: u64,
        tokens: usize,
        matched_patterns: impl Iterator<Item = u64>,
    ) -> io::Result<()> {
        let overflow = || io::Error::other("per-slot count overflow");
        self.matched.clear();
        self.matched.extend(matched_patterns);
        self.matched.sort_unstable();
        let tokens = tokens as u64;
        for &slot in slots {
            let totals = &mut self.totals[slot as usize];
            let last_document = &mut self.last_document[slot as usize];
            if *last_document != Some(document) {
                *last_document = Some(document);
                totals.documents = totals.documents.checked_add(1).ok_or_else(overflow)?;
            }
            totals.sentences = totals.sentences.checked_add(1).ok_or_else(overflow)?;
            totals.tokens = totals.tokens.checked_add(tokens).ok_or_else(overflow)?;
            for run in self.matched.chunk_by(|left, right| left == right) {
                let pattern = run[0];
                self.counts
                    .add((slot, pattern, SlotCount::Matches), run.len())?;
                self.counts
                    .add((slot, pattern, SlotCount::SentencesMatched), 1)?;
            }
        }
        Ok(())
    }

    fn into_drain<'a>(
        self,
        published: &[u64],
        pattern_names: &'a [&'a str],
    ) -> io::Result<SlotDrain<'a>> {
        let mut published = published.to_vec();
        published.sort_unstable();
        Ok(SlotDrain {
            totals: self.totals,
            next_slot: 0,
            drain: self.counts.into_sorted_drain()?,
            pending: None,
            published,
            pattern_names,
        })
    }
}

struct SlotDrain<'a> {
    totals: Vec<SlotTotals>,
    next_slot: usize,
    drain: SortedDrain<SlotCountKey>,
    pending: Option<(SlotCountKey, usize)>,
    published: Vec<u64>,
    pattern_names: &'a [&'a str],
}

impl SlotDrain<'_> {
    fn next_slot(&mut self) -> io::Result<Option<(usize, SlotTotals, Vec<PatternCount>)>> {
        while self.next_slot < self.totals.len() {
            let slot = self.next_slot;
            self.next_slot += 1;
            let totals = self.totals[slot];
            if totals.sentences == 0 {
                continue;
            }
            let mut patterns = Vec::with_capacity(self.published.len());
            for index in 0..self.published.len() {
                let ordinal = self.published[index];
                let mut count = PatternCount {
                    pattern: (*self.pattern_names.get(ordinal as usize).ok_or_else(|| {
                        io::Error::other("published pattern ordinal is out of range")
                    })?)
                    .to_owned(),
                    matches: 0,
                    sentences_matched: 0,
                };
                while let Some(((entry_slot, pattern, kind), value)) = self.peek()? {
                    if (entry_slot as usize, pattern) > (slot, ordinal) {
                        break;
                    }
                    self.pending = None;
                    if (entry_slot as usize, pattern) < (slot, ordinal) {
                        continue;
                    }
                    let value = value as u64;
                    match kind {
                        SlotCount::Matches => count.matches = value,
                        SlotCount::SentencesMatched => count.sentences_matched = value,
                    }
                }
                patterns.push(count);
            }
            return Ok(Some((slot, totals, patterns)));
        }
        Ok(None)
    }

    fn peek(&mut self) -> io::Result<Option<(SlotCountKey, usize)>> {
        if self.pending.is_none() {
            self.pending = self.drain.next_entry()?;
        }
        Ok(self.pending)
    }
}

/// Counts matches per section label, followed by an unlabelled slot.
pub struct SectionTally {
    labels: Vec<(String, roaring::RoaringTreemap)>,
    tally: SlotTally,
    active: Vec<u32>,
}

impl SectionTally {
    /// `labels` pairs each section label with the corpus sentence ordinals it
    /// covers; spilled runs reserve from `budget`.
    pub fn new(
        labels: Vec<(String, roaring::RoaringTreemap)>,
        budget: Arc<TempBudget>,
    ) -> io::Result<Self> {
        Self::with_limits(labels, DEFAULT_RESIDENT_KEYS, DEFAULT_MERGE_FAN_IN, budget)
    }

    pub fn with_limits(
        labels: Vec<(String, roaring::RoaringTreemap)>,
        resident_key_limit: usize,
        merge_fan_in: usize,
        budget: Arc<TempBudget>,
    ) -> io::Result<Self> {
        Ok(Self {
            tally: SlotTally::new(labels.len() + 1, resident_key_limit, merge_fan_in, budget)?,
            labels,
            active: Vec::new(),
        })
    }

    /// Counts one admitted sentence of `document` under every label covering
    /// it, or under the unlabelled slot, with one pattern ordinal per
    /// published match. Sentences must arrive in corpus order.
    pub fn observe(
        &mut self,
        sentence: u64,
        document: u64,
        tokens: usize,
        matched_patterns: impl Iterator<Item = u64>,
    ) -> io::Result<()> {
        self.active.clear();
        for (slot, (_, covered)) in (0u32..).zip(&self.labels) {
            if covered.contains(sentence) {
                self.active.push(slot);
            }
        }
        if self.active.is_empty() {
            self.active.push(self.labels.len() as u32);
        }
        self.tally
            .observe(&self.active, document, tokens, matched_patterns)
    }

    /// Merges any spilled runs and returns the records in slot order, one per
    /// slot with at least one admitted sentence. `published` lists the pattern
    /// ordinals to report, each named by `pattern_names[ordinal]`.
    pub fn into_records<'a>(
        self,
        published: &[u64],
        pattern_names: &'a [&'a str],
    ) -> io::Result<SectionRecords<'a>> {
        Ok(SectionRecords {
            labels: self.labels.into_iter().map(|(label, _)| label).collect(),
            slots: self.tally.into_drain(published, pattern_names)?,
        })
    }
}

pub struct SectionRecords<'a> {
    labels: Vec<String>,
    slots: SlotDrain<'a>,
}

impl SectionRecords<'_> {
    pub fn next_record(&mut self) -> io::Result<Option<SectionData>> {
        Ok(self
            .slots
            .next_slot()?
            .map(|(slot, totals, patterns)| SectionData {
                label: self.labels.get(slot).cloned(),
                sentences_admitted: totals.sentences,
                tokens_admitted: totals.tokens,
                patterns,
            }))
    }
}

/// Counts matches per metadata value in first-occurrence order, followed by a slot for documents without values.
pub struct MetadataValueTally {
    key: String,
    values: Vec<String>,
    documents: Vec<(std::ops::Range<u64>, Box<[u32]>)>,
    cursor: usize,
    tally: SlotTally,
}

impl MetadataValueTally {
    /// `documents` lists each document's corpus sentence range and its values
    /// for `key`, in corpus order; a document with no value lists none.
    /// Spilled runs reserve from `budget`.
    pub fn new(
        key: String,
        documents: impl IntoIterator<Item = (std::ops::Range<u64>, Vec<String>)>,
        budget: Arc<TempBudget>,
    ) -> io::Result<Self> {
        Self::with_limits(
            key,
            documents,
            DEFAULT_RESIDENT_KEYS,
            DEFAULT_MERGE_FAN_IN,
            budget,
        )
    }

    pub fn with_limits(
        key: String,
        documents: impl IntoIterator<Item = (std::ops::Range<u64>, Vec<String>)>,
        resident_key_limit: usize,
        merge_fan_in: usize,
        budget: Arc<TempBudget>,
    ) -> io::Result<Self> {
        let (ranges, values): (Vec<_>, Vec<_>) = documents.into_iter().unzip();
        let slots = lingrep_corpus::ValueSlots::number(values).map_err(io::Error::other)?;
        let mut documents = Vec::<(std::ops::Range<u64>, Box<[u32]>)>::new();
        for (range, document_slots) in ranges.into_iter().zip(slots.documents()) {
            if documents
                .last()
                .is_some_and(|(previous, _)| range.start < previous.end)
            {
                return Err(io::Error::other(
                    "document sentence ranges are not in corpus order",
                ));
            }
            if !range.is_empty() {
                documents.push((range, document_slots.clone()));
            }
        }
        let values = slots.values().to_vec();
        Ok(Self {
            key,
            tally: SlotTally::new(values.len() + 1, resident_key_limit, merge_fan_in, budget)?,
            values,
            documents,
            cursor: 0,
        })
    }

    /// Counts one admitted sentence under each value of its document, or
    /// under the no-value slot, with one pattern ordinal per published match.
    /// Sentences must arrive in corpus order.
    pub fn observe(
        &mut self,
        sentence: u64,
        tokens: usize,
        matched_patterns: impl Iterator<Item = u64>,
    ) -> io::Result<()> {
        if !self
            .documents
            .get(self.cursor)
            .is_some_and(|(range, _)| range.contains(&sentence))
        {
            self.cursor = self
                .documents
                .partition_point(|(range, _)| range.end <= sentence);
        }
        let Some((range, slots)) = self.documents.get(self.cursor) else {
            return Err(io::Error::other(format!(
                "sentence {sentence} lies outside every document"
            )));
        };
        if !range.contains(&sentence) {
            return Err(io::Error::other(format!(
                "sentence {sentence} lies outside every document"
            )));
        }
        self.tally
            .observe(slots, self.cursor as u64, tokens, matched_patterns)
    }

    /// Merges any spilled runs and returns the records in slot order, one per
    /// slot with at least one admitted sentence. `published` lists the pattern
    /// ordinals to report, each named by `pattern_names[ordinal]`.
    pub fn into_records<'a>(
        self,
        published: &[u64],
        pattern_names: &'a [&'a str],
    ) -> io::Result<MetadataValueRecords<'a>> {
        Ok(MetadataValueRecords {
            key: self.key,
            values: self.values,
            slots: self.tally.into_drain(published, pattern_names)?,
        })
    }
}

pub struct MetadataValueRecords<'a> {
    key: String,
    values: Vec<String>,
    slots: SlotDrain<'a>,
}

impl MetadataValueRecords<'_> {
    pub fn next_record(&mut self) -> io::Result<Option<MetadataValueData>> {
        Ok(self
            .slots
            .next_slot()?
            .map(|(slot, totals, patterns)| MetadataValueData {
                key: self.key.clone(),
                value: self.values.get(slot).cloned(),
                documents_admitted: totals.documents,
                sentences_admitted: totals.sentences,
                tokens_admitted: totals.tokens,
                patterns,
            }))
    }
}
