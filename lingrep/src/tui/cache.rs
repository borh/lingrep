use crate::tui::types::{PatternId, RequestId};
use lru::LruCache;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::tui::paged_store::{DEFAULT_PAGE_ROWS, PagedStore};
#[derive(Debug, Clone)]
pub struct ResultCache {
    pub frequencies: LruCache<RequestId, HashMap<PatternId, u64>>,
    pub(crate) surface: LruCache<(RequestId, PatternId), PagedResult<SurfaceEntry>>,
    pub(crate) kwic: LruCache<(RequestId, PatternId), PagedResult<KwicLine>>,
    pub config: CacheConfig,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SurfaceEntry {
    pub form: String,
    pub count: u64,
}

pub use crate::output::KwicLine;

#[derive(Debug, Clone)]
pub(crate) struct PagedResult<T> {
    pub(crate) store: Arc<PagedStore<T>>,
    pub window: Vec<T>,
    pub window_start: usize,
}

impl<T> PagedResult<T>
where
    T: serde::de::DeserializeOwned,
{
    pub(crate) fn new(store: Arc<PagedStore<T>>) -> std::io::Result<Self> {
        let window = store.read_window(0, DEFAULT_PAGE_ROWS)?;
        Ok(Self {
            store,
            window,
            window_start: 0,
        })
    }

    pub fn len(&self) -> usize {
        usize::try_from(self.store.len()).unwrap_or(usize::MAX)
    }

    pub fn ensure_window(&mut self, start: usize) -> std::io::Result<()> {
        let start = start.min(self.len().saturating_sub(1));
        if start != self.window_start {
            self.window = self.store.read_window(start as u64, DEFAULT_PAGE_ROWS)?;
            self.window_start = start;
        }
        Ok(())
    }
}

#[cfg(test)]
impl<T> PagedResult<T>
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    pub(crate) fn from_rows(rows: &[T]) -> Self {
        let budget = Arc::new(lingrep_core::spill::TempBudget::new(u64::MAX));
        let mut writer = crate::tui::paged_store::PagedStoreWriter::new(budget).unwrap();
        for row in rows {
            writer.push(row).unwrap();
        }
        Self::new(writer.finish().unwrap()).unwrap()
    }
}

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub max_requests: usize,
    pub max_patterns_per_request: usize,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            max_requests: 5,
            max_patterns_per_request: 50,
        }
    }
}

impl ResultCache {
    pub fn new(config: CacheConfig) -> Self {
        // `NonZeroUsize::new(1)` is always `Some` (1 is non-zero); this single
        // binding supplies every capacity fallback below without an unwrap.
        #[expect(
            clippy::unwrap_used,
            reason = "1 is non-zero, so `NonZeroUsize::new(1)` is always `Some`"
        )]
        let one = NonZeroUsize::new(1).unwrap();
        let pattern_cache_cap = config
            .max_requests
            .saturating_mul(config.max_patterns_per_request)
            .max(1);
        Self {
            frequencies: LruCache::new(
                NonZeroUsize::new(config.max_requests.max(1)).unwrap_or(one),
            ),
            surface: LruCache::new(NonZeroUsize::new(pattern_cache_cap).unwrap_or(one)),
            kwic: LruCache::new(NonZeroUsize::new(pattern_cache_cap).unwrap_or(one)),
            config,
        }
    }
}

impl Default for ResultCache {
    fn default() -> Self {
        Self::new(CacheConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache() -> ResultCache {
        ResultCache::new(CacheConfig {
            max_requests: 2,
            max_patterns_per_request: 3,
        })
    }

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = make_cache();
        let req_id = RequestId(1);

        let mut freqs = HashMap::new();
        freqs.insert(PatternId(0), 100);
        cache.frequencies.put(req_id, freqs);

        let result = cache.frequencies.get(&req_id);
        assert!(result.is_some());
        assert_eq!(result.unwrap().get(&PatternId(0)), Some(&100));
    }

    #[test]
    fn test_cache_eviction() {
        let mut cache = make_cache();

        cache.frequencies.put(RequestId(1), HashMap::new());
        cache.frequencies.put(RequestId(2), HashMap::new());

        cache.frequencies.put(RequestId(3), HashMap::new());

        assert!(cache.frequencies.get(&RequestId(1)).is_none());
        assert!(cache.frequencies.get(&RequestId(2)).is_some());
        assert!(cache.frequencies.get(&RequestId(3)).is_some());
    }

    #[test]
    fn test_surface_entry() {
        let entry = SurfaceEntry {
            form: "test".to_string(),
            count: 42,
        };
        assert_eq!(entry.form, "test");
        assert_eq!(entry.count, 42);
    }

    #[test]
    fn test_kwic_line() {
        let kwic = KwicLine {
            pre: "before".to_string(),
            key: "match".to_string(),
            post: "after".to_string(),
            match_start: 0,
            match_end: 0,
        };
        assert_eq!(kwic.pre, "before");
        assert_eq!(kwic.key, "match");
        assert_eq!(kwic.post, "after");
    }

    #[test]
    fn test_cache_config_default() {
        let config = CacheConfig::default();
        assert_eq!(config.max_requests, 5);
        assert_eq!(config.max_patterns_per_request, 50);
    }

    #[test]
    fn test_result_cache_default() {
        let cache = ResultCache::default();
        assert_eq!(cache.config.max_requests, 5);
    }

    #[test]
    fn test_surface_cache() {
        let mut cache = make_cache();

        let entry = SurfaceEntry {
            form: "だ".to_string(),
            count: 10,
        };
        cache.surface.put(
            (RequestId(1), PatternId(0)),
            PagedResult::from_rows(std::slice::from_ref(&entry)),
        );

        let result = cache.surface.get(&(RequestId(1), PatternId(0)));
        assert!(result.is_some());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries.window[0].form, "だ");
    }

    #[test]
    fn test_kwic_cache() {
        let mut cache = make_cache();

        let kwic = KwicLine {
            pre: "前".to_string(),
            key: "だ".to_string(),
            post: "後".to_string(),
            match_start: 0,
            match_end: 0,
        };
        cache.kwic.put(
            (RequestId(1), PatternId(0)),
            PagedResult::from_rows(&[kwic]),
        );

        let result = cache.kwic.get(&(RequestId(1), PatternId(0)));
        assert!(result.is_some());
        let lines = result.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines.window[0].key, "だ");
    }
}
