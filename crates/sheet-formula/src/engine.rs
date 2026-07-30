//! 公式引擎编排层（M3）。对标 cmx-megasheet 的 FormulaEngine.ts。
//!
//! 把 parse/evaluator/functions/depgraph/custom_fn 接到一个 Workbook 上，提供全量/增量重算。
//! 职责：扫描全簿公式格 → 解析 → 建依赖图 → 拓扑重算 → 计算值经 set_computed_value 回填；
//! 环上格标 #CIRC!；QM/QC/… 经 ReportValueMap 查表。
//!
//! Rust 移植取舍：
//!  - TS 里 `wb.setRecalcHook(() => engine.recalcAll())` 把引擎回调塞进 Workbook——Rust 里
//!    是 Workbook↔engine 借用环。改为**线程化**：`recalc_all(&mut wb)` 每次传入工作簿
//!    （与 WorkbookHistory 同构）。
//!  - 链式公式（A2=A1*2, A3=A2+5）要求前驱的**计算值**在后继求值时可见。TS 靠边算边
//!    `setComputedValue` 直接改 sheet 实现。Rust 里求值借 `&wb`、回填要 `&mut wb`，冲突。
//!    解法：**overlay 覆盖层**——accessor 先查 overlay（本轮已算出的值）再落 wb 原值；
//!    拓扑序内每算一格即写 overlay，故后继读得到最新值；整轮结束再用 `&mut` 批量刷回 wb。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use sheet_core::address::{label_to_col, parse_addr};
use sheet_core::workbook::Workbook;

use crate::custom_fn::{register_report_fetch_functions, ReportValueMap, SharedValueMap};
use crate::depgraph::{cell_key, extract_deps, CellKey, DependencyGraph};
use crate::evaluator::{CellAccessor, EvalContext, Evaluator, FunctionRegistry};
use crate::functions::BuiltinRegistry;
use crate::parse::{parse_formula, AstNode};
use crate::value::{FormulaError, FormulaValue};

/// 解析缓存条目。
#[derive(Clone)]
struct ParsedFormula {
    ast: Option<Rc<AstNode>>,
}

/// 一个公式格的登记信息。
struct FormulaCell {
    sheet_index: usize,
    sheet_name: String,
    row: u32,
    col: u32,
    ast: Option<Rc<AstNode>>,
}

/// 公式引擎。持函数注册表、取数表、依赖图、解析缓存；不持 Workbook（线程化传入）。
pub struct FormulaEngine {
    registry: BuiltinRegistry,
    value_map: SharedValueMap,
    graph: DependencyGraph,
    parse_cache: HashMap<String, ParsedFormula>,
    formula_cells: HashMap<CellKey, FormulaCell>,
    graph_built: bool,
}

impl Default for FormulaEngine {
    fn default() -> Self {
        FormulaEngine::new()
    }
}

impl FormulaEngine {
    pub fn new() -> Self {
        let mut registry = BuiltinRegistry::new();
        let value_map: SharedValueMap = Rc::new(RefCell::new(ReportValueMap::new()));
        register_report_fetch_functions(&mut registry, value_map.clone());
        FormulaEngine {
            registry,
            value_map,
            graph: DependencyGraph::new(),
            parse_cache: HashMap::new(),
            formula_cells: HashMap::new(),
            graph_built: false,
        }
    }

    /// 暴露注册表以叠加自定义函数（在灌值/重算前）。
    pub fn registry_mut(&mut self) -> &mut BuiltinRegistry {
        &mut self.registry
    }

    /// 灌报表取数值表并重算（键 `sheet!CELLREF` 或裸 CELLREF）。
    pub fn set_report_value_map(&mut self, wb: &mut Workbook, raw: &[(String, FormulaValue)]) {
        let active = wb
            .active_sheet()
            .map(|s| s.name().to_string())
            .unwrap_or_default();
        self.value_map.borrow_mut().set(raw, &active);
        self.recalc_all(wb);
    }

    fn parse(&mut self, src: &str) -> ParsedFormula {
        if let Some(hit) = self.parse_cache.get(src) {
            return hit.clone();
        }
        let parsed = ParsedFormula {
            ast: parse_formula(src).ok().map(Rc::new),
        };
        self.parse_cache.insert(src.to_string(), parsed.clone());
        parsed
    }

    /// 全量重算：扫描所有 sheet 公式格，重建依赖图，拓扑重算并回填。
    pub fn recalc_all(&mut self, wb: &mut Workbook) {
        self.graph.clear_all();
        self.formula_cells.clear();

        let bounds = collect_bounds(wb);
        let mut all_keys: Vec<CellKey> = Vec::new();
        for (si, ws) in wb.sheets().iter().enumerate() {
            let sheet_name = ws.name().to_string();
            let mut cells: Vec<(u32, u32, String)> = Vec::new();
            ws.for_each_cell(|data, row, col| {
                if let Some(f) = &data.formula {
                    cells.push((row, col, f.clone()));
                }
            });
            for (row, col, formula) in cells {
                let key = cell_key(&sheet_name, row, col);
                let parsed = self.parse(&formula);
                let deps = match &parsed.ast {
                    Some(ast) => extract_deps(ast, &sheet_name, Some(&bounds_fn(&bounds))),
                    None => Vec::new(),
                };
                self.graph.set_deps(&key, deps);
                all_keys.push(key.clone());
                self.formula_cells.insert(
                    key,
                    FormulaCell {
                        sheet_index: si,
                        sheet_name: sheet_name.clone(),
                        row,
                        col,
                        ast: parsed.ast,
                    },
                );
            }
        }
        self.graph_built = true;
        if all_keys.is_empty() {
            return;
        }
        let topo = self.graph.topo_sort(all_keys);
        self.compute_and_write(wb, &topo.order, &topo.cyclic);
    }

    /// 增量重算：编辑一批格后只重算受影响闭包。图未建则退化全量。
    pub fn recalc_cells(&mut self, wb: &mut Workbook, seeds: &[(String, u32, u32)]) {
        if !self.graph_built {
            self.recalc_all(wb);
            return;
        }
        let bounds = collect_bounds(wb);
        let mut seed_keys: Vec<CellKey> = Vec::new();
        for (sheet, row, col) in seeds {
            let Some(si) = wb.index_of_sheet(sheet) else {
                continue;
            };
            let key = cell_key(sheet, *row, *col);
            seed_keys.push(key.clone());
            let formula = wb
                .sheet(si)
                .and_then(|ws| ws.get_cell_data(*row, *col))
                .and_then(|d| d.formula.clone());
            match formula {
                Some(f) => {
                    let parsed = self.parse(&f);
                    let deps = match &parsed.ast {
                        Some(ast) => extract_deps(ast, sheet, Some(&bounds_fn(&bounds))),
                        None => Vec::new(),
                    };
                    self.graph.set_deps(&key, deps);
                    self.formula_cells.insert(
                        key,
                        FormulaCell {
                            sheet_index: si,
                            sheet_name: sheet.clone(),
                            row: *row,
                            col: *col,
                            ast: parsed.ast,
                        },
                    );
                }
                None => {
                    if self.formula_cells.contains_key(&key) {
                        self.graph.set_deps(&key, Vec::new());
                        self.formula_cells.remove(&key);
                    }
                }
            }
        }
        let mut affected = self.graph.affected_by(&seed_keys);
        for k in &seed_keys {
            if self.formula_cells.contains_key(k) {
                affected.insert(k.clone());
            }
        }
        for (k, f) in &self.formula_cells {
            if let Some(ast) = &f.ast {
                if self.is_volatile_ast(ast) {
                    affected.insert(k.clone());
                }
            }
        }
        if affected.is_empty() {
            return;
        }
        let topo = self.graph.topo_sort(affected);
        self.compute_and_write(wb, &topo.order, &topo.cyclic);
    }

    /// 直接求值一个公式串（不落格），供聚合层/预览用。
    pub fn evaluate_formula(
        &mut self,
        wb: &Workbook,
        sheet_name: &str,
        formula: &str,
        row: u32,
        col: u32,
    ) -> FormulaValue {
        let parsed = self.parse(formula);
        let Some(ast) = parsed.ast else {
            return FormulaValue::Error(FormulaError::Name);
        };
        let overlay = RefCell::new(HashMap::new());
        let acc = WorkbookAccessor {
            wb,
            default_sheet: sheet_name.to_string(),
            overlay: &overlay,
        };
        let ev = Evaluator::new(&self.registry);
        let ctx = EvalContext {
            accessor: &acc,
            row,
            col,
            sheet_name,
        };
        ev.evaluate(&ast, &ctx)
    }

    /// 拓扑序求值：overlay 承载本轮已算出的计算值（供链式公式即时可见），末尾刷回 wb。
    fn compute_and_write(&self, wb: &mut Workbook, order: &[CellKey], cyclic: &HashSet<CellKey>) {
        let overlay: RefCell<HashMap<(String, u32, u32), FormulaValue>> =
            RefCell::new(HashMap::new());
        let mut writes: Vec<(usize, u32, u32, FormulaValue)> = Vec::new();

        // 环上格：#CIRC!（也进 overlay，供依赖环的下游读到错误）
        for key in cyclic {
            if let Some(f) = self.formula_cells.get(key) {
                let val = FormulaValue::Error(FormulaError::Circ);
                overlay
                    .borrow_mut()
                    .insert((f.sheet_name.clone(), f.row, f.col), val.clone());
                writes.push((f.sheet_index, f.row, f.col, val));
            }
        }

        let ev = Evaluator::new(&self.registry);
        for key in order {
            let Some(f) = self.formula_cells.get(key) else {
                continue;
            };
            let val = match &f.ast {
                None => FormulaValue::Error(FormulaError::Name),
                Some(ast) => {
                    let acc = WorkbookAccessor {
                        wb,
                        default_sheet: f.sheet_name.clone(),
                        overlay: &overlay,
                    };
                    let ctx = EvalContext {
                        accessor: &acc,
                        row: f.row,
                        col: f.col,
                        sheet_name: &f.sheet_name,
                    };
                    ev.evaluate(ast, &ctx)
                }
            };
            overlay
                .borrow_mut()
                .insert((f.sheet_name.clone(), f.row, f.col), val.clone());
            writes.push((f.sheet_index, f.row, f.col, val));
        }
        drop(overlay);

        // 刷回 wb
        for (si, row, col, val) in writes {
            if let Some(ws) = wb.sheet_mut(si) {
                ws.set_computed_value(row, col, Some(formula_to_cell(&val)));
            }
        }
    }

    fn is_volatile_ast(&self, node: &AstNode) -> bool {
        match node {
            AstNode::Call { name, args } => {
                self.registry.is_volatile(name) || args.iter().any(|a| self.is_volatile_ast(a))
            }
            AstNode::Unary { operand, .. } => self.is_volatile_ast(operand),
            AstNode::Binary { left, right, .. } => {
                self.is_volatile_ast(left) || self.is_volatile_ast(right)
            }
            AstNode::Array(rows) => rows
                .iter()
                .any(|r| r.iter().any(|c| self.is_volatile_ast(c))),
            _ => false,
        }
    }
}

/// FormulaValue → CellValue（回填计算值；错误存文本 `#..`，Blank 存空文本）。
fn formula_to_cell(v: &FormulaValue) -> sheet_core::cell::CellValue {
    use sheet_core::cell::CellValue;
    match v {
        FormulaValue::Number(n) => CellValue::Number(*n),
        FormulaValue::Text(s) => CellValue::Text(s.clone()),
        FormulaValue::Bool(b) => CellValue::Bool(*b),
        FormulaValue::Error(e) => CellValue::Text(e.as_str().to_string()),
        FormulaValue::Blank => CellValue::Text(String::new()),
    }
}

/// CellValue → FormulaValue（读格：错误文本回读为错误，空文本→Blank）。
fn cell_to_formula(v: Option<sheet_core::cell::CellValue>) -> FormulaValue {
    use sheet_core::cell::CellValue;
    match v {
        None => FormulaValue::Blank,
        Some(CellValue::Number(n)) => FormulaValue::Number(n),
        Some(CellValue::Bool(b)) => FormulaValue::Bool(b),
        Some(CellValue::Text(s)) => {
            if s.is_empty() {
                FormulaValue::Blank
            } else if let Some(e) = FormulaError::from_str(&s) {
                FormulaValue::Error(e)
            } else {
                FormulaValue::Text(s)
            }
        }
    }
}

fn collect_bounds(wb: &Workbook) -> HashMap<String, (u32, u32)> {
    wb.sheets()
        .iter()
        .map(|ws| (ws.name().to_string(), (ws.row_count(), ws.column_count())))
        .collect()
}

fn bounds_fn(map: &HashMap<String, (u32, u32)>) -> impl Fn(&str) -> (u32, u32) + '_ {
    move |name: &str| map.get(name).copied().unwrap_or((0, 0))
}

/// 只读全簿 accessor + overlay 覆盖层（本轮已算出的计算值优先）。
struct WorkbookAccessor<'a> {
    wb: &'a Workbook,
    default_sheet: String,
    overlay: &'a RefCell<HashMap<(String, u32, u32), FormulaValue>>,
}

impl WorkbookAccessor<'_> {
    fn split_ref(&self, reference: &str) -> (String, i64, i64) {
        let (sheet, local) = match reference.find('!') {
            Some(b) => (
                reference[..b].trim_matches('\'').to_string(),
                &reference[b + 1..],
            ),
            None => (self.default_sheet.clone(), reference),
        };
        match parse_addr(&local.replace('$', "")) {
            Some(p) => (sheet, p.row as i64, p.col as i64),
            None => (sheet, -1, -1),
        }
    }

    fn split_endpoint(&self, reference: &str) -> (String, i64, i64) {
        let (sheet, local) = match reference.find('!') {
            Some(b) => (
                reference[..b].trim_matches('\'').to_string(),
                &reference[b + 1..],
            ),
            None => (self.default_sheet.clone(), reference),
        };
        let clean = local.replace('$', "");
        if let Some(p) = parse_addr(&clean) {
            return (sheet, p.row as i64, p.col as i64);
        }
        if !clean.is_empty() && clean.chars().all(|c| c.is_ascii_alphabetic()) {
            if let Some(c) = label_to_col(&clean) {
                return (sheet, -1, c as i64);
            }
        }
        if !clean.is_empty() && clean.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(r) = clean.parse::<i64>() {
                return (sheet, r - 1, -1);
            }
        }
        (sheet, -1, -1)
    }

    /// 读一格：overlay 优先，落 wb 原值。
    fn read(&self, sheet: &str, row: u32, col: u32) -> FormulaValue {
        if let Some(v) = self.overlay.borrow().get(&(sheet.to_string(), row, col)) {
            return v.clone();
        }
        match self.wb.sheet_by_name(sheet) {
            Some(ws) => cell_to_formula(ws.get_value(row, col)),
            None => FormulaValue::Error(FormulaError::Ref),
        }
    }
}

impl CellAccessor for WorkbookAccessor<'_> {
    fn get_cell_value(&self, reference: &str) -> FormulaValue {
        let (sheet, row, col) = self.split_ref(reference);
        if row < 0 || col < 0 {
            return FormulaValue::Error(FormulaError::Ref);
        }
        if self.wb.sheet_by_name(&sheet).is_none() {
            return FormulaValue::Error(FormulaError::Ref);
        }
        self.read(&sheet, row as u32, col as u32)
    }

    fn get_range_values(&self, start: &str, end: &str) -> Vec<Vec<FormulaValue>> {
        let a = self.split_endpoint(start);
        let b = self.split_endpoint(end);
        let Some(ws) = self.wb.sheet_by_name(&a.0) else {
            return vec![vec![FormulaValue::Error(FormulaError::Ref)]];
        };
        let max_row = ws.row_count() as i64 - 1;
        let max_col = ws.column_count() as i64 - 1;
        let ar = if a.1 < 0 { 0 } else { a.1 };
        let br = if b.1 < 0 { max_row } else { b.1 };
        let ac = if a.2 < 0 { 0 } else { a.2 };
        let bc = if b.2 < 0 { max_col } else { b.2 };
        let (r1, r2) = (ar.min(br).max(0), ar.max(br).min(max_row.max(0)));
        let (c1, c2) = (ac.min(bc).max(0), ac.max(bc).min(max_col.max(0)));
        let sheet = a.0.clone();
        let mut out = Vec::new();
        for r in r1..=r2 {
            let mut row_vals = Vec::new();
            for c in c1..=c2 {
                row_vals.push(self.read(&sheet, r as u32, c as u32));
            }
            out.push(row_vals);
        }
        out
    }

    fn resolve_name_ref(&self, name: &str) -> Option<String> {
        self.wb.resolve_name(name, Some(&self.default_sheet))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheet_core::worksheet::Worksheet;

    fn setup() -> (Workbook, FormulaEngine) {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("Sheet1", 30, 10));
        (wb, FormulaEngine::new())
    }

    fn num_at(wb: &Workbook, row: u32, col: u32) -> Option<f64> {
        wb.sheet(0)
            .unwrap()
            .get_value(row, col)
            .and_then(|v| v.as_number())
    }

    #[test]
    fn sum_over_range_writes_back() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 2, Some(100.into()));
            ws.set_value(1, 2, Some(200.into()));
            ws.set_value(2, 2, Some(300.into()));
            ws.set_formula(3, 2, "SUM(C1:C3)");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 3, 2), Some(600.0));
        assert_eq!(wb.sheet(0).unwrap().get_formula(3, 2), "SUM(C1:C3)");
    }

    #[test]
    fn chained_formulas_recompute_transitively() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_formula(1, 0, "A1*2");
            ws.set_formula(2, 0, "A2+5");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 1, 0), Some(20.0));
        assert_eq!(num_at(&wb, 2, 0), Some(25.0));
    }

    #[test]
    fn recalc_after_input_change_propagates() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_formula(1, 0, "A1*2");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 1, 0), Some(20.0));
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some(50.into()));
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 1, 0), Some(100.0));
    }

    #[test]
    fn marks_2_cycle_circ() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_formula(0, 0, "B1");
            ws.set_formula(0, 1, "A1");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("#CIRC!".into()));
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 1), Some("#CIRC!".into()));
    }

    #[test]
    fn marks_self_reference_circ() {
        let (mut wb, mut engine) = setup();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "A1+1");
        engine.recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(0, 0), Some("#CIRC!".into()));
    }

    #[test]
    fn if_nested_functions() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(8.into()));
            ws.set_formula(1, 0, "IF(A1>5, ROUND(A1*1.5,0), 0)");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 1, 0), Some(12.0));
    }

    #[test]
    fn div_by_zero() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(5.into()));
            ws.set_value(1, 0, Some(0.into()));
            ws.set_formula(2, 0, "A1/A2");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(wb.sheet(0).unwrap().get_value(2, 0), Some("#DIV/0!".into()));
    }

    #[test]
    fn qm_reads_per_cell_value() {
        let (mut wb, mut engine) = setup();
        wb.sheet_mut(0).unwrap().set_formula(2, 2, "QM()");
        engine.set_report_value_map(
            &mut wb,
            &[("Sheet1!C3".to_string(), FormulaValue::Number(885000.0))],
        );
        assert_eq!(num_at(&wb, 2, 2), Some(885000.0));
    }

    #[test]
    fn qm_participates_in_sum() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_formula(0, 2, "QM()");
            ws.set_formula(1, 2, "QM()");
            ws.set_formula(2, 2, "SUM(C1:C2)");
        }
        engine.set_report_value_map(
            &mut wb,
            &[
                ("Sheet1!C1".to_string(), FormulaValue::Number(100.0)),
                ("Sheet1!C2".to_string(), FormulaValue::Number(200.0)),
            ],
        );
        assert_eq!(num_at(&wb, 0, 2), Some(100.0));
        assert_eq!(num_at(&wb, 1, 2), Some(200.0));
        assert_eq!(num_at(&wb, 2, 2), Some(300.0));
    }

    #[test]
    fn evaluate_formula_direct() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_value(1, 0, Some(20.into()));
        }
        assert_eq!(
            engine.evaluate_formula(&wb, "Sheet1", "SUM(A1:A2)", 0, 0),
            FormulaValue::Number(30.0)
        );
    }

    #[test]
    fn cross_sheet_refs() {
        let (mut wb, mut engine) = setup();
        let mut ws2 = Worksheet::with_size("Sheet2", 10, 5);
        ws2.set_value(0, 0, Some(42.into()));
        wb.append_sheet(ws2);
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "Sheet2!A1*2");
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 0, 0), Some(84.0));
    }

    #[test]
    fn named_range_recalc() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_value(1, 0, Some(20.into()));
            ws.set_value(2, 0, Some(30.into()));
        }
        wb.define_name("SALES", "A1:A3", "workbook");
        wb.sheet_mut(0).unwrap().set_formula(0, 2, "SUM(SALES)");
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 0, 2), Some(60.0));
        wb.sheet_mut(0).unwrap().set_value(1, 0, Some(200.into()));
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 0, 2), Some(240.0));
    }

    #[test]
    fn whole_column_clamped_aggregation() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(5.into()));
            ws.set_value(3, 0, Some(7.into()));
            ws.set_value(29, 0, Some(3.into()));
            ws.set_formula(0, 2, "SUM(A:A)");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 0, 2), Some(15.0));
    }

    #[test]
    fn array_literal_end_to_end() {
        let (mut wb, mut engine) = setup();
        wb.sheet_mut(0).unwrap().set_formula(0, 0, "SUM({1,2;3,4})");
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 0, 0), Some(10.0));
    }

    #[test]
    fn incremental_recalc_after_edit() {
        let (mut wb, mut engine) = setup();
        {
            let ws = wb.sheet_mut(0).unwrap();
            ws.set_value(0, 0, Some(10.into()));
            ws.set_formula(1, 0, "A1*2");
            ws.set_formula(2, 0, "A2+5");
        }
        engine.recalc_all(&mut wb);
        assert_eq!(num_at(&wb, 2, 0), Some(25.0));
        // 改 A1，增量重算只算受影响闭包
        wb.sheet_mut(0).unwrap().set_value(0, 0, Some(100.into()));
        engine.recalc_cells(&mut wb, &[("Sheet1".to_string(), 0, 0)]);
        assert_eq!(num_at(&wb, 1, 0), Some(200.0));
        assert_eq!(num_at(&wb, 2, 0), Some(205.0));
    }
}
