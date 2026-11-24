use lingrep_web::corpus_set::CorpusName;
use lingrep_web::state::{Config, DetailLimits, RequestStore};

#[test]
fn plan_access_fails_cleanly_for_an_empty_dict_path() {
    // A default/empty-dict store must not panic on plan(); it returns Err.
    let store = RequestStore::default();
    assert!(
        store.context.plan().is_err(),
        "empty dict path yields TokenizerUnavailable, not a panic"
    );
}

#[test]
fn plan_access_memoizes_a_real_dictionary() {
    let dict = lingrep_core::testing::dictionary_path();
    let store = RequestStore::new(Config {
        dict_path: Some(dict),
        // new() builds a runtime; if that requires a real corpus,
        corpora: vec![(CorpusName::default_key(), "unused".into(), "unused".into())],
        input_format: None,
        markdown: lingrep_core::corpus::MarkdownReaderConfig::new(
            lingrep_core::corpus::SectionPolicy::default(),
        ),
        max_record_bytes: lingrep_core::execution::DEFAULT_MAX_INPUT_RECORD_BYTES,
        detail_limits: DetailLimits { max_kwic: 10 },
    });
    let a = store.context.plan().expect("real dict builds");
    let b = store.context.plan().expect("second call");
    use lingrep_core::analysis::{
        ExtractionProfile, NormalizationPolicy, SegmentationMode, VibratoPlan,
    };
    let fingerprint = store
        .context
        .dictionary_fingerprint()
        .expect("dictionary identity");
    assert_eq!(
        a.descriptor(),
        &VibratoPlan::descriptor_for(
            fingerprint,
            ExtractionProfile::Morphology,
            SegmentationMode::Input,
            NormalizationPolicy::None,
        ),
        "live and prepared analysis identify the same dictionary"
    );
    assert!(
        std::sync::Arc::ptr_eq(&a, &b),
        "the plan is memoized, not rebuilt"
    );
}

#[test]
fn concurrent_first_callers_share_one_plan() {
    const CALLERS: usize = 8;
    let dictionary = lingrep_core::testing::MiniDictionary::new();
    let context = lingrep_web::server_context::ServerContext::new(dictionary.path()).unwrap();
    let start = std::sync::Barrier::new(CALLERS);
    let plans = std::thread::scope(|scope| {
        let callers = (0..CALLERS)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    context.plan().expect("the mini dictionary builds")
                })
            })
            .collect::<Vec<_>>();
        callers
            .into_iter()
            .map(|caller| caller.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(
        plans
            .iter()
            .all(|plan| std::sync::Arc::ptr_eq(plan, &plans[0])),
        "each caller received the plan the first build produced"
    );
}
