use std::io::{BufReader, Cursor};

use lingrep_core::corpus::{
    AdmittedCorpus, AdmittedCorpusEvent, CorpusEvent, InputFormat, MarkdownDiagnostics,
    MarkdownReaderConfig, MetadataScalar, MetadataValue, SectionPolicy, read_markdown_events,
    stream_input_events, stream_input_reader,
};

fn read_markdown(source: &str, options: MarkdownReaderConfig) -> std::io::Result<Vec<String>> {
    stream_input_reader(
        InputFormat::Markdown(options),
        BufReader::new(Cursor::new(source.to_owned())),
        None,
    )
    .collect()
}

#[test]
fn jsonl_expands_sentence_arrays_without_exposing_json_syntax() {
    let source = r#"{"title":"ignored","sentences":["first","second"]}
"#;
    let units = stream_input_reader(
        InputFormat::Jsonl,
        BufReader::new(Cursor::new(source)),
        None,
    )
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();
    assert_eq!(units, ["first", "second"]);
}

#[test]
fn plaintext_compatibility_uses_the_single_whitespace_admission_boundary() {
    let source = "一。\n   \n二。\n";
    let units = stream_input_reader(
        InputFormat::Plaintext,
        BufReader::new(Cursor::new(source)),
        None,
    )
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();
    assert_eq!(units, ["一。", "二。"])
}

/// V1 moves this unconditional Japanese framing behind analyzer segmentation.
#[test]
fn plaintext_currently_splits_japanese_sentences_within_one_physical_line() {
    let units = stream_input_reader(
        InputFormat::Plaintext,
        BufReader::new(Cursor::new("一。二！三？\n")),
        None,
    )
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();

    assert_eq!(units, ["一。", "二！", "三？"]);
}

#[test]
fn markdown_uses_the_default_research_section_policy() {
    let source = "---\ntitle: secret\n---\n\nbody\n\n::: {.references}\nexcluded\n:::\n";
    let units = read_markdown(source, MarkdownReaderConfig::new(SectionPolicy::default())).unwrap();
    assert_eq!(units, ["body"]);
}

#[test]
fn markdown_rejects_cumulative_frontmatter_beyond_the_default_bound() {
    let mut source = String::from("---\n");
    for _ in 0..40_000 {
        source.push_str("# x\n");
    }
    source.push_str("---\nbody\n");

    let error = read_markdown(&source, MarkdownReaderConfig::new(SectionPolicy::default()))
        .expect_err("small physical lines must not accumulate unbounded frontmatter");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("frontmatter"), "{error}");
    assert!(error.to_string().contains("131072"), "{error}");
}

#[test]
fn strict_markdown_rejects_conversion_artifacts() {
    let error = read_markdown(
        "<sup>7 note\n",
        MarkdownReaderConfig::new(SectionPolicy::default()),
    )
    .unwrap_err();
    assert!(error.to_string().contains("raw HTML"), "{error}");
}

#[test]
fn recovering_markdown_strips_known_presentation_tags_and_preserves_notation() {
    let source = concat!(
        "<center>\n",
        "<p align=\"right\">author</p>\n",
        "note<sup>7\n",
        "before<br>after\n",
        "<si P, Q> notation\n",
    );
    let units = read_markdown(
        source,
        MarkdownReaderConfig::new(SectionPolicy::default()).recover_conversion_artifacts(),
    )
    .unwrap();
    assert_eq!(
        units,
        ["author", "note7", "before after", "<si P, Q> notation"]
    );
}

#[test]
fn recovering_markdown_tolerates_only_document_boundary_div_imbalance() {
    let unclosed = concat!(
        "body\n",
        "::: {.references}\n",
        "excluded reference\n",
        "::: {.author}\n",
        "excluded author\n",
        ":::\n",
    );
    let trailing_close = "body\n:::\n";
    for source in [unclosed, trailing_close] {
        let units = read_markdown(
            source,
            MarkdownReaderConfig::new(SectionPolicy::default()).recover_conversion_artifacts(),
        )
        .unwrap();
        assert_eq!(units, ["body"]);
    }
}

#[test]
fn recovering_markdown_still_rejects_unknown_html() {
    let error = read_markdown(
        "<script>alert('not corpus text')</script>\n",
        MarkdownReaderConfig::new(SectionPolicy::default()).recover_conversion_artifacts(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("raw HTML"), "{error}");
}

#[test]
fn structured_admission_assigns_identity_once_after_whitespace_filtering() {
    let events = vec![
        Ok(CorpusEvent::DocumentStart {
            source_name: "fixture".into(),
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::ParagraphStart {
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::Sentence {
            text: "一。".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::Sentence {
            text: " \t".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::Sentence {
            text: "二。".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::ParagraphEnd),
        Ok(CorpusEvent::ParagraphStart {
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::Sentence {
            text: "三。".into(),
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::ParagraphEnd),
        Ok(CorpusEvent::DocumentEnd),
    ];

    let admitted = AdmittedCorpus::new(events.into_iter())
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    let sentences = admitted
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();

    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence.id.get())
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence.document_sentence_ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence
                .structure
                .divisions
                .iter()
                .find(|division| division.kind == "paragraph")
                .map(|division| division.id.0))
            .collect::<Vec<_>>(),
        [Some(0), Some(0), Some(1)]
    );
}

#[test]
fn structured_admission_rejects_sentence_outside_paragraph() {
    let events = vec![Ok(CorpusEvent::Sentence {
        text: "orphan".into(),
        words: None,
        enhanced_graph: None,
        active_labels: Box::new([]),
        metadata: Box::new([]),
    })];
    let error = AdmittedCorpus::new(events.into_iter())
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[hegel::test]
fn admitted_sentence_ids_are_contiguous_for_arbitrary_text(tc: hegel::TestCase) {
    use hegel::generators;

    let texts = tc.draw(generators::vecs(generators::text()).max_size(40));
    let mut events = vec![
        Ok(CorpusEvent::DocumentStart {
            source_name: "generated".into(),
            metadata: Box::new([]),
        }),
        Ok(CorpusEvent::ParagraphStart {
            metadata: Box::new([]),
        }),
    ];
    events.extend(texts.iter().cloned().map(|text| {
        Ok(CorpusEvent::Sentence {
            text,
            words: None,
            enhanced_graph: None,
            active_labels: Box::new([]),
            metadata: Box::new([]),
        })
    }));
    events.extend([Ok(CorpusEvent::ParagraphEnd), Ok(CorpusEvent::DocumentEnd)]);

    let admitted = AdmittedCorpus::new(events.into_iter())
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    let ids = admitted
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .map(|sentence| sentence.id.get())
        .collect::<Vec<_>>();
    let expected =
        (0..texts.iter().filter(|text| !text.trim().is_empty()).count() as u64).collect::<Vec<_>>();

    assert_eq!(ids, expected);
}

#[test]
fn markdown_structured_provenance_preserves_frontmatter_paragraphs_and_labels() {
    let source = concat!(
        "---\n",
        "year: '2013'\n",
        "keywords: [社会学, 農学]\n",
        "---\n",
        "# 題 {.title}\n",
        "## 方法 {.methods}\n",
        "第一文。\n",
        "\n",
        "### 小見出し\n",
        "第二文。\n",
        "## 文献 {.references}\n",
        "::: {.caption}\n",
        "第三文。\n",
        ":::\n",
    );
    let permissive = MarkdownReaderConfig::new(SectionPolicy::new(None, Default::default()));
    let events = AdmittedCorpus::new(read_markdown_events(
        BufReader::new(Cursor::new(source)),
        permissive,
        MarkdownDiagnostics::default(),
        "fixture.md".into(),
    ))
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();

    let metadata = match &events[0] {
        AdmittedCorpusEvent::DocumentStart { metadata, .. } => metadata,
        event => panic!("expected document start, got {event:?}"),
    };
    assert!(metadata.iter().any(|(key, value)| {
        key == "year" && value == &MetadataValue::Scalar(MetadataScalar::String("2013".into()))
    }));
    let headings = events
        .iter()
        .filter_map(|event| match event {
            AdmittedCorpusEvent::DivisionStart(division) if division.kind == "section" => {
                Some(division)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let titles = headings
        .iter()
        .map(|division| {
            let metadata = lingrep_core::corpus::divisions::metadata_json(&division.metadata);
            let parent = division.parent_id.map(|id| {
                let parent = headings.iter().find(|division| division.id == id).unwrap();
                lingrep_core::corpus::divisions::metadata_json(&parent.metadata)["title"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            });
            (
                metadata["level"].as_u64().unwrap(),
                metadata["title"].as_str().unwrap().to_owned(),
                parent,
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        titles,
        vec![
            (1, "題".into(), None),
            (2, "方法".into(), Some("題".into())),
            (3, "小見出し".into(), Some("方法".into())),
            (2, "文献".into(), Some("題".into()))
        ]
    );

    let sentences = events
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence
                .structure
                .divisions
                .iter()
                .find(|division| division.kind == "paragraph")
                .map(|division| division.id.0))
            .collect::<Vec<_>>(),
        [Some(2), Some(4), Some(4)]
    );
    assert_eq!(sentences[0].active_labels.as_ref(), ["methods"]);
    assert_eq!(sentences[1].active_labels.as_ref(), ["methods"]);
    assert_eq!(
        sentences[2].active_labels.as_ref(),
        ["caption", "references"]
    );
    assert!(
        sentences
            .iter()
            .flat_map(|sentence| sentence.active_labels.iter())
            .all(|label| label != "title")
    );

    let default_events = AdmittedCorpus::new(read_markdown_events(
        BufReader::new(Cursor::new(source)),
        MarkdownReaderConfig::new(SectionPolicy::default()),
        MarkdownDiagnostics::default(),
        "fixture.md".into(),
    ))
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();
    let default_sentences = default_events
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();
    assert_eq!(
        default_sentences
            .iter()
            .map(|sentence| sentence.text.as_str())
            .collect::<Vec<_>>(),
        ["第一文。", "第二文。"]
    );
    assert_eq!(
        default_sentences
            .iter()
            .map(|sentence| sentence
                .structure
                .divisions
                .iter()
                .find(|division| division.kind == "paragraph")
                .map(|division| division.id.0))
            .collect::<Vec<_>>(),
        [Some(2), Some(4)]
    );
}

#[test]
fn jsonl_structured_documents_preserve_flexible_metadata_and_paragraphs() {
    let source = concat!(
        "{\"sentences\":[\"一。\",\"\",\"二。\"],\"corpus\":\"legacy-extra\"}\n",
        "{\"metadata\":{\"year\":\"2013\",\"keywords\":[\"社会学\",\"農学\"]},\"paragraphs\":[{\"metadata\":{\"speaker\":\"A\"},\"sentences\":[\"三。\"]},{\"sentences\":[\"四。\"]}]}\n",
    );
    let events = AdmittedCorpus::new(stream_input_events(
        InputFormat::Jsonl,
        BufReader::new(Cursor::new(source)),
        "fixture.jsonl".into(),
        None,
    ))
    .collect::<std::io::Result<Vec<_>>>()
    .unwrap();

    let sentences = events
        .iter()
        .filter_map(AdmittedCorpusEvent::sentence)
        .collect::<Vec<_>>();
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence.id.get())
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence.document_sentence_ordinal)
            .collect::<Vec<_>>(),
        [0, 1, 0, 1]
    );
    assert_eq!(
        sentences
            .iter()
            .map(|sentence| sentence
                .structure
                .divisions
                .iter()
                .find(|division| division.kind == "paragraph")
                .map(|division| division.id.0))
            .collect::<Vec<_>>(),
        [None, None, Some(0), Some(1)]
    );

    let documents = events
        .iter()
        .filter_map(|event| match event {
            AdmittedCorpusEvent::DocumentStart { metadata, .. } => Some(metadata),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(documents[0].iter().any(|(key, value)| key == "corpus"
        && value == &MetadataValue::Scalar(MetadataScalar::String("legacy-extra".into()))));
    assert!(documents[1].iter().any(|(key, value)| key == "keywords"
        && matches!(value, MetadataValue::Scalars(values) if values.len() == 2)));

    let paragraph_metadata = events
        .iter()
        .filter_map(|event| match event {
            AdmittedCorpusEvent::DivisionStart(division)
                if division.kind == "paragraph" && !division.metadata.is_empty() =>
            {
                Some(&division.metadata)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(paragraph_metadata.len(), 1);
}
