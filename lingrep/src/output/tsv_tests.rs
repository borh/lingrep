use super::{
    AttributesTsvRow, MatchTsvRow, SentenceFinalTsvRow, SourceTsvRow, SummaryTsvRow,
    TokenizeTsvRow, write_attributes_tsv, write_match_tsv, write_sentence_final_tsv,
    write_source_tsv, write_summary_tsv, write_tokenize_tsv,
};

fn decode_tsv_record(bytes: &[u8]) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = std::str::from_utf8(bytes).unwrap().chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            '\t' if !quoted => fields.push(std::mem::take(&mut field)),
            '\n' if !quoted => {
                fields.push(field);
                break;
            }
            character => field.push(character),
        }
    }
    fields
}

#[test]
fn match_tsv_has_fixed_eleven_columns_and_empty_context_is_not_omitted() {
    let row = MatchTsvRow {
        sequence: 4,
        pattern: "p".into(),
        source_name: "a.md".into(),
        document_ordinal: 3,
        document_sentence_ordinal: 2,
        corpus_sentence_ordinal: 9,
        match_start: 1,
        match_end: 3,
        left_surface: Some("⟨missing⟩".into()),
        surface: "猫\t犬".into(),
        right_surface: Some("後".into()),
    };
    let mut bytes = Vec::new();
    write_match_tsv(&mut bytes, &row).unwrap();
    assert_eq!(bytes, std::fs::read("tests/golden/match.tsv").unwrap());
    assert_eq!(decode_tsv_record(&bytes).len(), 11);
    let mut empty = Vec::new();
    write_match_tsv(
        &mut empty,
        &MatchTsvRow {
            left_surface: None,
            right_surface: None,
            ..row
        },
    )
    .unwrap();
    assert_eq!(decode_tsv_record(&empty).len(), 11);
    assert_eq!(
        std::str::from_utf8(&empty).unwrap(),
        "4\tp\ta.md\t3\t2\t9\t1\t3\t\t\"猫\t犬\"\t\n"
    );
}

#[test]
fn summary_tsv_is_fixed_four_columns_with_rfc4180_encoding() {
    let mut bytes = Vec::new();
    write_summary_tsv(
        &mut bytes,
        &SummaryTsvRow {
            pattern: "p\"q".into(),
            rank: 7,
            group: "猫\t犬".into(),
            count: 42,
        },
    )
    .unwrap();
    assert_eq!(bytes, std::fs::read("tests/golden/summary.tsv").unwrap());
    assert_eq!(decode_tsv_record(&bytes).len(), 4);
}

#[test]
fn new_tsv_contracts_have_fixed_headerless_rows_and_escaping() {
    let mut sentence_final = Vec::new();
    write_sentence_final_tsv(
        &mut sentence_final,
        &SentenceFinalTsvRow {
            pattern: "p\tq".into(),
            surface: "猫\n犬".into(),
            count: 2,
        },
    )
    .unwrap();
    assert_eq!(decode_tsv_record(&sentence_final), ["p\tq", "猫\n犬", "2"]);

    let mut tokenize = Vec::new();
    write_tokenize_tsv(
        &mut tokenize,
        &TokenizeTsvRow {
            corpus_sentence_ordinal: 4,
            source_name: "a.md".into(),
            document_ordinal: 1,
            document_sentence_ordinal: 2,
            token_index: 3,
            orth: "猫".into(),
            lemma: "猫".into(),
            pos: "名詞".into(),
            cform: String::new(),
        },
    )
    .unwrap();
    assert_eq!(decode_tsv_record(&tokenize).len(), 9);

    let mut source = Vec::new();
    write_source_tsv(
        &mut source,
        &SourceTsvRow {
            ordinal: 7,
            logical_name: "a\tb.md".into(),
            target_group_ordinal: 2,
            external_target: true,
            target_multiplicity: None,
        },
    )
    .unwrap();
    assert_eq!(
        decode_tsv_record(&source),
        ["7", "a\tb.md", "2", "true", ""]
    );

    // A lingrep patterns attribute key cannot contain a tab, so this row proves the five
    // numeric-and-key columns and their count rather than the escaping the
    // three rows above already cover.
    let mut attributes = Vec::new();
    write_attributes_tsv(
        &mut attributes,
        &AttributesTsvRow {
            attribute: "orthbase".into(),
            present: 18_364,
            unspecified: 0,
            absent: 475,
            admitted_tokens: 18_839,
        },
    )
    .unwrap();
    assert_eq!(
        decode_tsv_record(&attributes),
        ["orthbase", "18364", "0", "475", "18839"]
    );

    for bytes in [&sentence_final, &tokenize, &source, &attributes] {
        assert!(!bytes.contains(&0x1b));
    }
}
