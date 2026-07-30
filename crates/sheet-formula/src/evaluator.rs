//! AST 求值。formula 层核心（M3）。对标 cmx-megasheet 的 Evaluator.ts。
//!
//! 面向抽象 [`CellAccessor`]（给引用文本返回值）与 [`FunctionRegistry`]（函数名→实现），
//! 不直接依赖 Worksheet——纯逻辑可测；接入层（FormulaEngine）把 Workbook 包成 accessor。
//! 区域求值为二维 `Vec<Vec<FormulaValue>>`。上下文敏感函数（QM/QC/…）经 [`EvalContext`]
//! 拿到「当前所在格」。
//!
//! Rust 移植取舍：TS `FunctionImpl` 是可闭包捕获的箭头函数（QM 闭包捕获 valueMap）。
//! Rust 用 `Rc<dyn Fn>` 承载函数实现，QM/QC 得以闭包捕获 `Rc<RefCell<ReportValueMap>>`
//! （忠实翻译，见 custom_fn.rs）。Evaluator 借用 `&dyn FunctionRegistry`（不自持，规避
//! FormulaEngine 内 registry↔evaluator 自引用）。

use std::rc::Rc;

use crate::parse::AstNode;
use crate::value::{compare_values, to_number, to_text, FormulaError, FormulaValue};

/// 单元格访问器：把引用文本解析为值 / 区域展开为二维值。
pub trait CellAccessor {
    /// 单格引用（"A1" / "Sheet1!B2"）→ 值。越界/无效 sheet 返回 `#REF!`。
    fn get_cell_value(&self, reference: &str) -> FormulaValue;
    /// 区域引用（start/end 单格文本）→ 二维值（行×列）。
    fn get_range_values(&self, start: &str, end: &str) -> Vec<Vec<FormulaValue>>;
    /// 命名区域 → 引用文本（如 'Sheet1!A1:B3'）；未知返回 None。
    fn resolve_name_ref(&self, _name: &str) -> Option<String> {
        None
    }
    /// 命名标量解析（命名常量）；未知返回 None → #NAME?。
    fn resolve_name(&self, _name: &str) -> Option<FormulaValue> {
        None
    }
}

/// 求值上下文：当前所在格（供上下文敏感函数）+ accessor。
pub struct EvalContext<'a> {
    pub accessor: &'a dyn CellAccessor,
    /// 当前公式所在格（0-based），供 QM/QC/… 按格取数。
    pub row: u32,
    pub col: u32,
    /// 当前 sheet 名（跨表/取数键用）。
    pub sheet_name: &'a str,
}

/// 求值后的实参：标量值 或 区域（二维）。
#[derive(Debug, Clone)]
pub enum EvaluatedArg {
    Value(FormulaValue),
    Range(Vec<Vec<FormulaValue>>),
}

/// 函数实现：接收已求值的实参 + 上下文。用 Rc<dyn Fn> 以支持闭包捕获（QM 捕获取数表）。
pub type FunctionImpl = Rc<dyn Fn(&[EvaluatedArg], &EvalContext) -> FormulaValue>;

/// 函数注册表抽象。
pub trait FunctionRegistry {
    fn get(&self, name: &str) -> Option<FunctionImpl>;
    /// 是否 volatile（QM/QC/NOW/…）——依赖图据此每次重算。
    fn is_volatile(&self, _name: &str) -> bool {
        false
    }
}

/// AST 求值器：借用函数注册表。
pub struct Evaluator<'r> {
    registry: &'r dyn FunctionRegistry,
}

impl<'r> Evaluator<'r> {
    pub fn new(registry: &'r dyn FunctionRegistry) -> Self {
        Evaluator { registry }
    }

    /// 求值一个 AST → 标量（区域在标量上下文取左上角）。
    pub fn evaluate(&self, node: &AstNode, ctx: &EvalContext) -> FormulaValue {
        match self.eval_node(node, ctx) {
            EvaluatedArg::Range(values) => values
                .first()
                .and_then(|r| r.first())
                .cloned()
                .unwrap_or(FormulaValue::Blank),
            EvaluatedArg::Value(v) => v,
        }
    }

    fn eval_node(&self, node: &AstNode, ctx: &EvalContext) -> EvaluatedArg {
        match node {
            AstNode::Number(n) => EvaluatedArg::Value(FormulaValue::Number(*n)),
            AstNode::Str(s) => EvaluatedArg::Value(FormulaValue::Text(s.clone())),
            AstNode::Name(name) => self.eval_name_node(name, ctx),
            AstNode::Ref(r) => EvaluatedArg::Value(ctx.accessor.get_cell_value(r)),
            AstNode::Range { start, end } => {
                EvaluatedArg::Range(ctx.accessor.get_range_values(start, end))
            }
            AstNode::Array(rows) => EvaluatedArg::Range(self.eval_array(rows, ctx)),
            AstNode::Unary { op, operand } => {
                EvaluatedArg::Value(self.eval_unary(op, operand, ctx))
            }
            AstNode::Binary { op, left, right } => {
                EvaluatedArg::Value(self.eval_binary(op, left, right, ctx))
            }
            AstNode::Call { name, args } => EvaluatedArg::Value(self.eval_call(name, args, ctx)),
        }
    }

    fn eval_array(&self, rows: &[Vec<AstNode>], ctx: &EvalContext) -> Vec<Vec<FormulaValue>> {
        rows.iter()
            .map(|row| row.iter().map(|cell| self.evaluate(cell, ctx)).collect())
            .collect()
    }

    /// 命名节点：TRUE/FALSE → 布尔；命名区域 → 引用重解析（支持 SUM(myRange)）；命名标量；否则 #NAME?。
    fn eval_name_node(&self, name: &str, ctx: &EvalContext) -> EvaluatedArg {
        let upper = name.to_uppercase();
        if upper == "TRUE" {
            return EvaluatedArg::Value(FormulaValue::Bool(true));
        }
        if upper == "FALSE" {
            return EvaluatedArg::Value(FormulaValue::Bool(false));
        }
        if let Some(reference) = ctx.accessor.resolve_name_ref(name) {
            if let Some((start, end)) = split_top_range(&reference) {
                return EvaluatedArg::Range(ctx.accessor.get_range_values(start, end));
            }
            return EvaluatedArg::Value(ctx.accessor.get_cell_value(&reference));
        }
        match ctx.accessor.resolve_name(name) {
            Some(v) => EvaluatedArg::Value(v),
            None => EvaluatedArg::Value(FormulaValue::Error(FormulaError::Name)),
        }
    }

    fn eval_unary(&self, op: &str, operand: &AstNode, ctx: &EvalContext) -> FormulaValue {
        let v = self.evaluate(operand, ctx);
        if let FormulaValue::Error(e) = v {
            return FormulaValue::Error(e);
        }
        match op {
            "-" => match to_number(&v) {
                Ok(n) => FormulaValue::Number(-n),
                Err(e) => FormulaValue::Error(e),
            },
            "%" => match to_number(&v) {
                Ok(n) => FormulaValue::Number(n / 100.0),
                Err(e) => FormulaValue::Error(e),
            },
            _ => v,
        }
    }

    fn eval_binary(
        &self,
        op: &str,
        left: &AstNode,
        right: &AstNode,
        ctx: &EvalContext,
    ) -> FormulaValue {
        let a = self.evaluate(left, ctx);
        let b = self.evaluate(right, ctx);
        if let FormulaValue::Error(e) = a {
            return FormulaValue::Error(e);
        }
        if let FormulaValue::Error(e) = b {
            return FormulaValue::Error(e);
        }
        // 比较
        if matches!(op, "=" | "<>" | "<" | ">" | "<=" | ">=") {
            return match compare_values(&a, &b, op) {
                Ok(r) => FormulaValue::Bool(r),
                Err(e) => FormulaValue::Error(e),
            };
        }
        // 连接
        if op == "&" {
            let ta = match to_text(&a) {
                Ok(t) => t,
                Err(e) => return FormulaValue::Error(e),
            };
            let tb = match to_text(&b) {
                Ok(t) => t,
                Err(e) => return FormulaValue::Error(e),
            };
            return FormulaValue::Text(ta + &tb);
        }
        // 算术
        let na = match to_number(&a) {
            Ok(n) => n,
            Err(e) => return FormulaValue::Error(e),
        };
        let nb = match to_number(&b) {
            Ok(n) => n,
            Err(e) => return FormulaValue::Error(e),
        };
        match op {
            "+" => FormulaValue::Number(na + nb),
            "-" => FormulaValue::Number(na - nb),
            "*" => FormulaValue::Number(na * nb),
            "/" => {
                if nb == 0.0 {
                    FormulaValue::Error(FormulaError::Div0)
                } else {
                    FormulaValue::Number(na / nb)
                }
            }
            "^" => {
                let r = na.powf(nb);
                if r.is_finite() {
                    FormulaValue::Number(r)
                } else {
                    FormulaValue::Error(FormulaError::Num)
                }
            }
            _ => FormulaValue::Error(FormulaError::Value),
        }
    }

    fn eval_call(&self, name: &str, arg_nodes: &[AstNode], ctx: &EvalContext) -> FormulaValue {
        let Some(f) = self.registry.get(name) else {
            return FormulaValue::Error(FormulaError::Name);
        };
        let args: Vec<EvaluatedArg> = arg_nodes.iter().map(|n| self.eval_node(n, ctx)).collect();
        f(&args, ctx)
    }
}

// ── 实参展平助手（供内置函数）─────────────────────────

/// 把一个实参展平为标量值序列（区域按行优先展开）。
pub fn flatten_arg(arg: &EvaluatedArg) -> Vec<FormulaValue> {
    match arg {
        EvaluatedArg::Value(v) => vec![v.clone()],
        EvaluatedArg::Range(rows) => rows.iter().flat_map(|r| r.iter().cloned()).collect(),
    }
}

/// 把多个实参展平为标量序列。
pub fn flatten_args(args: &[EvaluatedArg]) -> Vec<FormulaValue> {
    args.iter().flat_map(flatten_arg).collect()
}

/// 取实参的标量值（区域取左上角；缺省 Blank）。
pub fn scalar_arg(arg: Option<&EvaluatedArg>) -> FormulaValue {
    match arg {
        None => FormulaValue::Blank,
        Some(EvaluatedArg::Value(v)) => v.clone(),
        Some(EvaluatedArg::Range(rows)) => rows
            .first()
            .and_then(|r| r.first())
            .cloned()
            .unwrap_or(FormulaValue::Blank),
    }
}

/// 第一个错误（若有），供函数短路。
pub fn first_error(values: &[FormulaValue]) -> Option<FormulaError> {
    values.iter().find_map(|v| v.as_error())
}

/// 把命名区域 refersTo 拆成 range 端点（含顶层 ':' 时）。单格返回 None。
pub fn split_top_range(reference: &str) -> Option<(&str, &str)> {
    reference
        .find(':')
        .map(|i| (&reference[..i], &reference[i + 1..]))
}

/// 实参 → 二维矩阵（标量升为 1×1）。查找类函数用。
pub fn as_matrix(arg: Option<&EvaluatedArg>) -> Vec<Vec<FormulaValue>> {
    match arg {
        None => vec![vec![FormulaValue::Blank]],
        Some(EvaluatedArg::Range(rows)) => rows.clone(),
        Some(EvaluatedArg::Value(v)) => vec![vec![v.clone()]],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::functions::BuiltinRegistry;
    use crate::parse::parse_formula;
    use sheet_core::address::parse_addr;
    use std::collections::HashMap;

    /// 内存网格 accessor：cells 键 "r,c" → 值。
    struct MapAccessor {
        cells: HashMap<(u32, u32), FormulaValue>,
    }

    fn addr_of(reference: &str) -> Option<(u32, u32)> {
        let local = match reference.find('!') {
            Some(b) => &reference[b + 1..],
            None => reference,
        };
        parse_addr(&local.replace('$', "")).map(|p| (p.row, p.col))
    }

    impl CellAccessor for MapAccessor {
        fn get_cell_value(&self, reference: &str) -> FormulaValue {
            match addr_of(reference) {
                Some(rc) => self.cells.get(&rc).cloned().unwrap_or(FormulaValue::Blank),
                None => FormulaValue::Error(FormulaError::Ref),
            }
        }
        fn get_range_values(&self, start: &str, end: &str) -> Vec<Vec<FormulaValue>> {
            let (Some(a), Some(b)) = (addr_of(start), addr_of(end)) else {
                return vec![vec![FormulaValue::Error(FormulaError::Ref)]];
            };
            let (r1, r2) = (a.0.min(b.0), a.0.max(b.0));
            let (c1, c2) = (a.1.min(b.1), a.1.max(b.1));
            (r1..=r2)
                .map(|r| {
                    (c1..=c2)
                        .map(|c| {
                            self.cells
                                .get(&(r, c))
                                .cloned()
                                .unwrap_or(FormulaValue::Blank)
                        })
                        .collect()
                })
                .collect()
        }
    }

    fn eval(src: &str, cells: &[((u32, u32), FormulaValue)]) -> FormulaValue {
        let acc = MapAccessor {
            cells: cells.iter().cloned().collect(),
        };
        let reg = BuiltinRegistry::new();
        let ev = Evaluator::new(&reg);
        let ctx = EvalContext {
            accessor: &acc,
            row: 0,
            col: 0,
            sheet_name: "Sheet1",
        };
        ev.evaluate(&parse_formula(src).unwrap(), &ctx)
    }

    fn num(v: FormulaValue) -> f64 {
        match v {
            FormulaValue::Number(n) => n,
            other => panic!("expected number, got {other:?}"),
        }
    }

    #[test]
    fn arithmetic() {
        assert_eq!(num(eval("1+2*3", &[])), 7.0);
        assert_eq!(num(eval("(1+2)*3", &[])), 9.0);
        assert_eq!(num(eval("2^10", &[])), 1024.0);
        assert_eq!(num(eval("10/4", &[])), 2.5);
        assert_eq!(num(eval("-5+3", &[])), -2.0);
        assert_eq!(num(eval("50%", &[])), 0.5);
        assert_eq!(eval("1/0", &[]), FormulaValue::Error(FormulaError::Div0));
        assert_eq!(eval("\"a\"&\"b\"&1", &[]), FormulaValue::Text("ab1".into()));
        assert_eq!(eval("3>2", &[]), FormulaValue::Bool(true));
        assert_eq!(eval("3<>3", &[]), FormulaValue::Bool(false));
    }

    #[test]
    fn cell_refs_and_ranges() {
        let cells = [
            ((0u32, 0u32), FormulaValue::Number(10.0)),
            ((1, 0), FormulaValue::Number(20.0)),
            ((2, 0), FormulaValue::Number(30.0)),
        ];
        assert_eq!(num(eval("A1+A2", &cells)), 30.0);
        assert_eq!(num(eval("SUM(A1:A3)", &cells)), 60.0);
        assert_eq!(num(eval("A1+Z9", &cells)), 10.0); // 空格算 0
    }
}
