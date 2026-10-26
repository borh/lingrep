use std::collections::HashMap;

use rust_xlsxwriter::{Format, Workbook, Worksheet, XlsxError};

use crate::export::names::sanitize_sheet_names;
use crate::export::sink::PatternScan;
use crate::export::text::truncate_cell;

const EXCEL_CELL_LIMIT: usize = 32_767;

pub(crate) struct AboutMeta {
    pub scan_scope: String,
    pub filters: String,
    pub corpus: String,
    pub corpus_id: String,
    pub dictionary: String,
    pub revision: u64,
    pub exact_matches: u64,
    pub exported_rows: u64,
    pub pattern_source: String,
}

/// Every string cell goes through this so the writer — not the caller — owns the
/// Excel-limit invariant.
fn write_cell(sheet: &mut Worksheet, row: u32, col: u16, text: &str) -> Result<(), XlsxError> {
    sheet.write_string(row, col, truncate_cell(text, EXCEL_CELL_LIMIT))?;
    Ok(())
}

pub(crate) fn build_workbook(
    about: &AboutMeta,
    patterns: &[PatternScan],
    sections: &HashMap<u64, String>,
    exported_at: &str,
) -> Result<Vec<u8>, XlsxError> {
    let mut workbook = Workbook::new();
    let bold = Format::new().set_bold();

    let represented: Vec<&PatternScan> = patterns.iter().filter(|p| !p.rows.is_empty()).collect();
    let represented_names: Vec<&str> = represented.iter().map(|p| p.name.as_str()).collect();
    let sheet_names = sanitize_sheet_names(&represented_names);

    // ----- About -----
    {
        let sheet = workbook.add_worksheet();
        sheet.set_name("About")?;
        let mut r = 0u32;

        write_cell(sheet, r, 0, "corpus")?;
        write_cell(sheet, r, 1, &about.corpus)?;
        r += 1;
        write_cell(sheet, r, 0, "corpus_id")?;
        write_cell(sheet, r, 1, &about.corpus_id)?;
        r += 1;
        write_cell(sheet, r, 0, "dictionary")?;
        write_cell(sheet, r, 1, &about.dictionary)?;
        r += 1;
        write_cell(sheet, r, 0, "revision")?;
        sheet.write_number(r, 1, about.revision as f64)?;
        r += 1;
        write_cell(sheet, r, 0, "exported")?;
        write_cell(sheet, r, 1, exported_at)?;
        r += 1;
        write_cell(sheet, r, 0, "scan_scope")?;
        write_cell(sheet, r, 1, &about.scan_scope)?;
        r += 1;
        write_cell(sheet, r, 0, "filters")?;
        write_cell(sheet, r, 1, &about.filters)?;
        r += 1;
        write_cell(sheet, r, 0, "exact_matches")?;
        sheet.write_number(r, 1, about.exact_matches as f64)?;
        r += 1;
        write_cell(sheet, r, 0, "exported_rows")?;
        sheet.write_number(r, 1, about.exported_rows as f64)?;
        r += 1;
        write_cell(sheet, r, 0, "rows_truncated")?;
        write_cell(
            sheet,
            r,
            1,
            if about.exact_matches > about.exported_rows {
                "true"
            } else {
                "false"
            },
        )?;
        r += 1;

        // pattern source, preserved across continuation rows (never destroyed)
        let chars: Vec<char> = about.pattern_source.chars().collect();
        if chars.is_empty() {
            write_cell(sheet, r, 0, "pattern")?;
            write_cell(sheet, r, 1, "")?;
            r += 1;
        } else {
            for (i, chunk) in chars.chunks(EXCEL_CELL_LIMIT).enumerate() {
                let key = if i == 0 { "pattern" } else { "pattern (cont.)" };
                let value: String = chunk.iter().collect();
                write_cell(sheet, r, 0, key)?;
                write_cell(sheet, r, 1, &value)?;
                r += 1;
            }
        }

        // per-pattern breakdown
        r += 1;
        for (i, h) in ["pattern", "sheet", "exact_matches", "exported_rows"]
            .iter()
            .enumerate()
        {
            sheet.write_string_with_format(
                r,
                i as u16,
                truncate_cell(h, EXCEL_CELL_LIMIT),
                &bold,
            )?;
        }
        r += 1;
        for p in patterns {
            let sheet_name = represented
                .iter()
                .position(|rp| std::ptr::eq(*rp, p))
                .map_or("—".to_string(), |idx| sheet_names[idx].clone());
            write_cell(sheet, r, 0, &p.name)?;
            write_cell(sheet, r, 1, &sheet_name)?;
            sheet.write_number(r, 2, p.exact_matches as f64)?;
            sheet.write_number(r, 3, p.rows.len() as f64)?;
            r += 1;
        }
    }

    // ----- per-pattern sheets -----
    for (p, name) in represented.iter().zip(&sheet_names) {
        let sheet = workbook.add_worksheet();
        sheet.set_name(name)?;
        for (c, h) in ["sentence", "left", "key", "right", "sections"]
            .iter()
            .enumerate()
        {
            sheet.write_string_with_format(
                0,
                c as u16,
                truncate_cell(h, EXCEL_CELL_LIMIT),
                &bold,
            )?;
        }
        sheet.set_freeze_panes(1, 0)?;
        for (i, row) in p.rows.iter().enumerate() {
            let rr = (i + 1) as u32;
            sheet.write_number(rr, 0, row.sentence_index as f64)?;
            write_cell(sheet, rr, 1, &row.left)?;
            write_cell(sheet, rr, 2, &row.key)?;
            write_cell(sheet, rr, 3, &row.right)?;
            write_cell(
                sheet,
                rr,
                4,
                sections.get(&row.sentence_index).map_or("", String::as_str),
            )?;
        }
    }

    workbook.save_to_buffer()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::sink::{ExportRowRaw, PatternScan};
    use calamine::{Data, Reader, Xlsx};
    use std::collections::HashMap;
    use std::io::Cursor;

    fn about() -> AboutMeta {
        AboutMeta {
            scan_scope: "complete unfiltered corpus".into(),
            filters: "none".into(),
            corpus: "science-corpus".into(),
            corpus_id: "a3f9c2e10b74".into(),
            dictionary: "unidic-cwj-202512".into(),
            revision: 7,
            exact_matches: 3,
            exported_rows: 2,
            pattern_source: "verb := [upos=VERB].".into(),
        }
    }
    fn open(bytes: Vec<u8>) -> Xlsx<Cursor<Vec<u8>>> {
        Xlsx::new(Cursor::new(bytes)).unwrap()
    }

    #[test]
    fn writes_about_and_pattern_sheets() {
        let patterns = vec![PatternScan {
            name: "verb".into(),
            exact_matches: 3,
            rows: vec![
                ExportRowRaw {
                    sentence_index: 142,
                    left: "だと".into(),
                    key: "思った".into(),
                    right: "のは".into(),
                },
                ExportRowRaw {
                    sentence_index: 207,
                    left: "すぐに".into(),
                    key: "走った".into(),
                    right: "。".into(),
                },
            ],
        }];
        let mut sections = HashMap::new();
        sections.insert(142u64, "methods".to_string());
        sections.insert(207u64, "results".to_string());
        let bytes = build_workbook(&about(), &patterns, &sections, "2026-07-23T14:20:31Z").unwrap();
        let mut wb = open(bytes);
        assert_eq!(
            wb.sheet_names(),
            vec!["About".to_string(), "verb".to_string()]
        );
        let verb = wb.worksheet_range("verb").unwrap();
        assert_eq!(
            verb.get_value((0, 0)),
            Some(&Data::String("sentence".into()))
        );
        assert_eq!(verb.get_value((1, 0)), Some(&Data::Float(142.0)));
        assert_eq!(verb.get_value((1, 2)), Some(&Data::String("思った".into())));
        assert_eq!(
            verb.get_value((1, 4)),
            Some(&Data::String("methods".into()))
        );
        let about_sheet = wb.worksheet_range("About").unwrap();
        assert!(
            about_sheet
                .rows()
                .any(|r| r.contains(&Data::String("a3f9c2e10b74".into())))
        );
        assert!(about_sheet.rows().any(|r| r.contains(&Data::Float(3.0))));
        assert!(
            about_sheet
                .rows()
                .any(|r| r.contains(&Data::String("complete unfiltered corpus".into())))
        );
    }

    #[test]
    fn every_cell_stays_within_excel_limit_and_source_is_preserved_in_continuations() {
        let big = "字".repeat(80_000);
        let mut meta = about();
        meta.pattern_source = big.clone();
        let patterns = vec![PatternScan {
            name: "字".repeat(50),
            exact_matches: 1,
            rows: vec![ExportRowRaw {
                sentence_index: 1,
                left: big.clone(),
                key: "猫".into(),
                right: String::new(),
            }],
        }];
        let bytes = build_workbook(
            &meta,
            &patterns,
            &std::collections::HashMap::new(),
            "2026-07-23T14:20:31Z",
        )
        .unwrap();
        let mut wb = open(bytes);
        for name in wb.sheet_names().clone() {
            for row in wb.worksheet_range(&name).unwrap().rows() {
                for cell in row {
                    if let Data::String(s) = cell {
                        assert!(
                            s.chars().count() <= 32_767,
                            "cell over Excel limit: {}",
                            s.chars().count()
                        );
                    }
                }
            }
        }
        let about_sheet = wb.worksheet_range("About").unwrap();
        let recovered: usize = about_sheet
            .rows()
            .filter(|r| matches!(r.first(), Some(Data::String(k)) if k.starts_with("pattern")))
            .map(|r| {
                if let Some(Data::String(v)) = r.get(1) {
                    v.chars().count()
                } else {
                    0
                }
            })
            .sum();
        assert!(
            recovered >= big.chars().count(),
            "source not fully preserved: {recovered}"
        );
    }

    #[test]
    fn zero_patterns_yields_about_only() {
        let bytes = build_workbook(
            &AboutMeta {
                exact_matches: 0,
                exported_rows: 0,
                ..about()
            },
            &[],
            &std::collections::HashMap::new(),
            "2026-07-23T14:20:31Z",
        )
        .unwrap();
        assert_eq!(open(bytes).sheet_names(), vec!["About".to_string()]);
    }

    #[test]
    fn zero_row_pattern_shown_in_breakdown_with_dash() {
        // A pattern with matches but zero exported rows (crowded out by the cap)
        // gets NO sheet, yet must still appear in the About breakdown with
        // sheet = "—" and its true exact_matches — so starvation is visible.
        let patterns = vec![
            PatternScan {
                name: "verb".into(),
                exact_matches: 1,
                rows: vec![ExportRowRaw {
                    sentence_index: 3,
                    left: String::new(),
                    key: "走った".into(),
                    right: String::new(),
                }],
            },
            PatternScan {
                name: "noun".into(),
                exact_matches: 42,
                rows: vec![],
            },
        ];
        let bytes = build_workbook(
            &about(),
            &patterns,
            &std::collections::HashMap::new(),
            "2026-07-23T14:20:31Z",
        )
        .unwrap();
        let mut wb = open(bytes);
        // Only the represented pattern gets a sheet.
        assert_eq!(
            wb.sheet_names(),
            vec!["About".to_string(), "verb".to_string()]
        );
        // The starved pattern's breakdown row: name, sheet = "—", exact = 42.
        let about_sheet = wb.worksheet_range("About").unwrap();
        let dash_row = about_sheet
            .rows()
            .find(|r| matches!(r.first(), Some(Data::String(name)) if name == "noun"))
            .expect("noun appears in the breakdown");
        assert_eq!(
            dash_row.get(1),
            Some(&Data::String("—".into())),
            "sheet cell is a dash"
        );
        assert_eq!(
            dash_row.get(2),
            Some(&Data::Float(42.0)),
            "true exact_matches retained"
        );
        assert_eq!(
            dash_row.get(3),
            Some(&Data::Float(0.0)),
            "zero exported rows"
        );
    }

    #[test]
    fn trust_boundary_no_path_in_any_cell() {
        let patterns = vec![PatternScan {
            name: "verb".into(),
            exact_matches: 1,
            rows: vec![ExportRowRaw {
                sentence_index: 1,
                left: String::new(),
                key: "猫".into(),
                right: String::new(),
            }],
        }];
        let bytes = build_workbook(
            &about(),
            &patterns,
            &std::collections::HashMap::new(),
            "2026-07-23T14:20:31Z",
        )
        .unwrap();
        let mut wb = open(bytes);
        for name in wb.sheet_names().clone() {
            for row in wb.worksheet_range(&name).unwrap().rows() {
                for cell in row {
                    if let Data::String(s) = cell {
                        assert!(!s.contains('/') && !s.contains("/tmp"), "path in cell: {s}");
                    }
                }
            }
        }
    }

    // Measurement harness (ignored, not a correctness assertion): build a
    // full-cap export so `/usr/bin/time -v` can capture the process peak RSS.
    // Run: /usr/bin/time -v cargo test -p lingrep-web --lib --release \
    //        measure_peak_rss_full_export -- --ignored --nocapture --exact
    #[test]
    #[ignore = "measurement only; see docs/PERFORMANCE_REQUIREMENTS.md"]
    fn measure_peak_rss_full_export() {
        const ROWS: usize = 100_000; // MAX_EXPORT_ROWS
        // Worst case: every KWIC field at EXPORT_KWIC_CHAR_CAP (256 chars) of
        // multibyte text — the memory ceiling the cap guarantees, far beyond
        // any real whole-sentence context.
        let left = "あ".repeat(256);
        let key = "い".repeat(256);
        let right = "う".repeat(256);
        let rows: Vec<ExportRowRaw> = (0..ROWS)
            .map(|i| ExportRowRaw {
                sentence_index: i as u64,
                left: left.clone(),
                key: key.clone(),
                right: right.clone(),
            })
            .collect();
        let mut sections = HashMap::new();
        for i in 0..ROWS as u64 {
            sections.insert(i, "methods".to_string());
        }
        let patterns = vec![PatternScan {
            name: "verb".into(),
            exact_matches: ROWS as u64,
            rows,
        }];
        let bytes = build_workbook(&about(), &patterns, &sections, "2026-07-23T14:20:31Z").unwrap();
        eprintln!("workbook bytes: {}", bytes.len());
        assert!(!bytes.is_empty());
    }
}
