use std::borrow::Cow;

/// Returns whether text can be emitted as trusted terminal framing unchanged.
///
/// Unsafe text includes C0/C1 controls, bidi controls, and Unicode line or
/// paragraph separators. Untrusted leaves use [`sanitize_terminal`].
#[must_use]
pub fn is_terminal_safe(value: &str) -> bool {
    !value.chars().any(needs_escape)
}

/// Text validated for use as trusted terminal framing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSafeText(String);

impl TerminalSafeText {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::str::FromStr for TerminalSafeText {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        is_terminal_safe(value)
            .then(|| Self(value.to_owned()))
            .ok_or_else(|| "value contains terminal-unsafe text; use printable Unicode".to_owned())
    }
}

/// Render terminal-unsafe text as visible Unicode escapes before any terminal
/// layout or display-width calculation. Machine projections must preserve
/// their own documented encodings instead of using this conversion.
pub fn sanitize_terminal(value: &str) -> Cow<'_, str> {
    if is_terminal_safe(value) {
        return Cow::Borrowed(value);
    }
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if needs_escape(character) {
            use std::fmt::Write as _;
            write!(escaped, "\\u{{{:04x}}}", character as u32).expect("writing to String");
        } else {
            escaped.push(character);
        }
    }
    Cow::Owned(escaped)
}

fn needs_escape(character: char) -> bool {
    matches!(character, '\u{0000}'..='\u{001f}' | '\u{007f}'..='\u{009f}')
        || matches!(
            character,
            '\u{061c}'
                | '\u{200e}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
        )
}
