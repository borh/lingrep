//! Display labels for document-metadata keys, read from `--metadata-labels`
//! files. A label is shown in place of a key's name; clauses, commands and
//! addresses keep the key.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::{Deserialize, Serialize};

/// The locales the page is served in, one per dictionary under `assets/i18n`.
pub const LOCALES: [&str; 2] = ["en", "ja"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LabelFile {
    vocabulary: String,
    edition: String,
    source: String,
    licence: String,
    #[serde(deserialize_with = "unique_map")]
    keys: BTreeMap<String, KeyEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyEntry {
    #[serde(deserialize_with = "unique_map")]
    labels: BTreeMap<String, String>,
}

fn unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct Unique<V>(PhantomData<V>);
    impl<'de, V: Deserialize<'de>> Visitor<'de> for Unique<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
            let mut map = BTreeMap::new();
            while let Some((name, value)) = access.next_entry::<String, V>()? {
                if map.contains_key(&name) {
                    return Err(serde::de::Error::custom(format!("{name:?} appears twice")));
                }
                map.insert(name, value);
            }
            Ok(map)
        }
    }
    deserializer.deserialize_map(Unique(PhantomData))
}

/// Labels by unqualified document-metadata key, then by locale.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct KeyLabels {
    labels: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(skip)]
    files: BTreeMap<String, PathBuf>,
}

const DOCUMENT_SCOPE: &str = "@document.";

impl KeyLabels {
    /// Reads every label file at `paths`.
    ///
    /// # Errors
    ///
    /// A message naming the file for a file that cannot be read, is not a
    /// label file, labels a key another file labels, or gives two keys the
    /// same label in one locale.
    pub fn read(paths: &[PathBuf]) -> Result<Self, String> {
        let mut read = Self::default();
        for path in paths {
            let named =
                |message: String| format!("--metadata-labels {}: {message}", path.display());
            let text = std::fs::read_to_string(path).map_err(|error| named(error.to_string()))?;
            let file = parse(&text).map_err(named)?;
            for (key, locales) in file {
                if let Some(other) = read.files.get(&key) {
                    return Err(format!(
                        "--metadata-labels {} and --metadata-labels {} both label \
                         {DOCUMENT_SCOPE}{key}",
                        other.display(),
                        path.display()
                    ));
                }
                read.files.insert(key.clone(), path.clone());
                read.labels.insert(key, locales);
            }
        }
        read.check_distinct()?;
        Ok(read)
    }

    fn check_distinct(&self) -> Result<(), String> {
        let mut seen: BTreeMap<(&str, &str), &str> = BTreeMap::new();
        for (key, locales) in &self.labels {
            for (locale, label) in locales {
                let Some(other) = seen.insert((locale, label), key) else {
                    continue;
                };
                let (first, second) = (&self.files[other], &self.files[key]);
                let files = if first == second {
                    format!("--metadata-labels {}", first.display())
                } else {
                    format!(
                        "--metadata-labels {} and --metadata-labels {}",
                        first.display(),
                        second.display()
                    )
                };
                return Err(format!(
                    "{files}: {DOCUMENT_SCOPE}{other} and {DOCUMENT_SCOPE}{key} share the \
                     {locale} label {label:?}"
                ));
            }
        }
        Ok(())
    }

    /// Each labelled key outside `keys`, with the file that labels it.
    pub fn unused<'a>(
        &'a self,
        keys: &'a BTreeSet<String>,
    ) -> impl Iterator<Item = (&'a str, &'a Path)> + 'a {
        self.files
            .iter()
            .filter(|(key, _)| !keys.contains(*key))
            .map(|(key, path)| (key.as_str(), path.as_path()))
    }
}

fn parse(text: &str) -> Result<BTreeMap<String, BTreeMap<String, String>>, String> {
    let file: LabelFile = serde_json::from_str(text).map_err(|error| error.to_string())?;
    for (field, value) in [
        ("vocabulary", &file.vocabulary),
        ("edition", &file.edition),
        ("source", &file.source),
        ("licence", &file.licence),
    ] {
        if value.trim().is_empty() {
            return Err(format!("{field} is empty"));
        }
    }
    let mut keys = BTreeMap::new();
    for (qualified, entry) in file.keys {
        let key = qualified
            .strip_prefix(DOCUMENT_SCOPE)
            .filter(|key| crate::frequency::writable_key(key))
            .ok_or_else(|| {
                format!("{qualified:?} is not a document key such as {DOCUMENT_SCOPE}author")
            })?;
        if entry.labels.is_empty() {
            return Err(format!("{qualified} has no labels"));
        }
        for (locale, label) in &entry.labels {
            if !LOCALES.contains(&locale.as_str()) {
                return Err(format!(
                    "{qualified}: {locale:?} is not a locale the page is served in: {}",
                    LOCALES.join(", ")
                ));
            }
            if label.trim().is_empty() {
                return Err(format!("{qualified}: the {locale} label is empty"));
            }
        }
        keys.insert(key.to_owned(), entry.labels);
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_soranoha_file_labels_every_catalog_column_in_english_and_japanese() {
        let path = PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/labels/soranoha-catalog.json"
        ));
        let keys = KeyLabels::read(&[path])
            .expect("the shipped file reads")
            .labels;
        assert_eq!(keys.keys().map(String::as_str).collect::<Vec<_>>(), {
            let mut columns = lingrep_core::corpus::soranoha::CATALOG_COLUMNS.to_vec();
            columns.sort_unstable();
            columns
        });
        for (key, locales) in &keys {
            assert_eq!(
                locales.keys().map(String::as_str).collect::<Vec<_>>(),
                LOCALES,
                "{key}"
            );
        }
    }

    #[test]
    fn an_invalid_label_file_fails_naming_it() {
        let directory = tempfile::tempdir().unwrap();
        let file = |keys: &str| {
            format!(
                r#"{{"vocabulary": "v", "edition": "1", "source": "s", "licence": "l", "keys": {keys}}}"#
            )
        };
        let cases = [
            ("not json".to_owned(), "expected"),
            (r#"{"keys": {}}"#.to_owned(), "missing field"),
            (
                file(r#"{"author": {"labels": {"en": "a"}}}"#),
                "\"author\" is not a document key",
            ),
            (
                file(r#"{"@document.a": {"labels": {"en": " "}}}"#),
                "the en label is empty",
            ),
            (file(r#"{"@document.a": {"labels": {}}}"#), "has no labels"),
            (
                file(r#"{"@document.a": {"labels": {"en": "a"}, "values": {}}}"#),
                "unknown field `values`",
            ),
            (
                file("{}").replace(r#""edition": "1""#, r#""edition": """#),
                "edition is empty",
            ),
            (
                file(r#"{"@document.a": {"labels": {"ja-JP": "a"}}}"#),
                "\"ja-JP\" is not a locale the page is served in: en, ja",
            ),
            (
                file(r#"{"@document.a": {"labels": {"EN": "a"}}}"#),
                "\"EN\" is not a locale",
            ),
            (
                file(
                    r#"{"@document.a": {"labels": {"en": "a"}}, "@document.a": {"labels": {"en": "b"}}}"#,
                ),
                "\"@document.a\" appears twice",
            ),
            (
                file(r#"{"@document.a": {"labels": {"en": "a", "en": "b"}}}"#),
                "\"en\" appears twice",
            ),
            (
                file(
                    r#"{"@document.a": {"labels": {"ja": "同じ"}}, "@document.b": {"labels": {"ja": "同じ"}}}"#,
                ),
                "@document.a and @document.b share the ja label \"同じ\"",
            ),
        ];
        for (index, (text, expected)) in cases.into_iter().enumerate() {
            let path = directory.path().join(format!("{index}.json"));
            std::fs::write(&path, &text).unwrap();
            let error = KeyLabels::read(std::slice::from_ref(&path)).unwrap_err();
            assert!(
                error.starts_with(&format!("--metadata-labels {}: ", path.display())),
                "{error}"
            );
            assert!(error.contains(expected), "{text}: {error}");
        }

        let missing = directory.path().join("missing.json");
        let error = KeyLabels::read(std::slice::from_ref(&missing)).unwrap_err();
        assert!(
            error.starts_with(&format!("--metadata-labels {}: ", missing.display())),
            "{error}"
        );
    }

    #[test]
    fn two_files_that_label_one_key_fail_naming_both() {
        let directory = tempfile::tempdir().unwrap();
        let text = r#"{"vocabulary": "v", "edition": "1", "source": "s", "licence": "l",
            "keys": {"@document.author": {"labels": {"en": "author"}}}}"#;
        let first = directory.path().join("a.json");
        let second = directory.path().join("b.json");
        std::fs::write(&first, text).unwrap();
        std::fs::write(&second, text).unwrap();
        let error = KeyLabels::read(&[first.clone(), second.clone()]).unwrap_err();
        assert_eq!(
            error,
            format!(
                "--metadata-labels {} and --metadata-labels {} both label @document.author",
                first.display(),
                second.display()
            )
        );
    }

    #[test]
    fn two_files_that_share_a_label_fail_naming_both() {
        let directory = tempfile::tempdir().unwrap();
        let file = |key: &str| {
            format!(
                r#"{{"vocabulary": "v", "edition": "1", "source": "s", "licence": "l",
                "keys": {{"@document.{key}": {{"labels": {{"en": "place"}}}}}}}}"#
            )
        };
        let first = directory.path().join("a.json");
        let second = directory.path().join("b.json");
        std::fs::write(&first, file("city")).unwrap();
        std::fs::write(&second, file("town")).unwrap();
        let error = KeyLabels::read(&[first.clone(), second.clone()]).unwrap_err();
        assert_eq!(
            error,
            format!(
                "--metadata-labels {} and --metadata-labels {}: @document.city and \
                 @document.town share the en label \"place\"",
                first.display(),
                second.display()
            )
        );
    }

    #[test]
    fn the_accepted_locales_are_the_page_dictionaries() {
        let mut dictionaries =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/i18n"))
                .unwrap()
                .map(|entry| {
                    entry
                        .unwrap()
                        .path()
                        .file_stem()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect::<Vec<_>>();
        dictionaries.sort_unstable();
        assert_eq!(dictionaries, LOCALES);
    }

    #[test]
    fn unused_names_each_labelled_key_outside_the_given_keys_with_its_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("labels.json");
        std::fs::write(
            &path,
            r#"{"vocabulary": "v", "edition": "1", "source": "s", "licence": "l",
            "keys": {"@document.author": {"labels": {"en": "author"}},
                     "@document.athor": {"labels": {"en": "writer"}}}}"#,
        )
        .unwrap();
        let labels = KeyLabels::read(std::slice::from_ref(&path)).unwrap();
        let keys = BTreeSet::from(["author".to_owned(), "year".to_owned()]);
        assert_eq!(
            labels.unused(&keys).collect::<Vec<_>>(),
            [("athor", path.as_path())]
        );
    }
}
