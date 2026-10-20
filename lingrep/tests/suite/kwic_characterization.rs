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
fn matches_tsv_context_is_joined_orth_not_admitted_source() {
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
            "[text=K].",
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
    assert_eq!((columns[8], columns[9], columns[10]), ("L", "K", "R。"));
    assert!(!"左鍵右。".contains(columns[9]));
}
