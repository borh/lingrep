use lingrep_core::fsm::{CompileLimits, PatternSource, compile_catalog};
use lingrep_core::token_match::{
    AttributeKey, BoundTokenSentence, TokenAttributeIndex, TokenInfo, TokenSchema,
};

fn catalog(source: &str) -> lingrep_core::fsm::CompiledCatalog {
    compile_catalog(
        PatternSource::named("structural", source),
        CompileLimits::default(),
    )
    .expect("structural catalog compiles")
    .matcher
}

#[test]
fn narrow_projection_is_the_union_of_matching_and_publication_requirements() {
    let compiled = catalog("p := [lemma=猫] [upos=NOUN].");
    let bound = compiled
        .bind_with_projection(
            &TokenSchema::unidic_morphology(),
            [TokenAttributeIndex::Orth, TokenAttributeIndex::Lemma],
        )
        .expect("current schema binds");

    assert_eq!(bound.projection().len(), 3);
    assert_eq!(
        bound.projection().attributes(),
        &[
            AttributeKey::new("text").unwrap(),
            AttributeKey::new("lemma").unwrap(),
            AttributeKey::new("upos").unwrap(),
        ]
    );
}

#[test]
fn repeated_and_independent_binds_have_stable_ordinals_and_state() {
    let compiled = catalog("p := [text=猫].");
    let schema = TokenSchema::unidic_morphology();
    let first = compiled.bind(&schema).expect("first bind");
    let second = compiled.bind(&schema).expect("second bind");
    assert_eq!(first.projection(), second.projection());

    let cat =
        BoundTokenSentence::project(first.projection(), &[TokenInfo::default().with_orth("猫")])
            .expect("cat projection");
    let dog =
        BoundTokenSentence::project(second.projection(), &[TokenInfo::default().with_orth("犬")])
            .expect("dog projection");
    assert_eq!(first.find_matches(&cat).expect("matching schema").len(), 1);
    assert!(
        second
            .find_matches(&dog)
            .expect("matching schema")
            .is_empty()
    );
}

#[test]
fn widest_projection_reconstructs_all_current_values_exactly() {
    let schema = TokenSchema::unidic_morphology();
    let projection = schema
        .bind_projection(schema.keys().iter().cloned())
        .expect("widest current projection binds");
    let mut expected = TokenInfo::default();
    for (index, attribute) in TokenAttributeIndex::ALL
        .into_iter()
        .filter(|attribute| *attribute != TokenAttributeIndex::Text)
        .enumerate()
    {
        expected.set(attribute, format!("value-{index}").into());
    }
    let sentence = BoundTokenSentence::project(&projection, &[expected.clone()])
        .expect("widest projection succeeds");
    let actual = TokenInfo::from_sequence(&sentence, 0).expect("the token exists");

    assert_eq!(sentence.width(), TokenAttributeIndex::COUNT - 1);
    assert_eq!(actual, expected);
}
