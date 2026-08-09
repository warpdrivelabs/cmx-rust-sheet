//! # sheet-core —— cmx-rust-sheet 核心数据模型
//!
//! 对标 cmx-megasheet（TypeScript）的 `core/` 层，去除一切渲染与 DOM 依赖。
//! 纯逻辑：坐标 · 区域 · 稀疏矩阵 · 样式级联 · 单元格 · 工作表 / 工作簿。
//!
//! 不变式（对齐父项目，随「无渲染」重解读）：
//!  - 纯逻辑、无 I/O（不碰 OS/net/文件；IO 集中在 sheet-io crate）。
//!  - 可撤销操作 = 命令 / 动作模式（[`workbook::UndoManager`]）。
//!  - 中性、稀疏：样式/单元格只记非默认键，serde 序列化对齐 TS 快照。

pub mod address;
pub mod cell;
pub mod chart;
pub mod clipboard;
pub mod condfmt;
pub mod date_serial;
pub mod edit;
pub mod fill;
pub mod find;
pub mod floating;
pub mod formula_ref;
pub mod numfmt;
pub mod numstr;
pub mod outline;
pub mod paginate;
pub mod range;
pub mod selection;
pub mod sort;
pub mod sparse;
pub mod style;
pub mod validation;
pub mod workbook;
pub mod worksheet;

// ── 顶层重导出（便捷门面）─────────────────────────────────
pub use address::{
    col_to_label, format_addr, format_range, label_to_col, parse_addr, parse_range, CellCoord,
    RangeCoord,
};
pub use cell::{
    normalize_formula, sanitize_imported_formula, CellData, CellValue, RichFont, RichRun, RichText,
};
pub use chart::{extract_chart_data, ChartData, ChartSeries};
pub use clipboard::{
    parse_clipboard_html, parse_tsv, serialize_html, serialize_tsv, Clipboard, PasteContent,
    PasteOperation, PasteResult, PasteSpecialOptions,
};
pub use condfmt::{evaluate_rules, CondFormatOverlay, DataBarOverlay, IconOverlay};
pub use date_serial::{
    date_to_serial, parts_to_serial, serial_to_parts, serial_to_time, time_to_fraction, DateParts,
};
pub use edit::{
    apply_style_command, clear_command, consolidate_command, delete_columns_command,
    delete_rows_command, fill_command, insert_columns_command, insert_rows_command, merge_command,
    move_range_command, paste_external_command, paste_format_command, remove_duplicates_command,
    replace_command, set_formula_command, set_value_command, sort_range_command,
    text_to_columns_command, unmerge_command, CellSnapshot, ClearMode, ConsolidateFunc,
    ConsolidateOptions, RemoveDuplicatesOptions, RemoveDuplicatesResult, RowColAxis, SnapshotEdit,
    TextToColumnsMode, WorkbookEdit, WorkbookHistory,
};
pub use fill::{infer_fill, FillAxis};
pub use find::{find_all, FindHit, FindOptions};
pub use floating::{
    comment_marker, hit_handle, hit_object, resize_handles, resolve_object_rect, CommentMarker,
    Handle, HandleName, ScreenRect,
};
pub use formula_ref::{
    adjust_for_structural, parse_struct_ref, translate_formula, RefAxis, RefOp, StructRef,
    StructuralEdit,
};
pub use numfmt::{apply_format, compile_format, format_with, CompiledFormat, FormatResult};
pub use outline::{OutlineAxis, OutlineGroup};
pub use paginate::{paginate, GridMetrics, PageDescriptor, PaginateResult, TitleRange};
pub use range::Range;
pub use selection::{MoveDir, SelectionModel};
pub use sort::{compare_cell_values, compute_sort_order, SortKey};
pub use sparse::SparseMatrix;
pub use style::{
    merge_style, resolve_style, BorderEdge, BorderLineStyle, Borders, CellFill, CellType,
    GradientKind, GradientStop, HAlign, PatternType, Style, StyleSheet, VAlign,
};
pub use validation::{validate_value, ValidationResult};
pub use workbook::{
    is_frozen, Axis, Command, CommandManager, CommandOptions, EventEmitter, StructuralEditMeta,
    StructuralOp, UndoManager, UndoableAction, ViewportState, Workbook, WorkbookEvent,
    WorkbookEventKind,
};
pub use worksheet::{
    AutoFilterState, CellComment, ChartOptions, ChartSpec, ChartType, CondFormatOperator,
    CondFormatType, CondValue, ConditionalRule, DataValidation, FilterCondition, FilterCriterion,
    FilterOp, FitToPages, FloatingKind, FloatingObject, Hyperlink, IconSet, ObjAnchor, Orientation,
    PageMargins, PageSetup, PrintTitles, RegionRect, ShapeSpec, SheetProtection, Span, Sparkline,
    SparklineType, ValidationBound, ValidationOperator, ValidationType, Worksheet,
};

/// 引擎版本。达到与 cmx-megasheet 功能对等并通过跨引擎 parity 后再对齐到 7.x。
pub const VERSION: &str = "0.1.0";
