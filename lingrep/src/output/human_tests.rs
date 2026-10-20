use crate::output::{
    ColorPolicy, ContextColorPalette, HumanKwicRow, HumanMatchLayout, HumanRenderer,
    MIN_HUMAN_MATCH_WIDTH, ResolvedContextColorTheme, TerminalColorCapability, visible_width,
};
use crate::presentation::ColorMode;

fn renderer(
    mode: ColorMode,
    width: usize,
    layout: HumanMatchLayout,
    separator: &str,
) -> HumanRenderer {
    HumanRenderer::new(
        ContextColorPalette::new(
            5,
            ResolvedContextColorTheme::Dark,
            TerminalColorCapability::Truecolor,
        ),
        ColorPolicy::resolve(mode, false, !matches!(mode, ColorMode::Auto)),
        width,
        layout,
        separator.parse().unwrap(),
    )
}

fn row() -> HumanKwicRow {
    HumanKwicRow {
        left: vec!["前二".into(), "前一".into()],
        keyword: vec!["猫".into(), "犬".into()],
        right: vec!["後一".into(), "後二".into()],
    }
}

fn render_row_with_depth(
    depth: usize,
    theme: ResolvedContextColorTheme,
    capability: TerminalColorCapability,
    row: &HumanKwicRow,
) -> String {
    let renderer = HumanRenderer::new(
        ContextColorPalette::new(depth, theme, capability),
        ColorPolicy::resolve(ColorMode::Always, false, true),
        120,
        HumanMatchLayout::Flowing,
        "".parse().unwrap(),
    );
    let mut output = Vec::new();
    renderer.match_row(&mut output, row).unwrap();
    String::from_utf8(output).unwrap()
}

fn row_with_three_context_tokens() -> HumanKwicRow {
    HumanKwicRow {
        left: vec!["左三".into(), "左二".into(), "左一".into()],
        keyword: vec!["鍵".into()],
        right: vec!["右一".into(), "右二".into(), "右三".into()],
    }
}

#[test]
fn renderer_emits_truecolor_proximity_and_dim_beyond_depth() {
    let output = render_row_with_depth(
        2,
        ResolvedContextColorTheme::Dark,
        TerminalColorCapability::Truecolor,
        &row_with_three_context_tokens(),
    );
    assert!(output.contains("\x1b[38;2;32;208;176m"), "{output:?}");
    assert!(output.contains("\x1b[38;2;255;255;48m"), "{output:?}");
    assert!(output.contains("\x1b[2m"), "{output:?}");
}

#[test]
fn depth_zero_dims_all_context_without_a_foreground_color() {
    let output = render_row_with_depth(
        0,
        ResolvedContextColorTheme::Dark,
        TerminalColorCapability::Truecolor,
        &row_with_three_context_tokens(),
    );
    assert!(output.contains("\x1b[2m"), "{output:?}");
    for foreground in ["\x1b[38;2;", "\x1b[38;5;", "\x1b[96m", "\x1b[36m"] {
        assert!(!output.contains(foreground), "{output:?}");
    }
}

#[test]
fn color_policy_respects_no_color_never_and_redirected_auto() {
    assert!(ColorPolicy::resolve(ColorMode::Always, false, false).enabled());
    assert!(!ColorPolicy::resolve(ColorMode::Always, true, true).enabled());
    assert!(!ColorPolicy::resolve(ColorMode::Never, false, true).enabled());
    assert!(!ColorPolicy::resolve(ColorMode::Auto, false, false).enabled());
    assert!(ColorPolicy::resolve(ColorMode::Auto, false, true).enabled());
}

#[test]
fn flowing_uses_quiet_styled_and_marked_plain_hits() {
    let mut plain = Vec::new();
    renderer(ColorMode::Never, 80, HumanMatchLayout::Flowing, "")
        .match_row(&mut plain, &row())
        .unwrap();
    assert_eq!(
        String::from_utf8(plain).unwrap(),
        "前二前一 ⟦猫犬⟧ 後一後二\n"
    );

    let mut redirected = Vec::new();
    renderer(ColorMode::Auto, 80, HumanMatchLayout::Flowing, "")
        .match_row(&mut redirected, &row())
        .unwrap();
    assert_eq!(
        String::from_utf8(redirected).unwrap(),
        "前二前一 ⟦猫犬⟧ 後一後二\n"
    );

    let mut styled = Vec::new();
    renderer(ColorMode::Always, 80, HumanMatchLayout::Flowing, "")
        .match_row(&mut styled, &row())
        .unwrap();
    let styled = String::from_utf8(styled).unwrap();
    assert!(styled.contains("\u{1b}[1m猫犬"), "{styled:?}");
    assert!(
        !['⟦', '⟧', '│']
            .into_iter()
            .any(|marker| styled.contains(marker))
    );
    assert!(!styled.contains("\u{1b}[7m"));
}

#[test]
fn custom_separator_joins_all_fields_not_hit_boundaries() {
    let mut plain = Vec::new();
    renderer(ColorMode::Never, 80, HumanMatchLayout::Flowing, "|")
        .match_row(&mut plain, &row())
        .unwrap();
    assert_eq!(
        String::from_utf8(plain).unwrap(),
        "前二|前一 ⟦猫|犬⟧ 後一|後二\n"
    );

    let mut styled = Vec::new();
    renderer(ColorMode::Always, 80, HumanMatchLayout::Flowing, "|")
        .match_row(&mut styled, &row())
        .unwrap();
    let styled = String::from_utf8(styled).unwrap();
    assert!(styled.contains("\u{1b}[1m猫|犬"), "{styled:?}");
    assert!(styled.contains("\u{1b}[0m|\u{1b}["), "{styled:?}");
}

#[test]
fn aligned_hit_content_starts_at_one_third_without_trailing_padding() {
    for mode in [ColorMode::Never, ColorMode::Always] {
        let mut out = Vec::new();
        renderer(mode, 30, HumanMatchLayout::AlignHits, "")
            .match_row(
                &mut out,
                &HumanKwicRow {
                    left: vec!["左".into()],
                    keyword: vec!["鍵".into()],
                    right: vec!["右".into()],
                },
            )
            .unwrap();
        let out = String::from_utf8(out).unwrap();
        let key = out.find('鍵').unwrap();
        let before_key = out.get(..key).expect("find returns a UTF-8 boundary");
        assert_eq!(visible_width(&strip_ansi(before_key)), 10, "{out:?}");
        assert!(out.ends_with("右\u{1b}[0m\n") || out.ends_with("⟧ 右\n"));
        assert!(!out.ends_with(" \n"));
    }
}

#[test]
fn aligned_absent_left_places_plain_marker_before_hit() {
    let mut out = Vec::new();
    renderer(ColorMode::Never, 30, HumanMatchLayout::AlignHits, "")
        .match_row(
            &mut out,
            &HumanKwicRow {
                left: Vec::new(),
                keyword: vec!["鍵".into()],
                right: Vec::new(),
            },
        )
        .unwrap();
    let out = String::from_utf8(out).unwrap();
    let marker = out.find('⟦').unwrap();
    let key = out.find('鍵').unwrap();
    assert_eq!(
        visible_width(out.get(..marker).expect("find returns a UTF-8 boundary")),
        9
    );
    assert_eq!(
        visible_width(out.get(..key).expect("find returns a UTF-8 boundary")),
        10
    );
}

#[test]
fn contexts_clip_only_at_morpheme_boundaries() {
    let mut out = Vec::new();
    renderer(ColorMode::Never, 14, HumanMatchLayout::Flowing, "")
        .match_row(
            &mut out,
            &HumanKwicRow {
                left: vec!["長長長".into(), "左".into()],
                keyword: vec!["鍵".into()],
                right: vec!["右".into(), "長長長".into()],
            },
        )
        .unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "左 ⟦鍵⟧ 右\n");
}

#[test]
fn width_floor_and_overlong_hit_use_grapheme_safe_ascii_tilde() {
    let mut out = Vec::new();
    renderer(ColorMode::Never, 0, HumanMatchLayout::Flowing, "")
        .match_row(
            &mut out,
            &HumanKwicRow {
                left: vec!["左側全体".into()],
                keyword: vec!["か\u{3099}".into(), "👨‍👩‍👧‍👦".into(), "あいうえお".into()],
                right: vec!["右側全体".into()],
            },
        )
        .unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains('~'));
    assert!(!out.contains('…'));
    assert!(!out.contains('\u{fffd}'));
    assert!(visible_width(out.trim_end()) <= MIN_HUMAN_MATCH_WIDTH);
}

fn strip_ansi(value: &str) -> String {
    let mut stripped = String::new();
    let mut chars = value.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' && chars.next_if_eq(&'[').is_some() {
            for control in chars.by_ref() {
                if control.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            stripped.push(character);
        }
    }
    stripped
}

/// Tokens chosen to attack the escape-sequence boundary: complete sequences,
/// a bare `ESC`, a payload with its introducer removed, and a C1 introducer.
/// Corpus text is untrusted, so each of these is a value a document can carry.
const ESCAPE_HOSTILE_TOKENS: &[&str] = &[
    "\u{1b}[38;2;32;208;176m",
    "\u{1b}[0m",
    "\u{1b}",
    "[38;2;32;208;176m",
    "\u{9b}38;2;0m",
    "限定さ",
    "の\u{300}",
    "𠀋",
];

/// Strictly scan one rendered row, returning its visible text.
///
/// Unlike [`strip_ansi`], which tolerates anything and is only used to recover
/// visible width, this rejects every departure from the one shape the renderer
/// is allowed to emit: a complete `ESC [ <0-9;>* m`, opened and closed in
/// strict alternation, closed before the row ends. A sliced sequence, a bare
/// `ESC`, a non-SGR final byte, or a row that ends mid-style is an error here.
fn scan_sgr(row: &str) -> Result<String, String> {
    let mut visible = String::new();
    let mut open = false;
    let mut chars = row.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            if character != '\n' && character.is_control() {
                return Err(format!("bare control character {character:?}"));
            }
            visible.push(character);
            continue;
        }
        if chars.next_if_eq(&'[').is_none() {
            return Err("`ESC` not followed by `[`".to_owned());
        }
        let mut parameters = String::new();
        let final_byte = loop {
            match chars.next() {
                None => return Err(format!("sequence truncated after `ESC[{parameters}`")),
                Some(next) if next.is_ascii_digit() || next == ';' => parameters.push(next),
                Some(next) => break next,
            }
        };
        if final_byte != 'm' {
            return Err(format!(
                "non-SGR final byte {final_byte:?} after `ESC[{parameters}`"
            ));
        }
        if parameters.is_empty() || parameters == "0" {
            if !open {
                return Err("reset with no style open".to_owned());
            }
            open = false;
        } else {
            if open {
                return Err(format!("`ESC[{parameters}m` opened while a style was open"));
            }
            open = true;
        }
    }
    if open {
        return Err("row ended with a style still open".to_owned());
    }
    Ok(visible)
}

#[test]
fn scan_sgr_rejects_the_shapes_the_guard_exists_to_catch() {
    assert!(scan_sgr("\u{1b}[1m猫\u{1b}[0m\n").is_ok());
    // The observed fragment: a sequence sliced before its final byte.
    assert!(scan_sgr("\u{1b}[38;2;32\n").is_err());
    assert!(scan_sgr("\u{1b}").is_err());
    assert!(scan_sgr("\u{1b}X").is_err());
    // A non-SGR final byte, which is what makes a partial sequence visible.
    assert!(scan_sgr("\u{1b}[38;2;32C;208;176m\n").is_err());
    assert!(scan_sgr("\u{1b}[1m猫\n").is_err());
    assert!(scan_sgr("\u{1b}[1m\u{1b}[2m猫\u{1b}[0m\n").is_err());
}

#[test]
fn every_rendered_row_is_a_complete_and_balanced_escape_stream() {
    let mut rows_checked = 0_usize;
    for &color in &[ColorMode::Always, ColorMode::Never] {
        for &layout in &[HumanMatchLayout::Flowing, HumanMatchLayout::AlignHits] {
            for &capability in &[
                TerminalColorCapability::Truecolor,
                TerminalColorCapability::Ansi256,
                TerminalColorCapability::Ansi16,
            ] {
                for &theme in &[
                    ResolvedContextColorTheme::Dark,
                    ResolvedContextColorTheme::Light,
                ] {
                    for &depth in &[0_usize, 1, 5, 20] {
                        for &width in &[MIN_HUMAN_MATCH_WIDTH, 11, 13, 20, 40, 120] {
                            for &separator in &["", "|", "・"] {
                                for split in 0..ESCAPE_HOSTILE_TOKENS.len() {
                                    let (left, rest) = ESCAPE_HOSTILE_TOKENS.split_at(split);
                                    let (keyword, right) = rest.split_at(rest.len().min(2));
                                    let row = HumanKwicRow {
                                        left: left.iter().map(|&t| t.to_owned()).collect(),
                                        keyword: if keyword.is_empty() {
                                            vec!["鍵".to_owned()]
                                        } else {
                                            keyword.iter().map(|&t| t.to_owned()).collect()
                                        },
                                        right: right.iter().map(|&t| t.to_owned()).collect(),
                                    };
                                    let renderer = HumanRenderer::new(
                                        ContextColorPalette::new(depth, theme, capability),
                                        ColorPolicy::resolve(color, false, true),
                                        width,
                                        layout,
                                        separator.parse().unwrap(),
                                    );
                                    let mut buffer = Vec::new();
                                    renderer.match_row(&mut buffer, &row).unwrap();
                                    let rendered = String::from_utf8(buffer)
                                        .expect("rendered rows are valid UTF-8");
                                    let context = format!(
                                        "color={color:?} layout={layout:?} \
                                         capability={capability:?} theme={theme:?} \
                                         depth={depth} width={width} separator={separator:?} \
                                         split={split} rendered={rendered:?}"
                                    );
                                    assert!(rendered.ends_with('\n'), "{context}");
                                    let visible = match scan_sgr(&rendered) {
                                        Ok(visible) => visible,
                                        Err(error) => panic!("{error}: {context}"),
                                    };
                                    assert!(!visible.contains('\u{1b}'), "{context}");
                                    if color == ColorMode::Never {
                                        assert!(!rendered.contains('\u{1b}'), "{context}");
                                    }
                                    rows_checked += 1;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(
        rows_checked,
        2 * 2 * 3 * 2 * 4 * 6 * 3 * ESCAPE_HOSTILE_TOKENS.len()
    );
}
