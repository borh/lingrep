use std::io::{self, Write};

use anstyle::Style;
use lingrep_cli_common::{SafeText, TerminalSafeText};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{ColorPolicy, ContextColorPalette, ContextStyle};

pub const MIN_HUMAN_MATCH_WIDTH: usize = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HumanMatchLayout {
    #[default]
    Flowing,
    AlignHits,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanKwicRow {
    pub left: Vec<String>,
    pub keyword: Vec<String>,
    pub right: Vec<String>,
}

pub struct HumanRenderer {
    palette: ContextColorPalette,
    color: ColorPolicy,
    width: usize,
    layout: HumanMatchLayout,
    morpheme_separator: TerminalSafeText,
}

impl HumanRenderer {
    pub fn new(
        palette: ContextColorPalette,
        color: ColorPolicy,
        width: usize,
        layout: HumanMatchLayout,
        morpheme_separator: TerminalSafeText,
    ) -> Self {
        Self {
            palette,
            color,
            width: width.max(MIN_HUMAN_MATCH_WIDTH),
            layout,
            morpheme_separator,
        }
    }

    pub fn match_row<W: Write>(&self, writer: &mut W, row: &HumanKwicRow) -> io::Result<()> {
        let left = sanitize_tokens(&row.left);
        let keyword = sanitize_tokens(&row.keyword);
        let right = sanitize_tokens(&row.right);
        match self.layout {
            HumanMatchLayout::Flowing => self.write_flowing(writer, &left, &keyword, &right),
            HumanMatchLayout::AlignHits => self.write_aligned(writer, &left, &keyword, &right),
        }
    }

    fn write_flowing<W: Write>(
        &self,
        writer: &mut W,
        left: &[String],
        keyword: &[String],
        right: &[String],
    ) -> io::Result<()> {
        let separator = self.morpheme_separator.as_str();
        let separator_width = visible_width(separator);
        let marker_width = usize::from(!self.color.enabled()).saturating_mul(2);
        let full_keyword_width = joined_width(keyword, separator_width);
        let keyword_capacity = self.width.saturating_sub(marker_width);
        let keyword_text = fit_hit(keyword, separator, keyword_capacity);
        let truncated = full_keyword_width > keyword_capacity;

        if truncated {
            self.write_hit(writer, &keyword_text)?;
            return writer.write_all(b"\n");
        }

        let left_boundary = usize::from(!left.is_empty());
        let right_boundary = usize::from(!right.is_empty());
        let context_budget = self
            .width
            .saturating_sub(marker_width + full_keyword_width + left_boundary + right_boundary);
        let (left_budget, right_budget) = match (left.is_empty(), right.is_empty()) {
            (true, false) => (0, context_budget),
            (false, true) => (context_budget, 0),
            _ => (context_budget / 2, context_budget - context_budget / 2),
        };
        let left_start = left_start(left, separator_width, left_budget);
        let right_end = right_end(right, separator_width, right_budget);
        let selected_left = &left[left_start..];
        let selected_right = &right[..right_end];

        if !selected_left.is_empty() {
            self.write_left_context(writer, selected_left)?;
            writer.write_all(b" ")?;
        }
        self.write_hit(writer, &keyword_text)?;
        if !selected_right.is_empty() {
            writer.write_all(b" ")?;
            self.write_right_context(writer, selected_right)?;
        }
        writer.write_all(b"\n")
    }

    fn write_aligned<W: Write>(
        &self,
        writer: &mut W,
        left: &[String],
        keyword: &[String],
        right: &[String],
    ) -> io::Result<()> {
        let separator = self.morpheme_separator.as_str();
        let separator_width = visible_width(separator);
        let hit_start = self.width / 3;
        let closing_marker_width = usize::from(!self.color.enabled());
        let keyword_capacity = self.width.saturating_sub(hit_start + closing_marker_width);
        let full_keyword_width = joined_width(keyword, separator_width);
        let keyword_text = fit_hit(keyword, separator, keyword_capacity);
        let keyword_width = visible_width(&keyword_text);
        let truncated = full_keyword_width > keyword_capacity;

        let left_slot = if self.color.enabled() {
            hit_start.saturating_sub(1)
        } else {
            hit_start.saturating_sub(2)
        };
        let left_start = if truncated {
            left.len()
        } else {
            left_start(left, separator_width, left_slot)
        };
        let selected_left = &left[left_start..];
        if selected_left.is_empty() {
            let padding = if self.color.enabled() {
                hit_start
            } else {
                hit_start.saturating_sub(1)
            };
            write_spaces(writer, padding)?;
        } else {
            let left_width = joined_width(selected_left, separator_width);
            write_spaces(writer, left_slot.saturating_sub(left_width))?;
            self.write_left_context(writer, selected_left)?;
            writer.write_all(b" ")?;
        }

        self.write_hit(writer, &keyword_text)?;

        if !truncated && !right.is_empty() {
            let occupied = hit_start
                .saturating_add(keyword_width)
                .saturating_add(closing_marker_width);
            let right_budget = self.width.saturating_sub(occupied.saturating_add(1));
            let right_end = right_end(right, separator_width, right_budget);
            let selected_right = &right[..right_end];
            if !selected_right.is_empty() {
                writer.write_all(b" ")?;
                self.write_right_context(writer, selected_right)?;
            }
        }
        writer.write_all(b"\n")
    }

    fn write_left_context<W: Write>(&self, writer: &mut W, tokens: &[String]) -> io::Result<()> {
        for (index, token) in tokens.iter().enumerate() {
            if index != 0 {
                writer.write_all(self.morpheme_separator.as_str().as_bytes())?;
            }
            let distance = tokens.len() - index;
            self.write_context(
                writer,
                token,
                -isize::try_from(distance).unwrap_or(isize::MAX),
            )?;
        }
        Ok(())
    }

    fn write_right_context<W: Write>(&self, writer: &mut W, tokens: &[String]) -> io::Result<()> {
        for (index, token) in tokens.iter().enumerate() {
            if index != 0 {
                writer.write_all(self.morpheme_separator.as_str().as_bytes())?;
            }
            self.write_context(
                writer,
                token,
                isize::try_from(index + 1).unwrap_or(isize::MAX),
            )?;
        }
        Ok(())
    }

    fn write_hit<W: Write>(&self, writer: &mut W, keyword: &str) -> io::Result<()> {
        if self.color.enabled() {
            self.write_styled(writer, keyword, Style::new().bold())
        } else {
            write!(writer, "⟦{keyword}⟧")
        }
    }

    fn write_context<W: Write>(
        &self,
        writer: &mut W,
        token: &str,
        distance: isize,
    ) -> io::Result<()> {
        let style = match self.palette.style(distance) {
            ContextStyle::Proximity(color) => Style::new().fg_color(Some(color)),
            ContextStyle::Distant => Style::new().dimmed(),
        };
        self.write_styled(writer, token, style)
    }

    fn write_styled<W: Write>(&self, writer: &mut W, value: &str, style: Style) -> io::Result<()> {
        if self.color.enabled() {
            write!(writer, "{style}{value}{style:#}")
        } else {
            writer.write_all(value.as_bytes())
        }
    }
}

#[must_use]
pub fn visible_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
}

fn sanitize_tokens(tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .map(|token| SafeText::from_untrusted(token).as_str().to_owned())
        .collect()
}

fn joined_width(tokens: &[String], separator_width: usize) -> usize {
    tokens
        .iter()
        .map(|token| visible_width(token))
        .sum::<usize>()
        .saturating_add(separator_width.saturating_mul(tokens.len().saturating_sub(1)))
}

fn left_start(tokens: &[String], separator_width: usize, budget: usize) -> usize {
    let mut width = 0usize;
    for index in (0..tokens.len()).rev() {
        let added = visible_width(&tokens[index])
            .saturating_add(usize::from(index + 1 < tokens.len()).saturating_mul(separator_width));
        if width.saturating_add(added) > budget {
            return index + 1;
        }
        width += added;
    }
    0
}

fn right_end(tokens: &[String], separator_width: usize, budget: usize) -> usize {
    let mut width = 0usize;
    for (index, token) in tokens.iter().enumerate() {
        let added = visible_width(token)
            .saturating_add(usize::from(index != 0).saturating_mul(separator_width));
        if width.saturating_add(added) > budget {
            return index;
        }
        width += added;
    }
    tokens.len()
}

fn fit_hit(tokens: &[String], separator: &str, capacity: usize) -> String {
    let joined = tokens.join(separator);
    if visible_width(&joined) <= capacity {
        return joined;
    }
    if capacity == 0 {
        return String::new();
    }

    let prefix_budget = capacity - 1;
    let mut result = String::new();
    let mut width = 0usize;
    for token in tokens {
        let separator_width =
            usize::from(!result.is_empty()).saturating_mul(visible_width(separator));
        let token_width = visible_width(token);
        if width
            .saturating_add(separator_width)
            .saturating_add(token_width)
            <= prefix_budget
        {
            if !result.is_empty() {
                result.push_str(separator);
            }
            result.push_str(token);
            width = width
                .saturating_add(separator_width)
                .saturating_add(token_width);
            continue;
        }

        let available = prefix_budget.saturating_sub(width.saturating_add(separator_width));
        let prefix = grapheme_prefix(token, available);
        if !prefix.is_empty() {
            if !result.is_empty() {
                result.push_str(separator);
            }
            result.push_str(prefix);
        }
        break;
    }
    result.push('~');
    result
}

fn grapheme_prefix(value: &str, capacity: usize) -> &str {
    let mut end = 0usize;
    let mut width = 0usize;
    for grapheme in value.graphemes(true) {
        let next_width = width.saturating_add(visible_width(grapheme));
        if next_width > capacity {
            break;
        }
        width = next_width;
        end += grapheme.len();
    }
    value
        .get(..end)
        .expect("grapheme boundaries are UTF-8 boundaries")
}

fn write_spaces<W: Write>(writer: &mut W, count: usize) -> io::Result<()> {
    for _ in 0..count {
        writer.write_all(b" ")?;
    }
    Ok(())
}
