//! 内部剪贴板模型（复制/剪切/粘贴）+ 系统剪贴板互通（TSV/HTML）。
//!
//! 对标 cmx-megasheet 的 Clipboard.ts。内部剪贴板独立于系统剪贴板：复制/剪切把一个
//! 矩形区域的单元格数据（值+公式+样式）快照下来；粘贴以目标活动格为左上锚写入，作为
//! 可撤销命令执行。剪切在**粘贴时**清空源区（Excel 语义：剪切后源保持显示直到粘贴）。
//!
//! Rust 移植取舍：TS 的粘贴命令闭包捕获 `this`（剪贴板）与 `sheet`；这里粘贴命令是
//! [`crate::edit::WorkbookEdit`]，携带自身负载 + 目标 sheet 索引，apply/revert 时传入
//! `&mut Workbook`。剪切「粘一次即失效」在 `create_paste_command` 里消费 payload 实现。

use crate::cell::{CellData, CellValue};
use crate::edit::{restore_snapshot, snapshot_region, write_cell_data, CellSnapshot, WorkbookEdit};
use crate::formula_ref::translate_formula;
use crate::range::Range;
use crate::workbook::Workbook;
use crate::worksheet::Worksheet;

/// 剪贴格：相对左上角偏移 + 数据。
#[derive(Debug, Clone)]
struct ClipCell {
    dr: u32,
    dc: u32,
    data: Option<CellData>,
}

/// 剪贴板负载。
#[derive(Debug, Clone)]
struct ClipboardPayload {
    row_count: u32,
    col_count: u32,
    cells: Vec<ClipCell>,
    is_cut: bool,
    source: Range,
}

/// 粘贴命令结果：可撤销命令 + 落区（供调用方更新选区）。
pub struct PasteResult {
    pub command: Box<dyn WorkbookEdit>,
    pub pasted_range: Range,
}

/// 选择性粘贴内容（M18）：全部 / 仅值 / 值+公式 / 仅格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasteContent {
    #[default]
    All,
    Values,
    Formulas,
    Formats,
}

/// 选择性粘贴运算（M18）：与目标现值做算术。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PasteOperation {
    #[default]
    None,
    Add,
    Subtract,
    Multiply,
    Divide,
}

/// 选择性粘贴选项（M18）。
#[derive(Debug, Clone, Copy, Default)]
pub struct PasteSpecialOptions {
    pub content: PasteContent,
    pub operation: PasteOperation,
    pub transpose: bool,
    pub skip_blanks: bool,
}

/// 内部剪贴板。
#[derive(Default)]
pub struct Clipboard {
    payload: Option<ClipboardPayload>,
}

impl Clipboard {
    pub fn new() -> Self {
        Clipboard::default()
    }

    /// 是否有可粘贴内容。
    pub fn has_content(&self) -> bool {
        self.payload.is_some()
    }

    /// 剪贴板尺寸（无内容返回 None）。
    pub fn size(&self) -> Option<(u32, u32)> {
        self.payload.as_ref().map(|p| (p.row_count, p.col_count))
    }

    /// 复制区域（快照，不改源）。
    pub fn copy(&mut self, sheet: &Worksheet, range: Range) {
        self.payload = Some(capture(sheet, range, false));
    }

    /// 剪切区域（快照 + 标记 cut，源在粘贴时清空）。
    pub fn cut(&mut self, sheet: &Worksheet, range: Range) {
        self.payload = Some(capture(sheet, range, true));
    }

    /// 清空剪贴板。
    pub fn clear(&mut self) {
        self.payload = None;
    }

    /// 生成粘贴命令（可撤销）。以 (target_row,target_col) 为左上锚写入剪贴板内容。
    /// 剪切模式下粘贴时清空源区，且剪切「粘一次即失效」（消费 payload）。无内容返回 None。
    pub fn create_paste_command(
        &mut self,
        target: usize,
        target_row: u32,
        target_col: u32,
    ) -> Option<PasteResult> {
        let p = self.payload.as_ref()?;
        let pasted_range = Range::new(target_row, target_col, p.row_count, p.col_count);
        let was_cut = p.is_cut;
        // 复制粘贴平移相对引用（Excel 语义）；剪切粘贴公式原样搬（引用不平移）。
        let d_row = if was_cut {
            0
        } else {
            target_row as i64 - p.source.row as i64
        };
        let d_col = if was_cut {
            0
        } else {
            target_col as i64 - p.source.col as i64
        };
        let command = Box::new(PasteEdit {
            target,
            cells: p.cells.clone(),
            was_cut,
            source: p.source,
            pasted_range,
            target_row,
            target_col,
            d_row,
            d_col,
            before: None,
        });
        // 剪切一旦生成粘贴命令即失效（不可重复粘贴清源）
        if was_cut {
            self.payload = None;
        }
        Some(PasteResult {
            command,
            pasted_range,
        })
    }

    /// 生成选择性粘贴命令（M18，可撤销）。相较普通粘贴支持 content/operation/transpose/skipBlanks。
    /// 不清剪切源（Paste Special 不消费剪切），也不平移公式。无内容返回 None。
    pub fn create_paste_special_command(
        &self,
        target: usize,
        target_row: u32,
        target_col: u32,
        options: PasteSpecialOptions,
    ) -> Option<PasteResult> {
        let p = self.payload.as_ref()?;
        let out_rows = if options.transpose {
            p.col_count
        } else {
            p.row_count
        };
        let out_cols = if options.transpose {
            p.row_count
        } else {
            p.col_count
        };
        let pasted_range = Range::new(target_row, target_col, out_rows, out_cols);
        let command = Box::new(PasteSpecialEdit {
            target,
            cells: p.cells.clone(),
            options,
            pasted_range,
            target_row,
            target_col,
            before: None,
        });
        Some(PasteResult {
            command,
            pasted_range,
        })
    }
}

/// 复制/剪切：快照区域为负载。
fn capture(sheet: &Worksheet, range: Range, is_cut: bool) -> ClipboardPayload {
    let mut cells = Vec::with_capacity(range.area() as usize);
    range.for_each_cell(|row, col| {
        cells.push(ClipCell {
            dr: row - range.row,
            dc: col - range.col,
            data: sheet.get_cell_data(row, col),
        });
    });
    ClipboardPayload {
        row_count: range.row_count,
        col_count: range.col_count,
        cells,
        is_cut,
        source: range,
    }
}

/// 粘贴命令：apply 写入（剪切先清源，复制平移公式），revert 恢复 before 快照。
struct PasteEdit {
    target: usize,
    cells: Vec<ClipCell>,
    was_cut: bool,
    source: Range,
    pasted_range: Range,
    target_row: u32,
    target_col: u32,
    d_row: i64,
    d_col: i64,
    before: Option<Vec<CellSnapshot>>,
}

impl WorkbookEdit for PasteEdit {
    fn label(&self) -> &str {
        "粘贴"
    }

    fn apply(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        if self.before.is_none() {
            let mut affected = vec![self.pasted_range];
            if self.was_cut {
                affected.push(self.source);
            }
            self.before = Some(snapshot_region(ws, &affected));
        }
        if self.was_cut {
            self.source
                .for_each_cell(|row, col| write_cell_data(ws, row, col, None));
        }
        for c in &self.cells {
            let shifted = shift_formula(c.data.as_ref(), self.d_row, self.d_col);
            write_cell_data(
                ws,
                self.target_row + c.dr,
                self.target_col + c.dc,
                shifted.as_ref(),
            );
        }
    }

    fn revert(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        if let Some(b) = &self.before {
            restore_snapshot(ws, b);
        }
    }
}

/// 复制粘贴时按平移量重写公式相对引用（绝对分量不动）。无公式/零平移→原样克隆。
fn shift_formula(data: Option<&CellData>, d_row: i64, d_col: i64) -> Option<CellData> {
    let d = data?;
    match &d.formula {
        Some(f) if d_row != 0 || d_col != 0 => Some(CellData {
            formula: Some(translate_formula(f, d_row, d_col)),
            ..d.clone()
        }),
        _ => Some(d.clone()),
    }
}

/// 选择性粘贴命令（M18）：apply 按 content/operation/transpose/skipBlanks 投影写入，revert 恢复 before。
struct PasteSpecialEdit {
    target: usize,
    cells: Vec<ClipCell>,
    options: PasteSpecialOptions,
    pasted_range: Range,
    target_row: u32,
    target_col: u32,
    before: Option<Vec<CellSnapshot>>,
}

impl WorkbookEdit for PasteSpecialEdit {
    fn label(&self) -> &str {
        "选择性粘贴"
    }

    fn apply(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        if self.before.is_none() {
            self.before = Some(snapshot_region(ws, &[self.pasted_range]));
        }
        for c in &self.cells {
            let (dr, dc) = if self.options.transpose {
                (c.dc, c.dr)
            } else {
                (c.dr, c.dc)
            };
            let row = self.target_row + dr;
            let col = self.target_col + dc;
            if self.options.skip_blanks && is_blank_clip(c.data.as_ref()) {
                continue;
            }
            let projected = project_cell(ws, row, col, c.data.as_ref(), self.options);
            write_cell_data(ws, row, col, projected.as_ref());
        }
    }

    fn revert(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        if let Some(b) = &self.before {
            restore_snapshot(ws, b);
        }
    }
}

/// 剪贴格是否空（无值无公式）。
fn is_blank_clip(data: Option<&CellData>) -> bool {
    match data {
        None => true,
        Some(d) => {
            let value_blank = match &d.value {
                None => true,
                Some(CellValue::Text(t)) => t.is_empty(),
                _ => false,
            };
            value_blank && d.formula.as_ref().is_none_or(|f| f.is_empty())
        }
    }
}

/// 按 content/operation 把剪贴格投影成要写入的 CellData（读目标现值做算术/纯格式）。
fn project_cell(
    sheet: &Worksheet,
    row: u32,
    col: u32,
    src: Option<&CellData>,
    opts: PasteSpecialOptions,
) -> Option<CellData> {
    let target = sheet.get_cell_data(row, col);
    let keep_value = matches!(
        opts.content,
        PasteContent::All | PasteContent::Values | PasteContent::Formulas
    );
    let keep_formula = matches!(opts.content, PasteContent::All | PasteContent::Formulas);
    let keep_style = matches!(opts.content, PasteContent::All | PasteContent::Formats);

    // 纯格式：保留目标值/公式，只换样式
    if opts.content == PasteContent::Formats {
        return build_cell(
            target.as_ref().and_then(|t| t.value.clone()),
            target.as_ref().and_then(|t| t.formula.clone()),
            src.and_then(|s| s.style.clone()),
        );
    }

    let mut value = if keep_value {
        src.and_then(|s| s.value.clone())
    } else {
        target.as_ref().and_then(|t| t.value.clone())
    };
    let mut formula = if keep_formula {
        src.and_then(|s| s.formula.clone())
    } else {
        None
    };

    // 算术运算：作用于数值，运算后落纯值（公式失效）
    if opts.operation != PasteOperation::None && keep_value {
        let a = match target.as_ref().and_then(|t| t.value.as_ref()) {
            Some(CellValue::Number(n)) => *n,
            _ => 0.0,
        };
        let b = match src.and_then(|s| s.value.as_ref()) {
            Some(CellValue::Number(n)) => Some(*n),
            _ => None,
        };
        if let Some(b) = b {
            value = Some(CellValue::Number(apply_op(a, b, opts.operation)));
            formula = None;
        }
    }

    let style = if keep_style {
        src.and_then(|s| s.style.clone())
    } else {
        target.as_ref().and_then(|t| t.style.clone())
    };
    build_cell(value, formula, style)
}

fn apply_op(a: f64, b: f64, op: PasteOperation) -> f64 {
    match op {
        PasteOperation::Add => a + b,
        PasteOperation::Subtract => a - b,
        PasteOperation::Multiply => a * b,
        PasteOperation::Divide => {
            if b == 0.0 {
                a
            } else {
                a / b
            }
        }
        PasteOperation::None => b,
    }
}

/// 构建 CellData（只带有值的键）。全空返回 None（→清格）。
fn build_cell(
    value: Option<CellValue>,
    formula: Option<String>,
    style: Option<crate::style::Style>,
) -> Option<CellData> {
    let formula = formula.filter(|f| !f.is_empty());
    if value.is_none() && formula.is_none() && style.is_none() {
        return None;
    }
    Some(CellData {
        value,
        formula,
        style,
        rich: None,
    })
}

// ── M10 系统剪贴板互通（TSV / HTML）─────────────────────────

/// 序列化选区为 TSV（text/plain）：制表符分列、换行分行。值取显示值。
pub fn serialize_tsv(sheet: &Worksheet, range: Range) -> String {
    let mut lines = Vec::with_capacity(range.row_count as usize);
    for r in range.row..range.row + range.row_count {
        let mut cells = Vec::with_capacity(range.col_count as usize);
        for c in range.col..range.col + range.col_count {
            cells.push(tsv_cell(sheet.get_value(r, c).as_ref()));
        }
        lines.push(cells.join("\t"));
    }
    lines.join("\n")
}

/// 单元格值 → TSV 字段（含制表/换行/引号则用双引号包裹并转义）。
fn tsv_cell(v: Option<&crate::cell::CellValue>) -> String {
    let s = match v {
        None => return String::new(),
        Some(cv) => cv.to_text(),
    };
    if s.contains('\t') || s.contains('\n') || s.contains('"') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

/// 序列化选区为 HTML `<table>`（text/html）：带基础样式 + 合并 rowspan/colspan。
pub fn serialize_html(sheet: &Worksheet, range: Range) -> String {
    let mut rows = Vec::with_capacity(range.row_count as usize);
    for r in range.row..range.row + range.row_count {
        let mut cells = Vec::new();
        for c in range.col..range.col + range.col_count {
            // 合并区非左上格跳过（由左上格的 rowspan/colspan 覆盖）
            if let Some(span) = sheet.get_span(r, c) {
                if span.row != r || span.col != c {
                    continue;
                }
            }
            let style = sheet.get_resolved_style(r, c);
            let v = sheet.get_value(r, c);
            let mut attrs = String::new();
            if let Some(span) = sheet.get_span(r, c) {
                if span.row_count > 1 {
                    attrs.push_str(&format!(" rowspan=\"{}\"", span.row_count));
                }
                if span.col_count > 1 {
                    attrs.push_str(&format!(" colspan=\"{}\"", span.col_count));
                }
            }
            let mut css: Vec<String> = Vec::new();
            if style.bold == Some(true) {
                css.push("font-weight:bold".to_string());
            }
            if style.italic == Some(true) {
                css.push("font-style:italic".to_string());
            }
            if style.underline == Some(true) {
                css.push("text-decoration:underline".to_string());
            }
            if let Some(h) = style.h_align {
                css.push(format!("text-align:{}", h_align_css(h)));
            }
            if let Some(fc) = &style.fore_color {
                css.push(format!("color:{fc}"));
            }
            if let Some(bc) = &style.back_color {
                css.push(format!("background-color:{bc}"));
            }
            let style_attr = if css.is_empty() {
                String::new()
            } else {
                format!(" style=\"{}\"", css.join(";"))
            };
            cells.push(format!(
                "<td{attrs}{style_attr}>{}</td>",
                escape_html(&tsv_cell(v.as_ref()))
            ));
        }
        rows.push(format!("<tr>{}</tr>", cells.join("")));
    }
    format!("<table><tbody>{}</tbody></table>", rows.join(""))
}

fn h_align_css(h: crate::style::HAlign) -> &'static str {
    use crate::style::HAlign::*;
    match h {
        Left => "left",
        Center | CenterContinuous => "center",
        Right => "right",
        Fill => "left",
        Justify => "justify",
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// 解析 TSV → 二维值（\n 分行、\t 分列，支持双引号包裹字段含制表/换行）。
pub fn parse_tsv(text: &str) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut i = 0;
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
        match ch {
            '"' => {
                in_quotes = true;
                i += 1;
            }
            '\t' => {
                row.push(std::mem::take(&mut field));
                i += 1;
            }
            '\r' => {
                i += 1;
            }
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
                i += 1;
            }
            _ => {
                field.push(ch);
                i += 1;
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

/// 解析 HTML 表格（剪贴板 text/html）→ 二维文本（保结构，不还原样式）。极简正则，零 DOM。
pub fn parse_clipboard_html(html: &str) -> Vec<Vec<String>> {
    use regex::Regex;
    use std::sync::OnceLock;
    static TABLE_RE: OnceLock<Regex> = OnceLock::new();
    static TR_RE: OnceLock<Regex> = OnceLock::new();
    static TD_RE: OnceLock<Regex> = OnceLock::new();
    let table_re = TABLE_RE.get_or_init(|| Regex::new(r"(?i)<table[\s\S]*?</table>").unwrap());
    let tr_re = TR_RE.get_or_init(|| Regex::new(r"(?i)<tr[^>]*>([\s\S]*?)</tr>").unwrap());
    let td_re = TD_RE.get_or_init(|| Regex::new(r"(?i)<t[dh][^>]*>([\s\S]*?)</t[dh]>").unwrap());

    let src = table_re.find(html).map(|m| m.as_str()).unwrap_or(html);
    let mut rows: Vec<Vec<String>> = Vec::new();
    for tr in tr_re.captures_iter(src) {
        let inner = &tr[1];
        let mut cells = Vec::new();
        for td in td_re.captures_iter(inner) {
            cells.push(strip_tags(&td[1]));
        }
        if !cells.is_empty() {
            rows.push(cells);
        }
    }
    rows
}

fn strip_tags(s: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;
    static BR_RE: OnceLock<Regex> = OnceLock::new();
    static TAG_RE: OnceLock<Regex> = OnceLock::new();
    let br_re = BR_RE.get_or_init(|| Regex::new(r"(?i)<br\s*/?>").unwrap());
    let tag_re = TAG_RE.get_or_init(|| Regex::new(r"<[^>]+>").unwrap());
    let s = br_re.replace_all(s, "\n");
    let s = tag_re.replace_all(&s, "");
    s.replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::WorkbookHistory;
    use crate::style::{HAlign, Style};
    use crate::worksheet::Worksheet;

    fn setup() -> (Workbook, WorkbookHistory, Clipboard) {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("S", 20, 10));
        (wb, WorkbookHistory::new(), Clipboard::new())
    }

    // ── copy / paste ──
    #[test]
    fn copy_region_paste_new_anchor() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("A".into()));
            ws.set_style(
                0,
                0,
                Some(Style {
                    bold: Some(true),
                    ..Default::default()
                }),
            );
            ws.set_value(0, 1, Some("B".into()));
            ws.set_formula(1, 0, "=X1");
        }
        clip.copy(wb.sheet(0).unwrap(), Range::new(0, 0, 2, 2));
        let res = clip.create_paste_command(0, 5, 5).unwrap();
        let pasted = res.pasted_range;
        h.do_edit(&mut wb, res.command);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(5, 5), Some("A".into()));
        assert_eq!(
            ws.get_style(5, 5),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
        assert_eq!(ws.get_value(5, 6), Some("B".into()));
        // 复制平移：X1 从 (1,0) 粘到 (6,5)，delta=(+5,+5) → AC6
        assert_eq!(ws.get_formula(6, 5), "AC6");
        assert_eq!(pasted.to_a1(), "F6:G7");
    }

    #[test]
    fn absolute_refs_pinned_on_paste() {
        let (mut wb, mut h, mut clip) = setup();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "=$B$2+C3");
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip.create_paste_command(0, 2, 2).unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_formula(2, 2), "$B$2+E5");
    }

    #[test]
    fn copy_leaves_source_intact() {
        let (mut wb, mut h, mut clip) = setup();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 0, Some("keep".into()));
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip.create_paste_command(0, 3, 3).unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("keep".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(3, 3), Some("keep".into()));
    }

    #[test]
    fn paste_is_undoable() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("v".into()));
            ws.set_value(5, 5, Some("old".into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip.create_paste_command(0, 5, 5).unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), Some("v".into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), Some("old".into()));
    }

    // ── cut ──
    #[test]
    fn cut_clears_source_on_paste() {
        let (mut wb, mut h, mut clip) = setup();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 0, Some("move".into()));
        clip.cut(wb.sheet(0).unwrap(), Range::cell(0, 0));
        // 源在粘贴前仍显示
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("move".into()));
        let res = clip.create_paste_command(0, 5, 5).unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), Some("move".into()));
    }

    #[test]
    fn cut_paste_undoable_restores_both() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("move".into()));
            ws.set_value(5, 5, Some("dest-old".into()));
        }
        clip.cut(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip.create_paste_command(0, 5, 5).unwrap();
        h.do_edit(&mut wb, res.command);
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("move".into()));
        assert_eq!(
            wb.sheet(0).unwrap().get_value(5, 5),
            Some("dest-old".into())
        );
    }

    #[test]
    fn cut_pasted_only_once() {
        let (mut wb, mut h, mut clip) = setup();
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some("x".into()));
        clip.cut(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip.create_paste_command(0, 5, 5).unwrap();
        h.do_edit(&mut wb, res.command);
        assert!(!clip.has_content());
        assert!(clip.create_paste_command(0, 7, 7).is_none());
    }

    // ── bounds & state ──
    #[test]
    fn reports_size_and_content_flags() {
        let (mut wb, _h, mut clip) = setup();
        assert!(!clip.has_content());
        assert_eq!(clip.size(), None);
        clip.copy(wb.sheet_mut(0).unwrap(), Range::new(0, 0, 2, 3));
        assert!(clip.has_content());
        assert_eq!(clip.size(), Some((2, 3)));
    }

    #[test]
    fn paste_clips_at_grid_edges() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("a".into()));
            ws.set_value(0, 1, Some("b".into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::new(0, 0, 1, 2));
        // 锚在末列，第二格落界外被静默跳过
        let res = clip.create_paste_command(0, 0, 9).unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 9), Some("a".into()));
    }

    #[test]
    fn clear_empties_clipboard() {
        let (mut wb, _h, mut clip) = setup();
        clip.copy(wb.sheet_mut(0).unwrap(), Range::cell(0, 0));
        clip.clear();
        assert!(!clip.has_content());
    }

    // ── TSV / HTML ──
    #[test]
    fn serialize_tsv_value_grid() {
        let (mut wb, _h, _c) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(1.into()));
            ws.set_value(0, 1, Some(2.into()));
            ws.set_value(1, 0, Some("a".into()));
            ws.set_value(1, 1, Some("b".into()));
        }
        assert_eq!(
            serialize_tsv(wb.sheet(0).unwrap(), Range::new(0, 0, 2, 2)),
            "1\t2\na\tb"
        );
    }

    #[test]
    fn serialize_tsv_escapes_tab_newline() {
        let (mut wb, _h, _c) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("a\tb".into()));
            ws.set_value(0, 1, Some("line1\nline2".into()));
        }
        let tsv = serialize_tsv(wb.sheet(0).unwrap(), Range::new(0, 0, 1, 2));
        assert!(tsv.contains("\"a\tb\""));
        assert!(tsv.contains("\"line1\nline2\""));
    }

    #[test]
    fn serialize_html_with_style_and_span() {
        let (mut wb, _h, _c) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("X".into()));
            ws.set_style(
                0,
                0,
                Some(Style {
                    bold: Some(true),
                    back_color: Some("#ff0000".into()),
                    ..Default::default()
                }),
            );
            ws.add_span(0, 0, 1, 2);
        }
        let html = serialize_html(wb.sheet(0).unwrap(), Range::new(0, 0, 1, 2));
        assert!(html.contains("<table>"));
        assert!(html.contains("font-weight:bold"));
        assert!(html.contains("colspan=\"2\""));
    }

    #[test]
    fn parse_tsv_round_trip() {
        assert_eq!(
            parse_tsv("1\t2\na\tb"),
            vec![vec!["1", "2"], vec!["a", "b"]]
        );
        assert_eq!(parse_tsv("\"a\tb\"\tc"), vec![vec!["a\tb", "c"]]);
    }

    #[test]
    fn parse_clipboard_html_table() {
        let html = "<table><tbody><tr><td>1</td><td>2</td></tr><tr><td>a</td><td>b</td></tr></tbody></table>";
        assert_eq!(
            parse_clipboard_html(html),
            vec![vec!["1", "2"], vec!["a", "b"]]
        );
    }

    #[test]
    fn serialize_parse_round_trip() {
        let (mut wb, _h, _c) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_value(0, 1, Some(20.into()));
            ws.set_value(1, 0, Some(30.into()));
            ws.set_value(1, 1, Some(40.into()));
        }
        let grid = parse_tsv(&serialize_tsv(wb.sheet(0).unwrap(), Range::new(0, 0, 2, 2)));
        assert_eq!(grid, vec![vec!["10", "20"], vec!["30", "40"]]);
    }

    #[test]
    fn html_align_css_maps() {
        assert_eq!(h_align_css(HAlign::Center), "center");
        assert_eq!(h_align_css(HAlign::Right), "right");
    }

    // ── M18 选择性粘贴 ──
    fn num_val(wb: &Workbook, r: u32, c: u32) -> Option<f64> {
        wb.sheet(0)
            .unwrap()
            .get_value(r, c)
            .and_then(|v| v.as_number())
    }

    #[test]
    fn paste_special_values_drops_formula() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_formula(0, 0, "=6*7");
            ws.set_computed_value(0, 0, Some(42.into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip
            .create_paste_special_command(
                0,
                5,
                5,
                PasteSpecialOptions {
                    content: PasteContent::Values,
                    ..Default::default()
                },
            )
            .unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(num_val(&wb, 5, 5), Some(42.0));
        assert!(wb
            .sheet(0)
            .unwrap()
            .get_cell_data(5, 5)
            .unwrap()
            .formula
            .is_none());
    }

    #[test]
    fn paste_special_formulas_keeps_formula() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_formula(0, 0, "=6*7");
            ws.set_computed_value(0, 0, Some(42.into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip
            .create_paste_special_command(
                0,
                5,
                5,
                PasteSpecialOptions {
                    content: PasteContent::Formulas,
                    ..Default::default()
                },
            )
            .unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(
            wb.sheet(0)
                .unwrap()
                .get_cell_data(5, 5)
                .unwrap()
                .formula
                .as_deref(),
            Some("6*7")
        );
    }

    #[test]
    fn paste_special_formats_keeps_target_value() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("SRC".into()));
            ws.set_style(
                0,
                0,
                Some(Style {
                    bold: Some(true),
                    back_color: Some("#ff0000".into()),
                    ..Default::default()
                }),
            );
            ws.set_value(5, 5, Some("KEEP".into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip
            .create_paste_special_command(
                0,
                5,
                5,
                PasteSpecialOptions {
                    content: PasteContent::Formats,
                    ..Default::default()
                },
            )
            .unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), Some("KEEP".into()));
        let st = wb.sheet(0).unwrap().get_style(5, 5).unwrap();
        assert_eq!(st.bold, Some(true));
        assert_eq!(st.back_color.as_deref(), Some("#ff0000"));
    }

    #[test]
    fn paste_special_transpose() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            let vals = [["a", "b", "c"], ["d", "e", "f"]];
            for (r, row) in vals.iter().enumerate() {
                for (c, v) in row.iter().enumerate() {
                    ws.set_value(r as u32, c as u32, Some((*v).into()));
                }
            }
        }
        clip.copy(wb.sheet(0).unwrap(), Range::new(0, 0, 2, 3));
        let res = clip
            .create_paste_special_command(
                0,
                10,
                0,
                PasteSpecialOptions {
                    transpose: true,
                    ..Default::default()
                },
            )
            .unwrap();
        let pasted = res.pasted_range;
        h.do_edit(&mut wb, res.command);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(10, 0), Some("a".into()));
        assert_eq!(ws.get_value(10, 1), Some("d".into()));
        assert_eq!(ws.get_value(11, 0), Some("b".into()));
        assert_eq!(ws.get_value(12, 1), Some("f".into()));
        assert_eq!(pasted.row_count, 3);
        assert_eq!(pasted.col_count, 2);
    }

    #[test]
    fn paste_special_add_multiply() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_value(5, 5, Some(100.into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip
            .create_paste_special_command(
                0,
                5,
                5,
                PasteSpecialOptions {
                    content: PasteContent::Values,
                    operation: PasteOperation::Add,
                    ..Default::default()
                },
            )
            .unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(num_val(&wb, 5, 5), Some(110.0));

        let (mut wb2, mut h2, mut clip2) = setup();
        {
            let ws = wb2.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(3.into()));
            ws.set_value(5, 5, Some(7.into()));
        }
        clip2.copy(wb2.sheet(0).unwrap(), Range::cell(0, 0));
        let res2 = clip2
            .create_paste_special_command(
                0,
                5,
                5,
                PasteSpecialOptions {
                    content: PasteContent::Values,
                    operation: PasteOperation::Multiply,
                    ..Default::default()
                },
            )
            .unwrap();
        h2.do_edit(&mut wb2, res2.command);
        assert_eq!(num_val(&wb2, 5, 5), Some(21.0));
    }

    #[test]
    fn paste_special_skip_blanks() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("X".into())); // (0,1) 空
            ws.set_value(5, 0, Some("keepA".into()));
            ws.set_value(5, 1, Some("keepB".into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::new(0, 0, 1, 2));
        let res = clip
            .create_paste_special_command(
                0,
                5,
                0,
                PasteSpecialOptions {
                    skip_blanks: true,
                    ..Default::default()
                },
            )
            .unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 0), Some("X".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 1), Some("keepB".into()));
    }

    #[test]
    fn paste_special_undoable_and_not_consumed() {
        let (mut wb, mut h, mut clip) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(99.into()));
            ws.set_value(5, 5, Some("orig".into()));
        }
        clip.copy(wb.sheet(0).unwrap(), Range::cell(0, 0));
        let res = clip
            .create_paste_special_command(
                0,
                5,
                5,
                PasteSpecialOptions {
                    content: PasteContent::Values,
                    ..Default::default()
                },
            )
            .unwrap();
        h.do_edit(&mut wb, res.command);
        assert_eq!(num_val(&wb, 5, 5), Some(99.0));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), Some("orig".into()));
        // 不消费剪切源：可重复
        assert!(clip
            .create_paste_special_command(0, 6, 6, PasteSpecialOptions::default())
            .is_some());
    }

    #[test]
    fn paste_special_none_when_empty() {
        let (_wb, _h, clip) = setup();
        assert!(clip
            .create_paste_special_command(0, 0, 0, PasteSpecialOptions::default())
            .is_none());
    }
}
