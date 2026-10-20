//! Thread-local resource pools for matcher performance optimization
//!
//! Reuses NFA state sets across pattern matching operations to reduce
//! allocation overhead.

use crate::fsm::types::StateInfo;
use std::cell::RefCell;
use std::collections::HashSet;
use std::hash::BuildHasherDefault;

/// Fast non-cryptographic hash builder for matcher hot-path sets.
///
/// NFA simulation hashes millions of small keys (state ids, positions) per
/// corpus; SipHash dominated the profile. HashDoS resistance is unnecessary
/// here because keys derive from compiled pattern state ids and token
/// positions, not attacker-controlled strings.
pub type FastBuild = BuildHasherDefault<rustc_hash::FxHasher>;
pub type StateSet = HashSet<StateInfo, FastBuild>;

// Thread-local arena for HashSets
//
// Reuses HashSet objects to reduce allocations during NFA matching.
thread_local! {
    pub static HASHSET_POOL: RefCell<Vec<StateSet>> = RefCell::new(Vec::with_capacity(64));
}

/// Clear this thread's matcher pools, cached regex clones, and execution
/// scratch, including lexical projection buffers, and release their memory.
pub fn clear_pools() {
    HASHSET_POOL.with(|pool| {
        let mut pool = pool.borrow_mut();
        pool.clear();
        pool.shrink_to_fit(); // Release excess capacity
    });
    super::predicate::clear_local_regexes();
    super::predicate::clear_predicate_scratch();
    super::exec::clear_exec_scratch();
    super::lexicon::clear_scratch();
}

#[cfg(test)]
mod tests {
    use super::clear_pools;
    use crate::fsm::matcher::exec::{ExecScratchCapacities, exec_scratch_capacities};
    use crate::fsm::matcher::predicate::predicate_scratch_capacity;
    use crate::fsm::{CompileLimits, PatternSource, compile_catalog};
    use crate::token_match::{TokenAttributeIndex, TokenInfo};

    #[test]
    fn clear_pools_releases_current_thread_exec_scratch() {
        clear_pools();
        let source = (0..128)
            .map(|index| format!("pattern p{index} := [text=value{index}]."))
            .collect::<Vec<_>>()
            .join("\n");
        let matcher = compile_catalog(
            PatternSource::named("clear-pools.lx", &source),
            CompileLimits::default(),
        )
        .expect("predicate-heavy fixture compiles")
        .matcher;
        let tokens = (0..64)
            .map(|_| {
                let mut token = TokenInfo::default();
                token.set(TokenAttributeIndex::Orth, "not-present".into());
                token
            })
            .collect::<Vec<_>>();

        assert!(matcher.find_matches(&tokens).is_empty());
        let warmed = exec_scratch_capacities();
        assert!(
            predicate_scratch_capacity() > 0
                || warmed.visited > 0
                || warmed.dfs_stack > 0
                || warmed.closure_seeds > 0
                || warmed.token_marks > 0
                || warmed.frontier > 0
                || warmed.frontier_next > 0
                || warmed.spawn_candidates > 0,
            "the fixture must warm execution scratch: {warmed:?}"
        );

        clear_pools();
        assert_eq!(
            exec_scratch_capacities(),
            ExecScratchCapacities {
                visited: 0,
                dfs_stack: 0,
                closure_seeds: 0,
                token_marks: 0,
                frontier: 0,
                frontier_next: 0,
                spawn_candidates: 0,
            }
        );
        assert_eq!(predicate_scratch_capacity(), 0);
    }
}
