//! 中性快照格式（cmx-megasheet 自有存储格式）。对标 cmx-megasheet 的 io/snapshot.ts。
//!
//! 后端把 doc_content 存为 OPAQUE bytea + doc_format 判别串；本模块定义 megasheet 自己的
//! 中性 JSON 快照（doc_format = "cmx-megasheet"）。原则：
//!  - 中性：不含 dark/light 视图色、不含几何像素派生态（大纲 level 派生、outline 隐藏派生）。
//!  - 稀疏：只序列化非默认值（空格不写、默认行高列宽不写）——serde `skip_serializing_if`。
//!  - 无损往返：workbook_to_json → workbook_from_json 还原全部持久状态。
//!  - **单一事实源**：`format:"cmx-megasheet"` `version:1`，两引擎共享。SNAPSHOT_VERSION 恒=1。
//!
//! Rust 移植取舍：TS `CellValue`/Span 直接复用；数字字节 parity 靠 core::CellValue 的自定义
//! Serialize（整值无 `.0`）+ 本模块 `js_num`（行高/列宽/zoom）。Style 内 f64（fontSize）的
//! JS 数字 parity 留最终 parity 一次性硬化。RS-M4 覆盖 M0/M2 面（单元格/样式/合并/几何/
//! 大纲/命名区域）；M11+ 字段（筛选/验证/条件格式/浮动/迷你图/页面/保护）随各里程碑追加。

use serde::{Deserialize, Serialize};

use sheet_core::cell::{CellValue, RichText};
use sheet_core::style::{Style, StyleSheet};
use sheet_core::workbook::Workbook;
use sheet_core::worksheet::{
    CellComment, ConditionalRule, DataValidation, FloatingObject, Hyperlink, PageSetup,
    SheetProtection, Span, Sparkline, Worksheet,
};

/// 快照格式标签（写入 doc_format 判别）。
pub const SNAPSHOT_FORMAT: &str = "cmx-megasheet";
/// 快照结构版本（破坏性变更时 +1）。恒=1，独立于引擎版本。
pub const SNAPSHOT_VERSION: u32 = 1;

/// 单元格快照：坐标 + 值/公式/样式/富文本（皆可选，只记非空）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CellSnapshot {
    pub r: u32,
    pub c: u32,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub v: Option<CellValue>,
    /// 公式源串，不含前导 '='。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub f: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub s: Option<Style>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub rich: Option<RichText>,
}

/// 大纲分组快照：只记持久态；level 还原时派生。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutlineGroupSnapshot {
    pub start: u32,
    pub count: u32,
    #[serde(skip_serializing_if = "is_false", default)]
    pub collapsed: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// JS 数字对齐：整值 f64 序列化为整数（`34` 而非 `34.0`），对齐 JSON.stringify。
mod js_num {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(n: &f64, s: S) -> Result<S::Ok, S::Error> {
        if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
            s.serialize_i64(*n as i64)
        } else {
            s.serialize_f64(*n)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        f64::deserialize(d)
    }
}

/// [index, jsNum] 对（行高/列宽用）：second 走 JS 数字格式。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NumPair(pub u32, #[serde(with = "js_num")] pub f64);

/// [index, Style] 对（行/列默认样式用）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StylePair(pub u32, pub Style);

/// 工作表快照。字段声明顺序对齐 TS SheetSnapshot（parity）。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SheetSnapshot {
    pub name: String,
    #[serde(rename = "rowCount")]
    pub row_count: u32,
    #[serde(rename = "colCount")]
    pub col_count: u32,
    pub cells: Vec<CellSnapshot>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub spans: Vec<Span>,
    #[serde(rename = "rowHeights", skip_serializing_if = "Vec::is_empty", default)]
    pub row_heights: Vec<NumPair>,
    #[serde(rename = "colWidths", skip_serializing_if = "Vec::is_empty", default)]
    pub col_widths: Vec<NumPair>,
    #[serde(rename = "rowStyles", skip_serializing_if = "Vec::is_empty", default)]
    pub row_styles: Vec<StylePair>,
    #[serde(rename = "colStyles", skip_serializing_if = "Vec::is_empty", default)]
    pub col_styles: Vec<StylePair>,
    #[serde(
        rename = "defaultStyle",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub default_style: Option<Style>,
    #[serde(rename = "hiddenRows", skip_serializing_if = "Vec::is_empty", default)]
    pub hidden_rows: Vec<u32>,
    #[serde(rename = "hiddenCols", skip_serializing_if = "Vec::is_empty", default)]
    pub hidden_cols: Vec<u32>,
    #[serde(rename = "rowOutlines", skip_serializing_if = "Vec::is_empty", default)]
    pub row_outlines: Vec<OutlineGroupSnapshot>,
    #[serde(rename = "colOutlines", skip_serializing_if = "Vec::is_empty", default)]
    pub col_outlines: Vec<OutlineGroupSnapshot>,
    #[serde(
        rename = "summaryBelow",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub summary_below: Option<bool>,
    #[serde(
        rename = "summaryRight",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub summary_right: Option<bool>,
    #[serde(
        with = "opt_js_num",
        rename = "zoom",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub zoom: Option<f64>,
    #[serde(rename = "activeRow", skip_serializing_if = "Option::is_none", default)]
    pub active_row: Option<u32>,
    #[serde(rename = "activeCol", skip_serializing_if = "Option::is_none", default)]
    pub active_col: Option<u32>,
    /// 数据验证规则（M12）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub validations: Vec<DataValidation>,
    /// 超链接（M12）：[row, col, link]。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub hyperlinks: Vec<HyperlinkEntry>,
    /// 条件格式规则（M13）。
    #[serde(
        rename = "conditionalRules",
        skip_serializing_if = "Vec::is_empty",
        default
    )]
    pub conditional_rules: Vec<ConditionalRule>,
    /// 单元格批注（M14）：[row, col, comment]。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub comments: Vec<CommentEntry>,
    /// 浮动对象（M14）：图片/图表/形状。
    #[serde(
        rename = "floatingObjects",
        skip_serializing_if = "Vec::is_empty",
        default
    )]
    pub floating_objects: Vec<FloatingObject>,
    /// 页面设置（M15）。
    #[serde(rename = "pageSetup", skip_serializing_if = "Option::is_none", default)]
    pub page_setup: Option<PageSetup>,
    /// 工作表保护（M20）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub protection: Option<SheetProtection>,
    /// 迷你图（M21）：[row, col, spec]。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub sparklines: Vec<SparklineEntry>,
    /// 冻结窗格 (frozen_rows, frozen_cols)——XLSX <pane> 导入的过渡态，提升到 workbook 级后
    /// 不参与中性快照序列化（中性快照冻结在 WorkbookSnapshot 顶层）。
    #[serde(skip)]
    pub frozen_pane: Option<(u32, u32)>,
}

/// 迷你图快照条目 [row, col, spec]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparklineEntry(pub u32, pub u32, pub Sparkline);

/// 批注快照条目 [row, col, comment]（元组对齐 TS Array）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentEntry(pub u32, pub u32, pub CellComment);

/// 超链接快照条目 [row, col, link]（元组序列化对齐 TS Array）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HyperlinkEntry(pub u32, pub u32, pub Hyperlink);

/// Option<f64> 的 JS 数字序列化（zoom 用）。
mod opt_js_num {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(n: &Option<f64>, s: S) -> Result<S::Ok, S::Error> {
        match n {
            None => s.serialize_none(),
            Some(v) if v.fract() == 0.0 && v.abs() < 9_007_199_254_740_992.0 => {
                s.serialize_i64(*v as i64)
            }
            Some(v) => s.serialize_f64(*v),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
        Option::<f64>::deserialize(d)
    }
}

/// 命名区域快照条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinedNameSnapshot {
    pub name: String,
    pub scope: String,
    #[serde(rename = "refersTo")]
    pub refers_to: String,
}

/// 工作簿快照（顶层）。字段声明顺序对齐 TS WorkbookSnapshot。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct WorkbookSnapshot {
    pub format: String,
    pub version: u32,
    #[serde(
        rename = "activeSheet",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub active_sheet: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub styles: Option<std::collections::BTreeMap<String, Style>>,
    #[serde(
        rename = "definedNames",
        skip_serializing_if = "Vec::is_empty",
        default
    )]
    pub defined_names: Vec<DefinedNameSnapshot>,
    /// M9 冻结窗格（视口态，element 注入/消费；IO 透传）。
    #[serde(rename = "frozenRowCount", skip_serializing_if = "is_zero", default)]
    pub frozen_row_count: u32,
    #[serde(rename = "frozenColCount", skip_serializing_if = "is_zero", default)]
    pub frozen_col_count: u32,
    /// M19 尾冻结（视口态）。
    #[serde(rename = "trailingRowCount", skip_serializing_if = "is_zero", default)]
    pub trailing_row_count: u32,
    #[serde(rename = "trailingColCount", skip_serializing_if = "is_zero", default)]
    pub trailing_col_count: u32,
    /// M19-step2 拆分模式（视口态）。
    #[serde(rename = "splitRow", skip_serializing_if = "is_false", default)]
    pub split_row: bool,
    #[serde(rename = "splitCol", skip_serializing_if = "is_false", default)]
    pub split_col: bool,
    pub sheets: Vec<SheetSnapshot>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

// ── 序列化 ───────────────────────────────────────────────

/// 序列化单工作表为中性快照。
pub fn sheet_to_json(ws: &Worksheet) -> SheetSnapshot {
    let mut cells: Vec<CellSnapshot> = Vec::new();
    ws.for_each_cell(|data, r, c| {
        let v = data.value.clone();
        let f = data.formula.clone().filter(|s| !s.is_empty());
        let s = data.style.clone().filter(|st| !st.is_empty());
        let rich = data.rich.clone();
        if v.is_some() || f.is_some() || s.is_some() || rich.is_some() {
            cells.push(CellSnapshot {
                r,
                c,
                v,
                f,
                s,
                rich,
            });
        }
    });
    cells.sort_by(|a, b| a.r.cmp(&b.r).then(a.c.cmp(&b.c)));

    let default_style = {
        let ds = ws.get_default_style();
        if ds.is_empty() {
            None
        } else {
            Some(ds)
        }
    };

    SheetSnapshot {
        name: ws.name().to_string(),
        row_count: ws.row_count(),
        col_count: ws.column_count(),
        cells,
        spans: ws.get_spans(),
        row_heights: ws
            .row_height_entries()
            .into_iter()
            .map(|(i, px)| NumPair(i, px))
            .collect(),
        col_widths: ws
            .column_width_entries()
            .into_iter()
            .map(|(i, px)| NumPair(i, px))
            .collect(),
        row_styles: ws
            .row_style_entries()
            .into_iter()
            .map(|(i, st)| StylePair(i, st))
            .collect(),
        col_styles: ws
            .column_style_entries()
            .into_iter()
            .map(|(i, st)| StylePair(i, st))
            .collect(),
        default_style,
        hidden_rows: ws.manual_hidden_rows(),
        hidden_cols: ws.manual_hidden_columns(),
        row_outlines: outlines_to_json(&ws.row_outlines),
        col_outlines: outlines_to_json(&ws.column_outlines),
        summary_below: if ws.summary_below { None } else { Some(false) },
        summary_right: if ws.summary_right { None } else { Some(false) },
        zoom: if ws.zoom() != 1.0 {
            Some(ws.zoom())
        } else {
            None
        },
        active_row: if ws.active_row_index() != 0 {
            Some(ws.active_row_index())
        } else {
            None
        },
        active_col: if ws.active_column_index() != 0 {
            Some(ws.active_column_index())
        } else {
            None
        },
        validations: ws.list_validations().to_vec(),
        hyperlinks: ws
            .list_hyperlinks()
            .into_iter()
            .map(|(r, c, l)| HyperlinkEntry(r, c, l))
            .collect(),
        conditional_rules: ws.list_conditional_rules().to_vec(),
        comments: ws
            .list_comments()
            .into_iter()
            .map(|(r, c, cm)| CommentEntry(r, c, cm))
            .collect(),
        floating_objects: ws.list_floating_objects(),
        page_setup: ws.get_page_setup().cloned(),
        protection: ws.protection().cloned(),
        sparklines: ws
            .list_sparklines()
            .into_iter()
            .map(|(r, c, s)| SparklineEntry(r, c, s))
            .collect(),
        frozen_pane: None,
    }
}

fn outlines_to_json(axis: &sheet_core::outline::OutlineAxis) -> Vec<OutlineGroupSnapshot> {
    axis.list()
        .iter()
        .map(|g| OutlineGroupSnapshot {
            start: g.start,
            count: g.count,
            collapsed: g.collapsed,
        })
        .collect()
}

/// 从快照重建单工作表。style_sheet 由调用方（工作簿层）注入以共享命名样式。
pub fn sheet_from_json(snap: &SheetSnapshot, style_sheet: Option<&StyleSheet>) -> Worksheet {
    let mut ws = Worksheet::with_size(&snap.name, snap.row_count, snap.col_count);
    if let Some(ss) = style_sheet {
        ws.style_sheet = ss.clone();
    }

    if let Some(ds) = &snap.default_style {
        ws.set_default_style(ds.clone());
    }
    for NumPair(r, px) in &snap.row_heights {
        ws.set_row_height(*r, *px);
    }
    for NumPair(c, px) in &snap.col_widths {
        ws.set_column_width(*c, *px);
    }
    for StylePair(r, st) in &snap.row_styles {
        ws.set_row_style(*r, Some(st.clone()));
    }
    for StylePair(c, st) in &snap.col_styles {
        ws.set_column_style(*c, Some(st.clone()));
    }

    // 单元格：先值/公式再样式（走底层 set*，做归一/剪枝）
    for cell in &snap.cells {
        if let Some(f) = &cell.f {
            ws.set_formula(cell.r, cell.c, f);
        }
        if let Some(v) = &cell.v {
            if cell.f.is_some() {
                ws.set_computed_value(cell.r, cell.c, Some(v.clone()));
            } else {
                ws.set_value(cell.r, cell.c, Some(v.clone()));
            }
        }
        if let Some(s) = &cell.s {
            ws.set_style(cell.r, cell.c, Some(s.clone()));
        }
        if let Some(rt) = &cell.rich {
            ws.set_rich_text(cell.r, cell.c, Some(rt.clone()));
        }
    }

    for s in &snap.spans {
        ws.add_span(s.row, s.col, s.row_count, s.col_count);
    }

    // 大纲：先建组（level 派生），再套折叠态
    for g in &snap.row_outlines {
        ws.row_outlines.group(g.start, g.count);
        if g.collapsed {
            ws.row_outlines.set_collapsed(g.start, true);
        }
    }
    for g in &snap.col_outlines {
        ws.column_outlines.group(g.start, g.count);
        if g.collapsed {
            ws.column_outlines.set_collapsed(g.start, true);
        }
    }
    if snap.summary_below == Some(false) {
        ws.summary_below = false;
    }
    if snap.summary_right == Some(false) {
        ws.summary_right = false;
    }

    // 手动隐藏（在 apply_outline_visibility 前设，二者分账）
    for r in &snap.hidden_rows {
        ws.set_row_visible(*r, false);
    }
    for c in &snap.hidden_cols {
        ws.set_column_visible(*c, false);
    }
    ws.apply_outline_visibility();

    if let Some(z) = snap.zoom {
        ws.set_zoom(z);
    }
    ws.set_selection(
        snap.active_row.unwrap_or(0),
        snap.active_col.unwrap_or(0),
        1,
        1,
    );
    // 数据验证 + 超链接（M12）
    for v in &snap.validations {
        ws.set_data_validation(v.clone());
    }
    for HyperlinkEntry(r, c, link) in &snap.hyperlinks {
        ws.set_hyperlink(*r, *c, Some(link.clone()));
    }
    for rule in &snap.conditional_rules {
        ws.add_conditional_rule(rule.clone());
    }
    for CommentEntry(r, c, cm) in &snap.comments {
        ws.set_comment(*r, *c, Some(cm.clone()));
    }
    for obj in &snap.floating_objects {
        ws.add_floating_object(obj.clone());
    }
    if let Some(ps) = &snap.page_setup {
        ws.set_page_setup(Some(ps.clone()));
    }
    if let Some(p) = &snap.protection {
        ws.set_protection(Some(p.clone()));
    }
    for SparklineEntry(r, c, spec) in &snap.sparklines {
        ws.set_sparkline(*r, *c, spec.clone());
    }
    ws
}

/// 序列化整个工作簿为中性快照。
pub fn workbook_to_json(wb: &Workbook) -> WorkbookSnapshot {
    // 命名样式：各 sheet 自持 style_sheet（值语义），取并集（按名去重，BTreeMap 稳定序）。
    let mut styles: std::collections::BTreeMap<String, Style> = std::collections::BTreeMap::new();
    for ws in wb.sheets() {
        for name in ws.style_sheet.names() {
            if let Some(st) = ws.style_sheet.get(&name) {
                styles.entry(name).or_insert(st);
            }
        }
    }
    let defined_names: Vec<DefinedNameSnapshot> = wb
        .list_names()
        .into_iter()
        .map(|(name, scope, refers_to)| DefinedNameSnapshot {
            name,
            scope,
            refers_to,
        })
        .collect();

    let vp = wb.viewport();
    WorkbookSnapshot {
        format: SNAPSHOT_FORMAT.to_string(),
        version: SNAPSHOT_VERSION,
        active_sheet: if wb.active_sheet_index() != 0 {
            Some(wb.active_sheet_index())
        } else {
            None
        },
        styles: if styles.is_empty() {
            None
        } else {
            Some(styles)
        },
        defined_names,
        frozen_row_count: vp.frozen_row_count,
        frozen_col_count: vp.frozen_col_count,
        trailing_row_count: vp.trailing_row_count,
        trailing_col_count: vp.trailing_col_count,
        split_row: vp.split_row,
        split_col: vp.split_col,
        sheets: wb.sheets().iter().map(sheet_to_json).collect(),
    }
}

/// 从中性快照重建工作簿。共享 style_sheet 先装命名样式，再逐表重建。
pub fn workbook_from_json(snap: &WorkbookSnapshot) -> Workbook {
    let mut wb = Workbook::empty();
    let mut ss = StyleSheet::new();
    if let Some(styles) = &snap.styles {
        for (name, st) in styles {
            ss.define(name, st.clone());
        }
    }
    for sheet_snap in &snap.sheets {
        wb.append_sheet(sheet_from_json(sheet_snap, Some(&ss)));
    }
    if wb.sheet_count() == 0 {
        let mut ws = Worksheet::new("Sheet1");
        ws.style_sheet = ss.clone();
        wb.append_sheet(ws);
    }
    for n in &snap.defined_names {
        wb.define_name(&n.name, &n.refers_to, &n.scope);
    }
    wb.set_active_sheet_index(snap.active_sheet.unwrap_or(0));
    wb.set_viewport(sheet_core::workbook::ViewportState {
        frozen_row_count: snap.frozen_row_count,
        frozen_col_count: snap.frozen_col_count,
        trailing_row_count: snap.trailing_row_count,
        trailing_col_count: snap.trailing_col_count,
        split_row: snap.split_row,
        split_col: snap.split_col,
    });
    wb
}

/// 便捷：工作簿 → JSON 字符串。pretty=true 时缩进 2 空格。
pub fn stringify_workbook(wb: &Workbook, pretty: bool) -> String {
    let snap = workbook_to_json(wb);
    if pretty {
        serde_json::to_string_pretty(&snap).unwrap_or_default()
    } else {
        serde_json::to_string(&snap).unwrap_or_default()
    }
}

/// 便捷：JSON 字符串 → 工作簿。非本格式返回 Err。
pub fn parse_workbook(json: &str) -> Result<Workbook, String> {
    let snap: WorkbookSnapshot =
        serde_json::from_str(json).map_err(|e| format!("invalid snapshot json: {e}"))?;
    if snap.format != SNAPSHOT_FORMAT {
        return Err(format!(
            "not a {SNAPSHOT_FORMAT} snapshot (format={})",
            snap.format
        ));
    }
    Ok(workbook_from_json(&snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheet_core::style::HAlign;

    fn sample_workbook() -> Workbook {
        let mut wb = Workbook::empty();
        let mut ss = StyleSheet::new();
        ss.define(
            "hdr",
            Style {
                bold: Some(true),
                back_color: Some("#eee".into()),
                h_align: Some(HAlign::Center),
                ..Default::default()
            },
        );
        ss.define(
            "money",
            Style {
                formatter: Some("#,##0.00".into()),
                h_align: Some(HAlign::Right),
                ..Default::default()
            },
        );

        let mut s1 = Worksheet::with_size("资产表", 30, 8);
        s1.style_sheet = ss.clone();
        s1.set_column_width(0, 200.0);
        s1.set_row_height(0, 34.0);
        s1.set_value(0, 0, Some("标题".into()));
        s1.set_style(
            0,
            0,
            Some(Style {
                style_name: Some("hdr".into()),
                ..Default::default()
            }),
        );
        s1.add_span(0, 0, 1, 4);
        s1.set_value(1, 0, Some("货币资金".into()));
        s1.set_value(1, 1, Some(620000.into()));
        s1.set_style(
            1,
            1,
            Some(Style {
                style_name: Some("money".into()),
                ..Default::default()
            }),
        );
        s1.set_value(2, 0, Some("应收账款".into()));
        s1.set_value(2, 1, Some(388000.into()));
        s1.set_formula(3, 1, "=SUM(B2:B3)");
        s1.set_style(
            3,
            1,
            Some(Style {
                style_name: Some("money".into()),
                ..Default::default()
            }),
        );
        s1.set_computed_value(3, 1, Some(1008000.into()));
        s1.row_outlines.group(1, 3);
        s1.apply_outline_visibility();
        s1.set_default_style(Style {
            font_family: Some("PingFang SC".into()),
            ..Default::default()
        });
        s1.set_column_style(
            1,
            Some(Style {
                h_align: Some(HAlign::Right),
                ..Default::default()
            }),
        );
        s1.set_row_visible(5, false);
        wb.append_sheet(s1);

        let mut s2 = Worksheet::with_size("利润表", 20, 6);
        s2.style_sheet = ss.clone();
        s2.set_value(0, 0, Some("营业收入".into()));
        s2.set_value(0, 1, Some(3200000.into()));
        s2.column_outlines.group(1, 3);
        s2.summary_right = false;
        s2.apply_outline_visibility();
        wb.append_sheet(s2);

        wb.set_active_sheet_index(1);
        wb
    }

    #[test]
    fn header_tags_format_version() {
        let snap = workbook_to_json(&Workbook::default());
        assert_eq!(snap.format, SNAPSHOT_FORMAT);
        assert_eq!(snap.version, SNAPSHOT_VERSION);
    }

    #[test]
    fn sheet_round_trip_cells_values_formulas_styles() {
        let wb = sample_workbook();
        let s1 = wb.sheet_by_name("资产表").unwrap();
        let snap = sheet_to_json(s1);
        let restored = sheet_from_json(&snap, Some(&s1.style_sheet));
        assert_eq!(restored.get_value(1, 0), Some("货币资金".into()));
        assert_eq!(restored.get_value(1, 1), Some(620000.into()));
        assert_eq!(restored.get_formula(3, 1), "SUM(B2:B3)");
        assert_eq!(restored.get_value(3, 1), Some(1008000.into()));
        assert_eq!(
            restored.get_style(0, 0).unwrap().style_name.as_deref(),
            Some("hdr")
        );
    }

    #[test]
    fn preserves_geometry() {
        let wb = sample_workbook();
        let s1 = wb.sheet_by_name("资产表").unwrap();
        let restored = sheet_from_json(&sheet_to_json(s1), Some(&s1.style_sheet));
        assert_eq!(
            restored.get_span(0, 0),
            Some(Span {
                row: 0,
                col: 0,
                row_count: 1,
                col_count: 4
            })
        );
        assert_eq!(restored.get_column_width(0), 200.0);
        assert_eq!(restored.get_row_height(0), 34.0);
    }

    #[test]
    fn preserves_default_row_col_styles() {
        let wb = sample_workbook();
        let s1 = wb.sheet_by_name("资产表").unwrap();
        let restored = sheet_from_json(&sheet_to_json(s1), Some(&s1.style_sheet));
        assert_eq!(
            restored.get_default_style().font_family.as_deref(),
            Some("PingFang SC")
        );
        assert!(restored.column_style_entries().contains(&(
            1,
            Style {
                h_align: Some(HAlign::Right),
                ..Default::default()
            }
        )));
    }

    #[test]
    fn preserves_outline_and_collapsed_derives_level() {
        let mut wb = sample_workbook();
        {
            let s1 = wb.sheet_by_name_mut("资产表").unwrap();
            s1.row_outlines.set_collapsed(1, true);
            s1.apply_outline_visibility();
        }
        let s1 = wb.sheet_by_name("资产表").unwrap();
        let restored = sheet_from_json(&sheet_to_json(s1), Some(&s1.style_sheet));
        let groups = restored.row_outlines.list();
        assert_eq!(groups.len(), 1);
        assert_eq!(
            (groups[0].start, groups[0].count, groups[0].collapsed),
            (1, 3, true)
        );
        assert!(!restored.is_row_visible(1));
        assert!(restored.is_row_visible(3));
    }

    #[test]
    fn separates_manual_from_outline_hidden() {
        let wb = sample_workbook();
        let s1 = wb.sheet_by_name("资产表").unwrap();
        let snap = sheet_to_json(s1);
        assert!(snap.hidden_rows.contains(&5));
        let restored = sheet_from_json(&snap, Some(&s1.style_sheet));
        assert!(!restored.is_row_visible(5));
    }

    #[test]
    fn preserves_summary_right_false() {
        let wb = sample_workbook();
        let s2 = wb.sheet_by_name("利润表").unwrap();
        let restored = sheet_from_json(&sheet_to_json(s2), Some(&s2.style_sheet));
        assert!(!restored.summary_right);
        assert_eq!(restored.column_outlines.list().len(), 1);
    }

    #[test]
    fn workbook_round_trip() {
        let wb = sample_workbook();
        let restored = workbook_from_json(&workbook_to_json(&wb));
        assert_eq!(restored.sheet_count(), 2);
        assert_eq!(restored.sheet(0).unwrap().name(), "资产表");
        assert_eq!(restored.sheet(1).unwrap().name(), "利润表");
        assert_eq!(restored.active_sheet_index(), 1);
        let hdr = restored.sheet(0).unwrap().style_sheet.get("hdr").unwrap();
        assert_eq!(hdr.bold, Some(true));
        assert_eq!(hdr.h_align, Some(HAlign::Center));
    }

    #[test]
    fn string_round_trip() {
        let wb = sample_workbook();
        let json = stringify_workbook(&wb, false);
        let restored = parse_workbook(&json).unwrap();
        assert_eq!(
            restored.sheet_by_name("资产表").unwrap().get_value(1, 1),
            Some(620000.into())
        );
    }

    #[test]
    fn parse_rejects_non_megasheet() {
        assert!(parse_workbook("{\"format\":\"other\",\"version\":18,\"sheets\":[]}").is_err());
    }

    #[test]
    fn empty_snapshot_yields_usable_workbook() {
        let snap = WorkbookSnapshot {
            format: SNAPSHOT_FORMAT.to_string(),
            version: 1,
            sheets: vec![],
            ..Default::default()
        };
        let restored = workbook_from_json(&snap);
        assert_eq!(restored.sheet_count(), 1);
        assert!(restored.active_sheet().is_some());
    }

    #[test]
    fn frozen_pane_fields_round_trip() {
        // M9：冻结窗格视口态经 JSON 往返存活
        let mut wb = sample_workbook();
        wb.freeze_panes(2, 1);
        let json = stringify_workbook(&wb, false);
        assert!(json.contains("\"frozenRowCount\":2"), "got {json}");
        assert!(json.contains("\"frozenColCount\":1"), "got {json}");
        let restored = parse_workbook(&json).unwrap();
        assert_eq!(restored.viewport().frozen_row_count, 2);
        assert_eq!(restored.viewport().frozen_col_count, 1);
        // 无冻结时不序列化（稀疏）
        let plain = stringify_workbook(&Workbook::default(), false);
        assert!(!plain.contains("frozenRowCount"), "got {plain}");
    }

    #[test]
    fn validations_hyperlinks_round_trip() {
        // M12：数据验证 + 超链接快照往返
        use sheet_core::worksheet::{
            DataValidation, Hyperlink, RegionRect, ValidationType, Worksheet,
        };
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("V", 10, 5);
        ws.set_data_validation(DataValidation {
            range: RegionRect::new(0, 0, 3, 1),
            validation_type: ValidationType::List,
            operator: None,
            formula1: None,
            formula2: None,
            list: Some(vec!["低".into(), "中".into(), "高".into()]),
            allow_blank: None,
            prompt: None,
            error: None,
        });
        ws.set_hyperlink(
            2,
            2,
            Some(Hyperlink {
                url: "https://cmx.dev".into(),
                tooltip: Some("官网".into()),
            }),
        );
        wb.append_sheet(ws);
        let restored = workbook_from_json(&workbook_to_json(&wb));
        let rs = restored.sheet(0).unwrap();
        assert_eq!(
            rs.get_validation_at(1, 0).and_then(|v| v.list.clone()),
            Some(vec!["低".into(), "中".into(), "高".into()])
        );
        assert_eq!(
            rs.get_hyperlink(2, 2).map(|l| l.url.as_str()),
            Some("https://cmx.dev")
        );
        assert_eq!(
            rs.get_hyperlink(2, 2).and_then(|l| l.tooltip.as_deref()),
            Some("官网")
        );
    }

    #[test]
    fn conditional_rules_and_rich_round_trip() {
        // M13：条件格式 add/remove/list + 富文本 IO 往返
        use sheet_core::cell::{RichFont, RichRun, RichText};
        use sheet_core::worksheet::{
            CondFormatOperator, CondFormatType, CondValue, ConditionalRule, RegionRect, Worksheet,
        };
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("C", 5, 3);
        ws.set_value(0, 0, Some(100.into()));
        ws.add_conditional_rule(ConditionalRule {
            range: RegionRect::new(0, 0, 3, 1),
            rule_type: CondFormatType::CellValue,
            operator: Some(CondFormatOperator::Gt),
            value1: Some(CondValue::Number(50.0)),
            value2: None,
            style: Some(Style {
                back_color: Some("#f00".into()),
                ..Default::default()
            }),
            colors: None,
            bar_color: None,
            icon_set: None,
        });
        ws.add_conditional_rule(ConditionalRule {
            range: RegionRect::new(0, 1, 3, 1),
            rule_type: CondFormatType::DataBar,
            operator: None,
            value1: None,
            value2: None,
            style: None,
            colors: None,
            bar_color: Some("#00f".into()),
            icon_set: None,
        });
        assert_eq!(ws.list_conditional_rules().len(), 2);
        ws.remove_conditional_rule(0);
        assert_eq!(ws.list_conditional_rules().len(), 1);
        ws.set_rich_text(
            1,
            1,
            Some(RichText {
                runs: vec![
                    RichRun {
                        text: "Hello ".into(),
                        font: Some(RichFont {
                            bold: Some(true),
                            ..Default::default()
                        }),
                    },
                    RichRun {
                        text: "World".into(),
                        font: Some(RichFont {
                            fore_color: Some("#f00".into()),
                            ..Default::default()
                        }),
                    },
                ],
            }),
        );
        wb.append_sheet(ws);
        let restored = workbook_from_json(&workbook_to_json(&wb));
        let rs = restored.sheet(0).unwrap();
        assert_eq!(rs.list_conditional_rules().len(), 1);
        assert_eq!(
            rs.list_conditional_rules()[0].rule_type,
            CondFormatType::DataBar
        );
        let rr = rs.get_rich_text(1, 1).unwrap();
        assert_eq!(rr.runs.len(), 2);
        assert_eq!(rr.runs[0].font.as_ref().unwrap().bold, Some(true));
        assert_eq!(
            rr.runs[1].font.as_ref().unwrap().fore_color.as_deref(),
            Some("#f00")
        );
    }

    #[test]
    fn floating_and_comments_round_trip() {
        // M14：浮动对象 + 批注快照往返
        use sheet_core::worksheet::{
            ChartSpec, ChartType, FloatingKind, FloatingObject, ObjAnchor, RegionRect, Worksheet,
        };
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("F", 10, 6);
        ws.set_comment(
            2,
            2,
            Some(sheet_core::worksheet::CellComment {
                text: "批注文本".into(),
                author: Some("A".into()),
            }),
        );
        let anchor = |fr, fc, tr, tc| ObjAnchor {
            from_row: fr,
            from_col: fc,
            to_row: tr,
            to_col: tc,
            from_dx: None,
            from_dy: None,
            to_dx: None,
            to_dy: None,
        };
        ws.add_floating_object(FloatingObject {
            id: "chart1".into(),
            kind: FloatingKind::Chart,
            anchor: anchor(1, 1, 5, 5),
            src: None,
            chart: Some(ChartSpec {
                chart_type: ChartType::Column,
                data_range: RegionRect::new(0, 0, 4, 3),
                title: Some("销售".into()),
                first_row_header: None,
                first_col_header: None,
                options: None,
            }),
            shape: None,
            z: None,
        });
        ws.add_floating_object(FloatingObject {
            id: "img1".into(),
            kind: FloatingKind::Image,
            anchor: anchor(6, 1, 8, 3),
            src: Some("data:image/png;base64,AAAA".into()),
            chart: None,
            shape: None,
            z: None,
        });
        wb.append_sheet(ws);
        let restored = workbook_from_json(&workbook_to_json(&wb));
        let rs = restored.sheet(0).unwrap();
        assert_eq!(
            rs.get_comment(2, 2).map(|c| c.text.as_str()),
            Some("批注文本")
        );
        assert_eq!(rs.list_floating_objects().len(), 2);
        assert_eq!(
            rs.get_floating_object("chart1")
                .and_then(|o| o.chart.as_ref())
                .map(|c| c.chart_type),
            Some(ChartType::Column)
        );
        assert_eq!(
            rs.get_floating_object("img1")
                .and_then(|o| o.src.as_deref()),
            Some("data:image/png;base64,AAAA")
        );
    }

    #[test]
    fn page_setup_round_trip() {
        // M15：页面设置快照往返
        use sheet_core::worksheet::{FitToPages, Orientation, PageSetup, PrintTitles, Worksheet};
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("P", 10, 5);
        ws.set_page_setup(Some(PageSetup {
            orientation: Some(Orientation::Landscape),
            paper_size: Some("A3".into()),
            fit_to_pages: Some(FitToPages {
                width: 1,
                height: 0,
            }),
            print_titles: Some(PrintTitles {
                row_start: Some(0),
                row_end: Some(0),
                col_start: None,
                col_end: None,
            }),
            ..Default::default()
        }));
        wb.append_sheet(ws);
        let restored = workbook_from_json(&workbook_to_json(&wb));
        let ps = restored.sheet(0).unwrap().get_page_setup().unwrap();
        assert_eq!(ps.orientation, Some(Orientation::Landscape));
        assert_eq!(ps.paper_size.as_deref(), Some("A3"));
        assert_eq!(ps.fit_to_pages.map(|f| f.width), Some(1));
        assert_eq!(ps.print_titles.and_then(|t| t.row_end), Some(0));
    }

    #[test]
    fn protection_and_locked_round_trip() {
        // M20：保护态 + locked 键随中性快照往返
        use sheet_core::style::Style;
        use sheet_core::worksheet::{SheetProtection, Worksheet};
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_style(
            0,
            0,
            Some(Style {
                locked: Some(false),
                ..Default::default()
            }),
        );
        ws.set_protection(Some(SheetProtection {
            enabled: true,
            allow_sort: Some(true),
            ..Default::default()
        }));
        wb.append_sheet(ws);
        let restored = workbook_from_json(&workbook_to_json(&wb));
        let s2 = restored.sheet(0).unwrap();
        assert_eq!(s2.get_style(0, 0).and_then(|st| st.locked), Some(false));
        assert!(s2.is_protected());
        assert_eq!(s2.protection().and_then(|p| p.allow_sort), Some(true));
    }

    #[test]
    fn sparkline_round_trip() {
        // M21：迷你图随中性快照往返
        use sheet_core::worksheet::{RegionRect, Sparkline, SparklineType, Worksheet};
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_sparkline(
            2,
            3,
            Sparkline {
                sparkline_type: SparklineType::Winloss,
                data_range: RegionRect::new(0, 0, 1, 5),
                color: None,
                negative_color: Some("#f00".into()),
                markers: None,
                high_low: Some(true),
                first_last: None,
                target: None,
            },
        );
        wb.append_sheet(ws);
        let restored = workbook_from_json(&workbook_to_json(&wb));
        let sp = restored.sheet(0).unwrap().get_sparkline(2, 3).unwrap();
        assert_eq!(sp.sparkline_type, SparklineType::Winloss);
        assert_eq!(sp.negative_color.as_deref(), Some("#f00"));
        assert_eq!(sp.high_low, Some(true));
        assert_eq!(sp.data_range, RegionRect::new(0, 0, 1, 5));
    }

    #[test]
    fn defined_names_round_trip() {
        let mut wb = sample_workbook();
        wb.define_name("SALES", "资产表!A1:A5", "workbook");
        wb.define_name("LOCAL", "B2", "利润表");
        let restored = workbook_from_json(&workbook_to_json(&wb));
        assert_eq!(
            restored.resolve_name("SALES", None).as_deref(),
            Some("资产表!A1:A5")
        );
        assert_eq!(
            restored.resolve_name("LOCAL", Some("利润表")).as_deref(),
            Some("B2")
        );
        assert_eq!(restored.list_names().len(), 2);
    }

    #[test]
    fn sparse_default_heights_widths_not_serialized() {
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("S", 10, 5);
        ws.set_value(0, 0, Some("x".into()));
        wb.append_sheet(ws);
        let snap = sheet_to_json(wb.sheet(0).unwrap());
        assert!(snap.row_heights.is_empty());
        assert!(snap.col_widths.is_empty());
        assert_eq!(snap.cells.len(), 1);
    }

    #[test]
    fn js_number_parity_integers_no_decimal() {
        // 整值数字/行高在 JSON 里无 `.0`（对齐 JS）
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("S", 10, 5);
        ws.set_value(0, 0, Some(620000.into()));
        ws.set_row_height(0, 34.0);
        wb.append_sheet(ws);
        let json = stringify_workbook(&wb, false);
        assert!(json.contains("620000"), "got {json}");
        assert!(!json.contains("620000.0"), "got {json}");
        assert!(json.contains("[0,34]"), "got {json}");
        assert!(!json.contains("34.0"), "got {json}");
    }
}
