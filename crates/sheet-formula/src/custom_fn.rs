//! 报表取数自定义函数 QM/QC/JE/FS/REF（M3）。对标 cmx-megasheet 的 CustomFunction.ts。
//!
//! 分工不变（方案 §5.3 / 约束④）：取数**真值**由后端 cmx-rpt-formula 算好，经
//! setReportValueMap 下发到一张「sheetName!CELLREF → 值」表；这些函数只做**查表**，
//! 不重造取数真值。语义严格对齐旧 wrapper：
//!  - 上下文敏感：值按**所在格**取（QM(...) 写在 C5 就取 C5 的预算值）
//!  - 易变（volatile）：map 更新后须重算 → 依赖图每次纳入
//!  - 数值语义：缺/空/非数值 → 0
//!
//! Rust 移植取舍：TS 闭包捕获 valueMap 引用；Rust 用 `Rc<RefCell<ReportValueMap>>` 共享
//! ——引擎持一份句柄灌值，注册进 registry 的闭包持另一份句柄查表（内部可变性等价表达）。

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use sheet_core::address::col_to_label;

use crate::evaluator::{EvalContext, EvaluatedArg};
use crate::functions::BuiltinRegistry;
use crate::value::FormulaValue;

/// 取数函数名（5 个）。
pub const REPORT_FETCH_NAMES: [&str; 5] = ["QM", "QC", "JE", "FS", "REF"];

/// 报表取数值表：键 `sheetName!CELLREF`（CELLREF 大写），值 number|string。
#[derive(Debug, Default)]
pub struct ReportValueMap {
    map: HashMap<String, FormulaValue>,
}

impl ReportValueMap {
    pub fn new() -> Self {
        ReportValueMap::default()
    }

    /// 灌值。键归一：`sheetName!CELLREF`（cellRef 大写）；裸 CELLREF 按 activeSheet 补前缀。
    pub fn set(&mut self, raw: &[(String, FormulaValue)], active_sheet: &str) {
        let mut norm = HashMap::new();
        for (k, v) in raw {
            let nk = match k.find('!') {
                Some(b) => format!("{}!{}", &k[..b], k[b + 1..].to_uppercase()),
                None => format!("{}!{}", active_sheet, k.to_uppercase()),
            };
            norm.insert(nk, v.clone());
        }
        self.map = norm;
    }

    /// 按所在格取数值（缺/空/非数值 → 0，数字字符串 → 数字）。
    pub fn get_number(&self, sheet_name: &str, row: u32, col: u32) -> f64 {
        let reference = format!("{}{}", col_to_label(col), row + 1);
        let key = format!("{sheet_name}!{reference}");
        match self.map.get(&key) {
            None => 0.0,
            Some(FormulaValue::Number(n)) if n.is_finite() => *n,
            Some(FormulaValue::Text(s)) => s
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .unwrap_or(0.0),
            _ => 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }
}

/// 共享取数表句柄。
pub type SharedValueMap = Rc<RefCell<ReportValueMap>>;

/// 把 QM/QC/JE/FS/REF 注册到 registry（volatile）。5 个函数取数逻辑一致（真值差异在后端算，
/// 前端只查同一张表按格取）。返回共享句柄供引擎灌值。
pub fn register_report_fetch_functions(registry: &mut BuiltinRegistry, value_map: SharedValueMap) {
    for name in REPORT_FETCH_NAMES {
        let vm = value_map.clone();
        let imp = Rc::new(move |_args: &[EvaluatedArg], ctx: &EvalContext| {
            FormulaValue::Number(vm.borrow().get_number(ctx.sheet_name, ctx.row, ctx.col))
        });
        registry.register(name, imp, true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::{CellAccessor, Evaluator, FunctionRegistry};
    use crate::parse::parse_formula;

    struct EmptyAccessor;
    impl CellAccessor for EmptyAccessor {
        fn get_cell_value(&self, _r: &str) -> FormulaValue {
            FormulaValue::Blank
        }
        fn get_range_values(&self, _s: &str, _e: &str) -> Vec<Vec<FormulaValue>> {
            vec![vec![FormulaValue::Blank]]
        }
    }

    fn setup(map: &[(&str, FormulaValue)], active: &str) -> (BuiltinRegistry, SharedValueMap) {
        let mut reg = BuiltinRegistry::new();
        let vm: SharedValueMap = Rc::new(RefCell::new(ReportValueMap::new()));
        let owned: Vec<(String, FormulaValue)> = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        vm.borrow_mut().set(&owned, active);
        register_report_fetch_functions(&mut reg, vm.clone());
        (reg, vm)
    }

    fn eval_at(reg: &BuiltinRegistry, src: &str, sheet: &str, row: u32, col: u32) -> FormulaValue {
        let acc = EmptyAccessor;
        let ev = Evaluator::new(reg);
        let ctx = EvalContext {
            accessor: &acc,
            row,
            col,
            sheet_name: sheet,
        };
        ev.evaluate(&parse_formula(src).unwrap(), &ctx)
    }

    #[test]
    fn value_map_normalizes_bare_keys() {
        let mut vm = ReportValueMap::new();
        vm.set(&[("c5".to_string(), FormulaValue::Number(100.0))], "Sheet1");
        assert_eq!(vm.get_number("Sheet1", 4, 2), 100.0); // C5
    }

    #[test]
    fn value_map_keeps_qualified_keys() {
        let mut vm = ReportValueMap::new();
        vm.set(
            &[("Sheet2!B2".to_string(), FormulaValue::Number(50.0))],
            "Sheet1",
        );
        assert_eq!(vm.get_number("Sheet2", 1, 1), 50.0);
        assert_eq!(vm.get_number("Sheet1", 1, 1), 0.0);
    }

    #[test]
    fn value_map_missing_blank_nonnumeric_zero() {
        let mut vm = ReportValueMap::new();
        vm.set(
            &[
                ("Sheet1!A1".to_string(), FormulaValue::Text("".into())),
                ("Sheet1!A2".to_string(), FormulaValue::Text("abc".into())),
            ],
            "Sheet1",
        );
        assert_eq!(vm.get_number("Sheet1", 0, 0), 0.0);
        assert_eq!(vm.get_number("Sheet1", 1, 0), 0.0);
        assert_eq!(vm.get_number("Sheet1", 9, 9), 0.0);
    }

    #[test]
    fn value_map_numeric_string() {
        let mut vm = ReportValueMap::new();
        vm.set(
            &[("Sheet1!A1".to_string(), FormulaValue::Text("123".into()))],
            "Sheet1",
        );
        assert_eq!(vm.get_number("Sheet1", 0, 0), 123.0);
    }

    #[test]
    fn registers_all_five_volatile() {
        let (reg, _vm) = setup(&[], "Sheet1");
        for name in REPORT_FETCH_NAMES {
            assert!(reg.get(name).is_some());
            assert!(reg.is_volatile(name));
        }
    }

    #[test]
    fn context_sensitive_fetch() {
        let (reg, _vm) = setup(&[("Sheet1!C3", FormulaValue::Number(885000.0))], "Sheet1");
        assert_eq!(
            eval_at(&reg, "QM(\"balance\",\"period\")", "Sheet1", 2, 2),
            FormulaValue::Number(885000.0)
        );
        assert_eq!(
            eval_at(&reg, "QM(\"balance\",\"period\")", "Sheet1", 5, 5),
            FormulaValue::Number(0.0)
        );
    }

    #[test]
    fn all_five_read_same_cell() {
        let (reg, _vm) = setup(&[("Sheet1!B2", FormulaValue::Number(42.0))], "Sheet1");
        for name in REPORT_FETCH_NAMES {
            assert_eq!(
                eval_at(&reg, &format!("{name}()"), "Sheet1", 1, 1),
                FormulaValue::Number(42.0)
            );
        }
    }

    #[test]
    fn participates_in_arithmetic() {
        let (reg, _vm) = setup(&[("Sheet1!C3", FormulaValue::Number(100.0))], "Sheet1");
        assert_eq!(
            eval_at(&reg, "QM()+QC()", "Sheet1", 2, 2),
            FormulaValue::Number(200.0)
        );
        assert_eq!(
            eval_at(&reg, "QM()+5", "Sheet1", 8, 8),
            FormulaValue::Number(5.0)
        );
    }

    #[test]
    fn volatile_map_update_changes_result() {
        let (reg, vm) = setup(&[("Sheet1!C3", FormulaValue::Number(100.0))], "Sheet1");
        assert_eq!(
            eval_at(&reg, "QM()", "Sheet1", 2, 2),
            FormulaValue::Number(100.0)
        );
        vm.borrow_mut().set(
            &[("Sheet1!C3".to_string(), FormulaValue::Number(999.0))],
            "Sheet1",
        );
        assert_eq!(
            eval_at(&reg, "QM()", "Sheet1", 2, 2),
            FormulaValue::Number(999.0)
        );
    }

    #[test]
    fn fetch_feeds_arithmetic_aggregation() {
        let (reg, _vm) = setup(&[("Sheet1!C3", FormulaValue::Number(100.0))], "Sheet1");
        assert_eq!(
            eval_at(&reg, "ROUND(QM()*1.5, 0)", "Sheet1", 2, 2),
            FormulaValue::Number(150.0)
        );
    }
}
