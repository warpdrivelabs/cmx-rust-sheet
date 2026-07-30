//! # sheet-io —— cmx-rust-sheet IO 层
//!
//! 对标 cmx-megasheet（TypeScript）的 `io/` 层。
//!  - [`snapshot`]：中性 JSON 快照（`format:"cmx-megasheet"` `version:1`），**单一事实源**，
//!    两引擎共享；serde 字节级 parity（camelCase + skip_serializing_if 保稀疏 + JS 数字格式）。
//!  - [`export_html`]：区域/工作表 → 自包含 HTML（内联样式 + CF 底色 + 合并 rowspan/colspan）。
//!  - [`export_pdf`]：分页 + printpdf 生成 PDF（CJK 需外部字体，内置字体走 ASCII 兜底）。
//!  - XLSX 往返（zip/quick-xml，语义级 parity）随 RS-M16 全保真接入。

pub mod csv;
pub mod export_html;
pub mod export_pdf;
pub mod snapshot;
pub mod xlsx;
pub mod xlsx_drawing;

pub use csv::{parse_csv, serialize_csv, CsvParseOptions, CsvSerializeOptions};
pub use export_html::{export_html, ExportHtmlOptions};
pub use export_pdf::{export_pdf, PdfFont};
pub use snapshot::{
    parse_workbook, sheet_from_json, sheet_to_json, stringify_workbook, workbook_from_json,
    workbook_to_json, CellSnapshot, DefinedNameSnapshot, OutlineGroupSnapshot, SheetSnapshot,
    WorkbookSnapshot, SNAPSHOT_FORMAT, SNAPSHOT_VERSION,
};
pub use xlsx::{export_xlsx, import_xlsx, snapshot_to_xlsx, xlsx_to_snapshot};
