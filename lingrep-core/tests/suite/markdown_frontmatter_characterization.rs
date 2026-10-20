// Exempt test/bench/example code from the workspace panic deny-floor (see docs/lint-policy.md).
#![allow(
    clippy::string_slice,
    clippy::panic,
    clippy::panic_in_result_fn,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable,
    clippy::exit,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used
)]
use std::{env, fs, path::Path};

const MAX_FIELDS: usize = 256;
const MAX_ARRAY_ELEMENTS: usize = 1_024;
const MAX_KEY_BYTES: usize = 256;
const MAX_SCALAR_BYTES: usize = 64 * 1_024;
const MAX_OPAQUE_BYTES: usize = 1_024 * 1_024;

#[test]
fn repository_markdown_frontmatter_fits_ingestion_contract() {
    let Ok(root) = env::var("LINGREP_MARKDOWN_CORPUS_PATH") else {
        eprintln!("skipped: LINGREP_MARKDOWN_CORPUS_PATH is unavailable");
        return;
    };
    let mut files = Vec::new();
    collect_markdown(Path::new(&root), &mut files);
    files.sort();

    let mut maxima = Maxima::default();
    let mut parsed = 0_usize;
    for path in &files {
        let source = fs::read_to_string(path).unwrap_or_else(|error| {
            panic!("read {}: {error}", path.display());
        });
        let Some(frontmatter) = leading_frontmatter(&source) else {
            continue;
        };
        let value: serde_yaml::Value = serde_yaml::from_str(frontmatter)
            .unwrap_or_else(|error| panic!("parse {} frontmatter: {error}", path.display()));
        let mapping = value
            .as_mapping()
            .unwrap_or_else(|| panic!("{} frontmatter must be a mapping", path.display()));
        parsed += 1;
        maxima.fields = maxima.fields.max(mapping.len());
        assert!(
            mapping.len() <= MAX_FIELDS,
            "{} has too many metadata fields",
            path.display()
        );
        for (key, value) in mapping {
            let key = key
                .as_str()
                .unwrap_or_else(|| panic!("{} has a non-string metadata key", path.display()));
            maxima.key_bytes = maxima.key_bytes.max(key.len());
            assert!(
                key.len() <= MAX_KEY_BYTES,
                "{} has an oversized metadata key",
                path.display()
            );
            measure_value(path, value, &mut maxima);
        }
    }

    assert!(!files.is_empty(), "no Markdown files found under {root}");
    eprintln!(
        "markdown_frontmatter files={} parsed={} max_fields={} max_key_bytes={} max_scalar_bytes={} max_array_elements={} max_opaque_bytes={}",
        files.len(),
        parsed,
        maxima.fields,
        maxima.key_bytes,
        maxima.scalar_bytes,
        maxima.array_elements,
        maxima.opaque_bytes
    );
}

#[derive(Default)]
struct Maxima {
    fields: usize,
    key_bytes: usize,
    scalar_bytes: usize,
    array_elements: usize,
    opaque_bytes: usize,
}

fn collect_markdown(path: &Path, files: &mut Vec<std::path::PathBuf>) {
    if path.file_name().is_some_and(|name| name == ".cache") {
        return;
    }
    if path.is_dir() {
        for entry in
            fs::read_dir(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        {
            collect_markdown(&entry.expect("read directory entry").path(), files);
        }
    } else if path.extension().is_some_and(|extension| extension == "md") {
        files.push(path.to_path_buf());
    }
}

fn leading_frontmatter(source: &str) -> Option<&str> {
    let rest = source
        .strip_prefix("---\n")
        .or_else(|| source.strip_prefix("---\r\n"))?;
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            return Some(&rest[..offset]);
        }
        offset += line.len();
    }
    panic!("unterminated leading YAML frontmatter");
}

fn measure_value(path: &Path, value: &serde_yaml::Value, maxima: &mut Maxima) {
    match value {
        serde_yaml::Value::Null => {}
        serde_yaml::Value::Bool(value) => measure_scalar(path, value.to_string().len(), maxima),
        serde_yaml::Value::Number(value) => measure_scalar(path, value.to_string().len(), maxima),
        serde_yaml::Value::String(value) => measure_scalar(path, value.len(), maxima),
        serde_yaml::Value::Sequence(values)
            if values.iter().all(|value| {
                matches!(
                    value,
                    serde_yaml::Value::Bool(_)
                        | serde_yaml::Value::Number(_)
                        | serde_yaml::Value::String(_)
                )
            }) =>
        {
            maxima.array_elements = maxima.array_elements.max(values.len());
            assert!(
                values.len() <= MAX_ARRAY_ELEMENTS,
                "{} has an oversized scalar array",
                path.display()
            );
            for value in values {
                measure_value(path, value, maxima);
            }
        }
        _ => {
            let bytes = serde_yaml::to_string(value)
                .expect("serialize YAML value")
                .len();
            maxima.opaque_bytes = maxima.opaque_bytes.max(bytes);
            assert!(
                bytes <= MAX_OPAQUE_BYTES,
                "{} has oversized opaque metadata",
                path.display()
            );
        }
    }
}

fn measure_scalar(path: &Path, bytes: usize, maxima: &mut Maxima) {
    maxima.scalar_bytes = maxima.scalar_bytes.max(bytes);
    assert!(
        bytes <= MAX_SCALAR_BYTES,
        "{} has oversized scalar metadata",
        path.display()
    );
}
