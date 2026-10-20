use super::KwicLine;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::sanitize_terminal;

/// Render one sanitized, display-width-aligned KWIC row.
pub fn format_kwic_aligned_line(line: &KwicLine, total_width: usize) -> String {
    const SEP: &str = " | ";
    const SEP_W: usize = 6;
    let pre = sanitize_terminal(&line.pre);
    let key = sanitize_terminal(&line.key);
    let post = sanitize_terminal(&line.post);
    let key_w = str_w(&key);
    let available = total_width.saturating_sub(SEP_W);
    if key_w >= available {
        return format!("{SEP}{}{SEP}", fit_left(&key, available));
    }
    let budget = available - key_w;
    let pre_budget = budget / 2;
    let post_budget = budget - pre_budget;
    format!(
        "{}{SEP}{key}{SEP}{}",
        fit_right(&pre, pre_budget),
        fit_left(&post, post_budget)
    )
}

fn str_w(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn fit_left(value: &str, budget: usize) -> String {
    let total = str_w(value);
    if total <= budget {
        return format!("{value}{}", " ".repeat(budget - total));
    }
    if budget == 0 {
        return String::new();
    }
    let mut width = 0;
    let mut content = String::new();
    for grapheme in value.graphemes(true) {
        let grapheme_width = str_w(grapheme);
        if width + grapheme_width > budget - 1 {
            break;
        }
        content.push_str(grapheme);
        width += grapheme_width;
    }
    format!("{content}…{}", " ".repeat(budget - width - 1))
}

fn fit_right(value: &str, budget: usize) -> String {
    let total = str_w(value);
    if total <= budget {
        return format!("{}{value}", " ".repeat(budget - total));
    }
    if budget == 0 {
        return String::new();
    }
    let mut width = 0;
    let mut kept = Vec::new();
    for grapheme in value.graphemes(true).rev() {
        let grapheme_width = str_w(grapheme);
        if width + grapheme_width > budget - 1 {
            break;
        }
        kept.push(grapheme);
        width += grapheme_width;
    }
    let content = kept.into_iter().rev().collect::<String>();
    format!("{}…{content}", " ".repeat(budget - width - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(pre: &str, key: &str, post: &str) -> KwicLine {
        KwicLine {
            pre: pre.into(),
            key: key.into(),
            post: post.into(),
            match_start: 0,
            match_end: 0,
        }
    }

    #[test]
    fn aligned_fills_exact_width() {
        assert_eq!(
            str_w(&format_kwic_aligned_line(
                &line("alpha", "KEY", "omega"),
                40
            )),
            40
        );
    }

    #[test]
    fn aligned_cjk_uses_display_width_two() {
        assert_eq!(
            str_w(&format_kwic_aligned_line(
                &line("前の腕を", "KEY", "中心に据え"),
                60
            )),
            60
        );
        assert_eq!(
            str_w(&format_kwic_aligned_line(
                &line("を中心に", "を住所を備考", "に入力した"),
                60
            )),
            60
        );
    }

    #[test]
    fn aligned_truncates_without_panicking_at_degenerate_widths() {
        for width in [0, 1, 2, 5, 6, 7, 20, 1_000] {
            let _ = format_kwic_aligned_line(
                &line("abcdefghijklmnop", "を住所を備考", "zyxwvutsrqponml"),
                width,
            );
        }
    }

    #[test]
    fn fit_helpers_preserve_padding_and_ellipsis() {
        assert_eq!(fit_left("abcdef", 4), "abc…");
        assert_eq!(fit_left("abc", 6), "abc   ");
        assert_eq!(fit_right("abcdef", 4), "…def");
        assert_eq!(fit_right("abc", 6), "   abc");
    }
}
