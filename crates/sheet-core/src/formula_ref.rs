//! 公式引用变换（词法级）：复制/粘贴平移相对引用、插入/删除行列后重写引用。
//!
//! 对标 cmx-megasheet 的 formula/refTransform.ts，但**不依赖 AST 解析器**——RS-M0/M2
//! 阶段公式引擎（RS-M3）尚未落地，这里用一个词法扫描器改写 A1 引用记号：跳过字符串
//! 字面量（`"..."`）与函数名（后接 `(`），只平移真正的单元格引用。绝对分量 `$` 不动，
//! 被删区间覆盖的引用坍缩为 `#REF!`。行为与 TS 版语义等价（RS-M3 若需可切 AST 版）。
//!
//! 这是电子表格「正确性」的地基：`=SUM(A1:A3)` 从第 1 行粘到第 5 行 → `=SUM(A5:A7)`；
//! 被引用行上方插入一行 → 引用自动 +1；删掉被引用的格 → 引用坍缩 `#REF!`。

use std::sync::OnceLock;

use regex::Regex;

use crate::address::{col_to_label, label_to_col};

/// 结构化单格引用：sheet 前缀、列/行索引（可为负→#REF!）及各自 $ 绝对标志。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructRef {
    /// sheet 前缀（不含 `!`，去引号）；同表内为空串。
    pub sheet: String,
    /// sheet 名原文是否带引号（`'My Sheet'`），序列化时保持。
    pub sheet_quoted: bool,
    pub col: i64,
    pub row: i64,
    pub col_abs: bool,
    pub row_abs: bool,
}

impl StructRef {
    /// StructRef → 引用文本（还原 $、sheet 前缀、引号）。col/row 越界(<0)时输出 `#REF!`。
    pub fn to_ref_string(&self) -> String {
        if self.col < 0 || self.row < 0 {
            return "#REF!".to_string();
        }
        let local = format!(
            "{}{}{}{}",
            if self.col_abs { "$" } else { "" },
            col_to_label(self.col as u32),
            if self.row_abs { "$" } else { "" },
            self.row + 1
        );
        if self.sheet.is_empty() {
            return local;
        }
        let name = if self.sheet_quoted {
            format!("'{}'", self.sheet.replace('\'', "''"))
        } else {
            self.sheet.clone()
        };
        format!("{name}!{local}")
    }
}

/// 单个引用记号的锚定解析正则（含可选 sheet 前缀与 $）。
fn ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:('[^']*'|[A-Za-z_][A-Za-z0-9_.]*)!)?(\$?)([A-Za-z]{1,3})(\$?)([0-9]+)$")
            .unwrap()
    })
}

/// 在文本片段里查找引用候选的扫描正则（同上，不锚定）。
fn scan_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:('[^']*'|[A-Za-z_][A-Za-z0-9_.]*)!)?(\$?)([A-Za-z]{1,3})(\$?)([0-9]+)")
            .unwrap()
    })
}

/// 解析单格引用文本 → StructRef。非法返回 None。
pub fn parse_struct_ref(s: &str) -> Option<StructRef> {
    let caps = ref_re().captures(s.trim())?;
    let (sheet, sheet_quoted) = match caps.get(1) {
        Some(m) => {
            let raw = m.as_str();
            if raw.starts_with('\'') && raw.ends_with('\'') {
                (raw[1..raw.len() - 1].replace("''", "'"), true)
            } else {
                (raw.to_string(), false)
            }
        }
        None => (String::new(), false),
    };
    let col = label_to_col(&caps[3])?;
    let row: i64 = caps[5].parse::<i64>().ok()? - 1;
    if row < 0 {
        return None;
    }
    Some(StructRef {
        sheet,
        sheet_quoted,
        col: col as i64,
        row,
        col_abs: &caps[2] == "$",
        row_abs: &caps[4] == "$",
    })
}

/// 把非字符串文本片段里的每个引用记号套 mapper 改写，其余原样。
fn rewrite_segment(seg: &str, map: &dyn Fn(StructRef) -> StructRef) -> String {
    let mut out = String::with_capacity(seg.len());
    let mut last = 0;
    for m in scan_re().find_iter(seg) {
        // 复制间隙
        out.push_str(&seg[last..m.start()]);
        let matched = m.as_str();
        // 边界校验：前一字符不得是标识符字符（避免截取更大记号的尾巴，如 SHEET1 里的 ET1）；
        // 后一字符不得是 '('（函数名）或标识符字符或 '!'（疑似 sheet 名）。
        let prev = seg[..m.start()].chars().next_back();
        let next = seg[m.end()..].chars().next();
        let prev_ok = prev.is_none_or(|c| !is_ident_char(c));
        let next_ok = next.is_none_or(|c| c != '(' && c != '!' && !is_ident_char(c));
        if prev_ok && next_ok {
            if let Some(r) = parse_struct_ref(matched) {
                out.push_str(&map(r).to_ref_string());
                last = m.end();
                continue;
            }
        }
        // 拒绝：原样复制
        out.push_str(matched);
        last = m.end();
    }
    out.push_str(&seg[last..]);
    out
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.'
}

/// 把公式串切成 (是否字符串字面量, 文本) 段；字符串段（含引号）原样保留不改写。
fn split_string_literals(f: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = f.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' {
            if !cur.is_empty() {
                out.push((false, std::mem::take(&mut cur)));
            }
            let mut s = String::from("\"");
            while let Some(c2) = chars.next() {
                if c2 == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        s.push_str("\"\"");
                        continue;
                    }
                    s.push('"');
                    break;
                }
                s.push(c2);
            }
            out.push((true, s));
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push((false, cur));
    }
    out
}

/// 剥前导 '=' 并 trim。
fn strip_eq(formula: &str) -> String {
    let f = formula.trim();
    if let Some(rest) = f.strip_prefix('=') {
        rest.trim().to_string()
    } else {
        f.to_string()
    }
}

/// 对公式串套引用改写：跳过字符串字面量，其余段改写。
fn rewrite_formula(src: &str, map: &dyn Fn(StructRef) -> StructRef) -> String {
    let mut out = String::with_capacity(src.len());
    for (is_str, seg) in split_string_literals(src) {
        if is_str {
            out.push_str(&seg);
        } else {
            out.push_str(&rewrite_segment(&seg, map));
        }
    }
    out
}

/// 平移公式中的相对引用（复制/粘贴用）。绝对分量（$col/$row）不动。
/// `d_row`/`d_col` = 目标 − 源。返回平移后公式串（不含 '='）；空/无引用原样返回。
pub fn translate_formula(formula: &str, d_row: i64, d_col: i64) -> String {
    let src = strip_eq(formula);
    if src.is_empty() || (d_row == 0 && d_col == 0) {
        return src;
    }
    rewrite_formula(&src, &|r: StructRef| StructRef {
        row: if r.row_abs { r.row } else { r.row + d_row },
        col: if r.col_abs { r.col } else { r.col + d_col },
        ..r
    })
}

/// 结构变更轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefAxis {
    Row,
    Col,
}

/// 结构变更操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefOp {
    Insert,
    Delete,
}

/// 结构编辑描述：在某轴 index 处插入/删除 count 行（或列），发生在 edit_sheet。
#[derive(Debug, Clone)]
pub struct StructuralEdit {
    pub axis: RefAxis,
    pub index: u32,
    pub count: u32,
    pub op: RefOp,
    /// 该编辑发生在哪个 sheet；仅改写指向此 sheet 的引用（无前缀引用归属 formula_sheet）。
    pub edit_sheet: String,
}

/// 计算单个索引在插入/删除后的新值（越界→-1，序列化时坍缩 #REF!）。
fn shift_index(pos: i64, edit: &StructuralEdit) -> i64 {
    let index = edit.index as i64;
    let count = edit.count as i64;
    match edit.op {
        RefOp::Insert => {
            if pos >= index {
                pos + count
            } else {
                pos
            }
        }
        RefOp::Delete => {
            if pos >= index && pos < index + count {
                -1
            } else if pos >= index + count {
                pos - count
            } else {
                pos
            }
        }
    }
}

/// 插入/删除行列后重写一条公式的引用（绝对与相对都移，Excel 语义）。
/// 只改写指向 edit_sheet 的引用；无前缀引用按 formula_sheet 归属判断。
pub fn adjust_for_structural(formula: &str, edit: &StructuralEdit, formula_sheet: &str) -> String {
    let src = strip_eq(formula);
    if src.is_empty() || edit.count == 0 {
        return src;
    }
    let formula_sheet = formula_sheet.to_string();
    rewrite_formula(&src, &move |r: StructRef| {
        let target_sheet = if r.sheet.is_empty() {
            formula_sheet.as_str()
        } else {
            r.sheet.as_str()
        };
        if target_sheet != edit.edit_sheet {
            return r; // 不指向被编辑的 sheet，不动
        }
        match edit.axis {
            RefAxis::Row => StructRef {
                row: shift_index(r.row, edit),
                ..r
            },
            RefAxis::Col => StructRef {
                col: shift_index(r.col, edit),
                ..r
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_stringify_round_trip() {
        for s in ["A1", "$A$1", "Sheet1!B2", "$B2", "C$3"] {
            let r = parse_struct_ref(s).unwrap();
            assert_eq!(r.to_ref_string(), s);
        }
    }

    #[test]
    fn translate_relative() {
        // X1 → AC6（+5,+5）
        assert_eq!(translate_formula("X1", 5, 5), "AC6");
        // A1+1 向下 → A2+1
        assert_eq!(translate_formula("A1+1", 1, 0), "A2+1");
        // A1 向右 → B1
        assert_eq!(translate_formula("A1", 0, 1), "B1");
    }

    #[test]
    fn translate_absolute_pinned() {
        // $B$2 固定；C3 相对 → E5（+2,+2）
        assert_eq!(translate_formula("$B$2+C3", 2, 2), "$B$2+E5");
    }

    #[test]
    fn translate_skips_functions_and_strings() {
        // 函数名 SUM 不动，区域两端平移
        assert_eq!(translate_formula("SUM(A1:A3)", 5, 0), "SUM(A6:A8)");
        // 字符串字面量里的 A1 不动
        assert_eq!(translate_formula("A1&\"see A1\"", 1, 0), "A2&\"see A1\"");
    }

    #[test]
    fn translate_zero_delta_noop() {
        assert_eq!(translate_formula("A1+B2", 0, 0), "A1+B2");
    }

    #[test]
    fn structural_insert_row_shifts_down() {
        let edit = StructuralEdit {
            axis: RefAxis::Row,
            index: 0,
            count: 1,
            op: RefOp::Insert,
            edit_sheet: "Sheet1".into(),
        };
        assert_eq!(
            adjust_for_structural("SUM(A2:A5)", &edit, "Sheet1"),
            "SUM(A3:A6)"
        );
    }

    #[test]
    fn structural_delete_collapses_to_ref_error() {
        let edit = StructuralEdit {
            axis: RefAxis::Row,
            index: 4,
            count: 1,
            op: RefOp::Delete,
            edit_sheet: "Sheet1".into(),
        };
        assert_eq!(adjust_for_structural("B5*2", &edit, "Sheet1"), "#REF!*2");
    }

    #[test]
    fn structural_insert_col_shifts_right() {
        let edit = StructuralEdit {
            axis: RefAxis::Col,
            index: 0,
            count: 1,
            op: RefOp::Insert,
            edit_sheet: "Sheet1".into(),
        };
        assert_eq!(adjust_for_structural("C1+D1", &edit, "Sheet1"), "D1+E1");
    }

    #[test]
    fn structural_only_edited_sheet() {
        // Sheet1!A5 指向被编辑表 → 移；裸 A5 属 formula_sheet=Sheet2 → 不动
        let edit = StructuralEdit {
            axis: RefAxis::Row,
            index: 0,
            count: 1,
            op: RefOp::Insert,
            edit_sheet: "Sheet1".into(),
        };
        assert_eq!(
            adjust_for_structural("Sheet1!A5+A5", &edit, "Sheet2"),
            "Sheet1!A6+A5"
        );
    }

    #[test]
    fn does_not_grab_larger_identifier_tail() {
        // 定义名 SHEET1（无 '!'）不应被当作 ref ET1 改写
        assert_eq!(translate_formula("SHEET1+A1", 1, 0), "SHEET1+A2");
    }
}
