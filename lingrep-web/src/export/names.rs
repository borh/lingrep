use std::collections::HashSet;

const MAX_SHEET_CHARS: usize = 31;
const FORBIDDEN: [char; 7] = [':', '\\', '/', '?', '*', '[', ']'];

fn sanitize_one(raw: &str, index: usize) -> String {
    let cleaned: String = raw.chars().filter(|c| !FORBIDDEN.contains(c)).collect();
    let trimmed = cleaned.trim().trim_matches('\'').trim();
    let truncated: String = trimmed.chars().take(MAX_SHEET_CHARS).collect();
    if truncated.is_empty() {
        format!("pattern-{index}")
    } else {
        truncated
    }
}

/// One Excel-legal, unique sheet name per input, aligned by index. "About" is
/// reserved (sheet 1), so a pattern that sanitises to "About" is de-duplicated.
pub(crate) fn sanitize_sheet_names(pattern_names: &[&str]) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::new();
    used.insert("about".to_string());
    let mut out = Vec::with_capacity(pattern_names.len());
    for (index, raw) in pattern_names.iter().enumerate() {
        let base = sanitize_one(raw, index);
        let mut candidate = base.clone();
        let mut suffix = 2;
        while !used.insert(candidate.to_lowercase()) {
            let tag = format!("~{suffix}");
            let keep = MAX_SHEET_CHARS.saturating_sub(tag.chars().count());
            let stem: String = base.chars().take(keep).collect();
            candidate = format!("{stem}{tag}");
            suffix += 1;
        }
        out.push(candidate);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_forbidden_chars_and_trims() {
        assert_eq!(sanitize_sheet_names(&["a/b:c?"]), vec!["abc".to_string()]);
        assert_eq!(sanitize_sheet_names(&["  'x'  "]), vec!["x".to_string()]);
    }
    #[test]
    fn truncates_to_31_characters_not_bytes() {
        let name = "あ".repeat(40);
        assert_eq!(
            sanitize_sheet_names(&[name.as_str()])[0].chars().count(),
            31
        );
    }
    #[test]
    fn blank_name_falls_back_to_index() {
        assert_eq!(
            sanitize_sheet_names(&["***"]),
            vec!["pattern-0".to_string()]
        );
    }
    #[test]
    fn dedups_case_insensitively_and_reserves_about() {
        assert_eq!(
            sanitize_sheet_names(&["verb", "VERB", "About"]),
            vec![
                "verb".to_string(),
                "VERB~2".to_string(),
                "About~2".to_string()
            ]
        );
    }
}
