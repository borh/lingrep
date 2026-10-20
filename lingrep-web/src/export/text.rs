pub(crate) const TRUNCATION_MARKER: &str = "…[truncated]";

/// Truncate `text` to at most `limit` characters. When truncation happens and
/// `limit` leaves room for the marker, the returned value ends with
/// `TRUNCATION_MARKER` and is still ≤ `limit` characters; if `limit` is smaller
/// than the marker, the value is a bare `limit`-char prefix.
pub(crate) fn truncate_cell(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let marker_len = TRUNCATION_MARKER.chars().count();
    if limit < marker_len {
        return text.chars().take(limit).collect();
    }
    let keep = limit - marker_len;
    let mut out: String = text.chars().take(keep).collect();
    out.push_str(TRUNCATION_MARKER);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_text_is_unchanged() {
        assert_eq!(truncate_cell("abc", 10), "abc");
    }
    #[test]
    fn truncated_value_is_at_most_limit_chars_including_marker() {
        let out = truncate_cell(&"あ".repeat(100), 20);
        assert!(out.chars().count() <= 20, "len {}", out.chars().count());
        assert!(out.ends_with(TRUNCATION_MARKER));
    }
    #[test]
    fn limit_smaller_than_marker_truncates_without_marker() {
        let out = truncate_cell(&"あ".repeat(100), 3);
        assert_eq!(out.chars().count(), 3);
        assert!(!out.ends_with(TRUNCATION_MARKER));
    }
}
