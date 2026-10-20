//! The BCCWJ `DOC/` metadata join table.
//!
//! `BccwjMetadata` joins the three context tables — `Joined_info.txt`
//! (20 columns), `BCCWJ-NDC.txt` (21 columns) and
//! `CopyRight_Annotation.txt` — into one map keyed by sample ID (basename).
//! The derivation is a faithful port of `corpus-utils.bccwj/parse-metadata`:
//! the same column bindings, the same NDC9 splicing and top-level fallback,
//! the same `なし`/`分類なし` filters, the same adjacent-duplicate removal,
//! and the same subcorpus-name prefix on `category`.
//!
//! A basename with no `Joined_info` row is an *error*, not a note: the
//! prepared build has no advisory channel, and publishing a metadata-less
//! document would make every later cached run silently degraded.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde_json::json;

use super::NDC9;

/// The thirteen BCCWJ subcorpora. The code is the tree/zip spelling
/// (`LB`, `OY`, …); the display name is the Japanese `corpusName` value the
/// `DOC/` tables carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subcorpus {
    /// 書籍 (fixed window)
    LB,
    /// 書籍 (variable window)
    PB,
    /// 書籍 (2005 variable)
    OB,
    /// 法律
    OL,
    /// 白書
    OW,
    /// 国会会議録
    OM,
    /// 広報紙
    OP,
    /// 検定教科書
    OT,
    /// 韻文
    OV,
    /// Yahoo!知恵袋
    OC,
    /// Yahoo!ブログ
    OY,
    /// 新聞
    PN,
    /// 雑誌
    PM,
}

impl Subcorpus {
    /// The two-letter code (`LB`, `OY`, …).
    #[must_use]
    pub const fn as_code(self) -> &'static str {
        match self {
            Self::LB => "LB",
            Self::PB => "PB",
            Self::OB => "OB",
            Self::OL => "OL",
            Self::OW => "OW",
            Self::OM => "OM",
            Self::OP => "OP",
            Self::OT => "OT",
            Self::OV => "OV",
            Self::OC => "OC",
            Self::OY => "OY",
            Self::PN => "PN",
            Self::PM => "PM",
        }
    }

    /// The Japanese `corpusName` value the `DOC/` tables carry.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::LB | Self::PB | Self::OB => "書籍",
            Self::OL => "法律",
            Self::OW => "白書",
            Self::OM => "国会会議録",
            Self::OP => "広報紙",
            Self::OT => "検定教科書",
            Self::OV => "韻文",
            Self::OC => "Yahoo!知恵袋",
            Self::OY => "Yahoo!ブログ",
            Self::PN => "新聞",
            Self::PM => "雑誌",
        }
    }

    /// Resolves a two-letter code directory (`LB`, `OY`, …) to its subcorpus.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "LB" => Some(Self::LB),
            "PB" => Some(Self::PB),
            "OB" => Some(Self::OB),
            "OL" => Some(Self::OL),
            "OW" => Some(Self::OW),
            "OM" => Some(Self::OM),
            "OP" => Some(Self::OP),
            "OT" => Some(Self::OT),
            "OV" => Some(Self::OV),
            "OC" => Some(Self::OC),
            "OY" => Some(Self::OY),
            "PN" => Some(Self::PN),
            "PM" => Some(Self::PM),
            _ => None,
        }
    }

    /// Whether the book category derivation applies (the `(L|O|P)B` branch).
    #[must_use]
    pub const fn is_book(self) -> bool {
        matches!(self, Self::LB | Self::PB | Self::OB)
    }
}

/// Author gender, after the reference's `/`-split rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gender {
    Male,
    Female,
    Mixed,
}

/// Which sampling tree a document entry came from (D3): the FIXED window or
/// the VARIABLE window. Both are imported; the field makes the dual-subcorpus
/// overlap visible and filterable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sampling {
    Fixed,
    Variable,
}

impl Sampling {
    /// The `sampling` metadata value (`fixed` / `variable`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fixed => "fixed",
            Self::Variable => "variable",
        }
    }
}

/// One joined metadata record, keyed by sample ID (basename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocMetadata {
    pub basename: String,
    /// Title, with `: subtitle` appended when the subtitle is non-empty.
    pub title: String,
    pub author: String,
    /// The author's birth decade, when it parses as an integer.
    pub author_year: Option<i64>,
    pub gender: Option<Gender>,
    /// `BCCWJ` when the publisher column is empty.
    pub publisher: String,
    /// The `CopyRight_Annotation.txt` value, when present.
    pub copyright: Option<String>,
    /// Books only: the first C-code digit's label.
    pub audience: Option<String>,
    /// Books only: the second C-code digit's label.
    pub media: Option<String>,
    /// Books only: the third/fourth C-code digits' label.
    pub topic: Option<String>,
    pub year: i64,
    pub subcorpus: Subcorpus,
    /// The derived category list; `genre` emits the same values.
    pub category: Vec<String>,
}

impl DocMetadata {
    /// The document-metadata JSON object the single typing rule
    /// (`metadata_from_json`) shapes into `Metadata`. Sparse fields are
    /// omitted, matching the reference's nil-drop.
    #[must_use]
    pub fn to_json(&self, sampling: Sampling) -> serde_json::Value {
        let mut object = serde_json::Map::new();
        object.insert("title".into(), json!(self.title));
        object.insert("author".into(), json!(self.author));
        if let Some(year) = self.author_year {
            object.insert("author_year".into(), json!(year));
        }
        if let Some(gender) = self.gender {
            object.insert(
                "gender".into(),
                json!(match gender {
                    Gender::Male => "male",
                    Gender::Female => "female",
                    Gender::Mixed => "mixed",
                }),
            );
        }
        object.insert("publisher".into(), json!(self.publisher));
        if let Some(copyright) = &self.copyright {
            object.insert("copyright".into(), json!(copyright));
        }
        if let Some(audience) = &self.audience {
            object.insert("audience".into(), json!(audience));
        }
        if let Some(media) = &self.media {
            object.insert("media".into(), json!(media));
        }
        if let Some(topic) = &self.topic {
            object.insert("topic".into(), json!(topic));
        }
        object.insert("year".into(), json!(self.year));
        object.insert("basename".into(), json!(self.basename));
        object.insert("corpus".into(), json!("BCCWJ"));
        object.insert("subcorpus".into(), json!(self.subcorpus.display_name()));
        object.insert("category".into(), json!(self.category));
        object.insert("genre".into(), json!(self.category));
        object.insert("permission".into(), json!(true));
        object.insert("sampling".into(), json!(sampling.as_str()));
        serde_json::Value::Object(object)
    }
}

/// A sample ID the `Joined_info` table has no row for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("bccwj metadata has no Joined_info row for basename {basename}")]
pub struct MissingBasename {
    pub basename: String,
}

/// Recoverable build counts: facts about the join that never fail it.
///
/// The differential harness (`lingrep-core/examples/bccwj-diff.rs`) prints these
/// beside the row count. Production run reports do not expose them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BccwjBuildNotes {
    /// Category values dropped by the reference's unknown-notation rule
    /// (digits-only values with no NDC9 entry).
    pub dropped_category_values: u64,
}

/// The built join table.
#[derive(Debug, Clone, Default)]
pub struct BccwjMetadata {
    by_basename: BTreeMap<String, DocMetadata>,
}

impl BccwjMetadata {
    /// Builds the table from a distribution root's `DOC/` directory.
    ///
    /// # Errors
    ///
    /// A malformed table (wrong column count, unknown `corpusName`, or an
    /// unparseable publication year) is an error naming the file and line.
    pub fn build(root: &Path) -> io::Result<(Self, BccwjBuildNotes)> {
        let doc = root.join("DOC");
        let joined = read_tsv(&doc.join("Joined_info.txt"))?;
        let ndc = read_tsv(&doc.join("BCCWJ-NDC.txt"))?;
        let copyright = read_tsv(&doc.join("CopyRight_Annotation.txt"))?;

        // The NDC patch table: basename -> [genre-1, ndc, genre-3, genre-4].
        let mut ndc_patches: BTreeMap<String, [&str; 4]> = BTreeMap::new();
        for (line, row) in ndc.iter().enumerate() {
            if row.len() != 21 {
                return Err(column_error("BCCWJ-NDC.txt", line + 2, row.len(), 21));
            }
            ndc_patches.insert(row[1].to_owned(), [&row[11], &row[0], &row[13], &row[14]]);
        }
        let mut copyright_map: BTreeMap<String, String> = BTreeMap::new();
        for (line, row) in copyright.iter().enumerate() {
            if row.len() != 2 {
                return Err(column_error(
                    "CopyRight_Annotation.txt",
                    line + 2,
                    row.len(),
                    2,
                ));
            }
            copyright_map.insert(row[0].clone(), row[1].clone());
        }

        let mut notes = BccwjBuildNotes::default();
        let mut by_basename = BTreeMap::new();
        for (line, row) in joined.iter().enumerate() {
            if row.len() != 20 {
                return Err(column_error("Joined_info.txt", line + 2, row.len(), 20));
            }
            let [
                basename,
                _bib,
                title,
                subtitle,
                _volume,
                _responsibility,
                publisher,
                year,
                _isbn,
                _page,
                g1,
                g2,
                g3,
                g4,
                _respid,
                _personid,
                author,
                author_year,
                gender,
                corpus_name,
            ] = row.as_slice()
            else {
                // The length check above makes this branch unreachable, but
                // the lint floor denies `unreachable!`; a typed error keeps
                // the function total without a panic construct.
                return Err(column_error("Joined_info.txt", line + 2, row.len(), 20));
            };
            let subcorpus = Subcorpus::from_code(corpus_name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Joined_info.txt line {}: unknown corpusName for {basename}",
                        line + 2
                    ),
                )
            })?;
            let patch = ndc_patches.get(basename);
            let tuple: [&str; 4] = patch.copied().unwrap_or([g1, g2, g3, g4]);
            let (category, dropped) = derive_category(subcorpus, [g1, g2, g3, g4], patch.copied());
            let g3_patched = tuple[2];
            notes.dropped_category_values = notes
                .dropped_category_values
                .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));
            let year = year.parse::<i64>().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Joined_info.txt line {}: unparseable year for {basename}",
                        line + 2
                    ),
                )
            })?;
            // The book branch's single four-digit key lookup, computed once
            // for the three fields that share it.
            let c_code = subcorpus
                .is_book()
                .then(|| c_code_lookup(g3_patched))
                .flatten();
            by_basename.insert(
                basename.to_owned(),
                DocMetadata {
                    basename: basename.to_owned(),
                    title: {
                        let title = title.trim();
                        let subtitle = subtitle.trim();
                        if subtitle.is_empty() {
                            title.to_owned()
                        } else {
                            format!("{title}: {subtitle}")
                        }
                    },
                    author: author.to_owned(),
                    author_year: author_year.parse().ok(),
                    gender: derive_gender(gender),
                    publisher: if publisher.is_empty() {
                        "BCCWJ".to_owned()
                    } else {
                        publisher.to_owned()
                    },
                    copyright: copyright_map.get(basename).cloned(),
                    // The reference gates the C-code fields on the book
                    // subcorpora and does a single four-digit key lookup on
                    // the *patched* `genre-3`; a shorter value misses
                    // entirely (all three `nil`).
                    audience: c_code.map(|code| code.0.to_owned()),
                    media: c_code.map(|code| code.1.to_owned()),
                    topic: c_code.map(|code| code.2.to_owned()),
                    year,
                    subcorpus,
                    category,
                },
            );
        }
        Ok((Self { by_basename }, notes))
    }

    /// The metadata for a basename, or [`MissingBasename`].
    pub fn lookup(&self, basename: &str) -> Result<&DocMetadata, MissingBasename> {
        self.by_basename
            .get(basename)
            .ok_or_else(|| MissingBasename {
                basename: basename.to_owned(),
            })
    }

    /// The number of joined rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_basename.len()
    }

    /// Whether the table is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_basename.is_empty()
    }
}

fn column_error(file: &str, line: usize, found: usize, expected: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{file} line {line} has {found} columns, expected {expected}"),
    )
}

/// Plain tab-split with the header dropped. No CSV quoting: the reference
/// reads with `:quote 0`.
///
/// `str::lines` strips a trailing `\r`, so a CRLF-terminated table reads
/// identically to an LF one and the last column never carries a stray `\r`.
fn read_tsv(path: &Path) -> io::Result<Vec<Vec<String>>> {
    let text = std::fs::read_to_string(path)?;
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i == 0 {
            continue; // header
        }
        if line.is_empty() {
            continue;
        }
        rows.push(line.split('\t').map(String::from).collect());
    }
    Ok(rows)
}

/// The reference's gender rule: all `/`-split parts equal decides the
/// single value; any disagreement is `Mixed`. Clojure's `string/split`
/// drops a single trailing empty part, so a trailing slash is not a
/// disagreement; the split below replicates that.
fn derive_gender(value: &str) -> Option<Gender> {
    let mut parts: Vec<&str> = value.split('/').collect();
    if parts.len() > 1 && parts.last().is_some_and(|part| part.is_empty()) {
        parts.pop();
    }
    let uniform = parts.iter().all(|part| *part == parts[0]);
    let effective = if uniform { parts[0] } else { "mixed" };
    match effective {
        "男" => Some(Gender::Male),
        "女" => Some(Gender::Female),
        "" => None,
        _ => Some(Gender::Mixed),
    }
}

/// One book-branch label through the reference's pipeline: the anchored
/// notation strip, the top-level fallback for `digits.digits` values, the
/// digits-only drop, and the keep.
fn book_label(label: &str, out: &mut Vec<String>, dropped: &mut usize) {
    let stripped = STRIP_LEADING_NOTATION.replace_all(label, "");
    if let Some(top) = decimal_top_level(&stripped) {
        // The reference falls back to the top-level NDC concept; a missing
        // top entry contributes nothing.
        if let Some(labels) = NDC9.get(&top) {
            out.extend(labels.iter().cloned());
        }
    } else if stripped.is_empty() || is_digits_and_spaces(&stripped) {
        if !stripped.is_empty() {
            *dropped += 1;
        }
    } else {
        out.push(stripped.into_owned());
    }
}

/// The reference's category derivation, per subcorpus. Returns the derived
/// list (subcorpus display name first) and the count of dropped
/// unknown-notation values (the books branch's digits-only rule).
fn derive_category(
    subcorpus: Subcorpus,
    genre: [&str; 4],
    ndc_patch: Option<[&str; 4]>,
) -> (Vec<String>, usize) {
    let tuple = ndc_patch.unwrap_or(genre);
    let (values, dropped) = if subcorpus.is_book() {
        let mut out = Vec::new();
        let mut dropped = 0usize;
        for value in tuple.iter().take(2) {
            if *value == "なし" || *value == "分類なし" {
                continue;
            }
            // `ndc-map % [%]`: expand through the NDC9 table, falling back
            // to the value itself.
            match NDC9.get(*value) {
                Some(labels) => {
                    for label in labels {
                        book_label(label, &mut out, &mut dropped);
                    }
                }
                None => book_label(value, &mut out, &mut dropped),
            }
        }
        (out, dropped)
    } else if subcorpus == Subcorpus::OL {
        let values = tuple
            .iter()
            .map(|value| {
                STRIP_TWO_DIGIT_NOTATIONS
                    .replace_all(value, "")
                    .into_owned()
            })
            .collect();
        (values, 0)
    } else if subcorpus == Subcorpus::OT {
        let values = match ot_grade(tuple[1], tuple[2]) {
            Some(grade) => vec![tuple[0].to_owned(), grade],
            None => tuple.iter().copied().map(String::from).collect(),
        };
        (values, 0)
    } else {
        (tuple.iter().copied().map(String::from).collect(), 0)
    };

    // The branches outside the reference's `condp`: drop empties, remove
    // adjacent duplicates, prefix the subcorpus display name.
    let mut deduped: Vec<String> = Vec::new();
    for value in values.into_iter().filter(|value| !value.is_empty()) {
        if deduped.last().is_some_and(|last| last == &value) {
            continue;
        }
        deduped.push(value);
    }
    let mut category = vec![subcorpus.display_name().to_owned()];
    category.extend(deduped);
    (category, dropped)
}

/// The reference's strip patterns (Clojure `string/replace`, Java regex
/// semantics): anchored one-digit notation for the books branch, unanchored
/// all-occurrences two-digit notation for OL.
static STRIP_LEADING_NOTATION: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"^\d\s").expect("fixed pattern"));
static STRIP_TWO_DIGIT_NOTATIONS: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"\d\d\s").expect("fixed pattern"));

/// `91.1` → `91` when the value is exactly `digits.digits`; otherwise `None`.
fn decimal_top_level(value: &str) -> Option<String> {
    let (top, rest) = value.split_once('.')?;
    if top.is_empty()
        || rest.is_empty()
        || !top.bytes().all(|byte| byte.is_ascii_digit())
        || !rest.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some(top.to_owned())
}

/// The reference's `^[\d\s]+$` test.
fn is_digits_and_spaces(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b' ')
}

/// OT grade: 小 → `G{number}`, 中 → `G{number+6}`, 高 → `G10`. `None` means
/// the reference's `case` would have fallen through (a non-grade marker or an
/// unparseable number); the port then uses the raw tuple rather than the
/// reference's `"Gnil"` artifact.
fn ot_grade(marker: &str, number: &str) -> Option<String> {
    let value: i64 = match marker {
        "小" => number.parse().ok()?,
        "中" => number.parse().ok().map(|value: i64| value + 6)?,
        "高" => 10,
        _ => return None,
    };
    Some(format!("G{value}"))
}

/// C-code digit 1: sales audience (reference `c-code "1"`).
fn c_code_1(digit: &str) -> Option<&'static str> {
    match digit {
        "0" => Some("一般"),
        "1" => Some("教養"),
        "2" => Some("実用"),
        "3" => Some("専門"),
        "4" => Some("検定教科書／消費税非課税品・その他"),
        "5" => Some("婦人"),
        "6" => Some("学参Ｉ（中・小）"),
        "7" => Some("学参Ⅱ（高校）"),
        "8" => Some("児童"),
        "9" => Some("雑誌扱"),
        _ => None,
    }
}

/// C-code digit 2: form (reference `c-code "2"`).
fn c_code_2(digit: &str) -> Option<&'static str> {
    match digit {
        "0" => Some("単行本"),
        "1" => Some("文庫"),
        "2" => Some("新書"),
        "3" => Some("全集・双書"),
        "4" => Some("ムック・その他"),
        "5" => Some("辞典・事典"),
        "6" => Some("図鑑"),
        "7" => Some("絵本"),
        "8" => Some("磁性媒体など"),
        "9" => Some("コミックス"),
        _ => None,
    }
}

/// C-code digits 3-4: content (reference `c-code "34"`).
fn c_code_34(digits: &str) -> Option<&'static str> {
    match digits {
        "00" => Some("総記"),
        "01" => Some("百科事典"),
        "02" => Some("年鑑･雑誌"),
        "04" => Some("情報科学"),
        "10" => Some("哲学"),
        "11" => Some("心理（学）"),
        "12" => Some("倫理（学）"),
        "14" => Some("宗教"),
        "15" => Some("仏教"),
        "16" => Some("キリスト教"),
        "20" => Some("歴史総記"),
        "21" => Some("日本歴史"),
        "22" => Some("外国歴史"),
        "23" => Some("伝記"),
        "25" => Some("地理"),
        "26" => Some("旅行"),
        "30" => Some("社会科学総記"),
        "31" => Some("政治-含む国防軍事"),
        "32" => Some("法律"),
        "33" => Some("経済･財政･統計"),
        "34" => Some("経営"),
        "36" => Some("社会"),
        "37" => Some("教育"),
        "39" => Some("民族･風習"),
        "40" => Some("自然科学総記"),
        "41" => Some("数学"),
        "42" => Some("物理学"),
        "43" => Some("化学"),
        "44" => Some("天文･地学"),
        "45" => Some("生物学"),
        "47" => Some("医学･歯学･薬学"),
        "50" => Some("工学･工学総記"),
        "51" => Some("土木"),
        "52" => Some("建築"),
        "53" => Some("機械"),
        "54" => Some("電気"),
        "55" => Some("電子通信"),
        "56" => Some("海事"),
        "57" => Some("採鉱･冶金"),
        "58" => Some("その他の工業"),
        "60" => Some("産業総記"),
        "61" => Some("農林業"),
        "62" => Some("水産業"),
        "63" => Some("商業"),
        "65" => Some("交通･通信"),
        "70" => Some("芸術総記"),
        "71" => Some("絵画･彫刻"),
        "72" => Some("写真･工芸"),
        "73" => Some("音楽･舞踊"),
        "74" => Some("演劇･映画"),
        "75" => Some("体育･スポーツ"),
        "76" => Some("諸芸･娯楽"),
        "77" => Some("家事"),
        "79" => Some("コミックス･劇画"),
        "80" => Some("語学総記"),
        "81" => Some("日本語"),
        "82" => Some("英米語"),
        "84" => Some("ドイツ語"),
        "85" => Some("フランス語"),
        "87" => Some("各国語"),
        "90" => Some("文学総記"),
        "91" => Some("日本文学総記"),
        "92" => Some("日本文学詩歌"),
        "93" => Some("日本文学、小説･物語"),
        "95" => Some("日本文学、評論、随筆、その他"),
        "97" => Some("外国文学小説"),
        "98" => Some("外国文学、その他"),
        _ => None,
    }
}

/// The reference builds one flat map keyed by the four-digit C-code
/// (`str k1 k2 k3`) and does a single lookup; a shorter or missing `genre-3`
/// misses entirely. Returns the three labels only for a full four-digit code.
fn c_code_lookup(code: &str) -> Option<(&'static str, &'static str, &'static str)> {
    let k1 = code.get(..1)?;
    let k2 = code.get(1..2)?;
    let k34 = code.get(2..4)?;
    Some((c_code_1(k1)?, c_code_2(k2)?, c_code_34(k34)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a synthetic distribution `DOC/` directory (all text
    /// self-authored, §2) and returns the root.
    fn dist(joined: &str, ndc: &str, copyright: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let doc = dir.path().join("DOC");
        std::fs::create_dir_all(&doc).expect("doc dir");
        std::fs::write(doc.join("Joined_info.txt"), joined).expect("joined");
        std::fs::write(doc.join("BCCWJ-NDC.txt"), ndc).expect("ndc");
        std::fs::write(doc.join("CopyRight_Annotation.txt"), copyright).expect("copyright");
        dir
    }

    const JOINED_HEADER: &str = "a\tb\tc\td\te\tf\tg\th\ti\tj\tk\tl\tm\tn\to\tp\tq\tr\ts\tu\n";
    const NDC_HEADER: &str = "a\tb\tc\td\te\tf\tg\th\ti\tj\tk\tl\tm\tn\to\tp\tq\tr\ts\tu\tv\n";
    const COPYRIGHT_HEADER: &str = "Sample_ID\tAnnotation\n";

    /// Builds one Joined_info row (20 columns).
    fn joined_row(
        basename: &str,
        title: &str,
        subtitle: &str,
        publisher: &str,
        year: &str,
        g1: &str,
        g2: &str,
        g3: &str,
        g4: &str,
        author: &str,
        author_year: &str,
        gender: &str,
        corpus_name: &str,
    ) -> String {
        format!(
            "{basename}\tbib\t{title}\t{subtitle}\tvol\tresp\t{publisher}\t{year}\tisbn\tpage\t{g1}\t{g2}\t{g3}\t{g4}\trid\tpid\t{author}\t{author_year}\t{gender}\t{corpus_name}\n"
        )
    }

    fn ndc_row(ndc: &str, basename: &str, g1: &str, g2: &str, g3: &str, g4: &str) -> String {
        format!(
            "{ndc}\t{basename}\tbib\tt\ts\tvol\tresp\tpub\tyear\tisbn\tpage\t{g1}\t{g2}\t{g3}\t{g4}\trid\tpid\ta\ty\tg\t書籍\n"
        )
    }

    fn build(joined: &str, ndc: &str, copyright: &str) -> (BccwjMetadata, BccwjBuildNotes) {
        let dir = dist(joined, ndc, copyright);
        BccwjMetadata::build(dir.path()).expect("build")
    }

    /// The code ↔ display-name table, pinned as data — the direction the
    /// production paths use (the code directory resolves the subcorpus;
    /// `display_name` emits the Japanese value the `DOC/` tables carry).
    #[test]
    fn subcorpus_code_and_display_name_are_pinned() {
        let pinned: [(&str, &str); 13] = [
            ("LB", "書籍"),
            ("PB", "書籍"),
            ("OB", "書籍"),
            ("OL", "法律"),
            ("OW", "白書"),
            ("OM", "国会会議録"),
            ("OP", "広報紙"),
            ("OT", "検定教科書"),
            ("OV", "韻文"),
            ("OC", "Yahoo!知恵袋"),
            ("OY", "Yahoo!ブログ"),
            ("PN", "新聞"),
            ("PM", "雑誌"),
        ];
        let mut books = 0;
        for (code, name) in pinned {
            let sub = Subcorpus::from_code(code).expect("the pinned code resolves");
            assert_eq!(sub.as_code(), code);
            assert_eq!(sub.display_name(), name);
            if sub.is_book() {
                books += 1;
            }
        }
        // The book branch is exactly the (L|O|P)B trio.
        assert_eq!(books, 3);
        assert!(Subcorpus::from_code("ZZ").is_none());
    }

    #[test]
    fn books_category_splices_ndc9_and_strips_the_top_notation() {
        // Fictional: top heading label + a notation that expands to three
        // labels.
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t1",
                "仮題",
                "",
                "出版社甲",
                "1950",
                "9 文学",
                "913",
                "0004",
                "",
                "作者甲",
                "1940",
                "男",
                "LB",
            );
        let (meta, notes) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let doc = meta.lookup("t1").expect("t1");
        assert_eq!(doc.category, ["書籍", "文学", "日本文学", "小説", "物語"]);
        assert_eq!(notes.dropped_category_values, 0);
        // The C-code (0004) resolves audience/media/topic for books.
        assert_eq!(doc.audience.as_deref(), Some("一般"));
        assert_eq!(doc.media.as_deref(), Some("単行本"));
        assert_eq!(doc.topic.as_deref(), Some("情報科学"));
        assert_eq!(doc.subcorpus, Subcorpus::LB);
    }

    #[test]
    fn ndc_patch_replaces_the_entire_genre_tuple() {
        // The NDC table's tuple [g1, ndc, g3, g4] replaces Joined_info's
        // genre pair for the books derivation; genre-2 of Joined_info is
        // never used when the patch is present.
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t2",
                "仮題二",
                "",
                "出版社乙",
                "1960",
                "3 社会科学",
                "30",
                "",
                "",
                "作者乙",
                "",
                "",
                "LB",
            );
        let ndc =
            NDC_HEADER.to_owned() + &ndc_row("913", "t2", "91 日本文学", "旧ジャンル", "0004", "");
        let (meta, _) = build(&joined, &ndc, COPYRIGHT_HEADER);
        let doc = meta.lookup("t2").expect("t2");
        // Patch tuple [91 日本文学, 913, …]: "91 日本文学" is two digits, so
        // the anchored one-digit strip leaves it untouched; 913 expands.
        assert_eq!(
            doc.category,
            ["書籍", "91 日本文学", "日本文学", "小説", "物語"]
        );
    }

    #[test]
    fn decimal_notation_falls_back_to_the_top_level() {
        // "91.1" is not a direct NDC9 key, so the reference falls back to
        // the top level "91".
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t3",
                "仮題三",
                "",
                "",
                "1970",
                "なし",
                "91.1",
                "",
                "",
                "",
                "",
                "",
                "LB",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let doc = meta.lookup("t3").expect("t3");
        assert_eq!(doc.category, ["書籍", "日本文学（綱目）"]);
    }

    #[test]
    fn digits_only_unknown_values_are_dropped_and_counted() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t4",
                "仮題四",
                "",
                "",
                "1980",
                "1 哲学",
                "1234",
                "",
                "",
                "",
                "",
                "",
                "LB",
            );
        let (meta, notes) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let doc = meta.lookup("t4").expect("t4");
        assert_eq!(doc.category, ["書籍", "哲学"]);
        assert_eq!(notes.dropped_category_values, 1);
    }

    #[test]
    fn nashi_and_bunrui_nashi_are_removed() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t5",
                "仮題五",
                "",
                "",
                "1990",
                "なし",
                "分類なし",
                "",
                "",
                "",
                "",
                "",
                "LB",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        assert_eq!(meta.lookup("t5").expect("t5").category, ["書籍"]);
    }

    #[test]
    fn adjacent_duplicates_are_removed_once() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t6",
                "仮題六",
                "",
                "",
                "1991",
                "哲学",
                "哲学",
                "",
                "",
                "",
                "",
                "",
                "LB",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        assert_eq!(meta.lookup("t6").expect("t6").category, ["書籍", "哲学"]);
    }

    #[test]
    fn ol_strips_two_digit_notations_everywhere() {
        // Unanchored, all occurrences — the Clojure string/replace rule.
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t7",
                "仮題七",
                "",
                "",
                "1992",
                "32 法律",
                "91 憲法 12 条文",
                "",
                "",
                "",
                "",
                "",
                "OL",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let doc = meta.lookup("t7").expect("t7");
        assert_eq!(doc.category, ["法律", "法律", "憲法 条文"]);
        assert!(doc.audience.is_none());
    }

    #[test]
    fn ot_grades_come_from_the_marker_and_number() {
        for (marker, number, grade) in [("小", "2", "G2"), ("中", "1", "G7"), ("高", "3", "G10")]
        {
            let joined = JOINED_HEADER.to_owned()
                + &joined_row(
                    "t8",
                    "仮題八",
                    "",
                    "",
                    "1993",
                    "9 文学",
                    marker,
                    number,
                    "",
                    "",
                    "",
                    "",
                    "OT",
                );
            let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
            let doc = meta.lookup("t8").expect("t8");
            assert_eq!(doc.category, ["検定教科書", "9 文学", grade]);
        }
        // A non-grade marker falls back to the raw tuple (the reference
        // would have emitted the "Gnil" artifact).
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t8b",
                "仮題八b",
                "",
                "",
                "1993",
                "9 文学",
                "x",
                "2",
                "",
                "",
                "",
                "",
                "OT",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        assert_eq!(
            meta.lookup("t8b").expect("t8b").category,
            ["検定教科書", "9 文学", "x", "2"]
        );
    }

    #[test]
    fn other_subcorpora_keep_the_raw_genre_tuple() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t9",
                "仮題九",
                "",
                "",
                "1994",
                "ゲーム",
                "",
                "",
                "",
                "",
                "",
                "",
                "OY",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let doc = meta.lookup("t9").expect("t9");
        assert_eq!(doc.subcorpus, Subcorpus::OY);
        assert_eq!(doc.category, ["Yahoo!ブログ", "ゲーム"]);
    }

    #[test]
    fn gender_follows_the_split_rule() {
        let cases = [
            ("男", Some(Gender::Male)),
            ("女", Some(Gender::Female)),
            ("", None),
            ("男/女", Some(Gender::Mixed)),
            ("男/男", Some(Gender::Male)),
            // Clojure `string/split` drops a single trailing empty part.
            ("男/", Some(Gender::Male)),
            ("女/", Some(Gender::Female)),
            ("男/女/", Some(Gender::Mixed)),
            ("?", Some(Gender::Mixed)),
        ];
        for (i, &(value, expected)) in cases.iter().enumerate() {
            let joined = JOINED_HEADER.to_owned()
                + &joined_row(
                    &format!("g{i}"),
                    "題",
                    "",
                    "",
                    "2000",
                    "",
                    "",
                    "",
                    "",
                    "著",
                    "",
                    value,
                    "OM",
                );
            let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
            assert_eq!(
                meta.lookup(&format!("g{i}")).expect("row").gender,
                expected,
                "gender {value:?}"
            );
        }
    }

    #[test]
    fn title_author_publisher_copyright_and_years() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t10",
                "  主題  ",
                "  副題  ",
                "",
                "2001",
                "",
                "",
                "",
                "",
                " 著者  ",
                "1985",
                "",
                "OW",
            )
            + &joined_row(
                "t11",
                "裸題",
                "",
                "出版社丙",
                "2002",
                "",
                "",
                "",
                "",
                "著者丙",
                "時代",
                "",
                "OW",
            );
        let copyright = COPYRIGHT_HEADER.to_owned() + "t10\t権利表示\n";
        let (meta, _) = build(&joined, NDC_HEADER, &copyright);
        let ten = meta.lookup("t10").expect("t10");
        assert_eq!(ten.title, "主題: 副題");
        assert_eq!(ten.author_year, Some(1985));
        assert_eq!(ten.year, 2001);
        assert_eq!(ten.publisher, "BCCWJ");
        assert_eq!(ten.copyright.as_deref(), Some("権利表示"));
        let eleven = meta.lookup("t11").expect("t11");
        assert_eq!(eleven.title, "裸題");
        assert_eq!(eleven.author_year, None);
        assert_eq!(eleven.publisher, "出版社丙");
        assert!(eleven.copyright.is_none());
    }

    #[test]
    fn missing_columns_fail_with_the_line_named() {
        let joined = JOINED_HEADER.to_owned() + "short\trow\n";
        let dir = dist(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let error = BccwjMetadata::build(dir.path()).expect_err("short row");
        assert!(error.to_string().contains("line 2"), "{error}");
        assert!(error.to_string().contains("Joined_info.txt"), "{error}");
    }

    #[test]
    fn unknown_corpus_name_fails_the_build() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t12",
                "題",
                "",
                "",
                "2003",
                "",
                "",
                "",
                "",
                "",
                "",
                "",
                "謎なコード",
            );
        let dir = dist(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        assert!(BccwjMetadata::build(dir.path()).is_err());
    }

    #[test]
    fn missing_basename_is_an_error_not_a_note() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t13", "題", "", "", "2004", "", "", "", "", "", "", "", "PN",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        assert_eq!(
            meta.lookup("nope"),
            Err(MissingBasename {
                basename: "nope".into()
            })
        );
    }

    #[test]
    fn the_json_shape_is_stable_for_the_single_typing_rule() {
        let joined = JOINED_HEADER.to_owned()
            + &joined_row(
                "t14", "題", "", "社", "2005", "9 文学", "913", "0004", "", "著", "1990", "女",
                "LB",
            );
        let (meta, _) = build(&joined, NDC_HEADER, COPYRIGHT_HEADER);
        let value = meta.lookup("t14").expect("t14").to_json(Sampling::Variable);
        let object = value.as_object().expect("object");
        assert_eq!(object.get("year"), Some(&serde_json::json!(2005)));
        assert_eq!(object.get("author_year"), Some(&serde_json::json!(1990)));
        assert_eq!(object.get("gender"), Some(&serde_json::json!("female")));
        assert_eq!(object.get("permission"), Some(&serde_json::json!(true)));
        assert_eq!(object.get("sampling"), Some(&serde_json::json!("variable")));
        assert_eq!(object.get("corpus"), Some(&serde_json::json!("BCCWJ")));
        assert_eq!(object.get("subcorpus"), Some(&serde_json::json!("書籍")));
        assert_eq!(
            object.get("category"),
            Some(&serde_json::json!([
                "書籍",
                "文学",
                "日本文学",
                "小説",
                "物語"
            ]))
        );
        assert_eq!(object.get("genre"), object.get("category"));
        assert_eq!(object.get("audience"), Some(&serde_json::json!("一般")));
        // Sparse fields are omitted, not null.
        assert!(object.get("copyright").is_none());
        // The single typing rule accepts the object.
        assert!(
            crate::corpus::metadata_from_json(value, "t14").is_ok(),
            "metadata_from_json must accept the emitted object"
        );
    }
}
