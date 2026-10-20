use std::borrow::Cow;
use std::fmt;

/// A terminal-safe untrusted leaf value.
///
/// Renderer framing such as newlines and separators must be emitted outside
/// this type so trusted layout is never escaped with the leaf.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SafeText<'a>(Cow<'a, str>);

impl<'a> SafeText<'a> {
    #[must_use]
    pub fn from_untrusted(value: &'a str) -> Self {
        Self(crate::sanitize_terminal(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SafeText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}
