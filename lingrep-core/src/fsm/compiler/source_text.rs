use std::borrow::Cow;

/// Returns the same-length parser view used by every lingrep patterns Tree-sitter consumer.
///
/// The pinned grammar misclassifies `begin\r` in CRLF input. Replacing only
/// the CR byte of CRLF pairs preserves all byte offsets into the caller's text.
pub(crate) fn parser_text(source: &str) -> Cow<'_, str> {
    if !source.as_bytes().windows(2).any(|window| window == b"\r\n") {
        return Cow::Borrowed(source);
    }

    let mut bytes = source.as_bytes().to_vec();
    for index in 0..bytes.len().saturating_sub(1) {
        if bytes[index] == b'\r' && bytes[index + 1] == b'\n' {
            bytes[index] = b'\n';
        }
    }
    Cow::Owned(String::from_utf8(bytes).expect("ASCII substitution preserves UTF-8"))
}
