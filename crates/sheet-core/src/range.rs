//! 矩形区域的不可变值对象 + 区域代数。
//!
//! 承载选区、合并 span、样式套用范围、公式引用区域的几何运算。全部 0-based 闭区间。
//! 采用 (row, col, row_count, col_count) 形态，对齐 cmx-megasheet 的 Range.ts。

use crate::address::{format_range, parse_range, RangeCoord};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub row: u32,
    pub col: u32,
    pub row_count: u32,
    pub col_count: u32,
}

impl Range {
    /// row/col 为 0-based 起点；row_count/col_count 归一为 ≥1。
    pub fn new(row: u32, col: u32, row_count: u32, col_count: u32) -> Self {
        Range {
            row,
            col,
            row_count: row_count.max(1),
            col_count: col_count.max(1),
        }
    }

    /// 单格 1×1。
    pub fn cell(row: u32, col: u32) -> Self {
        Range::new(row, col, 1, 1)
    }

    /// 末行索引（闭区间，含）。
    pub fn last_row(&self) -> u32 {
        self.row + self.row_count - 1
    }

    /// 末列索引（闭区间，含）。
    pub fn last_col(&self) -> u32 {
        self.col + self.col_count - 1
    }

    /// 单元格数（row_count × col_count）。
    pub fn area(&self) -> u32 {
        self.row_count * self.col_count
    }

    /// 是否单格（1×1）。
    pub fn is_single_cell(&self) -> bool {
        self.row_count == 1 && self.col_count == 1
    }

    /// 从归一化坐标 {r1,c1,r2,c2} 构造。
    pub fn from_coord(coord: RangeCoord) -> Self {
        let r1 = coord.r1.min(coord.r2);
        let c1 = coord.c1.min(coord.c2);
        let r2 = coord.r1.max(coord.r2);
        let c2 = coord.c1.max(coord.c2);
        Range::new(r1, c1, r2 - r1 + 1, c2 - c1 + 1)
    }

    /// 从两个角点（任意顺序）构造。
    pub fn from_corners(row1: u32, col1: u32, row2: u32, col2: u32) -> Self {
        Range::from_coord(RangeCoord {
            r1: row1,
            c1: col1,
            r2: row2,
            c2: col2,
        })
    }

    /// 从 A1 区域字符串构造（"A1:C3" / "B2"）；非法返回 None。
    pub fn from_a1(a1: &str) -> Option<Self> {
        parse_range(a1).map(Range::from_coord)
    }

    /// 归一化坐标视图。
    pub fn to_coord(&self) -> RangeCoord {
        RangeCoord {
            r1: self.row,
            c1: self.col,
            r2: self.last_row(),
            c2: self.last_col(),
        }
    }

    /// A1 区域字符串（单格无冒号）。
    pub fn to_a1(&self) -> String {
        format_range(self.to_coord())
    }

    /// 是否包含单元格 (row,col)。
    pub fn contains_cell(&self, row: u32, col: u32) -> bool {
        row >= self.row && row <= self.last_row() && col >= self.col && col <= self.last_col()
    }

    /// 是否完全包含另一区域。
    pub fn contains_range(&self, other: &Range) -> bool {
        other.row >= self.row
            && other.col >= self.col
            && other.last_row() <= self.last_row()
            && other.last_col() <= self.last_col()
    }

    /// 与另一区域是否相交（有公共单元格）。
    pub fn intersects(&self, other: &Range) -> bool {
        !(other.row > self.last_row()
            || other.last_row() < self.row
            || other.col > self.last_col()
            || other.last_col() < self.col)
    }

    /// 交集；无交返回 None。
    pub fn intersect(&self, other: &Range) -> Option<Range> {
        if !self.intersects(other) {
            return None;
        }
        let r1 = self.row.max(other.row);
        let c1 = self.col.max(other.col);
        let r2 = self.last_row().min(other.last_row());
        let c2 = self.last_col().min(other.last_col());
        Some(Range::from_corners(r1, c1, r2, c2))
    }

    /// 包围盒并集（覆盖两区域的最小矩形；非集合并）。
    pub fn bounding_union(&self, other: &Range) -> Range {
        let r1 = self.row.min(other.row);
        let c1 = self.col.min(other.col);
        let r2 = self.last_row().max(other.last_row());
        let c2 = self.last_col().max(other.last_col());
        Range::from_corners(r1, c1, r2, c2)
    }

    /// 平移（负数向上/左；结果行列被 clamp 到 ≥0）。
    pub fn translate(&self, delta_row: i64, delta_col: i64) -> Range {
        let nr = (self.row as i64 + delta_row).max(0) as u32;
        let nc = (self.col as i64 + delta_col).max(0) as u32;
        Range::new(nr, nc, self.row_count, self.col_count)
    }

    /// 遍历每个单元格坐标（行优先）。
    pub fn for_each_cell<F: FnMut(u32, u32)>(&self, mut f: F) {
        for r in self.row..=self.last_row() {
            for c in self.col..=self.last_col() {
                f(r, c);
            }
        }
    }

    /// 收集所有单元格坐标（行优先）。
    pub fn cells(&self) -> Vec<(u32, u32)> {
        let mut out = Vec::with_capacity(self.area() as usize);
        self.for_each_cell(|r, c| out.push((r, c)));
        out
    }
}

impl std::fmt::Display for Range {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Range({})", self.to_a1())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_counts() {
        let r = Range::new(2, 3, 0, 0);
        assert_eq!(r.row_count, 1);
        assert_eq!(r.col_count, 1);
    }

    #[test]
    fn last_area_single() {
        let r = Range::new(1, 1, 3, 2);
        assert_eq!(r.last_row(), 3);
        assert_eq!(r.last_col(), 2);
        assert_eq!(r.area(), 6);
        assert!(!r.is_single_cell());
        assert!(Range::cell(0, 0).is_single_cell());
    }

    #[test]
    fn from_coord_normalizes() {
        let r = Range::from_coord(RangeCoord {
            r1: 2,
            c1: 2,
            r2: 0,
            c2: 0,
        });
        assert_eq!((r.row, r.col, r.row_count, r.col_count), (0, 0, 3, 3));
    }

    #[test]
    fn from_corners_any_order() {
        assert_eq!(Range::from_corners(3, 0, 0, 3).to_a1(), "A1:D4");
    }

    #[test]
    fn a1_round_trip() {
        for s in ["A1:C3", "B2", "A1:Z100"] {
            assert_eq!(Range::from_a1(s).unwrap().to_a1(), s);
        }
    }

    #[test]
    fn from_a1_garbage() {
        assert_eq!(Range::from_a1("nope"), None);
    }

    #[test]
    fn contains_cell() {
        let b = Range::new(1, 1, 3, 3); // B2:D4
        assert!(b.contains_cell(1, 1));
        assert!(b.contains_cell(3, 3));
        assert!(!b.contains_cell(0, 0));
        assert!(!b.contains_cell(4, 3));
    }

    #[test]
    fn contains_range() {
        let b = Range::new(1, 1, 3, 3);
        assert!(b.contains_range(&Range::new(2, 2, 1, 1)));
        assert!(b.contains_range(&b));
        assert!(!b.contains_range(&Range::new(0, 0, 5, 5)));
    }

    #[test]
    fn intersect_union() {
        let a = Range::new(0, 0, 3, 3); // A1:C3
        let b = Range::new(2, 2, 3, 3); // C3:E5
        assert!(a.intersects(&b));
        assert_eq!(a.intersect(&b).unwrap().to_a1(), "C3");

        let a = Range::new(0, 0, 2, 2);
        let b = Range::new(10, 10, 2, 2);
        assert!(!a.intersects(&b));
        assert_eq!(a.intersect(&b), None);

        // 边邻不相交
        let a = Range::new(0, 0, 2, 2); // A1:B2
        let b = Range::new(0, 2, 2, 2); // C1:D2
        assert!(!a.intersects(&b));

        let a = Range::new(0, 0, 1, 1);
        let b = Range::new(4, 4, 1, 1);
        assert_eq!(a.bounding_union(&b).to_a1(), "A1:E5");
    }

    #[test]
    fn equality_translate_iter() {
        assert_eq!(Range::new(1, 1, 2, 2), Range::new(1, 1, 2, 2));
        assert_ne!(Range::new(1, 1, 2, 2), Range::new(1, 1, 2, 3));

        assert_eq!(Range::new(2, 2, 1, 1).translate(-1, 1).to_a1(), "D2");
        assert_eq!(Range::new(0, 0, 1, 1).translate(-5, -5).to_a1(), "A1");

        let mut seen = Vec::new();
        Range::new(0, 0, 2, 2).for_each_cell(|r, c| seen.push(format!("{r},{c}")));
        assert_eq!(seen, vec!["0,0", "0,1", "1,0", "1,1"]);

        let cells = Range::new(0, 0, 2, 3).cells();
        assert_eq!(cells.len(), 6);
        assert_eq!(cells[0], (0, 0));
        assert_eq!(cells[5], (1, 2));
    }
}
