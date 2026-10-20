use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use lingrep_core::dictionary::create_tokenizer_from_path;
use lingrep_core::token_match::{TokenAttributeIndex, TokenInfoTrait, tokenize};

const SAMPLE_CAP: usize = 20;

fn corpus_sentences(path: &Path) -> impl Iterator<Item = String> {
    BufReader::new(File::open(path).expect("real-world corpus must open"))
        .lines()
        .map(|line| line.expect("real-world corpus line must be readable"))
        .flat_map(|line| {
            let value: serde_json::Value =
                serde_json::from_str(&line).expect("real-world corpus must be JSONL");
            value
                .get("sentences")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|sentence| {
                    sentence
                        .as_str()
                        .expect("corpus sentence must be a string")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        })
}

#[test]
#[ignore = "requires LINGREP_DICTIONARY_PATH and LINGREP_REAL_WORLD_CORPUS_PATH"]
fn text_orth_divergence_is_censused_with_bounded_memory() {
    let dictionary = std::env::var("LINGREP_DICTIONARY_PATH")
        .expect("set LINGREP_DICTIONARY_PATH to a Nix UniDic artifact");
    let corpus = std::env::var("LINGREP_REAL_WORLD_CORPUS_PATH")
        .expect("set LINGREP_REAL_WORLD_CORPUS_PATH to the benchmark corpus");
    let tokenizer = create_tokenizer_from_path(&dictionary).expect("dictionary must load");
    let mut total = 0usize;
    let mut divergent_count = 0usize;
    let mut sample: Vec<(String, String)> = Vec::new();
    let mut distinct: BTreeSet<(String, String)> = BTreeSet::new();

    for sentence in corpus_sentences(Path::new(&corpus)) {
        for token in tokenize(&sentence, &tokenizer) {
            total += 1;
            let text = token
                .get(TokenAttributeIndex::Text)
                .expect("text is total")
                .to_string();
            let orth = token
                .get(TokenAttributeIndex::Orth)
                .expect("orth is total under the surface fallback")
                .to_string();
            if text == orth {
                continue;
            }
            divergent_count += 1;
            if sample.len() < SAMPLE_CAP {
                sample.push((text.clone(), orth.clone()));
            }
            distinct.insert((text, orth));
        }
    }

    assert!(
        total > 0,
        "census walked no tokens; the corpus wiring is wrong"
    );

    let mut hasher = blake3::Hasher::new();
    for (text, orth) in &distinct {
        hasher.update(text.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(orth.as_bytes());
        hasher.update(b"\x1e");
    }
    println!(
        "total={total} divergent={divergent_count} distinct={}",
        distinct.len()
    );
    println!("digest={}", hasher.finalize().to_hex());
    for (text, orth) in &sample {
        println!("  text={text:?} orth={orth:?}");
    }

    // The initial zero is intentional RED: replace it with the observed census.
    assert_eq!(divergent_count, 0, "divergent token count moved");
}
