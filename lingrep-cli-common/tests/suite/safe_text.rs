use std::borrow::Cow;
use std::str::FromStr as _;

use lingrep_cli_common::{SafeText, TerminalSafeText, is_terminal_safe, sanitize_terminal};

#[test]
fn terminal_safety_owns_controls_bidi_and_line_separators() {
    for value in [
        "\u{000a}", "\u{0085}", "\u{061c}", "\u{202e}", "\u{2066}", "\u{2028}", "\u{2029}",
    ] {
        assert!(!is_terminal_safe(value), "{value:?}");
        assert_ne!(sanitize_terminal(value).as_ref(), value);
    }
    let safe = "猫　⟦⟧";
    assert!(is_terminal_safe(safe));
    assert!(matches!(sanitize_terminal(safe), Cow::Borrowed(_)));
    assert_eq!(TerminalSafeText::from_str("|").unwrap().as_str(), "|");
    assert!(TerminalSafeText::from_str("\u{2028}").is_err());
}

#[test]
fn line_separators_are_visible_in_untrusted_leaves() {
    assert_eq!(
        SafeText::from_untrusted("左\u{2028}中\u{2029}右").as_str(),
        "左\\u{2028}中\\u{2029}右"
    );
}

#[test]
fn safe_text_escapes_controls_but_not_renderer_layout() {
    let leaf = SafeText::from_untrusted("猫\n\u{1b}[2J");
    assert_eq!(leaf.as_str(), "猫\\u{000a}\\u{001b}[2J");
    let rendered = format!("result: {leaf}\n");
    assert!(rendered.ends_with('\n'));
}
