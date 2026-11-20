use std::io::Write;

use tempfile::TempDir;
use vibrato_rkyv::{Dictionary, SystemDictionaryBuilder};

use crate::support::lingrep;

fn dictionary_with_non_surface_orth(directory: &TempDir) -> std::path::PathBuf {
    let lexicon = concat!(
        "左,0,0,1,名詞,普通名詞,一般,*,*,*,ヒダリ,左,L\n",
        "鍵,0,0,1,名詞,普通名詞,一般,*,*,*,カギ,鍵,K\n",
        "右,0,0,1,名詞,普通名詞,一般,*,*,*,ミギ,右,R\n",
        "。,0,0,1,補助記号,句点,*,*,*,*,*,。,。\n",
    );
    let dictionary = SystemDictionaryBuilder::from_readers(
        lexicon.as_bytes(),
        "1 1\n0 0 0".as_bytes(),
        "DEFAULT 0 1 0\nSPACE 0 1 0\n0x0020 SPACE".as_bytes(),
        "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*".as_bytes(),
    )
    .expect("characterization dictionary must build");
    let path = directory.path().join("kwic.dic");
    let mut bytes = Vec::new();
    dictionary.write(&mut bytes).expect("dictionary serializes");
    Dictionary::read(bytes.as_slice()).expect("dictionary round-trips");
    std::fs::write(&path, bytes).expect("dictionary fixture writes");
    path
}

#[test]
fn matches_tsv_context_is_joined_text_not_dictionary_orth() {
    let directory = TempDir::new().expect("temporary fixture directory");
    let dictionary = dictionary_with_non_surface_orth(&directory);
    let corpus = directory.path().join("corpus.txt");
    std::fs::File::create(&corpus)
        .expect("corpus creates")
        .write_all("左鍵右。\n".as_bytes())
        .expect("corpus writes");

    let output = lingrep()
        .args([
            "matches",
            "--dict-path",
            dictionary.to_str().expect("UTF-8 path"),
            "--pattern",
            "[text=鍵].",
            "--tsv",
            "--context",
            "2",
        ])
        .arg(&corpus)
        .output()
        .expect("ling runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("TSV is UTF-8");
    let columns = stdout.trim_end().split('\t').collect::<Vec<_>>();
    assert_eq!(columns.len(), 11, "matches TSV has its pinned fixed width");
    assert_eq!((columns[8], columns[9], columns[10]), ("左", "鍵", "右。"));
}

/// A dictionary whose `ＡＢ` keeps its full-width surface through the fold, so
/// its `text` key (`AB`) differs from the source.
fn dictionary_with_full_width_word(directory: &TempDir) -> std::path::PathBuf {
    let lexicon = concat!(
        "左,0,0,1,名詞,普通名詞,一般,*,*,*,ヒダリ,左,左\n",
        "ＡＢ,0,0,1,名詞,普通名詞,一般,*,*,*,エービー,ＡＢ,ＡＢ\n",
        "鍵,0,0,1,名詞,普通名詞,一般,*,*,*,カギ,鍵,鍵\n",
        "右,0,0,1,名詞,普通名詞,一般,*,*,*,ミギ,右,右\n",
        "。,0,0,1,補助記号,句点,*,*,*,*,*,。,。\n",
    );
    let dictionary = SystemDictionaryBuilder::from_readers(
        lexicon.as_bytes(),
        "1 1\n0 0 0".as_bytes(),
        "DEFAULT 0 1 0\nSPACE 0 1 0\n0x0020 SPACE".as_bytes(),
        "DEFAULT,0,0,100,名詞,普通名詞,一般,*,*,*".as_bytes(),
    )
    .expect("full-width dictionary must build");
    let path = directory.path().join("full-width.dic");
    let mut bytes = Vec::new();
    dictionary.write(&mut bytes).expect("dictionary serializes");
    std::fs::write(&path, bytes).expect("dictionary fixture writes");
    path
}

/// `ling matches` over `左ＡＢ鍵ＡＢ右。`, as JSONL or as TSV.
fn matches_over_full_width(json: bool) -> String {
    let directory = TempDir::new().expect("temporary fixture directory");
    let dictionary = dictionary_with_full_width_word(&directory);
    let corpus = directory.path().join("corpus.txt");
    std::fs::write(&corpus, "左ＡＢ鍵ＡＢ右。\n").expect("corpus writes");
    let output = lingrep()
        .args(if json { &["--json"][..] } else { &[] })
        .args(["matches", "--dict-path"])
        .arg(&dictionary)
        .args(["--pattern", "[text=AB].", "--context", "2"])
        .args(if json { &[][..] } else { &["--tsv"] })
        .arg(&corpus)
        .output()
        .expect("ling runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("output is UTF-8")
}

/// The rendered surface and context show the source; only the token's `text`
/// attribute carries the folded key.
#[test]
fn matches_render_the_exact_surface_where_text_is_folded() {
    let json = matches_over_full_width(true);
    let records = json
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("JSONL"))
        .filter(|event| event["type"] == "match")
        .map(|event| event["data"].clone())
        .collect::<Vec<_>>();
    let token = |index: usize, text: &str| serde_json::json!({"index": index, "attributes": {"text": text}});
    let record = |sequence: u64, start: usize, char_start: usize, left, right| {
        serde_json::json!({
            "sequence": sequence,
            "pattern": "inline",
            "span": {"start": start, "end": start + 1},
            "char_span": {"start": char_start, "end": char_start + 2},
            "captures": [],
            "corpus_sentence_ordinal": 0,
            "source_name": "corpus.txt",
            "document_ordinal": 0,
            "document_sentence_ordinal": 0,
            "surface": "ＡＢ",
            "tokens": [token(start, "AB")],
            "left_context": left,
            "right_context": right,
        })
    };
    assert_eq!(
        records,
        [
            record(
                0,
                1,
                1,
                serde_json::json!([token(0, "左")]),
                serde_json::json!([token(2, "鍵"), token(3, "AB")]),
            ),
            record(
                1,
                3,
                4,
                serde_json::json!([token(1, "AB"), token(2, "鍵")]),
                serde_json::json!([token(4, "右"), token(5, "。")]),
            ),
        ]
    );

    let tsv = matches_over_full_width(false);
    let rows = tsv
        .lines()
        .map(|line| line.split('\t').skip(8).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [vec!["左", "ＡＢ", "鍵ＡＢ"], vec!["ＡＢ鍵", "ＡＢ", "右。"]]
    );
}
