//! 公式依赖图 + 拓扑重算 + 三色环检测（M3）。对标 cmx-megasheet 的 DependencyGraph.ts。
//!
//! 每个公式格依赖它引用的其它格。重算须按拓扑序（被依赖者先算）。环用**三色 DFS** 检测
//! （白=未访、灰=在栈、黑=完成），灰→灰即环，环上格标 #CIRC!，对齐后端 cmx-rpt-formula
//! 的 REF 三色环检测语义。格用字符串键 `sheet!row,col` 标识，依赖由 AST 抽取。零 DOM。

use std::collections::{HashMap, HashSet};

use sheet_core::address::{label_to_col, parse_addr};

use crate::parse::AstNode;

/// 格键：`sheet!row,col`（sheet 可空，同表内用 ''）。
pub type CellKey = String;

/// 构造格键。
pub fn cell_key(sheet: &str, row: u32, col: u32) -> CellKey {
    format!("{sheet}!{row},{col}")
}

/// sheet 维度回调：整列/整行依赖钳到 sheet 尺寸（避免遍历百万格）。
pub type BoundsFn<'a> = dyn Fn(&str) -> (u32, u32) + 'a;

/// 从 AST 抽取该公式依赖的所有单元格键（展开区域为逐格）。
pub fn extract_deps(
    node: &AstNode,
    default_sheet: &str,
    bounds: Option<&BoundsFn>,
) -> Vec<CellKey> {
    let mut deps: HashSet<CellKey> = HashSet::new();
    walk(node, default_sheet, &mut deps, bounds);
    deps.into_iter().collect()
}

/// 端点解析：整列 col-only（row=-1）/整行 row-only（col=-1）/整格。
struct Endpoint {
    sheet: String,
    row: i64,
    col: i64,
}

fn split_local(reference: &str, default_sheet: &str) -> (String, String) {
    match reference.find('!') {
        Some(b) => {
            let sheet = reference[..b].trim_matches('\'').to_string();
            (sheet, reference[b + 1..].to_string())
        }
        None => (default_sheet.to_string(), reference.to_string()),
    }
}

fn ref_to_key(reference: &str, default_sheet: &str) -> Option<CellKey> {
    let (sheet, local) = split_local(reference, default_sheet);
    let clean = local.replace('$', "");
    parse_addr(&clean).map(|p| cell_key(&sheet, p.row, p.col))
}

fn endpoint_of(reference: &str, default_sheet: &str) -> Endpoint {
    let (sheet, local) = split_local(reference, default_sheet);
    let clean = local.replace('$', "");
    if let Some(p) = parse_addr(&clean) {
        return Endpoint {
            sheet,
            row: p.row as i64,
            col: p.col as i64,
        };
    }
    // 整列：纯字母
    if !clean.is_empty() && clean.chars().all(|c| c.is_ascii_alphabetic()) {
        if let Some(c) = label_to_col(&clean) {
            return Endpoint {
                sheet,
                row: -1,
                col: c as i64,
            };
        }
    }
    // 整行：纯数字
    if !clean.is_empty() && clean.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(r) = clean.parse::<i64>() {
            return Endpoint {
                sheet,
                row: r - 1,
                col: -1,
            };
        }
    }
    Endpoint {
        sheet,
        row: -1,
        col: -1,
    }
}

fn walk(node: &AstNode, sheet: &str, deps: &mut HashSet<CellKey>, bounds: Option<&BoundsFn>) {
    match node {
        AstNode::Ref(r) => {
            if let Some(k) = ref_to_key(r, sheet) {
                deps.insert(k);
            }
        }
        AstNode::Range { start, end } => {
            let a = endpoint_of(start, sheet);
            let b = endpoint_of(end, sheet);
            let range_sheet = a.sheet.clone();
            let (rows, cols) = bounds.map(|f| f(&range_sheet)).unwrap_or((0, 0));
            let ar = if a.row < 0 { 0 } else { a.row };
            let br = if b.row < 0 {
                (rows as i64 - 1).max(0)
            } else {
                b.row
            };
            let ac = if a.col < 0 { 0 } else { a.col };
            let bc = if b.col < 0 {
                (cols as i64 - 1).max(0)
            } else {
                b.col
            };
            let (r1, r2) = (ar.min(br), ar.max(br));
            let (c1, c2) = (ac.min(bc), ac.max(bc));
            for r in r1..=r2 {
                for c in c1..=c2 {
                    deps.insert(cell_key(&range_sheet, r as u32, c as u32));
                }
            }
        }
        AstNode::Unary { operand, .. } => walk(operand, sheet, deps, bounds),
        AstNode::Binary { left, right, .. } => {
            walk(left, sheet, deps, bounds);
            walk(right, sheet, deps, bounds);
        }
        AstNode::Call { args, .. } => {
            for a in args {
                walk(a, sheet, deps, bounds);
            }
        }
        AstNode::Array(rows) => {
            for row in rows {
                for cell in row {
                    walk(cell, sheet, deps, bounds);
                }
            }
        }
        // number/string/name: 无依赖（命名区域依赖由重算时动态解析，volatile 兜底）
        _ => {}
    }
}

/// 拓扑排序结果。
pub struct TopoResult {
    pub order: Vec<CellKey>,
    pub cyclic: HashSet<CellKey>,
}

/// 依赖图：记录每个公式格的依赖集，提供拓扑序 + 环检测。
#[derive(Default)]
pub struct DependencyGraph {
    deps: HashMap<CellKey, HashSet<CellKey>>,
    dependents: HashMap<CellKey, HashSet<CellKey>>,
}

impl DependencyGraph {
    pub fn new() -> Self {
        DependencyGraph::default()
    }

    /// 设/更新一个公式格的依赖。
    pub fn set_deps(&mut self, cell: &str, deps: Vec<CellKey>) {
        self.clear_deps(cell);
        let set: HashSet<CellKey> = deps.into_iter().collect();
        for d in &set {
            self.dependents
                .entry(d.clone())
                .or_default()
                .insert(cell.to_string());
        }
        self.deps.insert(cell.to_string(), set);
    }

    /// 移除一个公式格（改回普通值时）。
    pub fn clear_deps(&mut self, cell: &str) {
        if let Some(old) = self.deps.remove(cell) {
            for d in &old {
                if let Some(back) = self.dependents.get_mut(d) {
                    back.remove(cell);
                }
            }
        }
    }

    pub fn has_formula(&self, cell: &str) -> bool {
        self.deps.contains_key(cell)
    }

    pub fn clear_all(&mut self) {
        self.deps.clear();
        self.dependents.clear();
    }

    /// 直接依赖 cell 的公式格。
    pub fn get_dependents(&self, cell: &str) -> Vec<CellKey> {
        self.dependents
            .get(cell)
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// 受一批种子格变化影响的所有公式格（传递闭包，不含种子自身除非被别的依赖）。
    pub fn affected_by(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        let mut affected: HashSet<CellKey> = HashSet::new();
        let mut stack: Vec<CellKey> = seeds.to_vec();
        while let Some(cur) = stack.pop() {
            if let Some(deps) = self.dependents.get(&cur) {
                for dep in deps {
                    if affected.insert(dep.clone()) {
                        stack.push(dep.clone());
                    }
                }
            }
        }
        affected
    }

    /// 对给定公式格集合做拓扑排序（被依赖者在前）。环上格收集到 cyclic。
    pub fn topo_sort<I: IntoIterator<Item = CellKey>>(&self, cells: I) -> TopoResult {
        let targets: HashSet<CellKey> = cells.into_iter().collect();
        #[derive(Clone, Copy, PartialEq)]
        enum Color {
            White,
            Grey,
            Black,
        }
        let mut color: HashMap<CellKey, Color> = HashMap::new();
        let mut order: Vec<CellKey> = Vec::new();
        let mut cyclic: HashSet<CellKey> = HashSet::new();

        // 迭代式 DFS（避免深链爆栈）。栈帧记录当前格 + 其未处理依赖迭代器位置。
        for start in &targets {
            if color.get(start).copied().unwrap_or(Color::White) != Color::White {
                continue;
            }
            // path 显式栈：(cell, deps 列表, 下一个待访 index)
            let mut stack: Vec<(CellKey, Vec<CellKey>, usize)> = Vec::new();
            let deps0 = self.sorted_target_deps(start, &targets);
            color.insert(start.clone(), Color::Grey);
            stack.push((start.clone(), deps0, 0));

            while let Some((_cell, deps, idx)) = stack.last_mut() {
                if *idx < deps.len() {
                    let dep = deps[*idx].clone();
                    *idx += 1;
                    match color.get(&dep).copied().unwrap_or(Color::White) {
                        Color::Grey => {
                            // 环：把 path 中从 dep 起到栈顶全标环上
                            if let Some(start_idx) = stack.iter().position(|(c, _, _)| c == &dep) {
                                for (c, _, _) in &stack[start_idx..] {
                                    cyclic.insert(c.clone());
                                }
                            }
                        }
                        Color::White => {
                            let d = self.sorted_target_deps(&dep, &targets);
                            color.insert(dep.clone(), Color::Grey);
                            stack.push((dep, d, 0));
                        }
                        Color::Black => {}
                    }
                } else {
                    // 完成：出栈
                    let (c, _, _) = stack.pop().unwrap();
                    color.insert(c.clone(), Color::Black);
                    if !cyclic.contains(&c) {
                        order.push(c);
                    }
                }
            }
        }
        // order 是「完成序」= 依赖在前的拓扑序；环上格已排除
        order.retain(|c| !cyclic.contains(c));
        TopoResult { order, cyclic }
    }

    /// 取 cell 依赖中仍在 targets 集内的（稳定排序，保证确定性）。
    fn sorted_target_deps(&self, cell: &str, targets: &HashSet<CellKey>) -> Vec<CellKey> {
        let mut v: Vec<CellKey> = self
            .deps
            .get(cell)
            .map(|s| s.iter().filter(|d| targets.contains(*d)).cloned().collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_formula;

    fn deps(src: &str) -> Vec<String> {
        let mut v = extract_deps(&parse_formula(src).unwrap(), "S", None);
        v.sort();
        v
    }

    #[test]
    fn extract_single_and_multi() {
        assert_eq!(deps("A1+1"), vec![cell_key("S", 0, 0)]);
        let mut expect = vec![cell_key("S", 0, 0), cell_key("S", 1, 1)];
        expect.sort();
        assert_eq!(deps("A1+B2"), expect);
    }

    #[test]
    fn extract_range_expands() {
        let mut expect = vec![
            cell_key("S", 0, 0),
            cell_key("S", 1, 0),
            cell_key("S", 2, 0),
        ];
        expect.sort();
        assert_eq!(deps("SUM(A1:A3)"), expect);
    }

    #[test]
    fn extract_cross_sheet_and_dedup() {
        assert_eq!(deps("Other!B1"), vec![cell_key("Other", 0, 1)]);
        assert_eq!(deps("SUM(1, 2, \"x\")"), Vec::<String>::new());
        assert_eq!(deps("A1+A1*A1"), vec![cell_key("S", 0, 0)]);
        assert_eq!(deps("$A$1"), vec![cell_key("S", 0, 0)]);
    }

    #[test]
    fn forward_reverse_edges() {
        let mut g = DependencyGraph::new();
        let c = cell_key("S", 0, 2);
        g.set_deps(&c, vec![cell_key("S", 0, 0), cell_key("S", 0, 1)]);
        assert!(g.has_formula(&c));
        assert!(g.get_dependents(&cell_key("S", 0, 0)).contains(&c));
    }

    #[test]
    fn clear_and_replace() {
        let mut g = DependencyGraph::new();
        let c = cell_key("S", 0, 2);
        g.set_deps(&c, vec![cell_key("S", 0, 0)]);
        g.clear_deps(&c);
        assert!(!g.has_formula(&c));
        assert!(!g.get_dependents(&cell_key("S", 0, 0)).contains(&c));

        g.set_deps(&c, vec![cell_key("S", 0, 0)]);
        g.set_deps(&c, vec![cell_key("S", 0, 1)]);
        assert!(!g.get_dependents(&cell_key("S", 0, 0)).contains(&c));
        assert!(g.get_dependents(&cell_key("S", 0, 1)).contains(&c));
    }

    #[test]
    fn affected_transitive() {
        let mut g = DependencyGraph::new();
        let a = cell_key("S", 0, 0);
        let b = cell_key("S", 0, 1);
        let c = cell_key("S", 0, 2);
        g.set_deps(&b, vec![a.clone()]);
        g.set_deps(&c, vec![b.clone()]);
        let aff = g.affected_by(&[a]);
        assert!(aff.contains(&b));
        assert!(aff.contains(&c));
    }

    #[test]
    fn topo_orders_deps_first() {
        let mut g = DependencyGraph::new();
        let a = cell_key("S", 0, 0);
        let b = cell_key("S", 0, 1);
        let c = cell_key("S", 0, 2);
        g.set_deps(&b, vec![a]);
        g.set_deps(&c, vec![b.clone()]);
        let res = g.topo_sort([b.clone(), c.clone()]);
        assert_eq!(res.cyclic.len(), 0);
        let pb = res.order.iter().position(|x| x == &b).unwrap();
        let pc = res.order.iter().position(|x| x == &c).unwrap();
        assert!(pb < pc);
    }

    #[test]
    fn detects_2_cycle() {
        let mut g = DependencyGraph::new();
        let a = cell_key("S", 0, 0);
        let b = cell_key("S", 0, 1);
        g.set_deps(&a, vec![b.clone()]);
        g.set_deps(&b, vec![a.clone()]);
        let res = g.topo_sort([a.clone(), b.clone()]);
        assert!(res.cyclic.contains(&a));
        assert!(res.cyclic.contains(&b));
        assert_eq!(res.order.len(), 0);
    }

    #[test]
    fn detects_self_ref() {
        let mut g = DependencyGraph::new();
        let a = cell_key("S", 0, 0);
        g.set_deps(&a, vec![a.clone()]);
        let res = g.topo_sort([a.clone()]);
        assert!(res.cyclic.contains(&a));
    }

    #[test]
    fn detects_3_cycle() {
        let mut g = DependencyGraph::new();
        let a = cell_key("S", 0, 0);
        let b = cell_key("S", 0, 1);
        let c = cell_key("S", 0, 2);
        g.set_deps(&a, vec![c.clone()]);
        g.set_deps(&b, vec![a.clone()]);
        g.set_deps(&c, vec![b.clone()]);
        let res = g.topo_sort([a.clone(), b.clone(), c.clone()]);
        assert!(res.cyclic.contains(&a) && res.cyclic.contains(&b) && res.cyclic.contains(&c));
    }

    #[test]
    fn isolates_cycle_from_acyclic() {
        let mut g = DependencyGraph::new();
        let a = cell_key("S", 0, 0);
        let b = cell_key("S", 0, 1);
        let x = cell_key("S", 1, 0);
        let y = cell_key("S", 1, 1);
        g.set_deps(&a, vec![b.clone()]);
        g.set_deps(&b, vec![a.clone()]);
        g.set_deps(&y, vec![x]);
        let res = g.topo_sort([a.clone(), b.clone(), y.clone()]);
        assert!(res.cyclic.contains(&a) && res.cyclic.contains(&b));
        assert!(res.order.contains(&y));
        assert!(!res.order.contains(&a));
    }
}
