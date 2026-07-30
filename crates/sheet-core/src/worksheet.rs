//! 工作表数据模型（无渲染）。对标 cmx-megasheet 的 Worksheet.ts 的 **M0 数据侧**。
//!
//! 承载单元格（值/公式/样式/富文本）、合并 span、结构（行列数增删）、行列元数据
//! （行高/列宽/可见性/默认样式）、选区、大纲分组、缩放。行列增删会**同步搬移**
//! 单元格数据、合并 span、行列元数据、大纲分组，保持一致。
//!
//! 后续里程碑（筛选/验证/条件格式/浮动对象/保护/迷你图/页面设置…）逐步在此追加。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::address::format_addr;
use crate::cell::{normalize_formula, CellData, CellValue, RichText};
use crate::outline::OutlineAxis;
use crate::range::Range;
use crate::sparse::SparseMatrix;
use crate::style::{resolve_style, Style, StyleSheet};

/// 合并区（左上角持值）。serde camelCase 对齐 TS Span（`rowCount`/`colCount`），供快照直用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub row: u32,
    pub col: u32,
    pub row_count: u32,
    pub col_count: u32,
}

impl Span {
    fn as_range(&self) -> Range {
        Range::new(self.row, self.col, self.row_count, self.col_count)
    }
}

pub const DEFAULT_ROW_COUNT: u32 = 40;
pub const DEFAULT_COL_COUNT: u32 = 12;
pub const DEFAULT_ROW_HEIGHT: f64 = 20.0;
pub const DEFAULT_COL_WIDTH: f64 = 62.0;

/// 筛选条件运算符（M11）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FilterOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Between,
    TopN,
}

/// 条件式过滤（M11）。value/value2 用 CellValue 承载（数字或文本）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilterCondition {
    pub op: FilterOp,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value: Option<CellValue>,
    /// between 上界 / topN 的 N。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value2: Option<CellValue>,
}

/// 单列筛选条件（M11）。values=白名单（显示文本）；condition=表达式；二者可并存（AND）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FilterCriterion {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub condition: Option<FilterCondition>,
}

/// 自动筛选态：区域 + 每列（绝对列号）条件（M11）。
#[derive(Debug, Clone)]
pub struct AutoFilterState {
    pub range: Range,
    /// 列号 → 条件（BTreeMap 稳定序）。
    pub criteria: std::collections::BTreeMap<u32, FilterCriterion>,
}

/// 区域（快照/验证/超链接用；serde camelCase）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegionRect {
    pub row: u32,
    pub col: u32,
    pub row_count: u32,
    pub col_count: u32,
}

impl RegionRect {
    pub fn new(row: u32, col: u32, row_count: u32, col_count: u32) -> Self {
        RegionRect {
            row,
            col,
            row_count,
            col_count,
        }
    }

    fn contains(&self, row: u32, col: u32) -> bool {
        row >= self.row
            && row < self.row + self.row_count
            && col >= self.col
            && col < self.col + self.col_count
    }
}

/// 数据验证类型（M12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationType {
    List,
    Whole,
    Decimal,
    Date,
    TextLength,
    Custom,
}

/// 数据验证比较运算（M12）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ValidationOperator {
    Between,
    NotBetween,
    Eq,
    Ne,
    Gt,
    Lt,
    Ge,
    Le,
}

/// formula1/formula2 界值：数字或文本（custom 用文本公式）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ValidationBound {
    Number(f64),
    Text(String),
}

impl ValidationBound {
    /// 转数值（文本尝试 parse；失败 NaN）。
    pub fn as_number(&self) -> f64 {
        match self {
            ValidationBound::Number(n) => *n,
            ValidationBound::Text(s) => s.parse::<f64>().unwrap_or(f64::NAN),
        }
    }
    /// 取文本（custom 公式用）。
    pub fn as_text(&self) -> Option<&str> {
        match self {
            ValidationBound::Text(s) => Some(s),
            _ => None,
        }
    }
}

/// 一条数据验证规则（M12）。作用于一个区域。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataValidation {
    pub range: RegionRect,
    #[serde(rename = "type")]
    pub validation_type: ValidationType,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub operator: Option<ValidationOperator>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub formula1: Option<ValidationBound>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub formula2: Option<ValidationBound>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub list: Option<Vec<String>>,
    #[serde(
        rename = "allowBlank",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allow_blank: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
}

/// 超链接（M12）：URL + 可选提示。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hyperlink {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub tooltip: Option<String>,
}

/// 条件格式类型（M13）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CondFormatType {
    CellValue,
    ColorScale,
    DataBar,
    IconSet,
}

/// 单元格值规则比较运算（M13）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CondFormatOperator {
    Gt,
    Ge,
    Lt,
    Le,
    Eq,
    Ne,
    Between,
    NotBetween,
    Contains,
    NotContains,
    Top,
    Bottom,
    Duplicate,
    Unique,
}

/// 图标集组名（M13）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IconSet {
    Arrows,
    Traffic,
    Rating,
}

/// 条件格式阈值：数字或文本。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CondValue {
    Number(f64),
    Text(String),
}

impl CondValue {
    pub fn as_number(&self) -> Option<f64> {
        match self {
            CondValue::Number(n) => Some(*n),
            CondValue::Text(s) => s.parse::<f64>().ok(),
        }
    }
    pub fn as_text(&self) -> String {
        match self {
            CondValue::Number(n) => crate::numstr::num_to_string(*n),
            CondValue::Text(s) => s.clone(),
        }
    }
}

/// 一条条件格式规则（M13）。作用于一个区域，渲染时叠加计算，不改数据。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalRule {
    pub range: RegionRect,
    #[serde(rename = "type")]
    pub rule_type: CondFormatType,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub operator: Option<CondFormatOperator>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value1: Option<CondValue>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value2: Option<CondValue>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub style: Option<Style>,
    /// colorScale 2/3 色。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub colors: Option<Vec<String>>,
    /// dataBar 条颜色。
    #[serde(rename = "barColor", skip_serializing_if = "Option::is_none", default)]
    pub bar_color: Option<String>,
    /// iconSet 图标组。
    #[serde(rename = "iconSet", skip_serializing_if = "Option::is_none", default)]
    pub icon_set: Option<IconSet>,
}

/// 单元格批注（M14）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellComment {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub author: Option<String>,
}

/// 图表类型（M14 五类 + M24 扩六类）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChartType {
    Bar,
    Column,
    Line,
    Pie,
    Area,
    Doughnut,
    Scatter,
    Bubble,
    Radar,
    Stock,
    Combo,
}

/// 图表增强选项（M24）。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChartOptions {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub legend: Option<bool>,
    #[serde(
        rename = "dataLabels",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub data_labels: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub trendline: Option<String>,
    #[serde(
        rename = "secondaryAxis",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub secondary_axis: Option<Vec<u32>>,
    #[serde(
        rename = "seriesTypes",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub series_types: Option<Vec<String>>,
    #[serde(rename = "holeRatio", skip_serializing_if = "Option::is_none", default)]
    pub hole_ratio: Option<f64>,
}

/// 图表规格（M14）：类型 + 数据源区域 + 标题。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartSpec {
    #[serde(rename = "chartType")]
    pub chart_type: ChartType,
    #[serde(rename = "dataRange")]
    pub data_range: RegionRect,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub title: Option<String>,
    #[serde(
        rename = "firstRowHeader",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub first_row_header: Option<bool>,
    #[serde(
        rename = "firstColHeader",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub first_col_header: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub options: Option<ChartOptions>,
}

/// 浮动对象类型（M14）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FloatingKind {
    Image,
    Chart,
    Shape,
}

/// 双格锚点（M14）：从 fromCell 左上到 toCell 右下（随格缩放）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ObjAnchor {
    #[serde(rename = "fromRow")]
    pub from_row: u32,
    #[serde(rename = "fromCol")]
    pub from_col: u32,
    #[serde(rename = "toRow")]
    pub to_row: u32,
    #[serde(rename = "toCol")]
    pub to_col: u32,
    #[serde(rename = "fromDx", skip_serializing_if = "Option::is_none", default)]
    pub from_dx: Option<f64>,
    #[serde(rename = "fromDy", skip_serializing_if = "Option::is_none", default)]
    pub from_dy: Option<f64>,
    #[serde(rename = "toDx", skip_serializing_if = "Option::is_none", default)]
    pub to_dx: Option<f64>,
    #[serde(rename = "toDy", skip_serializing_if = "Option::is_none", default)]
    pub to_dy: Option<f64>,
}

/// 形状规格（M14）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShapeSpec {
    #[serde(rename = "type")]
    pub shape_type: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub stroke: Option<String>,
}

/// 浮动对象（M14）：脱离网格流、按几何锚定的对象（图片/图表/形状）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FloatingObject {
    pub id: String,
    pub kind: FloatingKind,
    pub anchor: ObjAnchor,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub chart: Option<ChartSpec>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub shape: Option<ShapeSpec>,
    /// z 序（越大越上）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub z: Option<f64>,
}

/// 纸张方向（M15）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    Portrait,
    Landscape,
}

/// 页边距（pt）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PageMargins {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

/// 适合 N 页宽 × M 页高（0=该方向不限）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FitToPages {
    pub width: u32,
    pub height: u32,
}

/// 重复标题（每页顶部行区间 + 左侧列区间）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrintTitles {
    #[serde(rename = "rowStart", skip_serializing_if = "Option::is_none", default)]
    pub row_start: Option<u32>,
    #[serde(rename = "rowEnd", skip_serializing_if = "Option::is_none", default)]
    pub row_end: Option<u32>,
    #[serde(rename = "colStart", skip_serializing_if = "Option::is_none", default)]
    pub col_start: Option<u32>,
    #[serde(rename = "colEnd", skip_serializing_if = "Option::is_none", default)]
    pub col_end: Option<u32>,
}

/// 页面设置（M15）：打印/导出的纸张/边距/缩放/打印区/重复标题/页眉页脚。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PageSetup {
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub orientation: Option<Orientation>,
    /// 纸张名（A4/A3/Letter/Legal）。
    #[serde(rename = "paperSize", skip_serializing_if = "Option::is_none", default)]
    pub paper_size: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub margins: Option<PageMargins>,
    /// 缩放百分比（100=原大小）；与 fitToPages 互斥。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub scale: Option<f64>,
    #[serde(
        rename = "fitToPages",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub fit_to_pages: Option<FitToPages>,
    #[serde(rename = "printArea", skip_serializing_if = "Option::is_none", default)]
    pub print_area: Option<RegionRect>,
    #[serde(
        rename = "printTitles",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub print_titles: Option<PrintTitles>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub header: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub footer: Option<String>,
    #[serde(
        rename = "showGridlines",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub show_gridlines: Option<bool>,
}

/// 工作表保护态（M20）。enabled 时，锁定单元格（locked !== false）拒交互编辑。
/// allow* 为放行开关，缺省 false（=禁止），对齐 Excel「保护后默认只允许选中」。
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SheetProtection {
    pub enabled: bool,
    #[serde(
        rename = "allowSelectLocked",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allow_select_locked: Option<bool>,
    #[serde(
        rename = "allowFormatCells",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allow_format_cells: Option<bool>,
    #[serde(
        rename = "allowInsertDelete",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allow_insert_delete: Option<bool>,
    #[serde(rename = "allowSort", skip_serializing_if = "Option::is_none", default)]
    pub allow_sort: Option<bool>,
    #[serde(
        rename = "allowFilter",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub allow_filter: Option<bool>,
}

/// 迷你图类型（M21）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SparklineType {
    Line,
    Area,
    Column,
    Winloss,
    Bar,
    Pie,
    Bullet,
}

/// 迷你图（M21）：格内微图。数据源为单行或单列区域。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sparkline {
    #[serde(rename = "type")]
    pub sparkline_type: SparklineType,
    #[serde(rename = "dataRange")]
    pub data_range: RegionRect,
    /// 主色（缺省蓝）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub color: Option<String>,
    /// winloss/column 负值色（缺省红）。
    #[serde(
        rename = "negativeColor",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub negative_color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub markers: Option<bool>,
    #[serde(rename = "highLow", skip_serializing_if = "Option::is_none", default)]
    pub high_low: Option<bool>,
    #[serde(rename = "firstLast", skip_serializing_if = "Option::is_none", default)]
    pub first_last: Option<bool>,
    /// bullet KPI 目标线值。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target: Option<f64>,
}

pub struct Worksheet {
    name: String,
    cells: SparseMatrix<CellData>,
    spans: Vec<Span>,
    row_heights: HashMap<u32, f64>,
    col_widths: HashMap<u32, f64>,
    hidden_rows: HashSet<u32>,
    hidden_cols: HashSet<u32>,
    /// 大纲折叠导致的隐藏（与手动隐藏分账）。
    outline_hidden_rows: HashSet<u32>,
    outline_hidden_cols: HashSet<u32>,
    /// 自动筛选导致的隐藏行（与手动/大纲隐藏分账，apply_filter_visibility 维护）。M11。
    filter_hidden_rows: HashSet<u32>,
    /// 数据验证规则（M12）。后加入优先。
    validations: Vec<DataValidation>,
    /// 超链接（M12）：(row,col) → link。
    hyperlinks: HashMap<(u32, u32), Hyperlink>,
    /// 条件格式规则（M13）。
    conditional_rules: Vec<ConditionalRule>,
    /// 单元格批注（M14）：(row,col) → comment。
    comments: HashMap<(u32, u32), CellComment>,
    /// 浮动对象（M14）：图片/图表/形状。
    floating_objects: Vec<FloatingObject>,
    /// 页面设置（M15）。
    page_setup: Option<PageSetup>,
    /// 工作表保护态（M20）。
    protection: Option<SheetProtection>,
    /// 迷你图（M21）：(row,col) → spec。
    sparklines: HashMap<(u32, u32), Sparkline>,
    row_styles: HashMap<u32, Style>,
    col_styles: HashMap<u32, Style>,
    default_style: Style,
    row_count: u32,
    col_count: u32,
    selections: Vec<Range>,
    active_row: u32,
    active_col: u32,
    zoom: f64,

    pub style_sheet: StyleSheet,
    pub row_outlines: OutlineAxis,
    pub column_outlines: OutlineAxis,
    /// 汇总行在明细下方（Excel summaryBelow，默认 true）。
    pub summary_below: bool,
    /// 汇总列在明细右侧（Excel summaryRight，默认 true）。
    pub summary_right: bool,
    /// 自动筛选态（M11）；None=无筛选。
    pub auto_filter: Option<AutoFilterState>,
}

impl Worksheet {
    /// 新建工作表（默认 40×12）。
    pub fn new(name: &str) -> Self {
        Worksheet::with_size(name, DEFAULT_ROW_COUNT, DEFAULT_COL_COUNT)
    }

    /// 新建指定尺寸的工作表。
    pub fn with_size(name: &str, row_count: u32, col_count: u32) -> Self {
        Worksheet {
            name: name.to_string(),
            cells: SparseMatrix::new(),
            spans: Vec::new(),
            row_heights: HashMap::new(),
            col_widths: HashMap::new(),
            hidden_rows: HashSet::new(),
            hidden_cols: HashSet::new(),
            outline_hidden_rows: HashSet::new(),
            outline_hidden_cols: HashSet::new(),
            filter_hidden_rows: HashSet::new(),
            validations: Vec::new(),
            hyperlinks: HashMap::new(),
            conditional_rules: Vec::new(),
            comments: HashMap::new(),
            floating_objects: Vec::new(),
            page_setup: None,
            protection: None,
            sparklines: HashMap::new(),
            row_styles: HashMap::new(),
            col_styles: HashMap::new(),
            default_style: Style::default(),
            row_count: row_count.max(1),
            col_count: col_count.max(1),
            selections: vec![Range::new(0, 0, 1, 1)],
            active_row: 0,
            active_col: 0,
            zoom: 1.0,
            style_sheet: StyleSheet::new(),
            row_outlines: OutlineAxis::new(),
            column_outlines: OutlineAxis::new(),
            summary_below: true,
            summary_right: true,
            auto_filter: None,
        }
    }

    // ── 名称 ──────────────────────────────────────────────
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: &str) {
        self.name = name.to_string();
    }

    // ── 结构：行列数 ──────────────────────────────────────
    pub fn row_count(&self) -> u32 {
        self.row_count
    }

    pub fn column_count(&self) -> u32 {
        self.col_count
    }

    pub fn set_row_count(&mut self, n: u32) {
        let next = n.max(1);
        if next < self.row_count {
            self.cells.delete_rows(next, self.row_count - next);
        }
        self.row_count = next;
    }

    pub fn set_column_count(&mut self, n: u32) {
        let next = n.max(1);
        if next < self.col_count {
            self.cells.delete_columns(next, self.col_count - next);
        }
        self.col_count = next;
    }

    // ── 单元格：值 / 公式 / 样式 ─────────────────────────
    pub fn get_value(&self, row: u32, col: u32) -> Option<CellValue> {
        self.cells.get(row, col).and_then(|c| c.value.clone())
    }

    /// 设标量值（清除公式与富文本，对齐 Excel：直接输入值覆盖公式）。
    pub fn set_value(&mut self, row: u32, col: u32, value: Option<CellValue>) {
        match self.cells.get(row, col) {
            None => {
                if let Some(v) = value {
                    self.cells.insert(
                        row,
                        col,
                        CellData {
                            value: Some(v),
                            ..Default::default()
                        },
                    );
                }
            }
            Some(cur) => {
                let mut next = cur.clone();
                next.value = value;
                next.formula = None;
                next.rich = None;
                self.prune_and_set(row, col, next);
            }
        }
    }

    /// 富文本读（M13）。
    pub fn get_rich_text(&self, row: u32, col: u32) -> Option<RichText> {
        self.cells.get(row, col).and_then(|c| c.rich.clone())
    }

    /// 富文本写（M13）：存 runs，同步 value 设为拼接纯文本；rich=None 清富文本。
    pub fn set_rich_text(&mut self, row: u32, col: u32, rich: Option<RichText>) {
        let cur = self.cells.get(row, col).cloned();
        match rich {
            None => {
                if let Some(mut c) = cur {
                    if c.rich.is_some() {
                        c.rich = None;
                        self.prune_and_set(row, col, c);
                    }
                }
            }
            Some(rt) => {
                let plain = rt.to_plain();
                let mut next = cur.unwrap_or_default();
                next.value = Some(CellValue::Text(plain));
                next.rich = Some(rt);
                next.formula = None;
                self.prune_and_set(row, col, next);
            }
        }
    }

    pub fn get_formula(&self, row: u32, col: u32) -> String {
        self.cells
            .get(row, col)
            .and_then(|c| c.formula.clone())
            .unwrap_or_default()
    }

    /// 设公式（归一剥 '='）。空串清公式但保留 value/style。
    pub fn set_formula(&mut self, row: u32, col: u32, formula: &str) {
        let f = normalize_formula(formula);
        let cur = self.cells.get(row, col).cloned();
        if f.is_empty() {
            if let Some(mut c) = cur {
                c.formula = None;
                self.prune_and_set(row, col, c);
            }
            return;
        }
        let mut next = cur.unwrap_or_default();
        next.formula = Some(f);
        self.cells.insert(row, col, next);
    }

    /// 写公式格的计算值（display value），保留 formula 源。非公式格等同 set_value。
    pub fn set_computed_value(&mut self, row: u32, col: u32, value: Option<CellValue>) {
        match self.cells.get(row, col) {
            None => {
                if let Some(v) = value {
                    self.cells.insert(
                        row,
                        col,
                        CellData {
                            value: Some(v),
                            ..Default::default()
                        },
                    );
                }
            }
            Some(cur) => {
                let mut next = cur.clone();
                next.value = value;
                self.prune_and_set(row, col, next);
            }
        }
    }

    pub fn get_style(&self, row: u32, col: u32) -> Option<Style> {
        self.cells.get(row, col).and_then(|c| c.style.clone())
    }

    pub fn set_style(&mut self, row: u32, col: u32, style: Option<Style>) {
        match self.cells.get(row, col) {
            None => {
                if let Some(s) = style {
                    self.cells.insert(
                        row,
                        col,
                        CellData {
                            style: Some(s),
                            ..Default::default()
                        },
                    );
                }
            }
            Some(cur) => {
                let mut next = cur.clone();
                next.style = style;
                self.prune_and_set(row, col, next);
            }
        }
    }

    /// 叠加样式（与现有 merge，非替换）——对齐 TS CellRange.style()。
    pub fn merge_cell_style(&mut self, row: u32, col: u32, patch: &Style) {
        let cur = self.get_style(row, col);
        let merged = crate::style::merge_style(cur.as_ref(), Some(patch));
        self.set_style(row, col, Some(merged));
    }

    /// 读级联解析后的最终样式（sheet默认 < 列 < 行 < 单元格）。
    pub fn get_resolved_style(&self, row: u32, col: u32) -> Style {
        let cell_style = self.get_style(row, col);
        resolve_style(
            &self.style_sheet,
            &[
                Some(&self.default_style),
                self.col_styles.get(&col),
                self.row_styles.get(&row),
                cell_style.as_ref(),
            ],
        )
    }

    /// 读整条单元格数据副本，空返回 None。
    pub fn get_cell_data(&self, row: u32, col: u32) -> Option<CellData> {
        self.cells.get(row, col).cloned()
    }

    /// 若单元格只剩空壳，删除以保持稀疏。
    fn prune_and_set(&mut self, row: u32, col: u32, data: CellData) {
        if data.is_blank() {
            self.cells.set(row, col, None);
        } else {
            self.cells.insert(row, col, data);
        }
    }

    // ── 区域批量（替代 TS 的 CellRange 链式）─────────────
    /// 区域内所有格设同值。
    pub fn set_range_value(&mut self, range: Range, value: Option<CellValue>) {
        range.for_each_cell(|r, c| self.set_value(r, c, value.clone()));
    }

    /// 区域内所有格叠加样式（merge）。
    pub fn set_range_style(&mut self, range: Range, patch: &Style) {
        range.for_each_cell(|r, c| self.merge_cell_style(r, c, patch));
    }

    // ── 合并 span ────────────────────────────────────────
    pub fn add_span(&mut self, row: u32, col: u32, row_count: u32, col_count: u32) {
        if row_count < 1 || col_count < 1 {
            return;
        }
        if row_count == 1 && col_count == 1 {
            return;
        }
        let range = Range::new(row, col, row_count, col_count);
        // 移除与之相交的旧 span（新合并覆盖旧）
        self.spans.retain(|s| !s.as_range().intersects(&range));
        self.spans.push(Span {
            row: range.row,
            col: range.col,
            row_count: range.row_count,
            col_count: range.col_count,
        });
    }

    pub fn remove_span(&mut self, row: u32, col: u32) {
        self.spans.retain(|s| !s.as_range().contains_cell(row, col));
    }

    pub fn get_span(&self, row: u32, col: u32) -> Option<Span> {
        self.spans
            .iter()
            .find(|s| s.as_range().contains_cell(row, col))
            .copied()
    }

    pub fn get_spans(&self) -> Vec<Span> {
        self.spans.clone()
    }

    /// 把区域扩展到包含所有与之相交的合并区（迭代到不动点）。
    pub fn expand_range_to_spans(&self, range: Range) -> Range {
        let mut cur = range;
        let mut guard = 0;
        loop {
            let mut changed = false;
            for s in &self.spans {
                let sr = s.as_range();
                if sr.intersects(&cur) && !cur.contains_range(&sr) {
                    cur = cur.bounding_union(&sr);
                    changed = true;
                }
            }
            guard += 1;
            if !changed || guard >= 64 {
                break;
            }
        }
        cur
    }

    // ── 行高 / 列宽 / 可见性 ─────────────────────────────
    pub fn get_row_height(&self, row: u32) -> f64 {
        self.row_heights
            .get(&row)
            .copied()
            .unwrap_or(DEFAULT_ROW_HEIGHT)
    }

    pub fn set_row_height(&mut self, row: u32, px: f64) {
        self.row_heights.insert(row, px.max(0.0));
    }

    pub fn get_column_width(&self, col: u32) -> f64 {
        self.col_widths
            .get(&col)
            .copied()
            .unwrap_or(DEFAULT_COL_WIDTH)
    }

    pub fn set_column_width(&mut self, col: u32, px: f64) {
        self.col_widths.insert(col, px.max(0.0));
    }

    pub fn is_row_visible(&self, row: u32) -> bool {
        !self.hidden_rows.contains(&row)
    }

    pub fn set_row_visible(&mut self, row: u32, visible: bool) {
        if visible {
            self.hidden_rows.remove(&row);
        } else {
            self.hidden_rows.insert(row);
        }
    }

    pub fn is_column_visible(&self, col: u32) -> bool {
        !self.hidden_cols.contains(&col)
    }

    pub fn set_column_visible(&mut self, col: u32, visible: bool) {
        if visible {
            self.hidden_cols.remove(&col);
        } else {
            self.hidden_cols.insert(col);
        }
    }

    /// 按大纲折叠态刷新行列可见性。大纲隐藏与手动隐藏分账。
    pub fn apply_outline_visibility(&mut self) {
        let next_rows = self.row_outlines.hidden_indices(self.summary_below);
        let next_cols = self.column_outlines.hidden_indices(self.summary_right);
        for r in &self.outline_hidden_rows {
            if !next_rows.contains(r) {
                self.hidden_rows.remove(r);
            }
        }
        for c in &self.outline_hidden_cols {
            if !next_cols.contains(c) {
                self.hidden_cols.remove(c);
            }
        }
        for &r in &next_rows {
            self.hidden_rows.insert(r);
        }
        for &c in &next_cols {
            self.hidden_cols.insert(c);
        }
        self.outline_hidden_rows = next_rows;
        self.outline_hidden_cols = next_cols;
    }

    // ── 自动筛选（M11）────────────────────────────────────
    /// 设/清自动筛选区域（None=清空整个筛选态 + 恢复筛选隐藏行）。
    pub fn set_auto_filter(&mut self, range: Option<Range>) {
        match range {
            None => {
                self.auto_filter = None;
                self.apply_filter_visibility();
            }
            Some(r) => {
                self.auto_filter = Some(AutoFilterState {
                    range: r,
                    criteria: std::collections::BTreeMap::new(),
                });
            }
        }
    }

    /// 设某列筛选条件并重算可见性。
    pub fn set_filter_criterion(&mut self, col: u32, crit: FilterCriterion) {
        if let Some(af) = &mut self.auto_filter {
            af.criteria.insert(col, crit);
        }
        self.apply_filter_visibility();
    }

    /// 清所有筛选条件（保留筛选区域），恢复筛选隐藏行（不动手动/大纲隐藏）。
    pub fn clear_filters(&mut self) {
        if let Some(af) = &mut self.auto_filter {
            af.criteria.clear();
        }
        self.apply_filter_visibility();
    }

    /// 按自动筛选条件刷新行可见性。筛选隐藏与手动/大纲隐藏分开记账。数据区首行为表头。
    pub fn apply_filter_visibility(&mut self) {
        let mut next: HashSet<u32> = HashSet::new();
        if let Some(af) = &self.auto_filter {
            if !af.criteria.is_empty() {
                let header_row = af.range.row;
                let r1 = af.range.row + 1;
                let r2 = af.range.last_row();
                let top_thresholds = self.compute_topn_thresholds(af, r1, r2);
                for r in r1..=r2 {
                    if r == header_row {
                        continue;
                    }
                    if !self.row_passes_filter(r, af, &top_thresholds) {
                        next.insert(r);
                    }
                }
            }
        }
        // 撤销上轮筛选隐藏（不动手动/大纲隐藏）
        for r in &self.filter_hidden_rows {
            if !next.contains(r) && !self.outline_hidden_rows.contains(r) {
                self.hidden_rows.remove(r);
            }
        }
        for &r in &next {
            self.hidden_rows.insert(r);
        }
        self.filter_hidden_rows = next;
    }

    /// 预计算各列 topN 阈值（第 N 大值；不足 N 个取最小值）。
    fn compute_topn_thresholds(&self, af: &AutoFilterState, r1: u32, r2: u32) -> HashMap<u32, f64> {
        let mut out = HashMap::new();
        for (&col, crit) in &af.criteria {
            let Some(cond) = &crit.condition else {
                continue;
            };
            if cond.op != FilterOp::TopN {
                continue;
            }
            let n = cond
                .value2
                .as_ref()
                .or(cond.value.as_ref())
                .and_then(|v| v.as_number())
                .unwrap_or(10.0)
                .max(1.0) as usize;
            let mut nums: Vec<f64> = Vec::new();
            for r in r1..=r2 {
                if let Some(CellValue::Number(v)) = self.get_value(r, col) {
                    nums.push(v);
                }
            }
            nums.sort_by(|a, b| b.partial_cmp(a).unwrap());
            let th = if nums.is_empty() {
                f64::NEG_INFINITY
            } else {
                nums[n.min(nums.len()) - 1]
            };
            out.insert(col, th);
        }
        out
    }

    /// 某行是否通过所有列条件（AND）。
    fn row_passes_filter(&self, row: u32, af: &AutoFilterState, top: &HashMap<u32, f64>) -> bool {
        for (&col, crit) in &af.criteria {
            let text = crate::find::cell_display(self.get_value(row, col).as_ref());
            if let Some(vals) = &crit.values {
                if !vals.is_empty() && !vals.contains(&text) {
                    return false;
                }
            }
            if let Some(cond) = &crit.condition {
                if cond.op == FilterOp::TopN {
                    let v = self.get_value(row, col).and_then(|c| c.as_number());
                    let th = top.get(&col).copied().unwrap_or(f64::NEG_INFINITY);
                    match v {
                        Some(n) if n >= th => {}
                        _ => return false,
                    }
                } else if !match_filter_condition(&text, self.get_value(row, col).as_ref(), cond) {
                    return false;
                }
            }
        }
        true
    }

    /// 列出筛选区域内某列的唯一显示值（供筛选下拉，字典序）。
    pub fn filter_unique_values(&self, col: u32) -> Vec<String> {
        let Some(af) = &self.auto_filter else {
            return Vec::new();
        };
        let mut set: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let r1 = af.range.row + 1;
        let r2 = af.range.last_row();
        for r in r1..=r2 {
            set.insert(crate::find::cell_display(self.get_value(r, col).as_ref()));
        }
        set.into_iter().collect()
    }

    // ── 数据验证（M12）────────────────────────────────────
    /// 加/覆盖一条数据验证规则（同区域旧规则先移除）。
    pub fn set_data_validation(&mut self, rule: DataValidation) {
        self.validations.retain(|v| v.range != rule.range);
        self.validations.push(rule);
    }

    /// 命中某格的验证规则（后加入优先，返回最后一条覆盖该格的）。
    pub fn get_validation_at(&self, row: u32, col: u32) -> Option<&DataValidation> {
        self.validations
            .iter()
            .rev()
            .find(|v| v.range.contains(row, col))
    }

    /// 列出全部验证规则（IO 用）。
    pub fn list_validations(&self) -> &[DataValidation] {
        &self.validations
    }

    /// 清除某区域验证规则（None=清全部）。
    pub fn clear_data_validation(&mut self, range: Option<RegionRect>) {
        match range {
            None => self.validations.clear(),
            Some(r) => self.validations.retain(|v| v.range != r),
        }
    }

    // ── 超链接（M12）──────────────────────────────────────
    pub fn set_hyperlink(&mut self, row: u32, col: u32, link: Option<Hyperlink>) {
        match link {
            Some(l) => {
                self.hyperlinks.insert((row, col), l);
            }
            None => {
                self.hyperlinks.remove(&(row, col));
            }
        }
    }

    pub fn get_hyperlink(&self, row: u32, col: u32) -> Option<&Hyperlink> {
        self.hyperlinks.get(&(row, col))
    }

    /// 列出全部超链接（按坐标升序，IO/断言稳定）。
    pub fn list_hyperlinks(&self) -> Vec<(u32, u32, Hyperlink)> {
        let mut v: Vec<_> = self
            .hyperlinks
            .iter()
            .map(|(&(r, c), l)| (r, c, l.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        v
    }

    // ── 条件格式（M13）────────────────────────────────────
    /// 加一条条件格式规则。
    pub fn add_conditional_rule(&mut self, rule: ConditionalRule) {
        self.conditional_rules.push(rule);
    }

    /// 移除第 index 条规则。
    pub fn remove_conditional_rule(&mut self, index: usize) {
        if index < self.conditional_rules.len() {
            self.conditional_rules.remove(index);
        }
    }

    /// 列出全部条件格式规则。
    pub fn list_conditional_rules(&self) -> &[ConditionalRule] {
        &self.conditional_rules
    }

    /// 清全部条件格式规则。
    pub fn clear_conditional_rules(&mut self) {
        self.conditional_rules.clear();
    }

    // ── 单元格批注（M14）──────────────────────────────────
    pub fn set_comment(&mut self, row: u32, col: u32, comment: Option<CellComment>) {
        match comment {
            Some(c) => {
                self.comments.insert((row, col), c);
            }
            None => {
                self.comments.remove(&(row, col));
            }
        }
    }

    pub fn get_comment(&self, row: u32, col: u32) -> Option<&CellComment> {
        self.comments.get(&(row, col))
    }

    /// 列出全部批注（按坐标升序）。
    pub fn list_comments(&self) -> Vec<(u32, u32, CellComment)> {
        let mut v: Vec<_> = self
            .comments
            .iter()
            .map(|(&(r, c), cm)| (r, c, cm.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        v
    }

    // ── 浮动对象（M14）────────────────────────────────────
    /// 加/覆盖浮动对象（同 id 覆盖）。
    pub fn add_floating_object(&mut self, obj: FloatingObject) {
        self.floating_objects.retain(|o| o.id != obj.id);
        self.floating_objects.push(obj);
    }

    pub fn remove_floating_object(&mut self, id: &str) {
        self.floating_objects.retain(|o| o.id != id);
    }

    pub fn get_floating_object(&self, id: &str) -> Option<&FloatingObject> {
        self.floating_objects.iter().find(|o| o.id == id)
    }

    /// 列出全部浮动对象（按 z 升序，渲染顺序）。
    pub fn list_floating_objects(&self) -> Vec<FloatingObject> {
        let mut v = self.floating_objects.clone();
        v.sort_by(|a, b| a.z.unwrap_or(0.0).partial_cmp(&b.z.unwrap_or(0.0)).unwrap());
        v
    }

    pub fn clear_floating_objects(&mut self) {
        self.floating_objects.clear();
    }

    // ── 页面设置（M15）────────────────────────────────────
    pub fn set_page_setup(&mut self, setup: Option<PageSetup>) {
        self.page_setup = setup;
    }

    pub fn get_page_setup(&self) -> Option<&PageSetup> {
        self.page_setup.as_ref()
    }

    // ── 工作表保护（M20）────────────────────────────────
    /// 某格是否锁定（Excel 语义：缺省锁定，仅 locked==Some(false) 解锁；级联解析后看 locked 键）。
    pub fn is_cell_locked(&self, row: u32, col: u32) -> bool {
        self.get_resolved_style(row, col).locked != Some(false)
    }

    /// 设/清工作表保护（None 或 {enabled:false} 均解除）。
    pub fn set_protection(&mut self, protection: Option<SheetProtection>) {
        self.protection = protection;
    }

    /// 读保护态。
    pub fn protection(&self) -> Option<&SheetProtection> {
        self.protection.as_ref()
    }

    /// 是否处于保护态（enabled）。
    pub fn is_protected(&self) -> bool {
        self.protection.as_ref().is_some_and(|p| p.enabled)
    }

    /// 某格是否可编辑：未保护恒可；保护时仅解锁格可编辑。
    pub fn can_edit_cell(&self, row: u32, col: u32) -> bool {
        !self.is_protected() || !self.is_cell_locked(row, col)
    }

    /// 区域内是否含锁定格（未保护恒 false）。
    pub fn range_has_locked(&self, row: u32, col: u32, row_count: u32, col_count: u32) -> bool {
        if !self.is_protected() {
            return false;
        }
        for r in row..row + row_count {
            for c in col..col + col_count {
                if self.is_cell_locked(r, c) {
                    return true;
                }
            }
        }
        false
    }

    // ── 迷你图（M21）─────────────────────────────────────
    pub fn set_sparkline(&mut self, row: u32, col: u32, spec: Sparkline) {
        self.sparklines.insert((row, col), spec);
    }

    /// 读某格迷你图（副本），无返回 None。
    pub fn get_sparkline(&self, row: u32, col: u32) -> Option<Sparkline> {
        self.sparklines.get(&(row, col)).cloned()
    }

    pub fn clear_sparkline(&mut self, row: u32, col: u32) {
        self.sparklines.remove(&(row, col));
    }

    /// 列出全部迷你图（按坐标升序）。
    pub fn list_sparklines(&self) -> Vec<(u32, u32, Sparkline)> {
        let mut v: Vec<_> = self
            .sparklines
            .iter()
            .map(|(&(r, c), s)| (r, c, s.clone()))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        v
    }

    // ── 行/列默认样式 & sheet 默认样式 ───────────────────
    pub fn set_default_style(&mut self, style: Style) {
        self.default_style = style;
    }

    pub fn set_row_style(&mut self, row: u32, style: Option<Style>) {
        match style {
            Some(s) => {
                self.row_styles.insert(row, s);
            }
            None => {
                self.row_styles.remove(&row);
            }
        }
    }

    pub fn set_column_style(&mut self, col: u32, style: Option<Style>) {
        match style {
            Some(s) => {
                self.col_styles.insert(col, s);
            }
            None => {
                self.col_styles.remove(&col);
            }
        }
    }

    // ── 行列增删（同步搬移一切）─────────────────────────
    pub fn add_rows(&mut self, before: u32, count: u32) {
        if count < 1 {
            return;
        }
        self.cells.insert_rows(before, count);
        shift_map_keys(&mut self.row_heights, before, count);
        shift_set(&mut self.hidden_rows, before, count);
        shift_map_keys(&mut self.row_styles, before, count);
        for s in &mut self.spans {
            if s.row >= before {
                s.row += count;
            }
        }
        self.row_outlines.shift_insert(before, count);
        self.row_count += count;
    }

    pub fn delete_rows(&mut self, start: u32, count: u32) {
        if count < 1 {
            return;
        }
        self.cells.delete_rows(start, count);
        delete_map_keys(&mut self.row_heights, start, count);
        delete_set(&mut self.hidden_rows, start, count);
        delete_map_keys(&mut self.row_styles, start, count);
        let end = start + count;
        self.spans.retain(|s| !(s.row >= start && s.row < end));
        for s in &mut self.spans {
            if s.row >= end {
                s.row -= count;
            }
        }
        self.row_outlines.shift_delete(start, count);
        self.row_count = self.row_count.saturating_sub(count).max(1);
    }

    pub fn add_columns(&mut self, before: u32, count: u32) {
        if count < 1 {
            return;
        }
        self.cells.insert_columns(before, count);
        shift_map_keys(&mut self.col_widths, before, count);
        shift_set(&mut self.hidden_cols, before, count);
        shift_map_keys(&mut self.col_styles, before, count);
        for s in &mut self.spans {
            if s.col >= before {
                s.col += count;
            }
        }
        self.column_outlines.shift_insert(before, count);
        self.col_count += count;
    }

    pub fn delete_columns(&mut self, start: u32, count: u32) {
        if count < 1 {
            return;
        }
        self.cells.delete_columns(start, count);
        delete_map_keys(&mut self.col_widths, start, count);
        delete_set(&mut self.hidden_cols, start, count);
        delete_map_keys(&mut self.col_styles, start, count);
        let end = start + count;
        self.spans.retain(|s| !(s.col >= start && s.col < end));
        for s in &mut self.spans {
            if s.col >= end {
                s.col -= count;
            }
        }
        self.column_outlines.shift_delete(start, count);
        self.col_count = self.col_count.saturating_sub(count).max(1);
    }

    // ── 选区 ─────────────────────────────────────────────
    pub fn get_selections(&self) -> Vec<Range> {
        self.selections.clone()
    }

    pub fn set_selection(&mut self, row: u32, col: u32, row_count: u32, col_count: u32) {
        self.selections = vec![Range::new(row, col, row_count, col_count)];
        self.active_row = row;
        self.active_col = col;
    }

    /// 便捷：设单格选区。
    pub fn set_selection_cell(&mut self, row: u32, col: u32) {
        self.set_selection(row, col, 1, 1);
    }

    pub fn add_selection(&mut self, row: u32, col: u32, row_count: u32, col_count: u32) {
        self.selections
            .push(Range::new(row, col, row_count, col_count));
        self.active_row = row;
        self.active_col = col;
    }

    pub fn clear_selections(&mut self) {
        self.selections = vec![Range::new(self.active_row, self.active_col, 1, 1)];
    }

    pub fn active_row_index(&self) -> u32 {
        self.active_row
    }

    pub fn active_column_index(&self) -> u32 {
        self.active_col
    }

    pub fn set_active_cell(&mut self, row: u32, col: u32) {
        self.active_row = row;
        self.active_col = col;
    }

    /// 活动格 A1 地址。
    pub fn active_addr(&self) -> String {
        format_addr(self.active_row, self.active_col)
    }

    // ── 缩放 ─────────────────────────────────────────────
    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn set_zoom(&mut self, factor: f64) {
        self.zoom = factor.clamp(0.1, 4.0);
    }

    // ── 快照读取（供 io 层中性 snapshot；只读副本）─────────
    pub fn get_default_style(&self) -> Style {
        self.default_style.clone()
    }

    /// 非默认行高 [row, px]（按行升序）。
    pub fn row_height_entries(&self) -> Vec<(u32, f64)> {
        let mut v: Vec<_> = self.row_heights.iter().map(|(&k, &px)| (k, px)).collect();
        v.sort_by_key(|&(k, _)| k);
        v
    }

    pub fn column_width_entries(&self) -> Vec<(u32, f64)> {
        let mut v: Vec<_> = self.col_widths.iter().map(|(&k, &px)| (k, px)).collect();
        v.sort_by_key(|&(k, _)| k);
        v
    }

    pub fn row_style_entries(&self) -> Vec<(u32, Style)> {
        let mut v: Vec<_> = self
            .row_styles
            .iter()
            .map(|(&k, s)| (k, s.clone()))
            .collect();
        v.sort_by_key(|&(k, _)| k);
        v
    }

    pub fn column_style_entries(&self) -> Vec<(u32, Style)> {
        let mut v: Vec<_> = self
            .col_styles
            .iter()
            .map(|(&k, s)| (k, s.clone()))
            .collect();
        v.sort_by_key(|&(k, _)| k);
        v
    }

    /// 手动隐藏的行（排除大纲折叠隐藏与筛选隐藏）。
    pub fn manual_hidden_rows(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .hidden_rows
            .iter()
            .copied()
            .filter(|r| {
                !self.outline_hidden_rows.contains(r) && !self.filter_hidden_rows.contains(r)
            })
            .collect();
        v.sort_unstable();
        v
    }

    pub fn manual_hidden_columns(&self) -> Vec<u32> {
        let mut v: Vec<u32> = self
            .hidden_cols
            .iter()
            .copied()
            .filter(|c| !self.outline_hidden_cols.contains(c))
            .collect();
        v.sort_unstable();
        v
    }

    // ── 遍历非空单元格 ───────────────────────────────────
    pub fn for_each_cell<F: FnMut(&CellData, u32, u32)>(&self, f: F) {
        self.cells.for_each(f);
    }

    /// 非空单元格数（调试/断言用）。
    pub fn cell_count(&self) -> usize {
        self.cells.size()
    }
}

/// 筛选条件匹配（非 topN；topN 在 row_passes_filter 内单独判）。对齐 TS matchCondition。
fn match_filter_condition(text: &str, raw: Option<&CellValue>, cond: &FilterCondition) -> bool {
    let num = match raw {
        Some(CellValue::Number(n)) => Some(*n),
        _ => text.parse::<f64>().ok(),
    };
    let cv = cond.value.as_ref();
    let cv_num = cv.and_then(|v| match v {
        CellValue::Number(n) => Some(*n),
        CellValue::Text(s) => s.parse::<f64>().ok(),
        _ => None,
    });
    let t = text.to_lowercase();
    let cv_str = cv.map(|v| v.to_text().to_lowercase()).unwrap_or_default();
    let is_numeric_cv = matches!(cv, Some(CellValue::Number(_))) || cv_num.is_some();
    match cond.op {
        FilterOp::Eq => {
            if is_numeric_cv {
                num == cv_num
            } else {
                t == cv_str
            }
        }
        FilterOp::Ne => {
            if is_numeric_cv {
                num != cv_num
            } else {
                t != cv_str
            }
        }
        FilterOp::Gt => matches!((num, cv_num), (Some(a), Some(b)) if a > b),
        FilterOp::Ge => matches!((num, cv_num), (Some(a), Some(b)) if a >= b),
        FilterOp::Lt => matches!((num, cv_num), (Some(a), Some(b)) if a < b),
        FilterOp::Le => matches!((num, cv_num), (Some(a), Some(b)) if a <= b),
        FilterOp::Contains => t.contains(&cv_str),
        FilterOp::NotContains => !t.contains(&cv_str),
        FilterOp::StartsWith => t.starts_with(&cv_str),
        FilterOp::EndsWith => t.ends_with(&cv_str),
        FilterOp::Between => {
            let hi = cond.value2.as_ref().and_then(|v| v.as_number());
            matches!((num, cv_num, hi), (Some(a), Some(lo), Some(h)) if a >= lo && a <= h)
        }
        FilterOp::TopN => true, // 由 row_passes_filter 处理
    }
}

// ── 行列增删的键搬移助手（对齐 TS 的 shiftMapKeys/deleteMapKeys/shiftSet/deleteSet）──

fn shift_map_keys<V>(m: &mut HashMap<u32, V>, before: u32, count: u32) {
    let old = std::mem::take(m);
    for (k, v) in old {
        let nk = if k >= before { k + count } else { k };
        m.insert(nk, v);
    }
}

fn delete_map_keys<V>(m: &mut HashMap<u32, V>, start: u32, count: u32) {
    let end = start + count;
    let old = std::mem::take(m);
    for (k, v) in old {
        if k >= start && k < end {
            continue;
        }
        let nk = if k >= end { k - count } else { k };
        m.insert(nk, v);
    }
}

fn shift_set(s: &mut HashSet<u32>, before: u32, count: u32) {
    let old = std::mem::take(s);
    for k in old {
        s.insert(if k >= before { k + count } else { k });
    }
}

fn delete_set(s: &mut HashSet<u32>, start: u32, count: u32) {
    let end = start + count;
    let old = std::mem::take(s);
    for k in old {
        if k >= start && k < end {
            continue;
        }
        s.insert(if k >= end { k - count } else { k });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::HAlign;

    #[test]
    fn set_get_values() {
        let mut ws = Worksheet::new("S1");
        ws.set_value(0, 0, Some(42.into()));
        ws.set_value(1, 1, Some("hi".into()));
        assert_eq!(ws.get_value(0, 0), Some(CellValue::Number(42.0)));
        assert_eq!(ws.get_value(1, 1), Some(CellValue::Text("hi".into())));
        assert_eq!(ws.get_value(2, 2), None);
    }

    #[test]
    fn set_value_clears_formula() {
        let mut ws = Worksheet::new("S1");
        ws.set_formula(0, 0, "=SUM(A2:A3)");
        assert_eq!(ws.get_formula(0, 0), "SUM(A2:A3)");
        ws.set_value(0, 0, Some(5.into()));
        assert_eq!(ws.get_formula(0, 0), "");
        assert_eq!(ws.get_value(0, 0), Some(CellValue::Number(5.0)));
    }

    #[test]
    fn set_formula_normalizes_equals() {
        let mut ws = Worksheet::new("S1");
        ws.set_formula(0, 0, "=A1+B1");
        assert_eq!(ws.get_formula(0, 0), "A1+B1");
    }

    #[test]
    fn clearing_formula_prunes() {
        let mut ws = Worksheet::new("S1");
        ws.set_formula(0, 0, "A1");
        ws.set_formula(0, 0, "");
        assert_eq!(ws.cell_count(), 0);
    }

    #[test]
    fn null_value_on_empty_no_slot() {
        let mut ws = Worksheet::new("S1");
        ws.set_value(0, 0, None);
        assert_eq!(ws.cell_count(), 0);
    }

    #[test]
    fn stores_and_reads_style() {
        let mut ws = Worksheet::new("S1");
        ws.set_style(
            0,
            0,
            Some(Style {
                bold: Some(true),
                ..Default::default()
            }),
        );
        assert_eq!(
            ws.get_style(0, 0),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
    }

    #[test]
    fn resolves_cascade() {
        let mut ws = Worksheet::new("S1");
        ws.set_default_style(Style {
            font_family: Some("Arial".into()),
            font_size: Some(10.0),
            ..Default::default()
        });
        ws.set_column_style(
            0,
            Some(Style {
                font_size: Some(12.0),
                ..Default::default()
            }),
        );
        ws.set_row_style(
            0,
            Some(Style {
                bold: Some(true),
                ..Default::default()
            }),
        );
        ws.set_style(
            0,
            0,
            Some(Style {
                font_size: Some(14.0),
                ..Default::default()
            }),
        );
        assert_eq!(
            ws.get_resolved_style(0, 0),
            Style {
                font_family: Some("Arial".into()),
                font_size: Some(14.0),
                bold: Some(true),
                ..Default::default()
            }
        );
    }

    #[test]
    fn range_value_and_style() {
        let mut ws = Worksheet::new("S1");
        let r = Range::new(0, 0, 2, 2);
        ws.set_range_value(r, Some(7.into()));
        ws.set_range_style(
            r,
            &Style {
                bold: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(ws.get_value(0, 0), Some(CellValue::Number(7.0)));
        assert_eq!(ws.get_value(1, 1), Some(CellValue::Number(7.0)));
        assert_eq!(
            ws.get_style(1, 1),
            Some(Style {
                bold: Some(true),
                ..Default::default()
            })
        );
    }

    #[test]
    fn style_merges_not_replaces() {
        let mut ws = Worksheet::new("S1");
        ws.merge_cell_style(
            0,
            0,
            &Style {
                bold: Some(true),
                ..Default::default()
            },
        );
        ws.merge_cell_style(
            0,
            0,
            &Style {
                italic: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            ws.get_style(0, 0),
            Some(Style {
                bold: Some(true),
                italic: Some(true),
                ..Default::default()
            })
        );
    }

    #[test]
    fn spans_add_query() {
        let mut ws = Worksheet::new("S1");
        ws.add_span(0, 0, 2, 3);
        assert_eq!(
            ws.get_span(0, 0),
            Some(Span {
                row: 0,
                col: 0,
                row_count: 2,
                col_count: 3
            })
        );
        assert_eq!(
            ws.get_span(1, 2),
            Some(Span {
                row: 0,
                col: 0,
                row_count: 2,
                col_count: 3
            })
        );
        assert_eq!(ws.get_span(5, 5), None);
    }

    #[test]
    fn spans_ignore_1x1() {
        let mut ws = Worksheet::new("S1");
        ws.add_span(0, 0, 1, 1);
        assert_eq!(ws.get_spans().len(), 0);
    }

    #[test]
    fn spans_overlap_replaces() {
        let mut ws = Worksheet::new("S1");
        ws.add_span(0, 0, 2, 2);
        ws.add_span(1, 1, 2, 2);
        assert_eq!(ws.get_spans().len(), 1);
        assert!(ws.get_span(2, 2).is_some());
    }

    #[test]
    fn remove_span_covering() {
        let mut ws = Worksheet::new("S1");
        ws.add_span(0, 0, 2, 2);
        ws.remove_span(1, 1);
        assert_eq!(ws.get_spans().len(), 0);
    }

    #[test]
    fn row_col_metadata_defaults() {
        let mut ws = Worksheet::new("S1");
        assert_eq!(ws.get_row_height(0), 20.0);
        assert_eq!(ws.get_column_width(0), 62.0);
        ws.set_row_height(0, 30.0);
        ws.set_column_width(0, 120.0);
        assert_eq!(ws.get_row_height(0), 30.0);
        assert_eq!(ws.get_column_width(0), 120.0);
    }

    #[test]
    fn row_col_visibility() {
        let mut ws = Worksheet::new("S1");
        assert!(ws.is_row_visible(2));
        ws.set_row_visible(2, false);
        assert!(!ws.is_row_visible(2));
        ws.set_column_visible(1, false);
        assert!(!ws.is_column_visible(1));
    }

    #[test]
    fn add_rows_shifts_everything() {
        let mut ws = Worksheet::with_size("S1", 10, 12);
        ws.set_value(2, 0, Some("r2".into()));
        ws.set_row_height(2, 44.0);
        ws.set_row_style(
            2,
            Some(Style {
                bold: Some(true),
                ..Default::default()
            }),
        );
        ws.add_span(2, 0, 1, 2);
        ws.add_rows(1, 2);
        assert_eq!(ws.row_count(), 12);
        assert_eq!(ws.get_value(4, 0), Some(CellValue::Text("r2".into())));
        assert_eq!(ws.get_row_height(4), 44.0);
        assert_eq!(
            ws.get_resolved_style(4, 0),
            Style {
                bold: Some(true),
                ..Default::default()
            }
        );
        assert!(ws.get_span(4, 0).is_some());
    }

    #[test]
    fn delete_rows_drops_and_shifts() {
        let mut ws = Worksheet::with_size("S1", 10, 12);
        ws.set_value(0, 0, Some("r0".into()));
        ws.set_value(3, 0, Some("r3".into()));
        ws.delete_rows(1, 2);
        assert_eq!(ws.row_count(), 8);
        assert_eq!(ws.get_value(0, 0), Some(CellValue::Text("r0".into())));
        assert_eq!(ws.get_value(1, 0), Some(CellValue::Text("r3".into())));
    }

    #[test]
    fn add_delete_columns() {
        let mut ws = Worksheet::with_size("S1", 40, 8);
        ws.set_value(0, 2, Some("c2".into()));
        ws.set_column_width(2, 99.0);
        ws.add_columns(1, 1);
        assert_eq!(ws.column_count(), 9);
        assert_eq!(ws.get_value(0, 3), Some(CellValue::Text("c2".into())));
        assert_eq!(ws.get_column_width(3), 99.0);
        ws.delete_columns(0, 1);
        assert_eq!(ws.get_value(0, 2), Some(CellValue::Text("c2".into())));
    }

    #[test]
    fn set_row_count_shrink_discards() {
        let mut ws = Worksheet::with_size("S1", 10, 12);
        ws.set_value(8, 0, Some("x".into()));
        ws.set_row_count(5);
        assert_eq!(ws.get_value(8, 0), None);
        assert_eq!(ws.cell_count(), 0);
    }

    #[test]
    fn selection_defaults_a1() {
        let ws = Worksheet::new("S1");
        assert_eq!(ws.active_addr(), "A1");
        assert_eq!(ws.get_selections()[0], Range::new(0, 0, 1, 1));
    }

    #[test]
    fn set_selection_updates_active() {
        let mut ws = Worksheet::new("S1");
        ws.set_selection(2, 3, 2, 2);
        assert_eq!(ws.active_row_index(), 2);
        assert_eq!(ws.active_column_index(), 3);
        assert_eq!(ws.active_addr(), "D3");
    }

    #[test]
    fn multi_range_selection() {
        let mut ws = Worksheet::new("S1");
        ws.set_selection_cell(0, 0);
        ws.add_selection(5, 5, 2, 2);
        assert_eq!(ws.get_selections().len(), 2);
    }

    #[test]
    fn outline_groups_shift_on_insert() {
        let mut ws = Worksheet::with_size("S1", 20, 12);
        ws.row_outlines.group(3, 4);
        ws.add_rows(0, 2);
        let g = &ws.row_outlines.list()[0];
        assert_eq!((g.start, g.count), (5, 4));
    }

    #[test]
    fn zoom_clamps() {
        let mut ws = Worksheet::new("S1");
        assert_eq!(ws.zoom(), 1.0);
        ws.set_zoom(2.0);
        assert_eq!(ws.zoom(), 2.0);
        ws.set_zoom(99.0);
        assert_eq!(ws.zoom(), 4.0);
        ws.set_zoom(0.0);
        assert_eq!(ws.zoom(), 0.1);
    }

    #[test]
    fn cascade_with_named_style() {
        let mut ws = Worksheet::new("S1");
        ws.style_sheet.define(
            "emph",
            Style {
                bold: Some(true),
                fore_color: Some("#c00".into()),
                ..Default::default()
            },
        );
        ws.set_style(
            0,
            0,
            Some(Style {
                style_name: Some("emph".into()),
                h_align: Some(HAlign::Center),
                ..Default::default()
            }),
        );
        let r = ws.get_resolved_style(0, 0);
        assert_eq!(r.bold, Some(true));
        assert_eq!(r.fore_color, Some("#c00".into()));
        assert_eq!(r.h_align, Some(HAlign::Center));
    }

    // ── M11 自动筛选 ──
    fn filter_sheet() -> Worksheet {
        let mut ws = Worksheet::with_size("S", 20, 8);
        ws.set_value(0, 0, Some("类别".into()));
        ws.set_value(0, 1, Some("金额".into()));
        ws.set_value(1, 0, Some("A".into()));
        ws.set_value(1, 1, Some(100.into()));
        ws.set_value(2, 0, Some("B".into()));
        ws.set_value(2, 1, Some(200.into()));
        ws.set_value(3, 0, Some("A".into()));
        ws.set_value(3, 1, Some(300.into()));
        ws.set_value(4, 0, Some("C".into()));
        ws.set_value(4, 1, Some(50.into()));
        ws
    }

    #[test]
    fn filter_by_values() {
        let mut ws = filter_sheet();
        ws.set_auto_filter(Some(Range::new(0, 0, 5, 2)));
        ws.set_filter_criterion(
            0,
            FilterCriterion {
                values: Some(vec!["A".into()]),
                condition: None,
            },
        );
        assert!(ws.is_row_visible(0)); // 表头
        assert!(ws.is_row_visible(1)); // A
        assert!(!ws.is_row_visible(2)); // B
        assert!(ws.is_row_visible(3)); // A
        assert!(!ws.is_row_visible(4)); // C
    }

    #[test]
    fn filter_by_condition_gt() {
        let mut ws = filter_sheet();
        ws.set_auto_filter(Some(Range::new(0, 0, 5, 2)));
        ws.set_filter_criterion(
            1,
            FilterCriterion {
                values: None,
                condition: Some(FilterCondition {
                    op: FilterOp::Gt,
                    value: Some(150.into()),
                    value2: None,
                }),
            },
        );
        assert!(!ws.is_row_visible(1)); // 100
        assert!(ws.is_row_visible(2)); // 200
        assert!(ws.is_row_visible(3)); // 300
        assert!(!ws.is_row_visible(4)); // 50
    }

    #[test]
    fn clear_filters_restores() {
        let mut ws = filter_sheet();
        ws.set_auto_filter(Some(Range::new(0, 0, 5, 2)));
        ws.set_filter_criterion(
            0,
            FilterCriterion {
                values: Some(vec!["A".into()]),
                condition: None,
            },
        );
        assert!(!ws.is_row_visible(2));
        ws.clear_filters();
        for r in 0..5 {
            assert!(ws.is_row_visible(r));
        }
    }

    #[test]
    fn filter_separates_from_manual_hidden() {
        let mut ws = filter_sheet();
        ws.set_row_visible(3, false); // 手动隐藏
        ws.set_auto_filter(Some(Range::new(0, 0, 5, 2)));
        ws.set_filter_criterion(
            0,
            FilterCriterion {
                values: Some(vec!["A".into(), "B".into(), "C".into()]),
                condition: None,
            },
        );
        ws.clear_filters();
        assert!(!ws.is_row_visible(3)); // 手动隐藏保留
    }

    #[test]
    fn filter_topn() {
        let mut ws = filter_sheet();
        ws.set_auto_filter(Some(Range::new(0, 0, 5, 2)));
        ws.set_filter_criterion(
            1,
            FilterCriterion {
                values: None,
                condition: Some(FilterCondition {
                    op: FilterOp::TopN,
                    value: None,
                    value2: Some(2.into()),
                }),
            },
        );
        assert!(ws.is_row_visible(2)); // 200
        assert!(ws.is_row_visible(3)); // 300
        assert!(!ws.is_row_visible(1)); // 100
        assert!(!ws.is_row_visible(4)); // 50
    }

    #[test]
    fn filter_unique_values_list() {
        let mut ws = filter_sheet();
        ws.set_auto_filter(Some(Range::new(0, 0, 5, 2)));
        assert_eq!(ws.filter_unique_values(0), vec!["A", "B", "C"]);
    }

    // ── M12 数据验证 / 超链接态 ──
    #[test]
    fn validation_hits_region() {
        let mut ws = Worksheet::with_size("S", 10, 6);
        ws.set_data_validation(DataValidation {
            range: RegionRect::new(1, 1, 3, 2),
            validation_type: ValidationType::List,
            operator: None,
            formula1: None,
            formula2: None,
            list: Some(vec!["x".into()]),
            allow_blank: None,
            prompt: None,
            error: None,
        });
        assert_eq!(
            ws.get_validation_at(1, 1).map(|v| v.validation_type),
            Some(ValidationType::List)
        );
        assert_eq!(
            ws.get_validation_at(3, 2).map(|v| v.validation_type),
            Some(ValidationType::List)
        );
        assert!(ws.get_validation_at(5, 5).is_none());
    }

    #[test]
    fn later_validation_overrides() {
        let mut ws = Worksheet::with_size("S", 10, 6);
        ws.set_data_validation(DataValidation {
            range: RegionRect::new(0, 0, 5, 5),
            validation_type: ValidationType::Whole,
            operator: None,
            formula1: None,
            formula2: None,
            list: None,
            allow_blank: None,
            prompt: None,
            error: None,
        });
        ws.set_data_validation(DataValidation {
            range: RegionRect::new(0, 0, 1, 1),
            validation_type: ValidationType::List,
            operator: None,
            formula1: None,
            formula2: None,
            list: Some(vec!["a".into()]),
            allow_blank: None,
            prompt: None,
            error: None,
        });
        assert_eq!(
            ws.get_validation_at(0, 0).map(|v| v.validation_type),
            Some(ValidationType::List)
        );
        assert_eq!(
            ws.get_validation_at(2, 2).map(|v| v.validation_type),
            Some(ValidationType::Whole)
        );
    }

    #[test]
    fn clear_validation() {
        let mut ws = Worksheet::with_size("S", 10, 6);
        let range = RegionRect::new(0, 0, 2, 2);
        ws.set_data_validation(DataValidation {
            range,
            validation_type: ValidationType::List,
            operator: None,
            formula1: None,
            formula2: None,
            list: Some(vec!["a".into()]),
            allow_blank: None,
            prompt: None,
            error: None,
        });
        ws.clear_data_validation(Some(range));
        assert!(ws.get_validation_at(0, 0).is_none());
    }

    #[test]
    fn hyperlink_get_set_list() {
        let mut ws = Worksheet::with_size("S", 10, 6);
        ws.set_hyperlink(
            1,
            1,
            Some(Hyperlink {
                url: "https://example.com".into(),
                tooltip: None,
            }),
        );
        assert_eq!(
            ws.get_hyperlink(1, 1).map(|l| l.url.as_str()),
            Some("https://example.com")
        );
        assert_eq!(ws.list_hyperlinks().len(), 1);
        ws.set_hyperlink(1, 1, None);
        assert!(ws.get_hyperlink(1, 1).is_none());
    }

    // ── M14 批注 / 浮动对象 ──
    fn m14_anchor(fr: u32, fc: u32, tr: u32, tc: u32) -> ObjAnchor {
        ObjAnchor {
            from_row: fr,
            from_col: fc,
            to_row: tr,
            to_col: tc,
            from_dx: None,
            from_dy: None,
            to_dx: None,
            to_dy: None,
        }
    }

    #[test]
    fn comment_set_get_remove_list() {
        let mut ws = Worksheet::with_size("S", 20, 10);
        ws.set_comment(
            1,
            1,
            Some(CellComment {
                text: "检查此值".into(),
                author: Some("张三".into()),
            }),
        );
        assert_eq!(
            ws.get_comment(1, 1).map(|c| c.text.as_str()),
            Some("检查此值")
        );
        assert_eq!(ws.list_comments().len(), 1);
        ws.set_comment(1, 1, None);
        assert!(ws.get_comment(1, 1).is_none());
    }

    #[test]
    fn floating_object_z_order() {
        let mut ws = Worksheet::with_size("S", 20, 10);
        ws.add_floating_object(FloatingObject {
            id: "a".into(),
            kind: FloatingKind::Image,
            anchor: m14_anchor(0, 0, 2, 2),
            src: Some("x".into()),
            chart: None,
            shape: None,
            z: Some(2.0),
        });
        ws.add_floating_object(FloatingObject {
            id: "b".into(),
            kind: FloatingKind::Chart,
            anchor: m14_anchor(3, 3, 6, 6),
            src: None,
            chart: None,
            shape: None,
            z: Some(1.0),
        });
        let list = ws.list_floating_objects();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "b"); // z=1 在前
        assert_eq!(list[1].id, "a");
        ws.remove_floating_object("a");
        assert_eq!(ws.list_floating_objects().len(), 1);
    }

    #[test]
    fn floating_object_same_id_overrides() {
        let mut ws = Worksheet::with_size("S", 20, 10);
        ws.add_floating_object(FloatingObject {
            id: "x".into(),
            kind: FloatingKind::Image,
            anchor: m14_anchor(0, 0, 1, 1),
            src: None,
            chart: None,
            shape: None,
            z: None,
        });
        ws.add_floating_object(FloatingObject {
            id: "x".into(),
            kind: FloatingKind::Chart,
            anchor: m14_anchor(0, 0, 1, 1),
            src: None,
            chart: None,
            shape: None,
            z: None,
        });
        assert_eq!(ws.list_floating_objects().len(), 1);
        assert_eq!(
            ws.get_floating_object("x").map(|o| o.kind),
            Some(FloatingKind::Chart)
        );
    }

    // ── M20 保护 / 锁定 ──
    #[test]
    fn locked_defaults_true() {
        let ws = Worksheet::with_size("S", 10, 10);
        assert!(ws.is_cell_locked(0, 0));
    }

    #[test]
    fn explicit_unlock_and_lock() {
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_style(
            0,
            0,
            Some(Style {
                locked: Some(false),
                ..Default::default()
            }),
        );
        assert!(!ws.is_cell_locked(0, 0));
        ws.set_style(
            1,
            1,
            Some(Style {
                locked: Some(true),
                ..Default::default()
            }),
        );
        assert!(ws.is_cell_locked(1, 1));
    }

    #[test]
    fn unprotected_always_editable() {
        let ws = Worksheet::with_size("S", 10, 10);
        assert!(!ws.is_protected());
        assert!(ws.can_edit_cell(0, 0));
    }

    #[test]
    fn protected_locked_rejects_edit() {
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
            ..Default::default()
        }));
        assert!(ws.is_protected());
        assert!(ws.can_edit_cell(0, 0));
        assert!(!ws.can_edit_cell(1, 1));
    }

    #[test]
    fn set_protection_none_or_disabled_releases() {
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_protection(Some(SheetProtection {
            enabled: true,
            ..Default::default()
        }));
        assert!(ws.is_protected());
        ws.set_protection(None);
        assert!(!ws.is_protected());
        ws.set_protection(Some(SheetProtection {
            enabled: false,
            ..Default::default()
        }));
        assert!(!ws.is_protected());
    }

    #[test]
    fn range_has_locked_predicate() {
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_protection(Some(SheetProtection {
            enabled: true,
            ..Default::default()
        }));
        for r in 0..3 {
            for c in 0..3 {
                ws.set_style(
                    r,
                    c,
                    Some(Style {
                        locked: Some(false),
                        ..Default::default()
                    }),
                );
            }
        }
        assert!(!ws.range_has_locked(0, 0, 3, 3));
        ws.set_style(
            1,
            1,
            Some(Style {
                locked: Some(true),
                ..Default::default()
            }),
        );
        assert!(ws.range_has_locked(0, 0, 3, 3));
    }

    #[test]
    fn range_has_locked_false_when_unprotected() {
        let ws = Worksheet::with_size("S", 10, 10);
        assert!(!ws.range_has_locked(0, 0, 5, 5));
    }

    // ── M21 迷你图 ──
    #[test]
    fn sparkline_set_get_clear_list() {
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_sparkline(
            2,
            3,
            Sparkline {
                sparkline_type: SparklineType::Line,
                data_range: RegionRect::new(0, 0, 1, 5),
                color: None,
                negative_color: None,
                markers: None,
                high_low: None,
                first_last: None,
                target: None,
            },
        );
        assert_eq!(
            ws.get_sparkline(2, 3).map(|s| s.sparkline_type),
            Some(SparklineType::Line)
        );
        assert_eq!(ws.list_sparklines().len(), 1);
        assert_eq!(ws.list_sparklines()[0].0, 2);
        ws.clear_sparkline(2, 3);
        assert!(ws.get_sparkline(2, 3).is_none());
    }

    #[test]
    fn sparkline_get_returns_copy() {
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_sparkline(
            0,
            0,
            Sparkline {
                sparkline_type: SparklineType::Column,
                data_range: RegionRect::new(0, 0, 1, 3),
                color: None,
                negative_color: None,
                markers: None,
                high_low: None,
                first_last: None,
                target: None,
            },
        );
        let mut a = ws.get_sparkline(0, 0).unwrap();
        a.data_range.col = 99;
        assert_eq!(ws.get_sparkline(0, 0).unwrap().data_range.col, 0);
    }
}
