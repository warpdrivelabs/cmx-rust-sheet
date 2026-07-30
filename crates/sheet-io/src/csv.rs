//! csv —— CSV 导入导出（M26）。RFC-4180 式引号转义，分隔符可参数化（, / ; / tab）。
//!
//! 对标 cmx-megasheet `io/csv.ts`：与 Clipboard 的 TSV 同款状态机，此处把分隔符抽成参数并补
//! CSV 特有的「含分隔符/引号/换行则包引号」+ 可选 BOM（Excel 中文兼容）。纯逻辑、零渲染。
//!
//! 分工：serialize 取显示值（公式取计算值语义由取值方决定，此处直读 `get_value`）；parse 出
//! 二维文本，落格（数字串是否转数）由调用方决定 —— 对齐 TS 的 `element.importCsv`。

use sheet_core::cell::CellValue;
use sheet_core::range::Range;
use sheet_core::worksheet::Worksheet;

/// CSV 序列化选项。
#[derive(Debug, Clone)]
pub struct CsvSerializeOptions {
    /// 字段分隔符，默认 ','。
    pub delimiter: String,
    /// 行结束符，默认 '\n'（Excel 兼容可用 '\r\n'）。
    pub eol: String,
    /// 是否加 UTF-8 BOM（Excel 打开中文 CSV 不乱码），默认 false。
    pub bom: bool,
}

impl Default for CsvSerializeOptions {
    fn default() -> Self {
        CsvSerializeOptions {
            delimiter: ",".to_string(),
            eol: "\n".to_string(),
            bom: false,
        }
    }
}

/// CSV 解析选项。
#[derive(Debug, Clone)]
pub struct CsvParseOptions {
    /// 字段分隔符，默认 ','（多字符时取首字符，对齐 TS `[0] ?? ','`）。
    pub delimiter: String,
}

impl Default for CsvParseOptions {
    fn default() -> Self {
        CsvParseOptions {
            delimiter: ",".to_string(),
        }
    }
}

/// 单元格值 → CSV 字段（含分隔符/引号/换行则包引号并 " → ""）。
fn csv_cell(v: Option<CellValue>, delimiter: &str) -> String {
    // to_text(): Bool→TRUE/FALSE，Number→JS 式数字串，Text→原串，与 TS csvCell 一致。
    let s = match v {
        None => return String::new(),
        Some(cv) => cv.to_text(),
    };
    let needs_quote =
        (!delimiter.is_empty() && s.contains(delimiter)) || s.contains(['"', '\n', '\r']);
    if needs_quote {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// 序列化选区为 CSV。取显示值（`get_value`：公式格取其缓存计算值）。
pub fn serialize_csv(sheet: &Worksheet, range: &Range, opts: &CsvSerializeOptions) -> String {
    let mut lines: Vec<String> = Vec::new();
    for r in range.row..range.row + range.row_count {
        let mut cells: Vec<String> = Vec::new();
        for c in range.col..range.col + range.col_count {
            cells.push(csv_cell(sheet.get_value(r, c), &opts.delimiter));
        }
        lines.push(cells.join(&opts.delimiter));
    }
    let body = lines.join(&opts.eol);
    if opts.bom {
        format!("\u{FEFF}{body}")
    } else {
        body
    }
}

/// 解析 CSV 文本 → 二维字符串（引号内可含分隔符/换行/双引号转义）。
/// 剥前导 BOM；\r\n 与 \n 均作换行。空文本 → []。
pub fn parse_csv(text: &str, opts: &CsvParseOptions) -> Vec<Vec<String>> {
    let delimiter = opts.delimiter.chars().next().unwrap_or(',');
    let src = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    let chars: Vec<char> = src.chars().collect();
    let n = chars.len();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut i = 0usize;
    while i < n {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' {
                if i + 1 < n && chars[i + 1] == '"' {
                    field.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
                continue;
            }
            field.push(ch);
            i += 1;
            continue;
        }
        if ch == '"' {
            in_quotes = true;
            i += 1;
            continue;
        }
        if ch == delimiter {
            row.push(std::mem::take(&mut field));
            i += 1;
            continue;
        }
        if ch == '\r' {
            i += 1;
            continue;
        }
        if ch == '\n' {
            row.push(std::mem::take(&mut field));
            rows.push(std::mem::take(&mut row));
            i += 1;
            continue;
        }
        field.push(ch);
        i += 1;
    }
    // 末尾字段/行（避免末尾换行产生空行）。
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet() -> Worksheet {
        Worksheet::with_size("S", 20, 10)
    }

    // ── parse ───────────────────────────────────────────────
    #[test]
    fn parse_basic_rows_cols() {
        assert_eq!(
            parse_csv("a,b,c\n1,2,3", &CsvParseOptions::default()),
            vec![vec!["a", "b", "c"], vec!["1", "2", "3"],]
        );
    }
    #[test]
    fn parse_quoted_comma() {
        assert_eq!(
            parse_csv("1,\"x,y\",3", &CsvParseOptions::default()),
            vec![vec!["1", "x,y", "3"]]
        );
    }
    #[test]
    fn parse_quoted_newline() {
        assert_eq!(
            parse_csv("\"line\nbreak\",5", &CsvParseOptions::default()),
            vec![vec!["line\nbreak", "5"]]
        );
    }
    #[test]
    fn parse_escaped_quote() {
        assert_eq!(
            parse_csv("\"he\"\"llo\",2", &CsvParseOptions::default()),
            vec![vec!["he\"llo", "2"]]
        );
    }
    #[test]
    fn parse_crlf_and_cr_skipped() {
        assert_eq!(
            parse_csv("a,b\r\nc,d", &CsvParseOptions::default()),
            vec![vec!["a", "b"], vec!["c", "d"]]
        );
    }
    #[test]
    fn parse_strips_bom() {
        assert_eq!(
            parse_csv("\u{FEFF}a,b", &CsvParseOptions::default()),
            vec![vec!["a", "b"]]
        );
    }
    #[test]
    fn parse_semicolon_delimiter() {
        assert_eq!(
            parse_csv(
                "a;b;c",
                &CsvParseOptions {
                    delimiter: ";".to_string()
                }
            ),
            vec![vec!["a", "b", "c"]]
        );
    }
    #[test]
    fn parse_empty_is_empty() {
        let out = parse_csv("", &CsvParseOptions::default());
        assert!(out.is_empty());
    }
    #[test]
    fn parse_trailing_newline_no_empty_row() {
        assert_eq!(
            parse_csv("a,b\n", &CsvParseOptions::default()),
            vec![vec!["a", "b"]]
        );
    }

    // ── serialize ───────────────────────────────────────────
    #[test]
    fn serialize_quotes_special_chars() {
        let mut s = sheet();
        s.set_value(0, 0, Some(CellValue::from("p,q")));
        s.set_value(0, 1, Some(CellValue::from(42.0)));
        s.set_value(0, 2, Some(CellValue::from("he\"llo")));
        assert_eq!(
            serialize_csv(&s, &Range::new(0, 0, 1, 3), &CsvSerializeOptions::default()),
            "\"p,q\",42,\"he\"\"llo\""
        );
    }
    #[test]
    fn serialize_plain_no_quotes() {
        let mut s = sheet();
        s.set_value(0, 0, Some(CellValue::from("abc")));
        s.set_value(0, 1, Some(CellValue::from(1.5)));
        assert_eq!(
            serialize_csv(&s, &Range::new(0, 0, 1, 2), &CsvSerializeOptions::default()),
            "abc,1.5"
        );
    }
    #[test]
    fn serialize_bool_true_false() {
        let mut s = sheet();
        s.set_value(0, 0, Some(CellValue::from(true)));
        s.set_value(0, 1, Some(CellValue::from(false)));
        assert_eq!(
            serialize_csv(&s, &Range::new(0, 0, 1, 2), &CsvSerializeOptions::default()),
            "TRUE,FALSE"
        );
    }
    #[test]
    fn serialize_blank_empty_field() {
        let mut s = sheet();
        s.set_value(0, 0, Some(CellValue::from("x")));
        assert_eq!(
            serialize_csv(&s, &Range::new(0, 0, 1, 2), &CsvSerializeOptions::default()),
            "x,"
        );
    }
    #[test]
    fn serialize_semicolon_crlf() {
        let mut s = sheet();
        s.set_value(0, 0, Some(CellValue::from("a")));
        s.set_value(0, 1, Some(CellValue::from("b")));
        s.set_value(1, 0, Some(CellValue::from("c")));
        s.set_value(1, 1, Some(CellValue::from("d")));
        assert_eq!(
            serialize_csv(
                &s,
                &Range::new(0, 0, 2, 2),
                &CsvSerializeOptions {
                    delimiter: ";".to_string(),
                    eol: "\r\n".to_string(),
                    bom: false,
                }
            ),
            "a;b\r\nc;d"
        );
    }
    #[test]
    fn serialize_bom_prefix() {
        let mut s = sheet();
        s.set_value(0, 0, Some(CellValue::from("中")));
        let out = serialize_csv(
            &s,
            &Range::new(0, 0, 1, 1),
            &CsvSerializeOptions {
                bom: true,
                ..Default::default()
            },
        );
        assert_eq!(out.chars().next(), Some('\u{FEFF}'));
    }
    #[test]
    fn serialize_parse_round_trip() {
        let mut s = sheet();
        let vals = ["plain", "a,b", "q\"q", "l\nn"];
        for (i, v) in vals.iter().enumerate() {
            s.set_value(0, i as u32, Some(CellValue::from(*v)));
        }
        let csv = serialize_csv(&s, &Range::new(0, 0, 1, 4), &CsvSerializeOptions::default());
        assert_eq!(parse_csv(&csv, &CsvParseOptions::default())[0], vals);
    }
}
