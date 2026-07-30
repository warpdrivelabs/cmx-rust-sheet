//! 可撤销编辑命令（M2）+ 工作簿级撤销栈。对标 cmx-megasheet 的 EditCommands.ts。
//!
//! 设计核心是「快照命令」：一次编辑执行前，先捕获受影响单元格的完整 CellData 快照，
//! revert 时原样恢复。值/公式/样式/清除/填充/移动等编辑统一走此机制，无需各写逆操作。
//! 合并/取消、行列增删因涉及结构位移，另配专门的逆操作。
//!
//! Rust 移植取舍：TS 命令在构造期**捕获 sheet 引用**，execute/undo 无参调用。Rust 无法
//! 长期持有 `&mut`，改为**线程化**：命令携带目标 sheet 索引，apply/revert 时传入
//! `&mut Workbook` 现取现用。这样单命令能定位自身 sheet，结构命令还能遍历兄弟 sheet
//! 做跨表公式重写（对齐 adjustWorkbookFormulas）——无需 Rc/RefCell，不动 M0 绿区。

use crate::cell::{CellData, CellValue};
use crate::formula_ref::{adjust_for_structural, RefAxis, RefOp, StructuralEdit as RefEdit};
use crate::range::Range;
use crate::style::Style;
use crate::workbook::Workbook;
use crate::worksheet::{Span, Worksheet};

// ── 快照与写格助手（clipboard 复用）──────────────────────────

/// 单元格快照条目（data=None 表示当时为空格，用于精确恢复）。
#[derive(Debug, Clone)]
pub struct CellSnapshot {
    pub row: u32,
    pub col: u32,
    pub data: Option<CellData>,
}

/// 捕获若干区域内所有单元格的当前数据（去重，空格记 None）。
pub(crate) fn snapshot_region(sheet: &Worksheet, ranges: &[Range]) -> Vec<CellSnapshot> {
    let mut seen = std::collections::HashSet::new();
    let mut snaps = Vec::new();
    for r in ranges {
        r.for_each_cell(|row, col| {
            if seen.insert((row, col)) {
                snaps.push(CellSnapshot {
                    row,
                    col,
                    data: sheet.get_cell_data(row, col),
                });
            }
        });
    }
    snaps
}

/// 恢复快照（None → 清空该格）。
pub(crate) fn restore_snapshot(sheet: &mut Worksheet, snaps: &[CellSnapshot]) {
    for s in snaps {
        write_cell_data(sheet, s.row, s.col, s.data.as_ref());
    }
}

/// 写一格完整 CellData（None=清空）。越界静默跳过。EditCommands / Clipboard 共用。
pub(crate) fn write_cell_data(sheet: &mut Worksheet, row: u32, col: u32, data: Option<&CellData>) {
    if row >= sheet.row_count() || col >= sheet.column_count() {
        return;
    }
    match data {
        None => {
            sheet.set_formula(row, col, "");
            sheet.set_value(row, col, None);
            sheet.set_style(row, col, None);
        }
        Some(d) => {
            sheet.set_style(row, col, d.style.clone());
            if let Some(f) = &d.formula {
                sheet.set_formula(row, col, f);
            } else if let Some(rt) = &d.rich {
                sheet.set_rich_text(row, col, Some(rt.clone()));
            } else {
                sheet.set_formula(row, col, "");
                sheet.set_value(row, col, d.value.clone());
            }
        }
    }
}

// ── 命令抽象 + 撤销栈 ────────────────────────────────────────

/// 工作簿级可撤销编辑。apply 施加变更，revert 回滚；redo = 再次 apply（幂等重放）。
pub trait WorkbookEdit {
    fn label(&self) -> &str;
    fn apply(&mut self, wb: &mut Workbook);
    fn revert(&mut self, wb: &mut Workbook);
}

/// 工作簿撤销栈：线程化 `&mut Workbook` 驱动做/撤/重。
pub struct WorkbookHistory {
    undo: Vec<Box<dyn WorkbookEdit>>,
    redo: Vec<Box<dyn WorkbookEdit>>,
    max_size: usize,
}

impl Default for WorkbookHistory {
    fn default() -> Self {
        WorkbookHistory {
            undo: Vec::new(),
            redo: Vec::new(),
            max_size: 100,
        }
    }
}

impl WorkbookHistory {
    pub fn new() -> Self {
        WorkbookHistory::default()
    }

    pub fn max_size(&self) -> usize {
        self.max_size
    }

    pub fn set_max_size(&mut self, n: usize) {
        self.max_size = n.max(1);
        self.trim();
    }

    /// 执行并压栈（清空 redo）。
    pub fn do_edit(&mut self, wb: &mut Workbook, mut cmd: Box<dyn WorkbookEdit>) {
        cmd.apply(wb);
        self.undo.push(cmd);
        self.redo.clear();
        self.trim();
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo(&mut self, wb: &mut Workbook) -> bool {
        if let Some(mut c) = self.undo.pop() {
            c.revert(wb);
            self.redo.push(c);
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self, wb: &mut Workbook) -> bool {
        if let Some(mut c) = self.redo.pop() {
            c.apply(wb);
            self.undo.push(c);
            true
        } else {
            false
        }
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    fn trim(&mut self) {
        while self.undo.len() > self.max_size {
            self.undo.remove(0);
        }
    }
}

// ── 快照命令：一次捕获 before，apply 重放 mutator，revert 恢复 ──

type SheetMutator = Box<dyn Fn(&mut Worksheet)>;

/// 快照命令：捕获目标 sheet 一片区域的 before，apply 跑 mutator，revert 还原。
pub struct SnapshotEdit {
    label: String,
    target: usize,
    ranges: Vec<Range>,
    apply_fn: SheetMutator,
    before: Option<Vec<CellSnapshot>>,
}

impl SnapshotEdit {
    fn new(label: &str, target: usize, ranges: Vec<Range>, apply_fn: SheetMutator) -> Self {
        SnapshotEdit {
            label: label.to_string(),
            target,
            ranges,
            apply_fn,
            before: None,
        }
    }
}

impl WorkbookEdit for SnapshotEdit {
    fn label(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        if self.before.is_none() {
            self.before = Some(snapshot_region(ws, &self.ranges));
        }
        (self.apply_fn)(ws);
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

// ── 编辑命令工厂 ─────────────────────────────────────────────

/// 写单格值。
pub fn set_value_command(
    target: usize,
    row: u32,
    col: u32,
    value: Option<CellValue>,
) -> Box<dyn WorkbookEdit> {
    Box::new(SnapshotEdit::new(
        "编辑单元格",
        target,
        vec![Range::cell(row, col)],
        Box::new(move |s| s.set_value(row, col, value.clone())),
    ))
}

/// 写单格公式。
pub fn set_formula_command(
    target: usize,
    row: u32,
    col: u32,
    formula: &str,
) -> Box<dyn WorkbookEdit> {
    let f = formula.to_string();
    Box::new(SnapshotEdit::new(
        "编辑公式",
        target,
        vec![Range::cell(row, col)],
        Box::new(move |s| s.set_formula(row, col, &f)),
    ))
}

/// 对区域叠加样式（合并，非替换）。
pub fn apply_style_command(
    target: usize,
    ranges: Vec<Range>,
    patch: Style,
) -> Box<dyn WorkbookEdit> {
    let rs = ranges.clone();
    Box::new(SnapshotEdit::new(
        "设置单元格格式",
        target,
        ranges,
        Box::new(move |s| {
            for r in &rs {
                r.for_each_cell(|row, col| s.merge_cell_style(row, col, &patch));
            }
        }),
    ))
}

/// 清除模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearMode {
    All,
    Value,
    Format,
}

/// 清除区域（全部 / 仅值 / 仅格式）。
pub fn clear_command(target: usize, ranges: Vec<Range>, mode: ClearMode) -> Box<dyn WorkbookEdit> {
    let label = match mode {
        ClearMode::Format => "清除格式",
        ClearMode::Value => "清除内容",
        ClearMode::All => "清除",
    };
    let rs = ranges.clone();
    Box::new(SnapshotEdit::new(
        label,
        target,
        ranges,
        Box::new(move |s| {
            for r in &rs {
                r.for_each_cell(|row, col| {
                    if mode == ClearMode::All || mode == ClearMode::Value {
                        s.set_formula(row, col, "");
                        s.set_value(row, col, None);
                    }
                    if mode == ClearMode::All || mode == ClearMode::Format {
                        s.set_style(row, col, None);
                    }
                });
            }
        }),
    ))
}

/// 自动填充：把 filled 序列写到 target_cells（一一对应）。可撤销。
pub fn fill_command(
    target: usize,
    target_cells: &[(u32, u32)],
    filled: Vec<CellData>,
) -> Box<dyn WorkbookEdit> {
    let ranges: Vec<Range> = target_cells
        .iter()
        .map(|&(r, c)| Range::cell(r, c))
        .collect();
    let cells: Vec<(u32, u32)> = target_cells.to_vec();
    Box::new(SnapshotEdit::new(
        "填充",
        target,
        ranges,
        Box::new(move |s| {
            for (i, &(r, c)) in cells.iter().enumerate() {
                if let Some(d) = filled.get(i) {
                    write_cell_data(s, r, c, Some(d));
                }
            }
        }),
    ))
}

/// 格式刷：把 style 施加到目标区（只搬样式不搬值，可撤销）。
pub fn paste_format_command(
    target: usize,
    ranges: Vec<Range>,
    style: Option<Style>,
) -> Box<dyn WorkbookEdit> {
    let rs = ranges.clone();
    Box::new(SnapshotEdit::new(
        "格式刷",
        target,
        ranges,
        Box::new(move |s| {
            for r in &rs {
                r.for_each_cell(|row, col| s.set_style(row, col, style.clone()));
            }
        }),
    ))
}

/// 外部粘贴（TSV/HTML 解析出的二维文本）：以 (target_row,target_col) 为左上锚写入。
/// 纯文本落值（可解析数字则转 number），可撤销。
pub fn paste_external_command(
    target: usize,
    target_row: u32,
    target_col: u32,
    grid: Vec<Vec<String>>,
) -> Box<dyn WorkbookEdit> {
    let rows = grid.len() as u32;
    let cols = grid.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
    let range = Range::new(target_row, target_col, rows.max(1), cols.max(1));
    Box::new(SnapshotEdit::new(
        "粘贴",
        target,
        vec![range],
        Box::new(move |s| {
            for (dr, line) in grid.iter().enumerate() {
                for (dc, raw) in line.iter().enumerate() {
                    let rr = target_row + dr as u32;
                    let cc = target_col + dc as u32;
                    if rr >= s.row_count() || cc >= s.column_count() {
                        continue;
                    }
                    s.set_formula(rr, cc, "");
                    s.set_value(rr, cc, parse_external_value(raw));
                }
            }
        }),
    ))
}

/// 文本串 → 数字或原串（可解析数字则转数，空串→None）。对齐 TS pasteExternal 语义。
pub(crate) fn parse_external_value(raw: &str) -> Option<CellValue> {
    if raw.is_empty() {
        return None;
    }
    let t = raw.trim();
    if !t.is_empty() {
        if let Ok(n) = t.parse::<f64>() {
            return Some(CellValue::Number(n));
        }
    }
    Some(CellValue::Text(raw.to_string()))
}

/// 拖拽移动选区块：src 区数据移到以 (target_row,target_col) 为左上的新位置（清源，可撤销）。
pub fn move_range_command(
    target: usize,
    src: Range,
    target_row: u32,
    target_col: u32,
) -> Box<dyn WorkbookEdit> {
    let dst = Range::new(target_row, target_col, src.row_count, src.col_count);
    Box::new(SnapshotEdit::new(
        "移动",
        target,
        vec![src, dst],
        Box::new(move |s| {
            // 先抓源数据（相对偏移）
            let mut data: Vec<(u32, u32, Option<CellData>)> = Vec::new();
            src.for_each_cell(|row, col| {
                data.push((row - src.row, col - src.col, s.get_cell_data(row, col)));
            });
            // 清源
            src.for_each_cell(|row, col| write_cell_data(s, row, col, None));
            // 写目标
            for (dr, dc, d) in &data {
                write_cell_data(s, target_row + dr, target_col + dc, d.as_ref());
            }
        }),
    ))
}

// ── 合并 / 取消合并 ──────────────────────────────────────────

/// 在命中格集合执行文本替换（值层，可撤销）。hits=命中坐标；search/replace 为字面串。
pub fn replace_command(
    target: usize,
    hits: &[(u32, u32)],
    search: &str,
    replace: &str,
    match_case: bool,
) -> Box<dyn WorkbookEdit> {
    let ranges: Vec<Range> = hits.iter().map(|&(r, c)| Range::cell(r, c)).collect();
    let hits = hits.to_vec();
    let search = search.to_string();
    let replace = replace.to_string();
    Box::new(SnapshotEdit::new(
        "替换",
        target,
        ranges,
        Box::new(move |s| {
            for &(r, c) in &hits {
                let v = s.get_value(r, c);
                let text = match v {
                    Some(CellValue::Text(t)) => t,
                    Some(CellValue::Number(n)) => crate::numstr::num_to_string(n),
                    _ => continue,
                };
                let replaced = replace_text(&text, &search, &replace, match_case);
                // 替换后尝试恢复数值类型
                let t = replaced.trim();
                if !t.is_empty() {
                    if let Ok(n) = t.parse::<f64>() {
                        s.set_value(r, c, Some(CellValue::Number(n)));
                        continue;
                    }
                }
                s.set_value(r, c, Some(CellValue::Text(replaced)));
            }
        }),
    ))
}

fn replace_text(text: &str, search: &str, replace: &str, match_case: bool) -> String {
    if search.is_empty() {
        return text.to_string();
    }
    if match_case {
        return text.replace(search, replace);
    }
    // 不区分大小写：逐位扫描替换
    let lower_text = text.to_lowercase();
    let lower_search = search.to_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let text_bytes: Vec<char> = text.chars().collect();
    let lt: Vec<char> = lower_text.chars().collect();
    let ls: Vec<char> = lower_search.chars().collect();
    while i < text_bytes.len() {
        if i + ls.len() <= lt.len() && lt[i..i + ls.len()] == ls[..] {
            out.push_str(replace);
            i += ls.len();
        } else {
            out.push(text_bytes[i]);
            i += 1;
        }
    }
    out
}

/// 排序区域命令（M11，可撤销）。按 keys 排 range 内数据行（整行随动），首行可选表头不参与。
pub fn sort_range_command(
    target: usize,
    range: Range,
    keys: Vec<crate::sort::SortKey>,
    has_header: bool,
) -> Box<dyn WorkbookEdit> {
    Box::new(SnapshotEdit::new(
        "排序",
        target,
        vec![range],
        Box::new(move |s| {
            let first_data_row = range.row + if has_header { 1 } else { 0 };
            let last_row = range.last_row();
            if first_data_row > last_row {
                return;
            }
            // 抽每行关键字值 + 整行数据快照
            let mut rows_meta: Vec<(u32, Vec<Option<CellValue>>)> = Vec::new();
            let mut row_data: std::collections::HashMap<u32, Vec<Option<CellData>>> =
                std::collections::HashMap::new();
            for r in first_data_row..=last_row {
                let values: Vec<Option<CellValue>> =
                    keys.iter().map(|k| s.get_value(r, k.col)).collect();
                rows_meta.push((r, values));
                let cells: Vec<Option<CellData>> = (range.col..range.col + range.col_count)
                    .map(|c| s.get_cell_data(r, c))
                    .collect();
                row_data.insert(r, cells);
            }
            let order = crate::sort::compute_sort_order(&rows_meta, &keys);
            for (i, &src_row) in order.iter().enumerate() {
                let target_row = first_data_row + i as u32;
                let cells = row_data.get(&src_row).cloned().unwrap_or_default();
                for (ci, cell) in cells.iter().enumerate() {
                    write_cell_data(s, target_row, range.col + ci as u32, cell.as_ref());
                }
            }
        }),
    ))
}

// ── M26 数据工具命令 ─────────────────────────────────────────
//
// 三命令均走 SnapshotEdit：读相位（构造期，借 `&Worksheet` 决定快照宽度/去重保留集/聚合结果），
// 写相位（apply 期，`&mut Worksheet` 落格）。对齐 TS EditCommands 的 textToColumns /
// removeDuplicates / consolidate，语义逐格等价。

/// 文本串 → 数字或原串（复用 pasteExternal 语义：可解析数字则转数，空串→None）。
fn to_cell_val(raw: &str) -> Option<CellValue> {
    parse_external_value(raw)
}

/// 文本分列模式：按分隔符（互斥于定宽）或定宽字符数拆分。
#[derive(Debug, Clone)]
pub enum TextToColumnsMode {
    /// 按分隔符拆（空串分隔符 → 整串不拆）。
    Delimiter(String),
    /// 定宽：各列字符宽度；余部归最后一列。
    FixedWidths(Vec<usize>),
}

/// 单串拆分（对齐 TS splitOne）。
fn split_one(text: &str, mode: &TextToColumnsMode) -> Vec<String> {
    match mode {
        TextToColumnsMode::FixedWidths(widths) if !widths.is_empty() => {
            // 按字符（非字节）切，兼容多字节。
            let chars: Vec<char> = text.chars().collect();
            let mut out: Vec<String> = Vec::new();
            let mut i = 0usize;
            for &w in widths {
                let end = (i + w).min(chars.len());
                out.push(chars[i.min(chars.len())..end].iter().collect());
                i += w;
            }
            if i < chars.len() {
                out.push(chars[i..].iter().collect()); // 余部归最后一列
            }
            out
        }
        TextToColumnsMode::FixedWidths(_) => vec![text.to_string()],
        TextToColumnsMode::Delimiter(d) if d.is_empty() => vec![text.to_string()],
        TextToColumnsMode::Delimiter(d) => text.split(d.as_str()).map(str::to_string).collect(),
    }
}

/// 文本分列（M26）：把 range 首列每行的文本按分隔符/定宽拆成多列，写到该行右侧。
/// 快照区 = range.row..+rowCount × range.col..+max(colCount, maxParts)（覆盖被写区），undo 自动。
pub fn text_to_columns_command(
    sheet: &Worksheet,
    target: usize,
    range: Range,
    mode: TextToColumnsMode,
) -> Box<dyn WorkbookEdit> {
    // 预算最大列数（决定快照宽度）——构造期借 &sheet 读源。
    let mut max_parts = 1usize;
    for r in range.row..range.row + range.row_count {
        let text = value_to_string(sheet.get_value(r, range.col));
        max_parts = max_parts.max(split_one(&text, &mode).len());
    }
    let snap_w = range.col_count.max(max_parts as u32);
    let snap = Range::new(range.row, range.col, range.row_count, snap_w);
    Box::new(SnapshotEdit::new(
        "文本分列",
        target,
        vec![snap],
        Box::new(move |s| {
            for r in range.row..range.row + range.row_count {
                let text = value_to_string(s.get_value(r, range.col));
                let parts = split_one(&text, &mode);
                for k in 0..max_parts {
                    let cc = range.col + k as u32;
                    if cc >= s.column_count() {
                        break;
                    }
                    s.set_formula(r, cc, "");
                    let v = parts.get(k).and_then(|p| to_cell_val(p));
                    s.set_value(r, cc, v);
                }
            }
        }),
    ))
}

/// 删除重复选项。
#[derive(Debug, Clone, Default)]
pub struct RemoveDuplicatesOptions {
    /// 参与比较的列（相对 range 左上的 0-based 偏移）；缺省用全部列。
    pub key_cols: Vec<u32>,
    /// 首行是表头（不参与去重，保留在顶）。
    pub has_header: bool,
}

/// removeDuplicates 结果：命令 + 删除行数（构造期即算出）。
pub struct RemoveDuplicatesResult {
    pub command: Box<dyn WorkbookEdit>,
    pub removed: usize,
}

/// 删除重复行（M26）：range 内按 key_cols 去重，保留首现，其余行上移压实，尾部清空。
/// 返回 { command, removed }——removed 为删除行数（构造期借 &sheet 算出，命令执行前已知）。
pub fn remove_duplicates_command(
    sheet: &Worksheet,
    target: usize,
    range: Range,
    opts: RemoveDuplicatesOptions,
) -> RemoveDuplicatesResult {
    let start_data = range.row + if opts.has_header { 1 } else { 0 };
    let key_cols: Vec<u32> = if opts.key_cols.is_empty() {
        (0..range.col_count).map(|k| range.col + k).collect()
    } else {
        opts.key_cols.iter().map(|k| range.col + k).collect()
    };
    // 收集去重后保留行的完整 CellData 快照（值+公式+样式）。
    let mut kept: Vec<Vec<Option<CellData>>> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut removed = 0usize;
    let end_row = range.row + range.row_count;
    for r in start_data..end_row {
        let key = key_cols
            .iter()
            .map(|&c| value_to_string(sheet.get_value(r, c)))
            .collect::<Vec<_>>()
            .join("");
        if seen.contains(&key) {
            removed += 1;
            continue;
        }
        seen.insert(key);
        let row_data: Vec<Option<CellData>> = (range.col..range.col + range.col_count)
            .map(|c| sheet.get_cell_data(r, c))
            .collect();
        kept.push(row_data);
    }
    let col0 = range.col;
    let col_count = range.col_count;
    let snap = Range::new(start_data, col0, end_row - start_data, col_count);
    let command = Box::new(SnapshotEdit::new(
        "删除重复",
        target,
        vec![snap],
        Box::new(move |s| {
            for (i, row_data) in kept.iter().enumerate() {
                let rr = start_data + i as u32;
                for (k, d) in row_data.iter().enumerate() {
                    write_cell_data(s, rr, col0 + k as u32, d.as_ref());
                }
            }
            // 尾部剩余行清空。
            for rr in start_data + kept.len() as u32..end_row {
                for c in col0..col0 + col_count {
                    write_cell_data(s, rr, c, None);
                }
            }
        }),
    ));
    RemoveDuplicatesResult { command, removed }
}

/// 合并计算聚合函数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsolidateFunc {
    Sum,
    Average,
    Count,
    Max,
    Min,
    Product,
}

fn consolidate_agg(func: ConsolidateFunc, nums: &[f64]) -> f64 {
    if nums.is_empty() {
        return 0.0;
    }
    match func {
        ConsolidateFunc::Sum => nums.iter().sum(),
        ConsolidateFunc::Average => nums.iter().sum::<f64>() / nums.len() as f64,
        ConsolidateFunc::Count => nums.len() as f64,
        ConsolidateFunc::Max => nums.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        ConsolidateFunc::Min => nums.iter().copied().fold(f64::INFINITY, f64::min),
        ConsolidateFunc::Product => nums.iter().product(),
    }
}

/// 合并计算选项。
#[derive(Debug, Clone)]
pub struct ConsolidateOptions {
    pub func: ConsolidateFunc,
    /// 按分类：以各源区首列为标签、其余列为值，按标签聚合（缺省 false = 按位置）。
    pub by_label: bool,
}

impl Default for ConsolidateOptions {
    fn default() -> Self {
        ConsolidateOptions {
            func: ConsolidateFunc::Sum,
            by_label: false,
        }
    }
}

/// 从 (r,c) 取数值（数值直取；数字串转数；否则 None），对齐 TS numAt。
fn num_at(sheet: &Worksheet, r: u32, c: u32) -> Option<f64> {
    match sheet.get_value(r, c) {
        Some(CellValue::Number(n)) => Some(n),
        Some(CellValue::Text(t)) => {
            let tt = t.trim();
            if tt.is_empty() {
                None
            } else {
                tt.parse::<f64>().ok()
            }
        }
        _ => None,
    }
}

/// 合并计算（M26）：多源区按位置或按分类聚合，写到 (target_row,target_col) 为左上的区域。
/// - 按位置：源区逐格用 func 聚合（不同形时以各源实际尺寸参与，越界不计）。
/// - 按分类：各源区首列为标签、右侧为数值，按标签合并行（首现顺序）、逐值列聚合。
pub fn consolidate_command(
    sheet: &Worksheet,
    target: usize,
    target_row: u32,
    target_col: u32,
    sources: Vec<Range>,
    opts: ConsolidateOptions,
) -> Box<dyn WorkbookEdit> {
    let func = opts.func;
    if opts.by_label {
        // 按分类：收集 标签→[各值列的数值数组]（构造期借 &sheet 读源、定序）。
        let val_cols = sources.first().map(|s| s.col_count).unwrap_or(1).max(1) - 1;
        let mut order: Vec<String> = Vec::new();
        let mut map: std::collections::HashMap<String, Vec<Vec<f64>>> =
            std::collections::HashMap::new();
        for src in &sources {
            for dr in 0..src.row_count {
                let label = value_to_string(sheet.get_value(src.row + dr, src.col));
                if label.is_empty() {
                    continue;
                }
                if !map.contains_key(&label) {
                    map.insert(label.clone(), vec![Vec::new(); val_cols as usize]);
                    order.push(label.clone());
                }
                let bucket = map.get_mut(&label).unwrap();
                for vc in 0..val_cols {
                    if let Some(n) = num_at(sheet, src.row + dr, src.col + 1 + vc) {
                        bucket[vc as usize].push(n);
                    }
                }
            }
        }
        let snap = Range::new(
            target_row,
            target_col,
            (order.len() as u32).max(1),
            (val_cols + 1).max(1),
        );
        return Box::new(SnapshotEdit::new(
            "合并计算",
            target,
            vec![snap],
            Box::new(move |s| {
                for (i, label) in order.iter().enumerate() {
                    let rr = target_row + i as u32;
                    s.set_formula(rr, target_col, "");
                    s.set_value(rr, target_col, Some(CellValue::Text(label.clone())));
                    let bucket = &map[label];
                    for vc in 0..val_cols {
                        let cc = target_col + 1 + vc;
                        s.set_formula(rr, cc, "");
                        s.set_value(
                            rr,
                            cc,
                            Some(CellValue::Number(consolidate_agg(
                                func,
                                &bucket[vc as usize],
                            ))),
                        );
                    }
                }
            }),
        ));
    }

    // 按位置：逐格聚合。快照区 = 各源最大行数 × 最大列数。
    let rows = sources.iter().map(|r| r.row_count).max().unwrap_or(0);
    let cols = sources.iter().map(|r| r.col_count).max().unwrap_or(0);
    let snap = Range::new(target_row, target_col, rows.max(1), cols.max(1));
    Box::new(SnapshotEdit::new(
        "合并计算",
        target,
        vec![snap],
        Box::new(move |s| {
            for dr in 0..rows {
                for dc in 0..cols {
                    let mut nums: Vec<f64> = Vec::new();
                    for src in &sources {
                        if dr < src.row_count && dc < src.col_count {
                            if let Some(n) = num_at(s, src.row + dr, src.col + dc) {
                                nums.push(n);
                            }
                        }
                    }
                    let rr = target_row + dr;
                    let cc = target_col + dc;
                    if rr >= s.row_count() || cc >= s.column_count() {
                        continue;
                    }
                    s.set_formula(rr, cc, "");
                    let v = if nums.is_empty() {
                        None
                    } else {
                        Some(CellValue::Number(consolidate_agg(func, &nums)))
                    };
                    s.set_value(rr, cc, v);
                }
            }
        }),
    ))
}

/// 值 → 比较/拼接用文本（None → 空串），对齐 TS `String(v ?? '')`。
fn value_to_string(v: Option<CellValue>) -> String {
    match v {
        None => String::new(),
        Some(cv) => cv.to_text(),
    }
}

/// 合并区域命令（记录被覆盖的旧 span + 区域内容以便 revert）。
pub struct MergeEdit {
    target: usize,
    range: Range,
    removed_spans: Vec<Span>,
    cell_snap: Vec<CellSnapshot>,
}

/// 合并区域。
pub fn merge_command(target: usize, range: Range) -> Box<dyn WorkbookEdit> {
    Box::new(MergeEdit {
        target,
        range,
        removed_spans: Vec::new(),
        cell_snap: Vec::new(),
    })
}

impl WorkbookEdit for MergeEdit {
    fn label(&self) -> &str {
        "合并单元格"
    }

    fn apply(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        // 记录将被覆盖的旧 span + 区域单元格（合并会清除非左上格内容）
        self.removed_spans = ws
            .get_spans()
            .into_iter()
            .filter(|sp| {
                Range::new(sp.row, sp.col, sp.row_count, sp.col_count).intersects(&self.range)
            })
            .collect();
        self.cell_snap = snapshot_region(ws, &[self.range]);
        ws.add_span(
            self.range.row,
            self.range.col,
            self.range.row_count,
            self.range.col_count,
        );
    }

    fn revert(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        ws.remove_span(self.range.row, self.range.col);
        for sp in &self.removed_spans {
            ws.add_span(sp.row, sp.col, sp.row_count, sp.col_count);
        }
        restore_snapshot(ws, &self.cell_snap);
    }
}

/// 取消区域内的合并命令。
pub struct UnmergeEdit {
    target: usize,
    ranges: Vec<Range>,
    removed: Vec<Span>,
}

/// 取消区域内的合并。
pub fn unmerge_command(target: usize, ranges: Vec<Range>) -> Box<dyn WorkbookEdit> {
    Box::new(UnmergeEdit {
        target,
        ranges,
        removed: Vec::new(),
    })
}

impl WorkbookEdit for UnmergeEdit {
    fn label(&self) -> &str {
        "取消合并"
    }

    fn apply(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        self.removed = Vec::new();
        for r in &self.ranges {
            for sp in ws.get_spans() {
                let sp_range = Range::new(sp.row, sp.col, sp.row_count, sp.col_count);
                if sp_range.intersects(r)
                    && !self
                        .removed
                        .iter()
                        .any(|x| x.row == sp.row && x.col == sp.col)
                {
                    self.removed.push(sp);
                    ws.remove_span(sp.row, sp.col);
                }
            }
        }
    }

    fn revert(&mut self, wb: &mut Workbook) {
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        for sp in &self.removed {
            ws.add_span(sp.row, sp.col, sp.row_count, sp.col_count);
        }
    }
}

// ── 行列增删（可撤销 + 跨表公式重写）────────────────────────

/// 结构编辑轴。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowColAxis {
    Row,
    Col,
}

/// 结构编辑操作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowColOp {
    Insert,
    Delete,
}

/// 一条跨表公式改写记录（供 revert 精确还原）。
struct FormulaEditRec {
    sheet_index: usize,
    row: u32,
    col: u32,
    before: String,
    after: String,
}

/// 行列增删命令：结构位移 + （可选）跨全簿公式引用重写。
pub struct StructuralCommand {
    label: &'static str,
    target: usize,
    axis: RowColAxis,
    op: RowColOp,
    index: u32,
    count: u32,
    adjust: bool,
    cell_snap: Vec<CellSnapshot>,
    span_snap: Vec<Span>,
    formula_edits: Vec<FormulaEditRec>,
}

impl StructuralCommand {
    fn new(
        label: &'static str,
        target: usize,
        axis: RowColAxis,
        op: RowColOp,
        index: u32,
        count: u32,
        adjust: bool,
    ) -> Self {
        StructuralCommand {
            label,
            target,
            axis,
            op,
            index,
            count,
            adjust,
            cell_snap: Vec::new(),
            span_snap: Vec::new(),
            formula_edits: Vec::new(),
        }
    }
}

/// 插入行（可撤销；adjust=true 时重写全簿指向本表的公式引用）。
pub fn insert_rows_command(
    target: usize,
    before: u32,
    count: u32,
    adjust: bool,
) -> Box<dyn WorkbookEdit> {
    Box::new(StructuralCommand::new(
        "插入行",
        target,
        RowColAxis::Row,
        RowColOp::Insert,
        before,
        count,
        adjust,
    ))
}

/// 插入列。
pub fn insert_columns_command(
    target: usize,
    before: u32,
    count: u32,
    adjust: bool,
) -> Box<dyn WorkbookEdit> {
    Box::new(StructuralCommand::new(
        "插入列",
        target,
        RowColAxis::Col,
        RowColOp::Insert,
        before,
        count,
        adjust,
    ))
}

/// 删除行（可撤销：execute 前对被删区做全宽快照）。
pub fn delete_rows_command(
    target: usize,
    start: u32,
    count: u32,
    adjust: bool,
) -> Box<dyn WorkbookEdit> {
    Box::new(StructuralCommand::new(
        "删除行",
        target,
        RowColAxis::Row,
        RowColOp::Delete,
        start,
        count,
        adjust,
    ))
}

/// 删除列。
pub fn delete_columns_command(
    target: usize,
    start: u32,
    count: u32,
    adjust: bool,
) -> Box<dyn WorkbookEdit> {
    Box::new(StructuralCommand::new(
        "删除列",
        target,
        RowColAxis::Col,
        RowColOp::Delete,
        start,
        count,
        adjust,
    ))
}

impl WorkbookEdit for StructuralCommand {
    fn label(&self) -> &str {
        self.label
    }

    fn apply(&mut self, wb: &mut Workbook) {
        let edit_sheet = match wb.sheet(self.target) {
            Some(s) => s.name().to_string(),
            None => return,
        };
        // 结构位移（删除前先快照）
        {
            let ws = wb.sheet_mut(self.target).unwrap();
            match self.op {
                RowColOp::Insert => match self.axis {
                    RowColAxis::Row => ws.add_rows(self.index, self.count),
                    RowColAxis::Col => ws.add_columns(self.index, self.count),
                },
                RowColOp::Delete => match self.axis {
                    RowColAxis::Row => {
                        let width = ws.column_count().max(1);
                        self.cell_snap =
                            snapshot_region(ws, &[Range::new(self.index, 0, self.count, width)]);
                        let end = self.index + self.count;
                        self.span_snap = ws
                            .get_spans()
                            .into_iter()
                            .filter(|sp| sp.row >= self.index && sp.row < end)
                            .collect();
                        ws.delete_rows(self.index, self.count);
                    }
                    RowColAxis::Col => {
                        let height = ws.row_count().max(1);
                        self.cell_snap =
                            snapshot_region(ws, &[Range::new(0, self.index, height, self.count)]);
                        let end = self.index + self.count;
                        self.span_snap = ws
                            .get_spans()
                            .into_iter()
                            .filter(|sp| sp.col >= self.index && sp.col < end)
                            .collect();
                        ws.delete_columns(self.index, self.count);
                    }
                },
            }
        }
        // 跨表公式重写
        if self.adjust {
            let edit = RefEdit {
                axis: match self.axis {
                    RowColAxis::Row => RefAxis::Row,
                    RowColAxis::Col => RefAxis::Col,
                },
                index: self.index,
                count: self.count,
                op: match self.op {
                    RowColOp::Insert => RefOp::Insert,
                    RowColOp::Delete => RefOp::Delete,
                },
                edit_sheet,
            };
            self.formula_edits = adjust_workbook_formulas(wb, &edit);
        }
    }

    fn revert(&mut self, wb: &mut Workbook) {
        // 先还原跨表公式改写
        for e in &self.formula_edits {
            if let Some(ws) = wb.sheet_mut(e.sheet_index) {
                ws.set_formula(e.row, e.col, &e.before);
            }
        }
        let Some(ws) = wb.sheet_mut(self.target) else {
            return;
        };
        match self.op {
            RowColOp::Insert => match self.axis {
                RowColAxis::Row => ws.delete_rows(self.index, self.count),
                RowColAxis::Col => ws.delete_columns(self.index, self.count),
            },
            RowColOp::Delete => {
                match self.axis {
                    RowColAxis::Row => ws.add_rows(self.index, self.count),
                    RowColAxis::Col => ws.add_columns(self.index, self.count),
                }
                restore_snapshot(ws, &self.cell_snap);
                for sp in &self.span_snap {
                    ws.add_span(sp.row, sp.col, sp.row_count, sp.col_count);
                }
            }
        }
    }
}

/// 插删行列后，扫描全簿每个公式格，把指向 editSheet 的引用按结构编辑重写。
/// 返回被改写记录（before/after），供 revert 还原。两阶段：先收集再写，避免遍历中扰动稀疏迭代。
fn adjust_workbook_formulas(wb: &mut Workbook, edit: &RefEdit) -> Vec<FormulaEditRec> {
    let mut edits: Vec<FormulaEditRec> = Vec::new();
    for (si, ws) in wb.sheets().iter().enumerate() {
        let formula_sheet = ws.name().to_string();
        ws.for_each_cell(|data, row, col| {
            if let Some(f) = &data.formula {
                let after = adjust_for_structural(f, edit, &formula_sheet);
                if &after != f {
                    edits.push(FormulaEditRec {
                        sheet_index: si,
                        row,
                        col,
                        before: f.clone(),
                        after,
                    });
                }
            }
        });
    }
    for e in &edits {
        if let Some(ws) = wb.sheet_mut(e.sheet_index) {
            ws.set_formula(e.row, e.col, &e.after);
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::HAlign;
    use crate::worksheet::Worksheet;

    fn wb_1sheet() -> Workbook {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("S", 20, 10));
        wb
    }

    // ── setValueCommand ──
    #[test]
    fn set_value_undo_redo_from_empty() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        h.do_edit(&mut wb, set_value_command(0, 0, 0, Some(42.into())));
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(42.into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
        h.redo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(42.into()));
    }

    #[test]
    fn set_value_restores_previous() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some("old".into()));
        h.do_edit(&mut wb, set_value_command(0, 0, 0, Some("new".into())));
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("new".into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("old".into()));
    }

    // ── setFormulaCommand ──
    #[test]
    fn set_formula_restores_prior_value() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some(5.into()));
        h.do_edit(&mut wb, set_formula_command(0, 0, 0, "=A2+A3"));
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 0), "A2+A3");
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 0), "");
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(5.into()));
    }

    // ── applyStyleCommand ──
    #[test]
    fn apply_style_to_range_and_restore() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        h.do_edit(
            &mut wb,
            apply_style_command(
                0,
                vec![Range::new(0, 0, 2, 2)],
                Style {
                    bold: Some(true),
                    ..Default::default()
                },
            ),
        );
        assert_eq!(
            wb.sheet(0).unwrap().get_style(1, 1),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_style(1, 1), None);
    }

    #[test]
    fn apply_style_merges_and_restores_exactly() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_style(
            0,
            0,
            Some(Style {
                italic: Some(true),
                ..Default::default()
            }),
        );
        h.do_edit(
            &mut wb,
            apply_style_command(
                0,
                vec![Range::cell(0, 0)],
                Style {
                    bold: Some(true),
                    ..Default::default()
                },
            ),
        );
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0),
            Some(Style {
                italic: Some(true),
                bold: Some(true),
                ..Default::default()
            })
        );
        h.undo(&mut wb);
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0),
            Some(Style {
                italic: Some(true),
                ..Default::default()
            })
        );
    }

    // ── clearCommand ──
    #[test]
    fn clear_all_and_restore() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("x".into()));
            ws.set_style(
                0,
                0,
                Some(Style {
                    bold: Some(true),
                    ..Default::default()
                }),
            );
        }
        h.do_edit(
            &mut wb,
            clear_command(0, vec![Range::cell(0, 0)], ClearMode::All),
        );
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
        assert_eq!(wb.sheet(0).unwrap().get_style(0, 0), None);
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("x".into()));
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
    }

    #[test]
    fn clear_value_keeps_format() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("x".into()));
            ws.set_style(
                0,
                0,
                Some(Style {
                    bold: Some(true),
                    ..Default::default()
                }),
            );
        }
        h.do_edit(
            &mut wb,
            clear_command(0, vec![Range::cell(0, 0)], ClearMode::Value),
        );
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
    }

    #[test]
    fn clear_format_keeps_value() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("x".into()));
            ws.set_style(
                0,
                0,
                Some(Style {
                    bold: Some(true),
                    ..Default::default()
                }),
            );
        }
        h.do_edit(
            &mut wb,
            clear_command(0, vec![Range::cell(0, 0)], ClearMode::Format),
        );
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("x".into()));
        assert_eq!(wb.sheet(0).unwrap().get_style(0, 0), None);
    }

    // ── merge / unmerge ──
    #[test]
    fn merge_and_undo() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        h.do_edit(&mut wb, merge_command(0, Range::new(0, 0, 2, 2)));
        assert!(wb.sheet(0).unwrap().get_span(0, 0).is_some());
        h.undo(&mut wb);
        assert!(wb.sheet(0).unwrap().get_span(0, 0).is_none());
    }

    #[test]
    fn unmerge_removes_and_restores() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().add_span(0, 0, 2, 2);
        h.do_edit(&mut wb, unmerge_command(0, vec![Range::new(0, 0, 2, 2)]));
        assert!(wb.sheet(0).unwrap().get_span(0, 0).is_none());
        h.undo(&mut wb);
        assert!(wb.sheet(0).unwrap().get_span(0, 0).is_some());
    }

    #[test]
    fn merge_over_existing_span_restores_old() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().add_span(1, 1, 2, 2);
        h.do_edit(&mut wb, merge_command(0, Range::new(0, 0, 3, 3)));
        assert_eq!(wb.sheet(0).unwrap().get_spans().len(), 1);
        h.undo(&mut wb);
        assert_eq!(
            wb.sheet(0).unwrap().get_span(1, 1),
            Some(Span {
                row: 1,
                col: 1,
                row_count: 2,
                col_count: 2
            })
        );
    }

    // ── insert / delete rows ──
    #[test]
    fn insert_rows_then_undo() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_value(2, 0, Some("r2".into()));
        h.do_edit(&mut wb, insert_rows_command(0, 1, 2, false));
        assert_eq!(wb.sheet(0).unwrap().get_value(4, 0), Some("r2".into()));
        assert_eq!(wb.sheet(0).unwrap().row_count(), 22);
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(2, 0), Some("r2".into()));
        assert_eq!(wb.sheet(0).unwrap().row_count(), 20);
    }

    #[test]
    fn delete_rows_preserves_content_for_undo() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(1, 0, Some("gone".into()));
            ws.set_value(1, 1, Some("gone2".into()));
        }
        h.do_edit(&mut wb, delete_rows_command(0, 1, 1, false));
        assert_eq!(wb.sheet(0).unwrap().row_count(), 19);
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), None);
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().row_count(), 20);
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), Some("gone".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 1), Some("gone2".into()));
    }

    #[test]
    fn delete_rows_restores_spans() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().add_span(1, 0, 1, 3);
        h.do_edit(&mut wb, delete_rows_command(0, 1, 1, false));
        assert!(wb.sheet(0).unwrap().get_span(1, 0).is_none());
        h.undo(&mut wb);
        assert!(wb.sheet(0).unwrap().get_span(1, 0).is_some());
    }

    // ── insert / delete columns ──
    #[test]
    fn insert_columns_then_undo() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_value(0, 2, Some("c2".into()));
        h.do_edit(&mut wb, insert_columns_command(0, 1, 1, false));
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 3), Some("c2".into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 2), Some("c2".into()));
    }

    #[test]
    fn delete_columns_preserves_content() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 1, Some("gone".into()));
        h.do_edit(&mut wb, delete_columns_command(0, 1, 1, false));
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 1), None);
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 1), Some("gone".into()));
    }

    // ── multi-step chain ──
    #[test]
    fn multi_step_undo_redo_chain() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        h.do_edit(&mut wb, set_value_command(0, 0, 0, Some(1.into())));
        h.do_edit(&mut wb, set_value_command(0, 0, 0, Some(2.into())));
        h.do_edit(
            &mut wb,
            apply_style_command(
                0,
                vec![Range::cell(0, 0)],
                Style {
                    bold: Some(true),
                    ..Default::default()
                },
            ),
        );
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(2.into()));
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
        h.undo(&mut wb); // style
        h.undo(&mut wb); // value=2
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(1.into()));
        assert_eq!(wb.sheet(0).unwrap().get_style(0, 0), None);
        h.undo(&mut wb); // value=1
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
        h.redo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(1.into()));
    }

    // ── 跨表公式重写（Excel 语义）──
    #[test]
    fn insert_row_shifts_formula_down_and_undo() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "=SUM(A2:A5)");
        h.do_edit(&mut wb, insert_rows_command(0, 0, 1, true));
        assert_eq!(wb.sheet(0).unwrap().get_formula(1, 0), "SUM(A3:A6)");
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 0), "SUM(A2:A5)");
    }

    #[test]
    fn delete_referenced_row_collapses_ref_error() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "=B5*2");
        h.do_edit(&mut wb, delete_rows_command(0, 4, 1, true));
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 0), "#REF!*2");
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 0), "B5*2");
    }

    #[test]
    fn insert_column_shifts_col_refs() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "=C1+D1");
        h.do_edit(&mut wb, insert_columns_command(0, 0, 1, true));
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 1), "D1+E1");
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_formula(0, 0), "C1+D1");
    }

    #[test]
    fn adjusts_cross_sheet_refs_only_edited_sheet() {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("Sheet1", 20, 10));
        wb.append_sheet(Worksheet::with_size("Sheet2", 20, 10));
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(1).unwrap().set_formula(0, 0, "=Sheet1!A5+A5");
        h.do_edit(&mut wb, insert_rows_command(0, 0, 1, true));
        assert_eq!(wb.sheet(1).unwrap().get_formula(0, 0), "Sheet1!A6+A5");
        h.undo(&mut wb);
        assert_eq!(wb.sheet(1).unwrap().get_formula(0, 0), "Sheet1!A5+A5");
    }

    #[test]
    fn without_adjust_no_formula_rewrite() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "=SUM(A2:A5)");
        h.do_edit(&mut wb, insert_rows_command(0, 0, 1, false));
        assert_eq!(wb.sheet(0).unwrap().get_formula(1, 0), "SUM(A2:A5)");
    }

    // ── M10 数据命令 ──
    #[test]
    fn fill_command_undoable() {
        let mut wb = wb_1sheet();
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some(1.into()));
        let mut cmd = fill_command(
            0,
            &[(1, 0), (2, 0)],
            vec![
                CellData {
                    value: Some(2.into()),
                    ..Default::default()
                },
                CellData {
                    value: Some(3.into()),
                    ..Default::default()
                },
            ],
        );
        cmd.apply(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), Some(2.into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(2, 0), Some(3.into()));
        cmd.revert(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), None);
    }

    #[test]
    fn paste_external_lands_and_detects_numbers() {
        let mut wb = wb_1sheet();
        let grid = vec![
            vec!["10".to_string(), "text".to_string()],
            vec!["20".to_string(), "30".to_string()],
        ];
        let mut cmd = paste_external_command(0, 0, 0, grid);
        cmd.apply(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some(10.into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 1), Some("text".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 1), Some(30.into()));
        cmd.revert(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
    }

    #[test]
    fn move_range_clears_source() {
        let mut wb = wb_1sheet();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("X".into()));
            ws.set_value(0, 1, Some("Y".into()));
        }
        let mut cmd = move_range_command(0, Range::new(0, 0, 1, 2), 5, 5);
        cmd.apply(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), None);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), Some("X".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 6), Some("Y".into()));
        cmd.revert(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("X".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 5), None);
    }

    #[test]
    fn paste_format_moves_style_only() {
        let mut wb = wb_1sheet();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 0, Some("keep".into()));
        let mut cmd = paste_format_command(
            0,
            vec![Range::cell(0, 0)],
            Some(Style {
                bold: Some(true),
                ..Default::default()
            }),
        );
        cmd.apply(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("keep".into()));
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0).unwrap().bold,
            Some(true)
        );
    }

    #[test]
    fn history_new_action_clears_redo() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        h.do_edit(&mut wb, set_value_command(0, 0, 0, Some(1.into())));
        h.undo(&mut wb);
        assert!(h.can_redo());
        h.do_edit(&mut wb, set_value_command(0, 0, 0, Some(2.into())));
        assert!(!h.can_redo());
    }

    #[test]
    fn history_respects_max_size() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        h.set_max_size(2);
        for i in 0..5 {
            h.do_edit(&mut wb, set_value_command(0, 0, 0, Some((i as i64).into())));
        }
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn style_named_survives_snapshot_round_trip() {
        // 快照/恢复保留 styleName 键（回归：write_cell_data 全量搬 style）
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        wb.sheet_mut(0).unwrap().set_style(
            0,
            0,
            Some(Style {
                style_name: Some("emph".into()),
                h_align: Some(HAlign::Center),
                ..Default::default()
            }),
        );
        h.do_edit(
            &mut wb,
            clear_command(0, vec![Range::cell(0, 0)], ClearMode::Format),
        );
        assert_eq!(wb.sheet(0).unwrap().get_style(0, 0), None);
        h.undo(&mut wb);
        assert_eq!(
            wb.sheet(0).unwrap().get_style(0, 0),
            Some(Style {
                style_name: Some("emph".into()),
                h_align: Some(HAlign::Center),
                ..Default::default()
            })
        );
    }

    // ── M11 替换 / 排序 ──
    #[test]
    fn replace_command_undoable() {
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("foo".into()));
            ws.set_value(1, 0, Some("foobar".into()));
        }
        h.do_edit(
            &mut wb,
            replace_command(0, &[(0, 0), (1, 0)], "foo", "BAZ", false),
        );
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("BAZ".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), Some("BAZbar".into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("foo".into()));
    }

    #[test]
    fn sort_range_command_with_header() {
        use crate::sort::SortKey;
        let mut wb = wb_1sheet();
        let mut h = WorkbookHistory::new();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("名称".into()));
            ws.set_value(0, 1, Some("值".into()));
            ws.set_value(1, 0, Some(3.into()));
            ws.set_value(1, 1, Some("c".into()));
            ws.set_value(2, 0, Some(1.into()));
            ws.set_value(2, 1, Some("a".into()));
            ws.set_value(3, 0, Some(2.into()));
            ws.set_value(3, 1, Some("b".into()));
        }
        h.do_edit(
            &mut wb,
            sort_range_command(0, Range::new(0, 0, 4, 2), vec![SortKey::new(0, true)], true),
        );
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("名称".into())); // 表头不动
        assert_eq!(ws.get_value(1, 0), Some(1.into()));
        assert_eq!(ws.get_value(2, 0), Some(2.into()));
        assert_eq!(ws.get_value(3, 0), Some(3.into()));
        assert_eq!(ws.get_value(1, 1), Some("a".into())); // B 列随动
        assert_eq!(ws.get_value(2, 1), Some("b".into()));
        assert_eq!(ws.get_value(3, 1), Some("c".into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), Some(3.into()));
    }

    // ── M26 数据工具 ─────────────────────────────────────
    fn wb_data() -> Workbook {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("S", 30, 12));
        wb
    }

    #[test]
    fn t2c_split_by_delimiter_to_right() {
        let mut wb = wb_data();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 0, Some("x;y;z".into()));
        let mut h = WorkbookHistory::new();
        let cmd = text_to_columns_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 1, 1),
            TextToColumnsMode::Delimiter(";".to_string()),
        );
        h.do_edit(&mut wb, cmd);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("x".into()));
        assert_eq!(ws.get_value(0, 1), Some("y".into()));
        assert_eq!(ws.get_value(0, 2), Some("z".into()));
    }

    #[test]
    fn t2c_numeric_strings_convert() {
        let mut wb = wb_data();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 0, Some("1,2,3".into()));
        let mut h = WorkbookHistory::new();
        let cmd = text_to_columns_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 1, 1),
            TextToColumnsMode::Delimiter(",".to_string()),
        );
        h.do_edit(&mut wb, cmd);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some(1.into()));
        assert_eq!(ws.get_value(0, 1), Some(2.into()));
        assert_eq!(ws.get_value(0, 2), Some(3.into()));
    }

    #[test]
    fn t2c_fixed_widths() {
        let mut wb = wb_data();
        wb.sheet_mut(0)
            .unwrap()
            .set_value(0, 0, Some("AAABBCCCC".into()));
        let mut h = WorkbookHistory::new();
        let cmd = text_to_columns_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 1, 1),
            TextToColumnsMode::FixedWidths(vec![3, 2]),
        );
        h.do_edit(&mut wb, cmd);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("AAA".into()));
        assert_eq!(ws.get_value(0, 1), Some("BB".into()));
        assert_eq!(ws.get_value(0, 2), Some("CCCC".into())); // 余部归最后一列
    }

    #[test]
    fn t2c_multi_row() {
        let mut wb = wb_data();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some("a-b".into()));
            ws.set_value(1, 0, Some("c-d".into()));
        }
        let mut h = WorkbookHistory::new();
        let cmd = text_to_columns_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 2, 1),
            TextToColumnsMode::Delimiter("-".to_string()),
        );
        h.do_edit(&mut wb, cmd);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 1), Some("b".into()));
        assert_eq!(ws.get_value(1, 1), Some("d".into()));
    }

    #[test]
    fn t2c_undo_restores() {
        let mut wb = wb_data();
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some("x,y".into()));
        let mut h = WorkbookHistory::new();
        let cmd = text_to_columns_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 1, 1),
            TextToColumnsMode::Delimiter(",".to_string()),
        );
        h.do_edit(&mut wb, cmd);
        h.undo(&mut wb);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("x,y".into()));
        assert_eq!(ws.get_value(0, 1), None);
    }

    #[test]
    fn dedup_full_column_compact_count() {
        let mut wb = wb_data();
        for (i, v) in ["a", "a", "b", "a", "c"].iter().enumerate() {
            wb.sheet_mut(0)
                .unwrap()
                .set_value(i as u32, 0, Some((*v).into()));
        }
        let mut h = WorkbookHistory::new();
        let rd = remove_duplicates_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 5, 1),
            RemoveDuplicatesOptions::default(),
        );
        assert_eq!(rd.removed, 2); // a 重复 2 次
        h.do_edit(&mut wb, rd.command);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("a".into()));
        assert_eq!(ws.get_value(1, 0), Some("b".into()));
        assert_eq!(ws.get_value(2, 0), Some("c".into()));
        assert_eq!(ws.get_value(3, 0), None); // 尾部清空
        assert_eq!(ws.get_value(4, 0), None);
    }

    #[test]
    fn dedup_by_key_cols() {
        let mut wb = wb_data();
        {
            let ws = wb.sheet_mut(0).unwrap();
            // (id, note): 1/x, 1/y, 2/z —— 按 id 去重保留首现
            for (i, (id, note)) in [(1, "x"), (1, "y"), (2, "z")].iter().enumerate() {
                ws.set_value(i as u32, 0, Some((*id).into()));
                ws.set_value(i as u32, 1, Some((*note).into()));
            }
        }
        let mut h = WorkbookHistory::new();
        let rd = remove_duplicates_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 3, 2),
            RemoveDuplicatesOptions {
                key_cols: vec![0],
                has_header: false,
            },
        );
        assert_eq!(rd.removed, 1);
        h.do_edit(&mut wb, rd.command);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some(1.into())); // 保留首现
        assert_eq!(ws.get_value(0, 1), Some("x".into()));
        assert_eq!(ws.get_value(1, 0), Some(2.into()));
        assert_eq!(ws.get_value(1, 1), Some("z".into()));
    }

    #[test]
    fn dedup_has_header_kept() {
        let mut wb = wb_data();
        for (i, v) in ["H", "a", "a", "b"].iter().enumerate() {
            wb.sheet_mut(0)
                .unwrap()
                .set_value(i as u32, 0, Some((*v).into()));
        }
        let mut h = WorkbookHistory::new();
        let rd = remove_duplicates_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 4, 1),
            RemoveDuplicatesOptions {
                key_cols: vec![],
                has_header: true,
            },
        );
        assert_eq!(rd.removed, 1);
        h.do_edit(&mut wb, rd.command);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("H".into()));
        assert_eq!(ws.get_value(1, 0), Some("a".into()));
        assert_eq!(ws.get_value(2, 0), Some("b".into()));
    }

    #[test]
    fn dedup_undo_restores_all() {
        let mut wb = wb_data();
        for (i, v) in ["a", "a", "b"].iter().enumerate() {
            wb.sheet_mut(0)
                .unwrap()
                .set_value(i as u32, 0, Some((*v).into()));
        }
        let mut h = WorkbookHistory::new();
        let rd = remove_duplicates_command(
            wb.sheet(0).unwrap(),
            0,
            Range::new(0, 0, 3, 1),
            RemoveDuplicatesOptions::default(),
        );
        h.do_edit(&mut wb, rd.command);
        h.undo(&mut wb);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(0, 0), Some("a".into()));
        assert_eq!(ws.get_value(1, 0), Some("a".into()));
        assert_eq!(ws.get_value(2, 0), Some("b".into()));
    }

    #[test]
    fn consolidate_by_position_sum() {
        let mut wb = wb_data();
        {
            let ws = wb.sheet_mut(0).unwrap();
            for (r, c, v) in [(0, 0, 1), (0, 1, 2), (1, 0, 3), (1, 1, 4)] {
                ws.set_value(r, c, Some(v.into()));
            }
            for (r, c, v) in [(0, 5, 10), (0, 6, 20), (1, 5, 30), (1, 6, 40)] {
                ws.set_value(r, c, Some(v.into()));
            }
        }
        let mut h = WorkbookHistory::new();
        let cmd = consolidate_command(
            wb.sheet(0).unwrap(),
            0,
            10,
            0,
            vec![Range::new(0, 0, 2, 2), Range::new(0, 5, 2, 2)],
            ConsolidateOptions {
                func: ConsolidateFunc::Sum,
                by_label: false,
            },
        );
        h.do_edit(&mut wb, cmd);
        let ws = wb.sheet(0).unwrap();
        assert_eq!(ws.get_value(10, 0), Some(11.into()));
        assert_eq!(ws.get_value(10, 1), Some(22.into()));
        assert_eq!(ws.get_value(11, 0), Some(33.into()));
        assert_eq!(ws.get_value(11, 1), Some(44.into()));
    }

    #[test]
    fn consolidate_by_position_average() {
        let mut wb = wb_data();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_value(0, 3, Some(20.into()));
        }
        let mut h = WorkbookHistory::new();
        let cmd = consolidate_command(
            wb.sheet(0).unwrap(),
            0,
            5,
            0,
            vec![Range::new(0, 0, 1, 1), Range::new(0, 3, 1, 1)],
            ConsolidateOptions {
                func: ConsolidateFunc::Average,
                by_label: false,
            },
        );
        h.do_edit(&mut wb, cmd);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 0), Some(15.into()));
    }

    #[test]
    fn consolidate_by_position_max() {
        let mut wb = wb_data();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(3.into()));
            ws.set_value(0, 3, Some(7.into()));
        }
        let mut h = WorkbookHistory::new();
        let cmd = consolidate_command(
            wb.sheet(0).unwrap(),
            0,
            5,
            0,
            vec![Range::new(0, 0, 1, 1), Range::new(0, 3, 1, 1)],
            ConsolidateOptions {
                func: ConsolidateFunc::Max,
                by_label: false,
            },
        );
        h.do_edit(&mut wb, cmd);
        assert_eq!(wb.sheet(0).unwrap().get_value(5, 0), Some(7.into()));
    }

    #[test]
    fn consolidate_by_label() {
        let mut wb = wb_data();
        {
            let ws = wb.sheet_mut(0).unwrap();
            // 源1: A/10, B/20 ; 源2: A/5, C/1 —— 按标签求和 → A15 B20 C1
            for (i, (l, v)) in [("A", 10), ("B", 20)].iter().enumerate() {
                ws.set_value(i as u32, 0, Some((*l).into()));
                ws.set_value(i as u32, 1, Some((*v).into()));
            }
            for (i, (l, v)) in [("A", 5), ("C", 1)].iter().enumerate() {
                ws.set_value(i as u32, 5, Some((*l).into()));
                ws.set_value(i as u32, 6, Some((*v).into()));
            }
        }
        let mut h = WorkbookHistory::new();
        let cmd = consolidate_command(
            wb.sheet(0).unwrap(),
            0,
            10,
            0,
            vec![Range::new(0, 0, 2, 2), Range::new(0, 5, 2, 2)],
            ConsolidateOptions {
                func: ConsolidateFunc::Sum,
                by_label: true,
            },
        );
        h.do_edit(&mut wb, cmd);
        let ws = wb.sheet(0).unwrap();
        // A 在最前（首现顺序）
        assert_eq!(ws.get_value(10, 0), Some("A".into()));
        assert_eq!(ws.get_value(10, 1), Some(15.into()));
        assert_eq!(ws.get_value(11, 0), Some("B".into()));
        assert_eq!(ws.get_value(11, 1), Some(20.into()));
        assert_eq!(ws.get_value(12, 0), Some("C".into()));
        assert_eq!(ws.get_value(12, 1), Some(1.into()));
    }

    #[test]
    fn consolidate_undo_restores_target() {
        let mut wb = wb_data();
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some(5.into()));
        let mut h = WorkbookHistory::new();
        let cmd = consolidate_command(
            wb.sheet(0).unwrap(),
            0,
            10,
            0,
            vec![Range::new(0, 0, 1, 1)],
            ConsolidateOptions {
                func: ConsolidateFunc::Sum,
                by_label: false,
            },
        );
        h.do_edit(&mut wb, cmd);
        assert_eq!(wb.sheet(0).unwrap().get_value(10, 0), Some(5.into()));
        h.undo(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(10, 0), None);
    }
}
