use lingrep_core::dictionary::{
    create_tokenizer, create_tokenizer_from_path, load_dictionary_data,
};
use lingrep_core::testing::{MiniDictionary, dictionary_path};
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfoTrait, tokenize};

const UNIDIC_ATTRIBUTES: [TokenAttributeIndex; 15] = [
    TokenAttributeIndex::Pos1,
    TokenAttributeIndex::Pos2,
    TokenAttributeIndex::Pos3,
    TokenAttributeIndex::Pos4,
    TokenAttributeIndex::CType,
    TokenAttributeIndex::CForm,
    TokenAttributeIndex::LForm,
    TokenAttributeIndex::Lemma,
    TokenAttributeIndex::Orth,
    TokenAttributeIndex::Pron,
    TokenAttributeIndex::OrthBase,
    TokenAttributeIndex::PronBase,
    TokenAttributeIndex::Goshu,
    TokenAttributeIndex::IType,
    TokenAttributeIndex::IForm,
];

fn observed_tokens(dictionary: &std::path::Path, text: &str) -> Vec<[Option<String>; 5]> {
    let tokenizer = create_tokenizer_from_path(
        dictionary
            .to_str()
            .expect("test dictionary path must be UTF-8"),
    )
    .expect("test dictionary must load");
    tokenize(text, &tokenizer)
        .into_iter()
        .map(|token| {
            [
                TokenAttributeIndex::Orth,
                TokenAttributeIndex::Tag,
                TokenAttributeIndex::Norm,
                TokenAttributeIndex::Text,
                TokenAttributeIndex::LexType,
            ]
            .map(|attribute| token.get(attribute).map(ToString::to_string))
        })
        .collect()
}

fn observed_projection(
    dictionary: &std::path::Path,
    text: &str,
    attributes: &[TokenAttributeIndex],
) -> Vec<Vec<Option<String>>> {
    let tokenizer = create_tokenizer_from_path(
        dictionary
            .to_str()
            .expect("test dictionary path must be UTF-8"),
    )
    .expect("test dictionary must load");
    tokenize(text, &tokenizer)
        .into_iter()
        .map(|token| {
            attributes
                .iter()
                .map(|attribute| token.get(*attribute).map(ToString::to_string))
                .collect()
        })
        .collect()
}

fn observed_loaded_projection(bytes: &[u8], text: &str) -> Vec<Vec<Option<String>>> {
    let tokenizer = create_tokenizer(bytes).expect("test dictionary bytes must load");
    tokenize(text, &tokenizer)
        .into_iter()
        .map(|token| {
            UNIDIC_ATTRIBUTES
                .iter()
                .map(|attribute| token.get(*attribute).map(ToString::to_string))
                .collect()
        })
        .collect()
}

fn observed_verbs(dictionary: &std::path::Path, text: &str) -> Vec<[Option<String>; 4]> {
    let tokenizer = create_tokenizer_from_path(
        dictionary
            .to_str()
            .expect("test dictionary path must be UTF-8"),
    )
    .expect("test dictionary must load");
    tokenize(text, &tokenizer)
        .into_iter()
        .filter(|token| {
            token
                .get(TokenAttributeIndex::Pos)
                .map(compact_str::CompactString::as_str)
                == Some("VERB")
        })
        .map(|token| {
            [
                TokenAttributeIndex::Orth,
                TokenAttributeIndex::Lemma,
                TokenAttributeIndex::CForm,
                TokenAttributeIndex::OrthBase,
            ]
            .map(|attribute| token.get(attribute).map(ToString::to_string))
        })
        .collect()
}

#[test]
fn mini_dictionary_owns_a_read_only_temporary_artifact() {
    let (path, compressed_path) = {
        let dictionary = MiniDictionary::new();
        let path = dictionary.path().to_path_buf();
        let compressed_path = dictionary.compressed_path().to_path_buf();
        assert!(path.is_file());
        assert!(compressed_path.is_file());
        assert!(std::fs::metadata(&path).unwrap().permissions().readonly());
        assert!(
            std::fs::metadata(&compressed_path)
                .unwrap()
                .permissions()
                .readonly()
        );
        create_tokenizer_from_path(path.to_str().unwrap()).unwrap();
        (path, compressed_path)
    };
    assert!(!path.exists());
    assert!(!compressed_path.exists());
}

#[test]
fn mini_dictionary_views_preserve_the_unidic_projection() {
    let real = std::path::PathBuf::from(dictionary_path());
    let mini = MiniDictionary::new();
    let plain = load_dictionary_data(mini.path().to_str().expect("UTF-8 path"))
        .expect("load plain miniature dictionary");
    let compressed = load_dictionary_data(
        mini.compressed_path()
            .to_str()
            .expect("UTF-8 compressed path"),
    )
    .expect("load compressed miniature dictionary");

    assert_eq!(compressed, plain);
    let text = "猫は走る。猫がいる。実験を行った。実験を行う。調査を行った。";
    let plain_projection = observed_loaded_projection(&plain, text);
    assert_eq!(
        observed_loaded_projection(&compressed, text),
        plain_projection
    );
    assert_eq!(
        observed_projection(&real, text, &UNIDIC_ATTRIBUTES),
        plain_projection
    );
    for (ordinal, attribute) in UNIDIC_ATTRIBUTES.iter().enumerate() {
        assert!(
            plain_projection
                .iter()
                .any(|token| token[ordinal].is_some()),
            "{attribute:?} must be present on a known miniature-dictionary token"
        );
    }
}

#[test]
fn mini_dictionary_matches_the_protocol_domain() {
    let real = std::path::PathBuf::from(dictionary_path());
    let mini = MiniDictionary::new();
    for sentence in ["猫。", "犬。", "鳥。", "猫と犬と猫"] {
        assert_eq!(
            observed_tokens(mini.path(), sentence),
            observed_tokens(&real, sentence),
            "{sentence}"
        );
    }

    let unknown_attributes = [
        TokenAttributeIndex::Orth,
        TokenAttributeIndex::Lemma,
        TokenAttributeIndex::Norm,
        TokenAttributeIndex::OrthBase,
        TokenAttributeIndex::Pron,
        TokenAttributeIndex::LexType,
        TokenAttributeIndex::Text,
    ];
    assert_eq!(
        observed_projection(mini.path(), "😀", &unknown_attributes),
        observed_projection(&real, "😀", &unknown_attributes),
        "unknown-word publication projection"
    );
}

#[test]
fn mini_dictionary_matches_the_web_domain() {
    let real = std::path::PathBuf::from(dictionary_path());
    let mini = MiniDictionary::new();
    for sentence in [
        "一。",
        "二。",
        "三。",
        "文。",
        "猫が鳴く。",
        "犬が走る。",
        "犬が眠る。",
        "犬も鳴く。",
    ] {
        assert_eq!(
            observed_projection(mini.path(), sentence, &[TokenAttributeIndex::Orth]),
            observed_projection(&real, sentence, &[TokenAttributeIndex::Orth]),
            "{sentence}"
        );
    }
    let query_attributes = [TokenAttributeIndex::Orth, TokenAttributeIndex::Pos];
    for sentence in ["私は食べる。", "彼は走る。", "彼は食べる。"] {
        assert_eq!(
            observed_projection(mini.path(), sentence, &query_attributes),
            observed_projection(&real, sentence, &query_attributes),
            "{sentence}"
        );
        assert_eq!(
            observed_verbs(mini.path(), sentence),
            observed_verbs(&real, sentence),
            "{sentence} verb projection"
        );
    }
}

#[test]
fn mini_dictionary_matches_the_sentence_final_domain() {
    let real = std::path::PathBuf::from(dictionary_path());
    let mini = MiniDictionary::new();
    let attributes = [TokenAttributeIndex::Orth, TokenAttributeIndex::Pos];
    for sentence in ["研究。", "研究が進む。"] {
        assert_eq!(
            observed_projection(mini.path(), sentence, &attributes),
            observed_projection(&real, sentence, &attributes),
            "{sentence}"
        );
    }
}
#[test]
fn invalid_dictionary_bytes_return_dictionary_error() {
    let result = create_tokenizer(b"not a dictionary");

    assert!(matches!(
        result,
        Err(lingrep_core::PipelineError::Dictionary(_))
    ));
}

#[test]
fn missing_dictionary_path_returns_dictionary_error() {
    let result = create_tokenizer_from_path("/missing/dict.dic.zst");

    assert!(matches!(
        result,
        Err(lingrep_core::PipelineError::Dictionary(_))
    ));
}
