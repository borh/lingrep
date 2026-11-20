pub fn split_sentence_ja(s: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut start = 0;
    let mut end = 0;
    let chars: Vec<char> = s.chars().collect();

    while end < chars.len() {
        if is_sentence_delimiter(chars[end]) {
            if end + 1 < chars.len() && is_sentence_closing_quotation(chars[end + 1]) {
                end += 1;
            }
            sentences.push(chars[start..=end].iter().collect());
            start = end + 1;
        }
        end += 1;
    }

    if start < chars.len() {
        sentences.push(chars[start..].iter().collect());
    }

    sentences
}

/// All delimiters must remain NFC starters.
///
/// Streaming callers frame before normalizing each sentence, so a delimiter
/// that can compose with the preceding scalar would make streamed boundaries
/// differ from whole-input NFC boundaries.
pub fn is_sentence_delimiter(character: char) -> bool {
    matches!(character, '!' | '?' | '。' | '！' | '？')
}

pub fn is_sentence_closing_quotation(character: char) -> bool {
    matches!(
        character,
        ')' | '）' | '」' | '』' | '】' | '］' | '〕' | '〉' | '》' | ']'
    )
}

fn is_cjk_or_fullwidth(character: char) -> bool {
    matches!(
        character,
        '\u{3000}'..='\u{303F}'
            | '\u{3040}'..='\u{30FF}'
            | '\u{3400}'..='\u{4DBF}'
            | '\u{4E00}'..='\u{9FFF}'
            | '\u{F900}'..='\u{FAFF}'
            | '\u{FF00}'..='\u{FFEF}'
            | '\u{2E80}'..='\u{2EFF}'
            | '\u{2F00}'..='\u{2FDF}'
            | '\u{2FF0}'..='\u{2FFF}'
            | '\u{20000}'..='\u{2EBEF}'
            | '\u{2F800}'..='\u{2FA1F}'
    )
}

fn is_spaced_script_letter(character: char) -> bool {
    character.is_alphabetic() && !is_cjk_or_fullwidth(character)
}

/// Whether two adjacent token texts are both written in a script that puts
/// spaces between words: `before` ends, and `after` begins, with a letter
/// outside the Japanese and fullwidth ranges.
///
/// [`crate::token_match::display_space_before`] uses this only when a sentence
/// carries no source spans. Spans answer the question exactly; this guess is
/// wrong wherever an analyzer splits a Latin word into several tokens.
pub(crate) fn spaced_script_boundary(before: &str, after: &str) -> bool {
    matches!(
        (before.chars().next_back(), after.chars().next()),
        (Some(last), Some(first))
            if is_spaced_script_letter(last) && is_spaced_script_letter(first)
    )
}

#[cfg(test)]
mod spaced_script_boundary_tests {
    use super::spaced_script_boundary;

    #[test]
    fn only_two_letters_of_a_spaced_script_form_a_boundary() {
        for (before, after, spaced) in [
            ("machine", "learning", true),
            ("今日", "は", false),
            ("を", "DNA", false),
            ("model", "2", false),
            ("alpha", "。", false),
            ("Ａ", "Ｂ", false),
            ("𠀀", "漢", false),
            ("alpha", "", false),
            ("", "beta", false),
        ] {
            assert_eq!(
                spaced_script_boundary(before, after),
                spaced,
                "{before:?} then {after:?}"
            );
        }
    }
}
