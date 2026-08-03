//! # sheet-formula —— cmx-rust-sheet 公式引擎
//!
//! 对标 cmx-megasheet（TypeScript）的 `formula/` 层。纯逻辑、零 DOM：
//! 词法 → Pratt 语法 → AST 求值 → 依赖图/三色环 → 内置函数 → 报表取数函数 → 引擎编排。
//!
//! 分层（对齐父项目，随「无渲染」重解读）：
//!  - [`token`] / [`parse`]：公式源串 → token → AST。
//!  - [`value`]：FormulaValue/FormulaError + Excel 强制转换。
//!  - [`evaluator`]：AST → 值，面向 CellAccessor / FunctionRegistry 抽象。
//!  - [`functions`]：内置函数核心集（RS-M3；全 272 在 RS-M17）。
//!  - [`custom_fn`]：QM/QC/JE/FS/REF 报表取数（volatile，查 ReportValueMap）。
//!  - [`depgraph`]：依赖图 + 拓扑序 + 三色环检测。
//!  - [`engine`]：编排层，接 Workbook 全量/增量重算（线程化 `&mut Workbook`）。

pub mod builtins_eng;
pub mod builtins_m17;
pub mod builtins_m8;
pub mod custom_fn;
pub mod depgraph;
pub mod engine;
pub mod evaluator;
pub mod functions;
pub mod parse;
pub mod token;
pub mod value;

// ── 顶层重导出（便捷门面）─────────────────────────────────
pub use custom_fn::{
    register_report_fetch_functions, ReportValueMap, SharedValueMap, REPORT_FETCH_NAMES,
};
pub use depgraph::{cell_key, extract_deps, CellKey, DependencyGraph, TopoResult};
pub use engine::FormulaEngine;
pub use evaluator::{
    as_matrix, first_error, flatten_arg, flatten_args, scalar_arg, split_top_range, CellAccessor,
    EvalContext, EvaluatedArg, Evaluator, FunctionImpl, FunctionRegistry,
};
pub use functions::BuiltinRegistry;
pub use parse::{parse_formula, AstNode, FormulaParseError};
pub use token::{tokenize, FormulaLexError, Token, TokenType};
pub use value::{
    compare_values, number_to_text, to_boolean, to_number, to_text, FormulaError, FormulaValue,
};
