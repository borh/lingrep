// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

#[cfg(not(target_env = "msvc"))]
mod supported {
    use compact_str::CompactString;
    use lingrep_core::fsm::{
        BoundCatalog, CompileLimits, CompiledCatalog, PatternSource, clear_pools, compile_catalog,
    };
    use lingrep_core::token_match::{
        AttributeKind, BoundTokenSentence, SchemaAttribute, TokenSchema,
    };
    use rayon::{BroadcastContext, ThreadPool, ThreadPoolBuilder};
    use std::mem::size_of;
    use tikv_jemalloc_ctl::{epoch, stats, thread};

    const PREDICATE_COUNT: usize = 512;
    const SENTENCE_LENGTHS: [usize; 4] = [128, 1_024, 4_096, 16_384];
    const WORKER_COUNTS: [usize; 2] = [1, 8];

    #[derive(Clone, Copy)]
    struct ProcessStats {
        allocated: usize,
        resident: usize,
    }

    #[derive(Clone, Copy)]
    struct WorkerStats {
        index: usize,
        before_warmup: u64,
        after_warmup: u64,
        after_match: u64,
        result_struct_bytes: usize,
    }

    fn matcher() -> CompiledCatalog {
        let source = predicate_catalog();
        compile_catalog(
            PatternSource::named("matcher-memory.lx", &source),
            CompileLimits::default(),
        )
        .expect("measurement catalog must compile")
        .matcher
    }

    fn predicate_catalog() -> String {
        (0..PREDICATE_COUNT)
            .map(|index| format!("pattern p{index} := [text=value{index}]."))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn bind(matcher: &CompiledCatalog) -> BoundCatalog {
        let schema = TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar)
            .expect("measurement schema key must be valid")])
        .expect("measurement schema must be valid");
        matcher
            .bind(&schema)
            .expect("measurement catalog must bind")
    }

    fn tokens(len: usize, matcher: &BoundCatalog) -> BoundTokenSentence {
        BoundTokenSentence::from_projected_values(
            matcher.projection(),
            len,
            (0..len)
                .map(|_| Some(CompactString::new("not-in-catalog")))
                .collect(),
        )
        .expect("measurement sentence must bind")
    }

    fn process_stats() -> ProcessStats {
        epoch::advance().expect("advance jemalloc stats epoch");
        ProcessStats {
            allocated: stats::allocated::read().expect("read live allocated bytes"),
            resident: stats::resident::read().expect("read resident bytes"),
        }
    }

    fn allocated_counter() -> thread::ThreadLocal<u64> {
        thread::allocatedp::mib()
            .expect("thread.allocatedp MIB")
            .read()
            .expect("thread allocation counter")
    }

    fn run_workers(
        pool: &ThreadPool,
        matcher: &BoundCatalog,
        tokens: &BoundTokenSentence,
    ) -> Vec<WorkerStats> {
        pool.broadcast(|context: BroadcastContext<'_>| {
            clear_pools();
            let allocated = allocated_counter();
            let before_warmup = allocated.get();
            drop(
                matcher
                    .find_matches(tokens)
                    .expect("measurement sentence schema must match"),
            );
            let after_warmup = allocated.get();
            let matches = matcher
                .find_matches(tokens)
                .expect("measurement sentence schema must match");
            let after_match = allocated.get();
            let result_struct_bytes = matches
                .capacity()
                .saturating_mul(size_of::<lingrep_core::fsm::Match>());
            drop(matches);
            WorkerStats {
                index: context.index(),
                before_warmup,
                after_warmup,
                after_match,
                result_struct_bytes,
            }
        })
    }

    fn clear_workers(pool: &ThreadPool) -> Vec<(usize, u64)> {
        pool.broadcast(|context: BroadcastContext<'_>| {
            clear_pools();
            (context.index(), allocated_counter().get())
        })
    }

    fn print_process(label: &str, stats: ProcessStats) {
        println!(
            "process\tphase={label}\tlive_allocated_bytes={}\tresident_bytes={}",
            stats.allocated, stats.resident
        );
    }

    fn run_case(worker_count: usize, sentence_len: usize, matcher: &BoundCatalog) {
        let pool = ThreadPoolBuilder::new()
            .num_threads(worker_count)
            .build()
            .expect("measurement pool must build");
        let tokens = tokens(sentence_len, matcher);
        let before_warmup = process_stats();
        let mut workers = run_workers(&pool, matcher, &tokens);
        let after_match = process_stats();
        let mut after_clear = clear_workers(&pool);
        let process_after_clear = process_stats();
        workers.sort_unstable_by_key(|worker| worker.index);
        after_clear.sort_unstable_by_key(|(index, _)| *index);

        println!(
            "case\tworkers={worker_count}\ttokens={sentence_len}\tpredicates={PREDICATE_COUNT}"
        );
        print_process("before_warmup", before_warmup);
        print_process("after_match", after_match);
        print_process("after_clear", process_after_clear);
        for (worker, (_, after_clear)) in workers.into_iter().zip(after_clear) {
            println!(
                "worker\tindex={}\tbefore_warmup={}\twarmup_allocated_bytes={}\tmeasured_allocated_bytes={}\tafter_clear={}\tresult_struct_bytes={}",
                worker.index,
                worker.before_warmup,
                worker.after_warmup.saturating_sub(worker.before_warmup),
                worker.after_match.saturating_sub(worker.after_warmup),
                after_clear,
                worker.result_struct_bytes,
            );
        }
    }

    fn measure_artifact(label: &str, source: &str) {
        clear_pools();
        let before = process_stats();
        let matcher = compile_catalog(
            PatternSource::named("matcher-artifact.lx", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{label} artifact must compile: {error}"))
        .matcher;
        let after_one = process_stats();
        let clones = (0..10).map(|_| matcher.clone()).collect::<Vec<_>>();
        let after_ten_clones = process_stats();
        println!(
            "artifact\tlabel={label}\tbefore_live_bytes={}\tone_matcher_live_bytes={}\tten_clones_live_bytes={}\tone_matcher_delta_bytes={}\tten_clone_delta_bytes={}",
            before.allocated,
            after_one.allocated,
            after_ten_clones.allocated,
            after_one.allocated.saturating_sub(before.allocated),
            after_ten_clones
                .allocated
                .saturating_sub(after_one.allocated),
        );
        drop(clones);
        drop(matcher);
    }

    pub(super) fn run() {
        let predicate_catalog = predicate_catalog();
        for (label, source) in [
            ("empty", ""),
            ("new-list", include_str!("../../patterns/sfe.lx")),
            (
                "sf-experimental",
                include_str!("../../patterns/sf-experimental.lx"),
            ),
            ("predicate-512", predicate_catalog.as_str()),
        ] {
            measure_artifact(label, source);
        }

        let matcher = bind(&matcher());
        for worker_count in WORKER_COUNTS {
            for sentence_len in SENTENCE_LENGTHS {
                run_case(worker_count, sentence_len, &matcher);
            }
        }
    }
}

#[cfg(not(target_env = "msvc"))]
fn main() {
    supported::run();
}

#[cfg(target_env = "msvc")]
fn main() {
    println!(
        "measure_matcher_memory is unsupported on MSVC because jemalloc counters are unavailable"
    );
}
