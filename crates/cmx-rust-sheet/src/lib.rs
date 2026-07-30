//! # cmx-rust-sheet —— 门面 umbrella crate
//!
//! 把 [`sheet-core`](sheet_core) / [`sheet-formula`](sheet_formula) / [`sheet-io`](sheet_io)
//! 三层收口成一个可嵌入的电子表格引擎库。对标 cmx-megasheet（TypeScript）的
//! `<cmx-megasheet>` 元素**数据类方法**——去掉 DOM/主题/视图，只留纯 Rust API。
//!
//! ## 分层与依赖纪律
//!  - 三层各自的依赖**不上浮**：`sheet-core` 只碰 chrono/regex（纯计算）；重 IO 依赖
//!    （serde/zip/quick-xml/printpdf）关在 `sheet-io`。门面层唯一新依赖是 **rayon**，
//!    仅用于[「多表并行」](batch)。
//!  - 中性快照 `format:"cmx-megasheet"` `version:1` 是**单一事实源**，两引擎（TS/Rust）共享。
//!
//! ## 快速上手
//! ```
//! use cmx_rust_sheet::prelude::*;
//!
//! // 从中性 JSON 快照装载 → 改格 → 重算 → 导出。
//! let mut wb = Workbook::empty();
//! wb.append_sheet(Worksheet::with_size("Sheet1", 16, 8));
//! wb.sheet_mut(0).unwrap().set_value(0, 0, Some(42.into()));
//! wb.sheet_mut(0).unwrap().set_formula(1, 0, "=A1*2");
//!
//! let mut engine = FormulaEngine::new();
//! engine.recalc_all(&mut wb);
//! assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), Some(84.into()));
//!
//! let json = wb.to_json_string(false);
//! assert!(json.contains("\"format\""));
//! ```

// ── 分层命名空间重导出（保留各层完整表面，避免同名符号打架）──────────
pub use sheet_core as core;
pub use sheet_formula as formula;
pub use sheet_io as io;

/// 门面版本。达到与 cmx-megasheet 功能对等并通过跨引擎 parity 后，再对齐到 7.x。
/// 与 [`sheet_core::VERSION`] 同源。
pub const VERSION: &str = sheet_core::VERSION;

/// 常用类型的扁平出口。`use cmx_rust_sheet::prelude::*;` 一次拉齐建表/改格/公式/IO 所需。
pub mod prelude {
    pub use sheet_core::{
        // 坐标 / 区域
        parse_addr,
        parse_range,
        CellCoord,
        CellData,
        // 单元格 / 样式 / 值
        CellValue,
        ChartData,
        // 图表取数（M24，非渲染）
        ChartSeries,
        RangeCoord,
        Style,
        Workbook,
        // 编辑 / 撤销
        WorkbookEdit,
        WorkbookHistory,
        Worksheet,
    };
    pub use sheet_formula::FormulaEngine;
    pub use sheet_io::{
        export_html, export_xlsx, import_xlsx, ExportHtmlOptions, PdfFont, SNAPSHOT_FORMAT,
        SNAPSHOT_VERSION,
    };

    pub use crate::{batch, WorkbookExt, VERSION};
}

// 门面直接重导出最常用的高层符号（无需走 prelude）。
pub use sheet_core::{Workbook, Worksheet};
pub use sheet_formula::FormulaEngine;
pub use sheet_io::{PdfFont, SNAPSHOT_FORMAT, SNAPSHOT_VERSION};

/// 高层便捷 API：把散在 `sheet-io` 的自由函数收成 [`Workbook`] 上的方法，
/// 对标 TS `<cmx-megasheet>` 的 `loadWorkbook` / `setWorkbookJson` / `exportXlsx` / `exportPdf`。
///
/// 纯薄封装（零新语义）——底层仍是 [`sheet_io`] 的 `workbook_from_json` /
/// `export_xlsx` / `export_pdf` 等；只为省去调用方到处 `use` 自由函数。
pub trait WorkbookExt: Sized {
    /// 从中性 JSON 快照装载（对齐 TS `loadWorkbook` / `setWorkbookJson`）。
    fn from_json(json: &str) -> Result<Self, String>;
    /// 从 XLSX 字节装载（经中性快照中转）。
    fn from_xlsx(bytes: &[u8]) -> Self;
    /// 序列化为中性 JSON 快照字符串（`pretty` 控制缩进）。
    fn to_json_string(&self, pretty: bool) -> String;
    /// 导出为 XLSX 字节（语义级 parity；DEFLATE 字节不必与 TS 逐字节相同）。
    fn to_xlsx(&self) -> Vec<u8>;
    /// 导出某工作表为 PDF 字节（按 pageSetup 分页；CJK 需外部字体）。
    /// 越界索引返回 `None`。
    fn export_pdf(&self, sheet_index: usize, font: PdfFont) -> Option<Vec<u8>>;
    /// 导出某工作表为自包含 HTML。越界索引返回 `None`。
    fn export_html(&self, sheet_index: usize, opts: &sheet_io::ExportHtmlOptions)
        -> Option<String>;
    /// 全量重算（新建临时引擎跑一轮）。链式/跨表公式一次算齐。
    /// 需要复用依赖图或叠加自定义函数时，请直接持有 [`FormulaEngine`]。
    fn recalc(&mut self);
}

impl WorkbookExt for Workbook {
    fn from_json(json: &str) -> Result<Self, String> {
        sheet_io::parse_workbook(json)
    }

    fn from_xlsx(bytes: &[u8]) -> Self {
        sheet_io::import_xlsx(bytes)
    }

    fn to_json_string(&self, pretty: bool) -> String {
        sheet_io::stringify_workbook(self, pretty)
    }

    fn to_xlsx(&self) -> Vec<u8> {
        sheet_io::export_xlsx(self)
    }

    fn export_pdf(&self, sheet_index: usize, font: PdfFont) -> Option<Vec<u8>> {
        self.sheet(sheet_index)
            .map(|ws| sheet_io::export_pdf(ws, font))
    }

    fn export_html(
        &self,
        sheet_index: usize,
        opts: &sheet_io::ExportHtmlOptions,
    ) -> Option<String> {
        self.sheet(sheet_index)
            .map(|ws| sheet_io::export_html(ws, opts))
    }

    fn recalc(&mut self) {
        let mut engine = FormulaEngine::new();
        engine.recalc_all(self);
    }
}

/// 批量并行（rayon）。对标 docs/方案.html「一次算几千张表」的卖点。
///
/// **关键约束**：[`Workbook`] 内含 `Rc`/`RefCell`（事件总线/撤销栈），故 **`!Send`**——
/// 不能把活的 `Workbook` 送过线程边界。所以并行**只在 `Send` 载荷（JSON 串 / XLSX 字节）
/// 之间做**，每个 `Workbook` 在 worker 闭包**内部**现造现用、算完即转回 `Send` 的产物。
/// 这与 [[cmx-database-pg]] 的并行纪律同理：跨线程的永远是纯数据，不是带内部可变性的句柄。
pub mod batch {
    use super::*;
    use rayon::prelude::*;

    /// 并行把多份中性 JSON 快照各自装载→重算→导出为 XLSX 字节。
    /// 输入/输出都是 `Send` 载荷，`Workbook` 只活在每个 worker 内。
    /// 任一份解析失败 → 该位置 `Err(错误串)`，不拖累其余。
    pub fn json_to_xlsx(jsons: &[String]) -> Vec<Result<Vec<u8>, String>> {
        jsons
            .par_iter()
            .map(|j| {
                let mut wb = Workbook::from_json(j)?;
                wb.recalc();
                Ok(wb.to_xlsx())
            })
            .collect()
    }

    /// 并行把多份 XLSX 字节各自装载→重算→导出为中性 JSON 快照串。
    pub fn xlsx_to_json(bytes: &[Vec<u8>]) -> Vec<String> {
        bytes
            .par_iter()
            .map(|b| {
                let mut wb = Workbook::from_xlsx(b);
                wb.recalc();
                wb.to_json_string(false)
            })
            .collect()
    }

    /// 通用并行映射：对每份 `Send` 载荷，在 worker 内造一个 `Workbook` 交给 `f`，收集 `Send` 产物。
    /// `f` 拿到的是**独立**的可变 `Workbook`，彼此无共享——安全并行。
    ///
    /// ```
    /// use cmx_rust_sheet::{batch, WorkbookExt, Workbook};
    /// let jsons: Vec<String> = vec![/* … */];
    /// // 并行数每张表的单元格数（纯数据产物 usize 是 Send）。
    /// let counts: Vec<usize> = batch::map(&jsons, |j| {
    ///     let wb = Workbook::from_json(j).unwrap();
    ///     wb.sheet(0).map(|s| s.cell_count()).unwrap_or(0)
    /// });
    /// let _ = counts;
    /// ```
    pub fn map<T, R, F>(items: &[T], f: F) -> Vec<R>
    where
        T: Sync,
        R: Send,
        F: Fn(&T) -> R + Sync + Send,
    {
        items.par_iter().map(f).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::prelude::*;

    fn one_cell_wb() -> Workbook {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("Sheet1", 8, 4));
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some(21.into()));
        wb.sheet_mut(0).unwrap().set_formula(1, 0, "=A1*2");
        wb
    }

    #[test]
    fn version_is_shared_with_core() {
        assert_eq!(VERSION, sheet_core::VERSION);
        assert_eq!(SNAPSHOT_FORMAT, "cmx-megasheet");
        assert_eq!(SNAPSHOT_VERSION, 1);
    }

    #[test]
    fn ext_recalc_computes_formula() {
        let mut wb = one_cell_wb();
        wb.recalc();
        assert_eq!(wb.sheet(0).unwrap().get_value(1, 0), Some(42.into()));
    }

    #[test]
    fn ext_json_round_trip() {
        let mut wb = one_cell_wb();
        wb.recalc();
        let json = wb.to_json_string(false);
        let wb2 = Workbook::from_json(&json).expect("parse");
        assert_eq!(wb2.sheet(0).unwrap().get_value(1, 0), Some(42.into()));
    }

    #[test]
    fn ext_xlsx_round_trip() {
        let mut wb = one_cell_wb();
        wb.recalc();
        let bytes = wb.to_xlsx();
        assert!(!bytes.is_empty());
        let wb2 = Workbook::from_xlsx(&bytes);
        // 值经 XLSX 往返保真（公式缓存值 42 或公式重算后 42）。
        assert_eq!(wb2.sheet(0).unwrap().get_value(0, 0), Some(21.into()));
    }

    #[test]
    fn ext_export_pdf_and_html_bounds() {
        let wb = one_cell_wb();
        assert!(wb.export_pdf(0, PdfFont::Builtin).is_some());
        assert!(wb.export_pdf(9, PdfFont::Builtin).is_none()); // 越界
        let html = wb
            .export_html(0, &ExportHtmlOptions::default())
            .expect("html");
        assert!(html.contains("<table"));
        assert!(wb.export_html(9, &ExportHtmlOptions::default()).is_none());
    }

    #[test]
    fn ext_from_json_error_is_reported() {
        assert!(Workbook::from_json("{ not valid json").is_err());
    }

    #[test]
    fn batch_json_to_xlsx_parallel() {
        let mut wb = one_cell_wb();
        wb.recalc();
        let json = wb.to_json_string(false);
        let inputs = vec![json.clone(), json.clone(), json];
        let out = batch::json_to_xlsx(&inputs);
        assert_eq!(out.len(), 3);
        assert!(out
            .iter()
            .all(|r| r.as_ref().map(|b| !b.is_empty()).unwrap_or(false)));
    }

    #[test]
    fn batch_json_to_xlsx_isolates_failures() {
        let mut wb = one_cell_wb();
        wb.recalc();
        let good = wb.to_json_string(false);
        let inputs = vec![good, "{bad".to_string()];
        let out = batch::json_to_xlsx(&inputs);
        assert!(out[0].is_ok());
        assert!(out[1].is_err());
    }

    #[test]
    fn batch_map_generic() {
        let mut wb = one_cell_wb();
        wb.recalc();
        let json = wb.to_json_string(false);
        let inputs = vec![json.clone(), json];
        let sheet_counts: Vec<usize> =
            batch::map(&inputs, |j| Workbook::from_json(j).unwrap().sheet_count());
        assert_eq!(sheet_counts, vec![1, 1]);
    }
}
