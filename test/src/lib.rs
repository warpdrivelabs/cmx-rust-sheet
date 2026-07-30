//! m99-test —— 占位库；真正的断言在 [`../tests/m99_multi_sheet.rs`](自动发现)。
//!
//! 对标 cmx-mega-sheet 的 `test/`：这里只做「后端构建 M99 六 sheet → XLSX/JSON 往返」的
//! 集成校验，不含任何渲染。构建器来自 `m99-demo`（演示与测试同源，杜绝「演示能过、测试另写」）。
