//! 查找（M11）。纯数据层：在 Worksheet 里按查询串扫描命中格。对标 cmx-megasheet 的 find.ts。
//!
//! 支持匹配值/公式源、大小写敏感、整格匹配、区域限定、正则可选。返回命中坐标列表（行优先），
//! 供高亮/轮转/定位与替换命令消费。纯逻辑、零 DOM。替换走既有 SnapshotEdit（edit::replace_command）。

use crate::cell::CellValue;
use crate::worksheet::Worksheet;

/// 查找选项。
#[derive(Debug, Clone, Default)]
pub struct FindOptions {
    /// 大小写敏感（默认 false）。
    pub match_case: bool,
    /// 整格匹配（单元格文本须与查询完全相等；默认 false=子串包含）。
    pub whole_cell: bool,
    /// 匹配公式源而非显示值（默认 false）。
    pub search_formula: bool,
    /// 正则模式（query 作为 RegExp 源）。
    pub use_regex: bool,
    /// 限定区域（缺省=全表）。
    pub range: Option<crate::range::Range>,
}

/// 命中项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindHit {
    pub row: u32,
    pub col: u32,
    /// 命中处的显示文本。
    pub text: String,
}

/// 单元格值 → 显示文本（find/filter 共用；空→""，布尔→TRUE/FALSE）。
pub(crate) fn cell_display(v: Option<&CellValue>) -> String {
    match v {
        None => String::new(),
        Some(CellValue::Bool(b)) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Some(cv) => cv.to_text(),
    }
}

/// 在一个 sheet 中查找所有命中格。
pub fn find_all(sheet: &Worksheet, query: &str, opts: &FindOptions) -> Vec<FindHit> {
    let mut hits: Vec<FindHit> = Vec::new();
    if query.is_empty() {
        return hits;
    }
    let matcher = build_matcher(query, opts);
    sheet.for_each_cell(|_data, row, col| {
        if let Some(r) = &opts.range {
            if !r.contains_cell(row, col) {
                return;
            }
        }
        let cell_text = if opts.search_formula {
            sheet.get_formula(row, col)
        } else {
            cell_display(sheet.get_value(row, col).as_ref())
        };
        if cell_text.is_empty() {
            return;
        }
        if matcher(&cell_text) {
            hits.push(FindHit {
                row,
                col,
                text: cell_text,
            });
        }
    });
    hits.sort_by(|a, b| a.row.cmp(&b.row).then(a.col.cmp(&b.col)));
    hits
}

type Matcher = Box<dyn Fn(&str) -> bool>;

fn build_matcher(query: &str, opts: &FindOptions) -> Matcher {
    if opts.use_regex {
        let flags = if opts.match_case { "" } else { "(?i)" };
        let src = if opts.whole_cell {
            format!("{flags}^(?:{query})$")
        } else {
            format!("{flags}{query}")
        };
        if let Ok(re) = regex::Regex::new(&src) {
            return Box::new(move |t: &str| re.is_match(t));
        }
        // 非法正则 → 退化字面
    }
    literal_matcher(query, opts)
}

fn literal_matcher(query: &str, opts: &FindOptions) -> Matcher {
    let q = if opts.match_case {
        query.to_string()
    } else {
        query.to_lowercase()
    };
    let match_case = opts.match_case;
    let whole = opts.whole_cell;
    Box::new(move |text: &str| {
        let t = if match_case {
            text.to_string()
        } else {
            text.to_lowercase()
        };
        if whole {
            t == q
        } else {
            t.contains(&q)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range::Range;
    use crate::workbook::Workbook;
    use crate::worksheet::Worksheet;

    fn sheet() -> Workbook {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("S", 20, 8));
        wb
    }

    #[test]
    fn substring_and_case() {
        let mut wb = sheet();
        let ws = wb.sheet_mut(0).unwrap();
        ws.set_value(0, 0, Some("Hello".into()));
        ws.set_value(1, 0, Some("hello world".into()));
        ws.set_value(2, 0, Some("HELLO".into()));
        assert_eq!(find_all(ws, "hello", &FindOptions::default()).len(), 3);
        assert_eq!(
            find_all(
                ws,
                "hello",
                &FindOptions {
                    match_case: true,
                    ..Default::default()
                }
            )
            .len(),
            1
        );
    }

    #[test]
    fn whole_cell() {
        let mut wb = sheet();
        let ws = wb.sheet_mut(0).unwrap();
        ws.set_value(0, 0, Some("cat".into()));
        ws.set_value(1, 0, Some("category".into()));
        assert_eq!(
            find_all(
                ws,
                "cat",
                &FindOptions {
                    whole_cell: true,
                    ..Default::default()
                }
            )
            .len(),
            1
        );
        assert_eq!(find_all(ws, "cat", &FindOptions::default()).len(), 2);
    }

    #[test]
    fn range_scoped() {
        let mut wb = sheet();
        let ws = wb.sheet_mut(0).unwrap();
        ws.set_value(0, 0, Some("x".into()));
        ws.set_value(5, 5, Some("x".into()));
        let opts = FindOptions {
            range: Some(Range::new(0, 0, 3, 3)),
            ..Default::default()
        };
        assert_eq!(find_all(ws, "x", &opts).len(), 1);
    }

    #[test]
    fn match_formula_source() {
        let mut wb = sheet();
        let ws = wb.sheet_mut(0).unwrap();
        ws.set_formula(0, 0, "SUM(A1:A2)");
        assert_eq!(
            find_all(
                ws,
                "SUM",
                &FindOptions {
                    search_formula: true,
                    ..Default::default()
                }
            )
            .len(),
            1
        );
        assert_eq!(find_all(ws, "SUM", &FindOptions::default()).len(), 0);
    }

    #[test]
    fn regex() {
        let mut wb = sheet();
        let ws = wb.sheet_mut(0).unwrap();
        ws.set_value(0, 0, Some("abc123".into()));
        ws.set_value(1, 0, Some("xyz".into()));
        assert_eq!(
            find_all(
                ws,
                r"\d+",
                &FindOptions {
                    use_regex: true,
                    ..Default::default()
                }
            )
            .len(),
            1
        );
    }
}
