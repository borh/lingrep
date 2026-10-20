use lingrep_core::token_match::{
    AttributeKey, AttributeKind, AttributeOrdinal, AttributeValueRef, BoundTokenError,
    BoundTokenSentence, DenseStorageKind, ProjectedAttributeValue, SchemaAttribute,
    TokenAttributeIndex, TokenInfo, TokenSchema,
};

#[test]
fn v8_schema_canonicalizes_namespaces_and_hashes_value_kinds() {
    let attributes = [
        SchemaAttribute::new("spacy.morph.reading", AttributeKind::StringSet).unwrap(),
        SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
        SchemaAttribute::new("xpos", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("feat.Gender[psor]", AttributeKind::StringSet).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
    ];
    let schema = TokenSchema::new(attributes).unwrap();

    assert_eq!(
        schema
            .attributes()
            .iter()
            .map(|attribute| (attribute.key().as_str(), attribute.kind()))
            .collect::<Vec<_>>(),
        [
            ("text", AttributeKind::Scalar),
            ("lemma", AttributeKind::Scalar),
            ("xpos", AttributeKind::Scalar),
            ("feat.Gender[psor]", AttributeKind::StringSet),
            ("feat.Number", AttributeKind::StringSet),
            ("spacy.morph.reading", AttributeKind::StringSet),
        ]
    );

    let scalar =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::Scalar).unwrap()])
            .unwrap();
    let set =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap()])
            .unwrap();
    assert_ne!(scalar.digest(), set.digest());
    assert!(
        TokenSchema::new([
            SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
            SchemaAttribute::new("text", AttributeKind::StringSet).unwrap(),
        ])
        .is_err()
    );
}

#[test]
fn scalar_and_mixed_sentences_select_distinct_dense_layouts() {
    let scalar_schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
    ])
    .unwrap();
    let scalar_projection = scalar_schema
        .bind_projection([
            AttributeKey::new("text").unwrap(),
            AttributeKey::new("lemma").unwrap(),
        ])
        .unwrap();
    let scalar = BoundTokenSentence::from_projected_attribute_values(
        &scalar_projection,
        1,
        vec![
            ProjectedAttributeValue::scalar("Cats"),
            ProjectedAttributeValue::scalar("cat"),
        ],
    )
    .unwrap();
    assert_eq!(scalar.storage_kind(), DenseStorageKind::Scalars);
    assert_eq!(
        scalar.token(0).unwrap().value(AttributeOrdinal::new(0)),
        Some(AttributeValueRef::Scalar("Cats"))
    );

    let mixed_schema = TokenSchema::new([
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap(),
    ])
    .unwrap();
    let mixed_projection = mixed_schema
        .bind_projection([
            AttributeKey::new("text").unwrap(),
            AttributeKey::new("feat.Number").unwrap(),
        ])
        .unwrap();
    let mixed = BoundTokenSentence::from_projected_attribute_values(
        &mixed_projection,
        2,
        vec![
            ProjectedAttributeValue::scalar("cats"),
            ProjectedAttributeValue::string_set(["Sing", "Plur", "Sing"]),
            ProjectedAttributeValue::scalar("run"),
            ProjectedAttributeValue::string_set(std::iter::empty::<&str>()),
        ],
    )
    .unwrap();
    assert_eq!(mixed.storage_kind(), DenseStorageKind::Mixed);
    assert_eq!(
        mixed.token(0).unwrap().value(AttributeOrdinal::new(1)),
        Some(AttributeValueRef::StringSet(&[
            "Plur".into(),
            "Sing".into()
        ]))
    );
    assert_eq!(
        mixed.token(1).unwrap().value(AttributeOrdinal::new(1)),
        None
    );
}

#[test]
fn dense_constructor_rejects_schema_kind_mismatch() {
    let schema =
        TokenSchema::new([SchemaAttribute::new("feat.Number", AttributeKind::StringSet).unwrap()])
            .unwrap();
    let projection = schema
        .bind_projection([AttributeKey::new("feat.Number").unwrap()])
        .unwrap();
    assert!(matches!(
        BoundTokenSentence::from_projected_attribute_values(
            &projection,
            1,
            vec![ProjectedAttributeValue::scalar("Sing")],
        ),
        Err(BoundTokenError::KindMismatch { .. })
    ));
}

#[test]
fn scalar_schema_accepts_keys_without_a_rust_enum_variant() {
    let custom = AttributeKey::new("provider.scalar9").expect("canonical scalar key");
    let schema = TokenSchema::from_keys(["lemma", custom.as_str()]).expect("dynamic schema");
    let projection = schema
        .bind_projection([custom.clone()])
        .expect("dynamic key binds without a Rust enum variant");

    assert_eq!(projection.keys(), &[custom]);
    assert_eq!(
        projection.ordinal_key("provider.scalar9"),
        Some(AttributeOrdinal::new(0))
    );
}

#[test]
fn scalar_keys_are_case_sensitive_and_reject_malformed_paths() {
    for invalid in [
        "",
        ".lemma",
        "provider..lemma",
        "provider lemma",
        "feat.number",
    ] {
        assert!(AttributeKey::new(invalid).is_err(), "accepted {invalid:?}");
    }
}

#[test]
fn current_schema_binds_a_sorted_unique_projection_to_dense_ordinals() {
    let schema = TokenSchema::unidic_morphology();
    let projection = schema
        .bind_projection([
            TokenAttributeIndex::Orth,
            TokenAttributeIndex::Lemma,
            TokenAttributeIndex::Orth,
        ])
        .expect("the current attributes belong to the current schema");

    assert_eq!(
        projection.attributes(),
        &[TokenAttributeIndex::Orth, TokenAttributeIndex::Lemma]
    );
    assert_eq!(
        projection.ordinal(TokenAttributeIndex::Lemma),
        Some(AttributeOrdinal::new(1))
    );
    assert_eq!(
        projection.ordinal(TokenAttributeIndex::Orth),
        Some(AttributeOrdinal::new(0))
    );
    assert_eq!(projection.ordinal(TokenAttributeIndex::Pos), None);
}

#[test]
fn schema_digest_is_canonical_and_covers_spelling() {
    let current = TokenSchema::unidic_morphology();
    let mut reversed = current.attributes().to_vec();
    reversed.reverse();
    let reversed = TokenSchema::new(reversed).expect("the reversed key set is valid");
    let renamed =
        TokenSchema::from_keys(["lemma", "norm"]).expect("the distinct renamed keys are valid");
    let original =
        TokenSchema::from_keys(["text", "lemma"]).expect("the distinct original keys are valid");

    assert_eq!(current.digest(), reversed.digest());
    assert_ne!(original.digest(), renamed.digest());
}

#[test]
fn schema_rejects_duplicate_and_empty_keys() {
    assert!(TokenSchema::from_keys(["text", "text"]).is_err());
    assert!(TokenSchema::from_keys(["text", ""]).is_err());
}

#[test]
fn projection_ordinals_follow_the_bound_schema_order() {
    let schema = TokenSchema::new([
        SchemaAttribute::new("lemma", AttributeKind::Scalar).unwrap(),
        SchemaAttribute::new("text", AttributeKind::Scalar).unwrap(),
    ])
    .expect("the canonical schema is valid");
    let projection = schema
        .bind_projection([
            AttributeKey::new("lemma").unwrap(),
            AttributeKey::new("text").unwrap(),
        ])
        .expect("both requested attributes belong to the schema");

    assert_eq!(
        projection.ordinal_key("text"),
        Some(AttributeOrdinal::new(0))
    );
    assert_eq!(
        projection.ordinal_key("lemma"),
        Some(AttributeOrdinal::new(1))
    );
}

#[test]
fn bound_sentence_projects_values_in_one_row_major_backing_store() {
    let schema = TokenSchema::unidic_morphology();
    let projection = schema
        .bind_projection([TokenAttributeIndex::Orth, TokenAttributeIndex::Lemma])
        .expect("the current attributes belong to the current schema");
    let source = [
        TokenInfo::default().with_orth("猫").with_lemma("猫"),
        TokenInfo::default().with_orth("走る"),
    ];

    let sentence = BoundTokenSentence::project_for_schema(&schema, &projection, &source)
        .expect("the projection and source schema agree");

    assert_eq!(sentence.len(), 2);
    assert_eq!(sentence.width(), 2);
    assert_eq!(sentence.backing_len(), 4);
    assert_eq!(
        sentence.token(0).unwrap().get(AttributeOrdinal::new(0)),
        Some("猫")
    );
    assert_eq!(
        sentence.token(1).unwrap().get(AttributeOrdinal::new(0)),
        Some("走る")
    );
    assert_eq!(
        sentence.token(1).unwrap().get(AttributeOrdinal::new(1)),
        None
    );
}

#[test]
fn bound_sentence_rejects_a_projection_from_another_schema() {
    let current = TokenSchema::unidic_morphology();
    let projection = current
        .bind_projection([TokenAttributeIndex::Orth])
        .expect("orth belongs to the current schema");
    let other = TokenSchema::from_keys(["text", "lemma"]).expect("the alternate schema is valid");

    assert!(matches!(
        BoundTokenSentence::project_for_schema(&other, &projection, &[]),
        Err(BoundTokenError::SchemaMismatch { .. })
    ));
}
