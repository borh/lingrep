use proptest::prelude::*;

pub(crate) const HTML_SECURITY_DELIMITERS: [char; 5] = ['<', '>', '&', '\'', '"'];

pub(crate) fn safe_affix() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9]{1,12}"
}

pub(crate) fn security_payload(prefix: &str, delimiter: char, suffix: &str) -> String {
    format!("lingrep-security-{prefix}-{delimiter}-{suffix}-end")
}

/// The HTML text-context rule used as the security oracle in render tests.
///
/// Keep this deliberately independent of Askama and production rendering: the
/// five substitutions below are derived directly from HTML delimiter rules.
pub(crate) fn expected_html_text(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for character in raw.chars() {
        match character {
            '&' => escaped.push_str("&#38;"),
            '\'' => escaped.push_str("&#39;"),
            '<' => escaped.push_str("&#60;"),
            '>' => escaped.push_str("&#62;"),
            '"' => escaped.push_str("&#34;"),
            _ => escaped.push(character),
        }
    }
    escaped
}
