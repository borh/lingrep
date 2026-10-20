use lingrep_core::fsm::{
    AnalyzerCompatibility, BoundExecutionError, CompileLimits, PatternSource, SchemaBindError,
    compile_catalog,
};
use lingrep_core::token_match::{
    AttributeKey, AttributeKind, BoundTokenSentence, ProjectedAttributeValue, SchemaAttribute,
    TokenAttributeIndex, TokenInfo, TokenSchema,
};

#[test]
fn canonical_keys_compile_symbolically_and_bind_by_kind() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [text=猫]."),
        CompileLimits::default(),
    )
    .expect("the canonical portable key compiles")
    .matcher;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap();

    let bound = compiled.bind(&schema).expect("scalar form binds as scalar");
    assert_eq!(
        bound.projection().keys(),
        &[AttributeKey::new("text").unwrap()]
    );
}

#[test]
fn set_membership_binds_to_a_set_ordinal_and_matches_canonical_values() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [feat.Number has Sing]."),
        CompileLimits::default(),
    )
    .expect("the set predicate compiles symbolically")
    .matcher;
    let schema =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap()])
            .unwrap();
    let bound = compiled
        .bind(&schema)
        .expect("the set predicate binds to a set");
    let sentence = BoundTokenSentence::from_projected_attribute_values(
        bound.projection(),
        1,
        vec![ProjectedAttributeValue::string_set(["Plur", "Sing"])],
    )
    .unwrap();

    assert_eq!(bound.find_matches(&sentence).unwrap().len(), 1);
}

#[test]
fn scalar_operators_reject_set_schema_keys_at_bind_time() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [feat.Number=Sing]."),
        CompileLimits::default(),
    )
    .expect("operator kind is resolved only when a schema is selected")
    .matcher;
    let schema =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap()])
            .unwrap();

    let error = compiled
        .bind(&schema)
        .expect_err("scalar equality cannot read a set-valued key");
    assert!(error.to_string().contains("feat.Number"));
    assert!(error.to_string().contains("scalar"));
}

#[test]
fn scalar_key_projections_reject_set_schema_keys_at_bind_time() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "key: [feat.Number]\npattern p := [feat.Number has Sing].",
        ),
        CompileLimits::default(),
    )
    .expect("the set predicate and symbolic key compile")
    .matcher;
    let schema =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap()])
            .unwrap();

    let error = compiled
        .bind(&schema)
        .expect_err("a scalar record key cannot project a set-valued attribute");
    assert!(error.to_string().contains("feat.Number"));
    assert!(error.to_string().contains("scalar"));
}

#[test]
fn every_scalar_publication_reader_rejects_a_set_schema_key() {
    let sources = [
        (
            "transform: [{op: \"substitute\", component: \"last\", attr: \"feat.Number\"}, {op: \"join\"}]\npattern p := [text=x].",
            "feat.Number",
        ),
        (
            "tags: {number: {$token: \"feat.Number\"}}\npattern p := (?<token> [text=x]).",
            "feat.Number",
        ),
        ("lexicon words by [norm] := {\"x\"}.", "norm"),
    ];
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
        SchemaAttribute::new("norm", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap();

    for (source, key) in sources {
        let compiled = compile_catalog(
            PatternSource::named("probe", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("fixture must compile: {source}\n{error}"))
        .matcher;
        let error = match compiled.bind(&schema) {
            Ok(_) => panic!("set-valued publication read must be refused: {source}"),
            Err(error) => error,
        };
        assert!(error.to_string().contains(key), "{error}");
        assert!(error.to_string().contains("scalar"), "{error}");
    }
}

#[test]
fn transform_guard_operators_contribute_their_value_kind() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "transform: [{op: \"drop_edge\", edge: \"trailing\", drop: \"[feat.Number has Sing]\"}, {op: \"join\"}]\npattern p := [text=x]+.",
        ),
        CompileLimits::default(),
    )
    .expect("the set-valued guard compiles")
    .matcher;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("feat.Number", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();

    let error = compiled
        .bind(&schema)
        .expect_err("a set-valued guard cannot read a scalar attribute");
    assert!(error.to_string().contains("feat.Number"));
    assert!(error.to_string().contains("string-set"));
}

#[test]
fn binding_can_add_a_scalar_key_without_an_enum_variant() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [lemma=猫]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["lemma", "provider.scalar9"]).unwrap();
    let bound = compiled
        .bind_with_projection(&schema, [AttributeKey::new("provider.scalar9").unwrap()])
        .expect("symbolic publication key binds");

    assert_eq!(
        bound
            .projection()
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["lemma", "provider.scalar9"]
    );
}

#[test]
fn one_compiled_catalog_binds_to_a_schema_once() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [text=猫 & upos=NOUN]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;

    let bound = compiled
        .bind(
            &TokenSchema::new([
                SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
                SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
            ])
            .unwrap(),
        )
        .expect("the canonical schema provides both attributes");

    assert_eq!(
        bound
            .projection()
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["text", "upos"]
    );
}

/// The 2026-08-28 contract move: a referenced attribute the schema cannot
/// supply binds as universally absent — reported, not refused. Refusal would
/// reject working catalogs, because a complemented test over another
/// analyzer's attribute legitimately holds on a corpus that lacks it.
#[test]
fn an_unavailable_attribute_binds_as_universally_absent() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [text=猫]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["lemma"]).expect("the narrow schema is valid");

    let bound = compiled
        .bind(&schema)
        .expect("an unavailable ordinary attribute is a report, not a refusal");

    assert_eq!(
        bound.unavailable_attributes(),
        &[AttributeKey::new("text").unwrap()]
    );
    assert!(bound.projection().keys().is_empty());
    let tokens =
        BoundTokenSentence::from_projected_values(bound.projection(), 1, Vec::new()).unwrap();
    assert_eq!(
        bound.find_matches(&tokens).unwrap(),
        vec![],
        "a positive test over an absent attribute matches no token"
    );
}

/// The case that makes the relaxation semantics, not leniency: `!(…)` is a
/// true complement, so a token — and hence a whole corpus — lacking the
/// attribute satisfies it.
#[test]
fn a_true_complement_over_an_unavailable_attribute_holds() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [!(upos=NOUN)]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["text"]).unwrap();

    let bound = compiled
        .bind(&schema)
        .expect("the complement catalog binds");

    assert_eq!(
        bound.unavailable_attributes(),
        &[AttributeKey::new("upos").unwrap()]
    );
    // The catalog references only `upos`, so the bound projection is empty.
    let tokens =
        BoundTokenSentence::from_projected_values(bound.projection(), 1, Vec::new()).unwrap();
    assert_eq!(
        bound.find_matches(&tokens).unwrap().len(),
        1,
        "the complement holds for every token of a schema lacking the attribute"
    );
}

/// `!=` is not the complement: absence satisfies neither polarity, so the
/// exclusion matches nothing on a schema that lacks its attribute.
#[test]
fn an_absence_sensitive_exclusion_over_an_unavailable_attribute_never_matches() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [lemma!=x]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["text"]).unwrap();

    let bound = compiled.bind(&schema).expect("the exclusion catalog binds");

    // The catalog references only `lemma`, so the bound projection is empty.
    let tokens =
        BoundTokenSentence::from_projected_values(bound.projection(), 1, Vec::new()).unwrap();
    assert_eq!(bound.find_matches(&tokens).unwrap(), vec![]);
}

/// An unavailable `same_as` projection reads as absent on both spans, and
/// absent-to-absent is not equality.
#[test]
fn a_same_as_projection_over_an_unavailable_attribute_never_equates() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "p := (?<u> [text=あ]) same_as($u, by=unidic.kana).",
        ),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["text"]).unwrap();

    let bound = compiled.bind(&schema).expect("the same_as catalog binds");

    assert_eq!(
        bound.unavailable_attributes(),
        &[AttributeKey::new("unidic.kana").unwrap()]
    );
    let tokens = BoundTokenSentence::from_projected_values(
        bound.projection(),
        2,
        vec![Some("あ".into()), Some("あ".into())],
    )
    .unwrap();
    assert_eq!(bound.find_matches(&tokens).unwrap(), vec![]);
}

/// A publication attribute the caller projects is reported the same way; the
/// projection simply omits it, so a published token never carries it.
#[test]
fn an_unavailable_publication_attribute_binds_and_is_reported() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [lemma=猫]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["lemma"]).unwrap();

    let bound = compiled
        .bind_with_projection(&schema, [AttributeKey::new("provider.absent9").unwrap()])
        .expect("an unavailable publication key is a report, not a refusal");

    assert_eq!(
        bound.unavailable_attributes(),
        &[AttributeKey::new("provider.absent9").unwrap()]
    );
    assert_eq!(
        bound
            .projection()
            .keys()
            .iter()
            .map(AttributeKey::as_str)
            .collect::<Vec<_>>(),
        ["lemma"]
    );
}

/// The one absence that stays a refusal: `where`-clause edge attributes, whose
/// absence makes the constraint meaningless rather than unsatisfied. The
/// refusal names only those keys, even when ordinary references are missing
/// alongside them.
#[test]
fn a_where_clause_refusal_names_only_the_edge_attributes() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "pattern p := (?<s> [unidic.pos1=名詞]) (?<v> [text=走る]) where $v -[nsubj]-> $s.",
        ),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["text"]).unwrap();

    let error = compiled
        .bind(&schema)
        .expect_err("dependency constraints cannot bind without their edges");

    assert_eq!(
        error,
        SchemaBindError::MissingKeys(vec!["dep.head".into(), "dep.rel".into()]),
        "unidic.pos1 is unavailable too, but only the edge keys refuse"
    );
    assert_eq!(error.missing_keys(), &["dep.head", "dep.rel"]);
}

#[test]
fn a_basic_head_endpoint_refuses_a_schema_without_basic_heads() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "pattern p := (?<d> []) where not basic_head($d) -[enhanced *]-> $d.",
        ),
        CompileLimits::default(),
    )
    .expect("the derived endpoint compiles")
    .matcher;
    let schema = TokenSchema::from_keys(["dep.enhanced"]).unwrap();

    let error = compiled
        .bind(&schema)
        .expect_err("a derived basic head cannot bind without dep.head");

    assert_eq!(error, SchemaBindError::MissingKeys(vec!["dep.head".into()]));
}

#[test]
fn relation_owned_attributes_refuse_the_wrong_schema_kind() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "pattern p := (?<d> []) where not basic_head($d) -[enhanced *]-> $d.",
        ),
        CompileLimits::default(),
    )
    .expect("the derived endpoint compiles")
    .matcher;

    for (key, actual, expected) in [
        ("dep.head", AttributeKind::StringSet, AttributeKind::Scalar),
        (
            "dep.enhanced",
            AttributeKind::Scalar,
            AttributeKind::StringSet,
        ),
    ] {
        let schema = TokenSchema::new([
            SchemaAttribute::new(
                "dep.head",
                if key == "dep.head" {
                    actual
                } else {
                    AttributeKind::Scalar
                },
            )
            .unwrap(),
            SchemaAttribute::new(
                "dep.enhanced",
                if key == "dep.enhanced" {
                    actual
                } else {
                    AttributeKind::StringSet
                },
            )
            .unwrap(),
        ])
        .unwrap();
        assert_eq!(
            compiled
                .bind(&schema)
                .expect_err("the wrong relation kind must be refused"),
            SchemaBindError::KindMismatch {
                key: AttributeKey::new(key).unwrap(),
                expected,
                actual,
            }
        );
    }
}

#[test]
fn a_bound_catalog_rejects_tokens_from_another_schema_digest() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [text=猫]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let bound = compiled
        .bind(
            &TokenSchema::new([SchemaAttribute::new("text", AttributeKind::Scalar).unwrap()])
                .unwrap(),
        )
        .expect("the canonical schema provides form");
    let other = TokenSchema::from_keys(["lemma"]).expect("the alternate schema is valid");
    let other_projection = other
        .bind_projection([TokenAttributeIndex::Lemma])
        .expect("the alternate schema provides lemma");
    let tokens = BoundTokenSentence::project_for_schema(
        &other,
        &other_projection,
        &[TokenInfo::default().with_orth("猫")],
    )
    .expect("the alternate projection agrees with its schema");

    assert!(matches!(
        bound.validate_tokens(&tokens),
        Err(BoundExecutionError::SchemaMismatch { .. })
    ));
}

#[test]
fn a_bound_catalog_executes_over_projected_tokens() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [text=猫 & upos=NOUN]."),
        CompileLimits::default(),
    )
    .expect("the probe catalog compiles")
    .matcher;
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("upos", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let bound = compiled
        .bind(&schema)
        .expect("the current schema provides the projection");
    let tokens = BoundTokenSentence::from_projected_values(
        bound.projection(),
        1,
        vec![Some("猫".into()), Some("NOUN".into())],
    )
    .expect("the projected values agree with the canonical schema");

    let matches = bound.find_matches(&tokens).expect("schemas agree");

    assert_eq!(matches.len(), 1);
    assert_eq!((matches[0].start, matches[0].end), (0, 1));
    assert_eq!(matches[0].pattern.as_str(), "p");
}

#[test]
fn analyzer_declarations_are_complete_alternatives() {
    let compiled = compile_catalog(
        PatternSource::named(
            "probe",
            "analyzer: { family: \"sudachi\", resource: \"core\", split: \"C\", segmentation: [\"input\"] }\n\
             analyzer: { family: \"spacy\", resource: \"ginza\", segmentation: [\"input\", \"analyzer\"] }\n\
             p := [text=猫].",
        ),
        CompileLimits::default(),
    )
    .unwrap()
    .matcher;

    assert_eq!(compiled.analyzer_constraints().len(), 2);
    assert_eq!(
        compiled.analyzer_compatibility("spacy", Some("ginza"), None, "analyzer"),
        AnalyzerCompatibility::Compatible
    );
    assert_eq!(
        compiled.analyzer_compatibility("sudachi", Some("core"), Some("A"), "input"),
        AnalyzerCompatibility::Mismatch
    );
}

#[test]
fn analyzer_requires_one_family_and_unique_fields() {
    for source in [
        "analyzer: { resource: \"core\" }\np := [].",
        "analyzer: { family: \"spacy\", family: \"stanza\" }\np := [].",
        "analyzer: { family: \"spacy\", segmentation: [\"input\", \"input\"] }\np := [].",
    ] {
        let error = match compile_catalog(
            PatternSource::named("probe", source),
            CompileLimits::default(),
        ) {
            Ok(_) => panic!("malformed analyzer constraints must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("analyzer"), "{error}");
    }
}

#[test]
fn retired_dictionary_metadata_is_a_hard_error() {
    let error = match compile_catalog(
        PatternSource::named("probe", "dictionary: \"unidic-cwj\"\np := []."),
        CompileLimits::default(),
    ) {
        Ok(_) => panic!("dictionary metadata was removed at the V1 cutover"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("retired `dictionary:`"),
        "{error}"
    );
}

/// `~` and `!~` read whichever kind the schema declares.
///
/// Every other operator names one physical kind, so the schema decides
/// whether a catalog binds at all. Containment and regex search do not: an
/// entry of a string set is a string, so the test has an answer about it, and
/// over a set the operator is existential exactly as `has` is. This is the one
/// route to GiNZA's `Inf=` MISC entry, whose value is
/// `ginza.inflection()` — 139 distinct spellings on a 600-sentence sample, an
/// inventory that belongs to the data rather than to the query.
#[test]
fn containment_and_regex_read_either_value_kind() {
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("conllu.misc", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap();

    for (source, expected) in [
        // Prefix, which `has` could only reach by enumerating every value.
        ("p := [conllu.misc ~ Inf=五段].", 1),
        ("p := [conllu.misc ~ Inf=一段].", 0),
        // Regex, the form natsume-simple spells as
        // `^(五|上|下|サ|.変格|助動詞).+` over `ginza.inflection()`.
        (
            "p := [conllu.misc ~ /\\AInf=(五|上|下|サ|.変格|助動詞)/].",
            1,
        ),
        ("p := [conllu.misc ~ /\\AInf=(上|下)/].", 0),
        // Negation is "no entry matches", mirroring `not has`.
        ("p := [conllu.misc !~ Inf=一段].", 1),
        ("p := [conllu.misc !~ Inf=五段].", 0),
    ] {
        let compiled = compile_catalog(
            PatternSource::named("probe", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{source} must compile: {error}"))
        .matcher;
        let bound = compiled
            .bind(&schema)
            .unwrap_or_else(|error| panic!("{source} must bind to a string-set schema: {error}"));
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            bound.projection(),
            1,
            vec![ProjectedAttributeValue::string_set([
                "BunsetuBILabel=B",
                "Inf=五段-カ行,連体形-一般",
            ])],
        )
        .unwrap();
        assert_eq!(
            bound.find_matches(&sentence).unwrap().len(),
            expected,
            "{source}"
        );
    }
}

/// Kind polymorphism is not kind blindness: the scalar reading is unchanged,
/// and it is the reading a scalar schema still gets.
#[test]
fn containment_over_a_scalar_attribute_is_unchanged() {
    let compiled = compile_catalog(
        PatternSource::named("probe", "p := [lemma ~ 猫]."),
        CompileLimits::default(),
    )
    .expect("the scalar containment catalog compiles")
    .matcher;
    let schema =
        TokenSchema::new([SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap()]).unwrap();
    let bound = compiled.bind(&schema).expect("a scalar schema still binds");
    let sentence =
        BoundTokenSentence::from_projected_values(bound.projection(), 1, vec![Some("子猫".into())])
            .unwrap();

    assert_eq!(bound.find_matches(&sentence).unwrap().len(), 1);
}

/// A token lacking the attribute satisfies neither polarity, over a set for
/// the same reason it does over a scalar. Nothing about the set reading
/// weakens the language's one three-valuedness rule; `!(…)` remains the
/// spelling that holds for absence.
#[test]
fn neither_polarity_holds_for_a_token_with_no_entries() {
    let schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("conllu.misc", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap();

    for (source, expected) in [
        ("p := [conllu.misc ~ Inf=].", 0),
        ("p := [conllu.misc !~ Inf=].", 0),
        ("p := [!(conllu.misc ~ Inf=)].", 1),
    ] {
        let compiled = compile_catalog(
            PatternSource::named("probe", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{source} must compile: {error}"))
        .matcher;
        let bound = compiled.bind(&schema).expect("the catalog binds");
        let sentence = BoundTokenSentence::from_projected_attribute_values(
            bound.projection(),
            1,
            vec![ProjectedAttributeValue::string_set(Vec::<String>::new())],
        )
        .unwrap();
        assert_eq!(
            bound.find_matches(&sentence).unwrap().len(),
            expected,
            "{source}"
        );
    }
}

/// The refusals that stay. `=`, `!=`, `in` and `not in` still name one kind,
/// so widening `~` did not turn the bind-time kind check into advice.
#[test]
fn the_scalar_only_operators_still_refuse_a_set_schema_key() {
    let schema =
        TokenSchema::new([SchemaAttribute::new("conllu.misc", AttributeKind::StringSet).unwrap()])
            .unwrap();

    for source in [
        "p := [conllu.misc=Inf=x].",
        "p := [conllu.misc!=x].",
        "p := [conllu.misc in {x, y}].",
        "p := [conllu.misc not in {x, y}].",
    ] {
        let compiled = compile_catalog(
            PatternSource::named("probe", source),
            CompileLimits::default(),
        )
        .unwrap_or_else(|error| panic!("{source} must compile: {error}"))
        .matcher;
        let error = match compiled.bind(&schema) {
            Ok(_) => panic!("{source} must be refused against a string-set schema"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("scalar"), "{source}: {error}");
    }
}
