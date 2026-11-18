//! Size thresholds that tests lower so small fixtures reach the code paths
//! that otherwise appear only with large inputs, and a count of open spill
//! files for asserting that those paths stay bounded.
//!
//! Only builds with this crate's `testing` feature, or its own unit tests,
//! read an override or count files. In every other build [`ScaleKnob::or`]
//! returns its default, [`SpillFileGuard`] is zero-sized, and
//! [`write_spill_file_report`] does nothing.

/// A threshold a test can lower, either through the environment variable
/// [`ScaleKnob::variable`] of the process or, in builds that read overrides,
/// with `ScaleKnob::set`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScaleKnob {
    /// Distinct summary keys held in memory before a spill run is written.
    SummaryResidentKeys,
    /// Runs combined by one summary spill merge.
    SummaryMergeFanIn,
    /// Token-window records held in memory before a spill run is written.
    WindowRunRecords,
    /// Estimated bytes of results one matcher batch may allocate.
    MatchOutputBytes,
    /// Pending posting bytes held in memory before a postings run is written.
    PostingsRunBytes,
    /// Estimated bytes of compound chains derived from one sentence.
    CompoundChainBytes,
    /// Compound patterns one search may derive.
    CompoundPatterns,
}

impl ScaleKnob {
    #[cfg(any(test, feature = "testing"))]
    const ALL: [Self; 7] = [
        Self::SummaryResidentKeys,
        Self::SummaryMergeFanIn,
        Self::WindowRunRecords,
        Self::MatchOutputBytes,
        Self::PostingsRunBytes,
        Self::CompoundChainBytes,
        Self::CompoundPatterns,
    ];

    /// The environment variable that sets this knob, read once per process.
    pub const fn variable(self) -> &'static str {
        match self {
            Self::SummaryResidentKeys => "LINGREP_TEST_SUMMARY_RESIDENT_KEYS",
            Self::SummaryMergeFanIn => "LINGREP_TEST_SUMMARY_MERGE_FAN_IN",
            Self::WindowRunRecords => "LINGREP_TEST_WINDOW_RUN_RECORDS",
            Self::MatchOutputBytes => "LINGREP_TEST_MATCH_OUTPUT_BYTES",
            Self::PostingsRunBytes => "LINGREP_TEST_POSTINGS_RUN_BYTES",
            Self::CompoundChainBytes => "LINGREP_TEST_COMPOUND_CHAIN_BYTES",
            Self::CompoundPatterns => "LINGREP_TEST_COMPOUND_PATTERNS",
        }
    }

    /// The overriding value, else `default`.
    #[inline]
    pub fn or(self, default: usize) -> usize {
        #[cfg(any(test, feature = "testing"))]
        {
            overrides::get(self).unwrap_or(default)
        }
        #[cfg(not(any(test, feature = "testing")))]
        {
            default
        }
    }

    /// Overrides this knob for the whole process until the guard drops.
    ///
    /// # Panics
    ///
    /// Panics if `value` is zero.
    #[cfg(any(test, feature = "testing"))]
    pub fn set(self, value: usize) -> ScaleOverride {
        assert!(value > 0, "{} must be positive", self.variable());
        let previous = overrides::replace(self, value);
        ScaleOverride {
            knob: self,
            previous,
        }
    }
}

/// Restores a knob's previous value when dropped.
#[cfg(any(test, feature = "testing"))]
#[must_use = "the override ends when the guard drops"]
pub struct ScaleOverride {
    knob: ScaleKnob,
    previous: usize,
}

#[cfg(any(test, feature = "testing"))]
impl Drop for ScaleOverride {
    fn drop(&mut self) {
        overrides::replace(self.knob, self.previous);
    }
}

#[cfg(any(test, feature = "testing"))]
mod overrides {
    use std::sync::Once;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::ScaleKnob;

    /// Zero means no override.
    static VALUES: [AtomicUsize; ScaleKnob::ALL.len()] =
        [const { AtomicUsize::new(0) }; ScaleKnob::ALL.len()];
    static FROM_ENVIRONMENT: Once = Once::new();

    fn index(knob: ScaleKnob) -> usize {
        ScaleKnob::ALL
            .iter()
            .position(|candidate| *candidate == knob)
            .expect("every knob is listed")
    }

    fn load_environment() {
        FROM_ENVIRONMENT.call_once(|| {
            for knob in ScaleKnob::ALL {
                let Some(raw) = std::env::var_os(knob.variable()) else {
                    continue;
                };
                let value = raw
                    .to_str()
                    .and_then(|raw| raw.parse::<usize>().ok())
                    .filter(|value| *value > 0);
                assert!(
                    value.is_some(),
                    "{} must be a positive integer: {raw:?}",
                    knob.variable()
                );
                VALUES[index(knob)].store(value.unwrap_or_default(), Ordering::Relaxed);
            }
        });
    }

    pub(super) fn get(knob: ScaleKnob) -> Option<usize> {
        load_environment();
        match VALUES[index(knob)].load(Ordering::Relaxed) {
            0 => None,
            value => Some(value),
        }
    }

    pub(super) fn replace(knob: ScaleKnob, value: usize) -> usize {
        load_environment();
        VALUES[index(knob)].swap(value, Ordering::Relaxed)
    }
}

/// Held for as long as one spill file is open.
#[derive(Debug)]
#[must_use = "the file counts as open until the guard drops"]
pub struct SpillFileGuard(());

impl SpillFileGuard {
    #[inline]
    pub fn open() -> Self {
        #[cfg(any(test, feature = "testing"))]
        spill_files::opened();
        Self(())
    }
}

#[cfg(any(test, feature = "testing"))]
impl Drop for SpillFileGuard {
    fn drop(&mut self) {
        spill_files::closed();
    }
}

#[cfg(any(test, feature = "testing"))]
mod spill_files {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static OPEN: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);
    static OPENED: AtomicUsize = AtomicUsize::new(0);

    pub(super) fn opened() {
        let open = OPEN.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(open, Ordering::SeqCst);
        OPENED.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) fn closed() {
        OPEN.fetch_sub(1, Ordering::SeqCst);
    }

    pub(super) fn counts() -> super::SpillFileCounts {
        super::SpillFileCounts {
            peak_open: PEAK.load(Ordering::SeqCst),
            opened: OPENED.load(Ordering::SeqCst),
        }
    }
}

/// Spill files this process has held open at once, and opened in total.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpillFileCounts {
    pub peak_open: usize,
    pub opened: usize,
}

impl SpillFileCounts {
    /// The counts so far; zero in builds that do not count.
    pub fn current() -> Self {
        #[cfg(any(test, feature = "testing"))]
        {
            spill_files::counts()
        }
        #[cfg(not(any(test, feature = "testing")))]
        {
            Self {
                peak_open: 0,
                opened: 0,
            }
        }
    }
}

/// The variable naming the file [`write_spill_file_report`] writes.
pub const SPILL_FILE_REPORT_VARIABLE: &str = "LINGREP_TEST_SPILL_FILE_REPORT";

/// Writes `peak_open opened` to the file named by
/// [`SPILL_FILE_REPORT_VARIABLE`], when it is set.
///
/// # Panics
///
/// Panics if the report cannot be written.
pub fn write_spill_file_report() {
    #[cfg(any(test, feature = "testing"))]
    if let Some(path) = std::env::var_os(SPILL_FILE_REPORT_VARIABLE) {
        let counts = SpillFileCounts::current();
        std::fs::write(path, format!("{} {}\n", counts.peak_open, counts.opened))
            .expect("write the spill-file report");
    }
}
