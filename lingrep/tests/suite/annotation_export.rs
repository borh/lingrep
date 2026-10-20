#![allow(clippy::unwrap_used, clippy::panic, clippy::string_slice)]

use crate::support::lingrep_without_dictionary;
use std::process::Command;
use tempfile::TempDir;

#[test]
fn saved_extractions_map_to_exact_annotation_views_and_preserve_coverage() {
    let scratch = TempDir::new().unwrap();
    let source = scratch.path().join("source.conllu");
    let catalog = scratch.path().join("query.lx");
    std::fs::write(&source, "# sent_id = aligned\n# text = 猫 😀\n1\t猫\t猫\tNOUN\t_\t_\t0\troot\t_\t_\n2\t😀\t😀\tSYM\t_\t_\t1\tdep\t_\tSpaceAfter=No\n\n# sent_id = unavailable\n# text = 犬です\n1\t犬\t犬\tNOUN\t_\t_\t0\troot\t_\tSpaceAfter=No\n\n").unwrap();
    std::fs::write(&catalog, "p := (?<n> [upos=NOUN]) []*.\n").unwrap();
    let query = |extra: &[&str], path: &str| {
        let output = lingrep_without_dictionary()
            .args(["--json", "matches", "--patterns"])
            .arg(&catalog)
            .args(if extra.contains(&"--limit") {
                vec![]
            } else {
                vec!["--limit", "all"]
            })
            .args(extra)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::write(scratch.path().join(path), output.stdout).unwrap();
    };
    query(&[], "full.jsonl");
    query(&[], "repeat.jsonl");
    query(&["--sample", "1", "--sample-seed", "7"], "sample.jsonl");
    query(&["--limit", "1"], "prefix.jsonl");
    let script = r#"
import copy, importlib.util, json, pathlib, sys
root, scratch = map(pathlib.Path, sys.argv[1:])
spec = importlib.util.spec_from_file_location('annotations', root / 'examples/soranoha_annotations.py')
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
corpus = (scratch / 'source.conllu').read_bytes()
catalog = (scratch / 'query.lx').read_bytes()
text = '前置\n猫 😀\n犬です'
binding = {'view': {'text': text, 'id': m.digest(m.canonical({'policy': 'body-v1', 'unit': 'utf8-bytes', 'text': text}).encode()), 'eligible_spans': [[0, len(text.encode())]]}, 'corpus_sha256': m.digest(corpus), 'sentences': [{'text': '猫 😀', 'start': len('前置\n'.encode()), 'end': len('前置\n猫 😀'.encode())}, {'text': '犬です', 'start': len('前置\n猫 😀\n'.encode()), 'end': len(text.encode())}]}
def events(name): return [json.loads(line) for line in (scratch/name).read_text().splitlines()]
def convert(e, b=binding, code=0): return m.convert(e, b, corpus, catalog, b'p,n', code)
full = events('full.jsonl')
layer, omitted = convert(full)
assert layer['status'] == 'partial' and omitted == 1
assert len(layer['records']) == 2
body = text.encode()
assert [body[r['start']:r['end']].decode() for r in layer['records']] == ['猫 😀', '猫']
assert layer == convert(events('repeat.jsonl'))[0]
assert json.loads(layer['records'][0]['features']['lingrep'])['complete']['source_identity'] == full[-1]['data']['source_identity']
assert layer['producer']['inputs']['corpus'] == m.digest(corpus)
aligned = copy.deepcopy(full)
aligned = [e for e in aligned if e['type'] != 'match' or e['data']['corpus_sentence_ordinal'] == 0]
aligned[-1]['data']['counters']['records_emitted'] = 1
aligned[-1]['data']['counters'].pop('offsets_omitted', None)
assert convert(aligned)[0]['status'] == 'completed'
sampled = copy.deepcopy(aligned); sampled[0]['data']['sample'] = {'rate_per_million': 500000, 'seed': 7, 'unit': 'sentence'}
assert convert(sampled)[0]['status'] == 'partial'
assert convert(events('sample.jsonl'))[0]['status'] == 'partial'
assert convert(events('prefix.jsonl'))[0]['status'] == 'partial'
failed = convert(full[:-1], code=1)[0]
assert failed['status'] == 'failed' and failed['records'] == []
empty = copy.deepcopy(full); empty = [e for e in empty if e['type'] != 'match']
empty[-1]['data']['counters']['records_emitted'] = 0
empty[-1]['data']['counters'].pop('offsets_omitted', None)
assert convert(empty)[0]['status'] == 'completed' and convert(empty)[0]['records'] == []
bad = copy.deepcopy(binding); bad['sentences'][0]['text'] = '猫  😀'
try: convert(full, bad)
except ValueError: pass
else: raise AssertionError('guessed mismatched sentence')
bad = copy.deepcopy(binding); bad['sentences'][0]['start'] += 1
try: convert(full, bad)
except ValueError: pass
else: raise AssertionError('accepted non-boundary byte offset')
bad = copy.deepcopy(full); bad[2]['data']['char_span']['end'] = 999
try: convert(bad)
except ValueError: pass
else: raise AssertionError('accepted code point outside sentence')
(scratch/'layer.json').write_text(m.canonical(layer))
(scratch/'binding.json').write_text(m.canonical(binding))
"#;
    let output = Command::new("python3")
        .args(["-c", script])
        .arg(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap(),
        )
        .arg(scratch.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
