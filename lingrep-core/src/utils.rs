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
/// Streaming callers frame before normalizing each sentence, so adding a
/// delimiter that can compose with the preceding scalar would change the
/// legacy whole-input NFC boundaries.
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

/// Join token orthographies for display.
///
/// Japanese and fullwidth runs concatenate. A single space is inserted only
/// between adjacent alphabetic tokens outside those ranges. Empty values are
/// transparent and preserve the preceding boundary character.
pub fn join_orth<'a>(orths: impl IntoIterator<Item = &'a str>) -> String {
    let mut joined = String::new();
    join_orth_into(&mut joined, orths);
    joined
}

/// [`join_orth`] appending into a caller-owned buffer.
///
/// The same function; the buffer is the point. A caller joining one surface
/// per match over a whole corpus would otherwise allocate per match, and
/// clearing a reused `String` keeps its capacity. `joined` is appended to
/// rather than cleared, so the caller decides.
pub fn join_orth_into<'a>(joined: &mut String, orths: impl IntoIterator<Item = &'a str>) {
    let mut previous_last = joined.chars().next_back();
    for orth in orths {
        if let (Some(previous), Some(first)) = (previous_last, orth.chars().next())
            && is_spaced_script_letter(previous)
            && is_spaced_script_letter(first)
        {
            joined.push(' ');
        }
        joined.push_str(orth);
        if let Some(last) = orth.chars().next_back() {
            previous_last = Some(last);
        }
    }
}

#[cfg(test)]
mod join_orth_tests {
    use super::join_orth;

    #[test]
    fn joins_japanese_without_spaces_and_latin_with_spaces() {
        assert_eq!(join_orth(["今日", "は"]), "今日は");
        assert_eq!(join_orth(["machine", "learning"]), "machine learning");
        assert_eq!(join_orth(["を", "DNA"]), "をDNA");
    }

    #[test]
    fn empty_values_are_transparent() {
        assert_eq!(join_orth(["alpha", "", "beta"]), "alpha beta");
        assert_eq!(join_orth(["", "解析", ""]), "解析");
    }

    #[test]
    fn digits_punctuation_and_fullwidth_letters_do_not_insert_spaces() {
        assert_eq!(join_orth(["model", "2"]), "model2");
        assert_eq!(join_orth(["alpha", "。", "beta"]), "alpha。beta");
        assert_eq!(join_orth(["Ａ", "Ｂ"]), "ＡＢ");
    }

    #[test]
    fn extension_b_han_concatenates_with_bmp_han() {
        assert_eq!(join_orth(["𠀀", "漢"]), "𠀀漢");
    }
}
