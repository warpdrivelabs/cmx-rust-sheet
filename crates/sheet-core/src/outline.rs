//! 大纲轴（行或列共用逻辑）：多级嵌套分组 + 折叠态 + 层级派生。
//!
//! 对标 cmx-megasheet 的 Worksheet.ts 内 OutlineAxis 类。level 由包含关系自动算
//! （A 包含 B → B.level = A.level+1）。折叠隐藏明细（不含汇总行/列），汇总位置由
//! summaryBelow/summaryRight 决定（在 Worksheet 层传入）。

use std::collections::HashSet;

/// 大纲分组（一段连续行/列 + 层级 + 折叠态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineGroup {
    pub start: u32,
    pub count: u32,
    pub collapsed: bool,
    /// 层级：被多少已有分组严格包含（0 = 顶层）。派生缓存。
    pub level: u32,
}

#[derive(Debug, Clone, Default)]
pub struct OutlineAxis {
    groups: Vec<OutlineGroup>,
}

impl OutlineAxis {
    pub fn new() -> Self {
        OutlineAxis::default()
    }

    /// 成组 [start, start+count)。count<1 忽略。level 自动派生。已存在完全相同段则忽略。
    pub fn group(&mut self, start: u32, count: u32) {
        if count < 1 {
            return;
        }
        if self
            .groups
            .iter()
            .any(|g| g.start == start && g.count == count)
        {
            return;
        }
        self.groups.push(OutlineGroup {
            start,
            count,
            collapsed: false,
            level: 0,
        });
        self.recompute_levels();
    }

    /// 取消覆盖某索引的**最内层**分组（对齐 Excel：ungroup 从最深层剥）。
    pub fn ungroup(&mut self, index: u32) {
        let target = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, g)| index >= g.start && index < g.start + g.count)
            .max_by_key(|(_, g)| g.level)
            .map(|(i, _)| i);
        if let Some(i) = target {
            self.groups.remove(i);
            self.recompute_levels();
        }
    }

    /// 移除完全等于 [start,count) 的分组（供 ungroup 命令精确撤销）。
    pub fn remove_exact(&mut self, start: u32, count: u32) -> bool {
        if let Some(idx) = self
            .groups
            .iter()
            .position(|g| g.start == start && g.count == count)
        {
            self.groups.remove(idx);
            self.recompute_levels();
            true
        } else {
            false
        }
    }

    /// 设某索引所在**最内层**分组的折叠态。
    pub fn set_collapsed(&mut self, index: u32, collapsed: bool) {
        let mut best: Option<usize> = None;
        let mut best_level = 0u32;
        for (i, g) in self.groups.iter().enumerate() {
            if index >= g.start
                && index < g.start + g.count
                && (best.is_none() || g.level > best_level)
            {
                best = Some(i);
                best_level = g.level;
            }
        }
        if let Some(i) = best {
            self.groups[i].collapsed = collapsed;
        }
    }

    /// 直接设第 i 个分组折叠态（供按索引切）。
    pub fn set_collapsed_at(&mut self, group_index: usize, collapsed: bool) {
        if let Some(g) = self.groups.get_mut(group_index) {
            g.collapsed = collapsed;
        }
    }

    pub fn list(&self) -> &[OutlineGroup] {
        &self.groups
    }

    pub fn clear(&mut self) {
        self.groups.clear();
    }

    /// 最深层级（无分组返回 None；对齐 TS 的 -1）。层级按钮渲染 1..maxLevel+2。
    pub fn max_level(&self) -> Option<u32> {
        self.groups.iter().map(|g| g.level).max()
    }

    /// 某行/列的 **Excel 大纲层级** = 把它当**明细**（非汇总）包含的分组数（0 = 顶层/汇总）。
    /// 这是 XLSX `<row/col outlineLevel>` 的正确取值：汇总行/列比其明细低一级。
    /// summary_after=true → 汇总在组末（明细 = `[start, start+count-1)`）；
    /// false → 汇总在组首（明细 = `(start, start+count)`）。
    pub fn detail_level_at(&self, index: u32, summary_after: bool) -> u32 {
        self.groups
            .iter()
            .filter(|g| {
                if summary_after {
                    index >= g.start && index < g.start + g.count - 1
                } else {
                    index > g.start && index < g.start + g.count
                }
            })
            .count() as u32
    }

    /// 是否某行/列因所在某个**已折叠**分组而被隐藏（供 XLSX 导出标 hidden + collapsed）。
    /// summary_after 决定汇总在明细后（true）还是前（false），与 [`Self::hidden_indices`] 同义。
    pub fn is_hidden_by_collapse(&self, index: u32, summary_after: bool) -> bool {
        self.hidden_indices(summary_after).contains(&index)
    }

    /// 是否该行/列是某个**已折叠**分组的「汇总边界」——即 Excel 里应标 `collapsed="1"`
    /// 的那一格（明细全隐后，仍可见的汇总行/列，带 `+` 展开钮）。
    /// summary_after=true → 汇总在明细后一格（组末+1）；false → 汇总在明细前一格（组首）。
    pub fn is_collapse_boundary(&self, index: u32, summary_after: bool) -> bool {
        self.groups.iter().any(|g| {
            if !g.collapsed {
                return false;
            }
            if summary_after {
                // 汇总在组的下/右侧：明细 [start, start+count-1)，汇总在 start+count-1
                index == g.start + g.count - 1
            } else {
                // 汇总在组的上/左侧：汇总在 start，明细 (start, start+count)
                index == g.start
            }
        })
    }

    /// 层级折叠：折叠 level ≥ (n-1) 的所有分组，展开更浅的（n 为 1-based）。
    pub fn collapse_to_level(&mut self, n: u32) {
        let threshold = n.saturating_sub(1);
        for g in &mut self.groups {
            g.collapsed = g.level >= threshold;
        }
    }

    pub fn expand_all(&mut self) {
        for g in &mut self.groups {
            g.collapsed = false;
        }
    }

    /// 当前折叠态下应隐藏的索引集合。折叠一个分组 → 隐藏其明细（不含汇总）。
    /// summary_after: true=汇总在末端（明细=[start,end-1)）；false=汇总在首端（明细=[start+1,end)）。
    pub fn hidden_indices(&self, summary_after: bool) -> HashSet<u32> {
        let mut hidden = HashSet::new();
        for g in &self.groups {
            if !g.collapsed {
                continue;
            }
            let end = g.start + g.count;
            if summary_after {
                for i in g.start..end.saturating_sub(1) {
                    hidden.insert(i);
                }
            } else {
                for i in (g.start + 1)..end {
                    hidden.insert(i);
                }
            }
        }
        hidden
    }

    /// 派生每个分组的 level = 被多少其它分组严格包含，并稳定排序（start 升、level 升）。
    fn recompute_levels(&mut self) {
        let snapshot: Vec<(u32, u32)> = self.groups.iter().map(|g| (g.start, g.count)).collect();
        for g in &mut self.groups {
            let mut level = 0;
            for &(os, oc) in &snapshot {
                if os == g.start && oc == g.count {
                    continue; // 跳过自身（含同段重复，理论上已去重）
                }
                // other 严格包含 g（且不等）
                if os <= g.start && os + oc >= g.start + g.count {
                    level += 1;
                }
            }
            g.level = level;
        }
        self.groups
            .sort_by(|a, b| a.start.cmp(&b.start).then(a.level.cmp(&b.level)));
    }

    /// 行列增删时搬移分组起点（插入）。
    pub fn shift_insert(&mut self, before: u32, count: u32) {
        for g in &mut self.groups {
            if g.start >= before {
                g.start += count;
            }
        }
    }

    /// 行列增删时搬移分组起点（删除）；与删除区间重叠的分组丢弃。
    pub fn shift_delete(&mut self, start: u32, count: u32) {
        let end = start + count;
        self.groups
            .retain(|g| g.start >= end || g.start + g.count <= start);
        for g in &mut self.groups {
            if g.start >= end {
                g.start -= count;
            }
        }
        self.recompute_levels();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_and_derive_level() {
        let mut ax = OutlineAxis::new();
        ax.group(1, 8); // 外层
        ax.group(2, 4); // 内层（被外层包含）
        let list = ax.list();
        assert_eq!(list.len(), 2);
        // 排序后：start 升
        assert_eq!(list[0].start, 1);
        assert_eq!(list[0].level, 0);
        assert_eq!(list[1].start, 2);
        assert_eq!(list[1].level, 1);
        assert_eq!(ax.max_level(), Some(1));
    }

    #[test]
    fn dedup_same_segment() {
        let mut ax = OutlineAxis::new();
        ax.group(3, 4);
        ax.group(3, 4);
        assert_eq!(ax.list().len(), 1);
    }

    #[test]
    fn ungroup_removes_innermost() {
        let mut ax = OutlineAxis::new();
        ax.group(1, 8);
        ax.group(2, 4);
        ax.ungroup(3); // 落在两层内，剥最深
        assert_eq!(ax.list().len(), 1);
        assert_eq!(ax.list()[0].count, 8); // 外层留存
    }

    #[test]
    fn hidden_indices_summary_after() {
        let mut ax = OutlineAxis::new();
        ax.group(2, 4); // rows 2..5, 汇总在 row5
        ax.set_collapsed(2, true);
        let hidden = ax.hidden_indices(true);
        // 明细 = [2,5) 去掉末端汇总 5 → {2,3,4}... 实际 end-1=5, 区间 2..5 = {2,3,4}
        assert_eq!(hidden, HashSet::from([2, 3, 4]));
    }

    #[test]
    fn hidden_indices_summary_before() {
        let mut ax = OutlineAxis::new();
        ax.group(2, 4); // rows 2..5, 汇总在 row2
        ax.set_collapsed(2, true);
        let hidden = ax.hidden_indices(false);
        // 明细 = [3,6) → {3,4,5}
        assert_eq!(hidden, HashSet::from([3, 4, 5]));
    }

    #[test]
    fn collapse_to_level() {
        let mut ax = OutlineAxis::new();
        ax.group(1, 8);
        ax.group(2, 4);
        ax.collapse_to_level(2); // 折叠 level>=1
        assert!(!ax.list()[0].collapsed); // level0 展开
        assert!(ax.list()[1].collapsed); // level1 折叠
        ax.expand_all();
        assert!(ax.list().iter().all(|g| !g.collapsed));
    }

    #[test]
    fn detail_level_at_summary_first() {
        // M99 行分组（summaryBelow=false，汇总在组首）：合计[2..8] ⊃ 甲[3..5] / 乙[6..8]
        let mut ax = OutlineAxis::new();
        ax.group(2, 7); // 2..8, 汇总在 row2
        ax.group(3, 3); // 3..5, 汇总在 row3
        ax.group(6, 3); // 6..8, 汇总在 row6
        assert_eq!(ax.detail_level_at(1, false), 0); // 组外
        assert_eq!(ax.detail_level_at(2, false), 0); // 合计（汇总行本身，顶层）
        assert_eq!(ax.detail_level_at(3, false), 1); // 甲小计（合计的明细 + 甲的汇总）
        assert_eq!(ax.detail_level_at(4, false), 2); // 上海A（合计+甲 的明细）
        assert_eq!(ax.detail_level_at(5, false), 2);
        assert_eq!(ax.detail_level_at(6, false), 1); // 华南小计
        assert_eq!(ax.detail_level_at(7, false), 2);
        assert_eq!(ax.detail_level_at(8, false), 2);
    }

    #[test]
    fn detail_level_at_summary_after() {
        // 汇总在组末：组[2..6)=2,3,4,5，汇总在 5
        let mut ax = OutlineAxis::new();
        ax.group(2, 4); // 2..5, 汇总在 row5
        assert_eq!(ax.detail_level_at(2, true), 1); // 明细
        assert_eq!(ax.detail_level_at(4, true), 1);
        assert_eq!(ax.detail_level_at(5, true), 0); // 汇总行本身
    }

    #[test]
    fn is_hidden_by_collapse_matches_hidden_indices() {
        let mut ax = OutlineAxis::new();
        ax.group(2, 4); // 2..5, 汇总在后（summary_after=true）→ 明细 2..4
        ax.set_collapsed(2, true);
        assert!(ax.is_hidden_by_collapse(2, true));
        assert!(ax.is_hidden_by_collapse(4, true));
        assert!(!ax.is_hidden_by_collapse(5, true)); // 汇总行不隐
    }

    #[test]
    fn shift_on_insert() {
        let mut ax = OutlineAxis::new();
        ax.group(3, 4);
        ax.shift_insert(0, 2);
        assert_eq!(ax.list()[0].start, 5);
        assert_eq!(ax.list()[0].count, 4);
    }

    #[test]
    fn shift_on_delete_drops_overlap() {
        let mut ax = OutlineAxis::new();
        ax.group(3, 4); // 3..7
        ax.group(10, 2); // 10..12
        ax.shift_delete(3, 2); // 删 3..5，与首组重叠→丢弃；次组左移
        assert_eq!(ax.list().len(), 1);
        assert_eq!(ax.list()[0].start, 8); // 10-2
    }
}
