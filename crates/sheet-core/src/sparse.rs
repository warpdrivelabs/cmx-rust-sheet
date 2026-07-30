//! 稀疏二维存储。
//!
//! 电子表格绝大多数单元格为空，稠密数组会浪费海量内存。此结构只存非空槽位，
//! 空格零成本。键为 (row, col)，值为泛型 T。对标 cmx-megasheet 的 SparseMatrix.ts。
//!
//! 关键职责是**行列增删时的坐标搬移**：插入/删除行列后受影响的已存槽位整体平移，
//! 删除区间内的槽位丢弃。这是 Worksheet 行列操作在数据层的真正实现。

use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct SparseMatrix<T> {
    map: HashMap<(u32, u32), T>,
}

impl<T> SparseMatrix<T> {
    pub fn new() -> Self {
        SparseMatrix {
            map: HashMap::new(),
        }
    }

    /// 非空槽位数。
    pub fn size(&self) -> usize {
        self.map.len()
    }

    pub fn get(&self, row: u32, col: u32) -> Option<&T> {
        self.map.get(&(row, col))
    }

    pub fn get_mut(&mut self, row: u32, col: u32) -> Option<&mut T> {
        self.map.get_mut(&(row, col))
    }

    pub fn has(&self, row: u32, col: u32) -> bool {
        self.map.contains_key(&(row, col))
    }

    /// 设值。Rust 侧用 `Option` 显式表达「None = 删除」（对齐 TS 的 `set(...,undefined)`）。
    pub fn set(&mut self, row: u32, col: u32, value: Option<T>) {
        match value {
            Some(v) => {
                self.map.insert((row, col), v);
            }
            None => {
                self.map.remove(&(row, col));
            }
        }
    }

    /// 便捷：直接存一个值。
    pub fn insert(&mut self, row: u32, col: u32, value: T) {
        self.map.insert((row, col), value);
    }

    pub fn delete(&mut self, row: u32, col: u32) -> bool {
        self.map.remove(&(row, col)).is_some()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    /// 遍历所有非空槽位（顺序 = HashMap 迭代序，不保证行列有序）。
    pub fn for_each<F: FnMut(&T, u32, u32)>(&self, mut f: F) {
        for (&(row, col), v) in &self.map {
            f(v, row, col);
        }
    }

    /// 逐槽位 (row, col, &value) 迭代。
    pub fn entries(&self) -> impl Iterator<Item = (u32, u32, &T)> {
        self.map.iter().map(|(&(r, c), v)| (r, c, v))
    }

    /// 已占用的最大行索引（无数据返回 None；对齐 TS 的 -1）。
    pub fn max_row(&self) -> Option<u32> {
        self.map.keys().map(|&(r, _)| r).max()
    }

    /// 已占用的最大列索引（无数据返回 None）。
    pub fn max_col(&self) -> Option<u32> {
        self.map.keys().map(|&(_, c)| c).max()
    }

    /// 在 `before` 行之前插入 count 行：row ≥ before 的槽位整体下移 count。
    pub fn insert_rows(&mut self, before: u32, count: u32) {
        if count == 0 {
            return;
        }
        self.shift_rows(before, count as i64);
    }

    /// 删除 [start, start+count) 行：区间内槽位丢弃，row ≥ start+count 的上移 count。
    pub fn delete_rows(&mut self, start: u32, count: u32) {
        if count == 0 {
            return;
        }
        let end = start + count;
        self.map.retain(|&(row, _), _| !(row >= start && row < end));
        self.shift_rows(end, -(count as i64));
    }

    /// 在 `before` 列之前插入 count 列。
    pub fn insert_columns(&mut self, before: u32, count: u32) {
        if count == 0 {
            return;
        }
        self.shift_cols(before, count as i64);
    }

    /// 删除 [start, start+count) 列。
    pub fn delete_columns(&mut self, start: u32, count: u32) {
        if count == 0 {
            return;
        }
        let end = start + count;
        self.map.retain(|&(_, col), _| !(col >= start && col < end));
        self.shift_cols(end, -(count as i64));
    }

    /// 把 row ≥ threshold 的槽位行号加 delta，重建 map（避免搬移途中键碰撞）。
    fn shift_rows(&mut self, threshold: u32, delta: i64) {
        if delta == 0 {
            return;
        }
        let old = std::mem::take(&mut self.map);
        for ((row, col), v) in old {
            let nr = if row >= threshold {
                (row as i64 + delta) as u32
            } else {
                row
            };
            self.map.insert((nr, col), v);
        }
    }

    fn shift_cols(&mut self, threshold: u32, delta: i64) {
        if delta == 0 {
            return;
        }
        let old = std::mem::take(&mut self.map);
        for ((row, col), v) in old {
            let nc = if col >= threshold {
                (col as i64 + delta) as u32
            } else {
                col
            };
            self.map.insert((row, nc), v);
        }
    }
}

impl<T: Clone> SparseMatrix<T> {
    /// 深拷贝（值 clone）。TS 的 clone 是浅拷贝，Rust 无共享引用语义，用 Clone 等价。
    pub fn clone_matrix(&self) -> SparseMatrix<T> {
        SparseMatrix {
            map: self.map.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// 快照成有序 map，便于断言（对齐 TS 测试的 snapshot 助手）。
    fn snapshot<T: Clone>(m: &SparseMatrix<T>) -> BTreeMap<(u32, u32), T> {
        let mut out = BTreeMap::new();
        m.for_each(|v, r, c| {
            out.insert((r, c), v.clone());
        });
        out
    }

    #[test]
    fn basic_get_set() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "a");
        m.insert(3, 5, "b");
        assert_eq!(m.get(0, 0), Some(&"a"));
        assert_eq!(m.get(3, 5), Some(&"b"));
        assert_eq!(m.get(1, 1), None);
        assert_eq!(m.size(), 2);
    }

    #[test]
    fn set_none_deletes() {
        let mut m: SparseMatrix<i32> = SparseMatrix::new();
        m.insert(1, 1, 9);
        m.set(1, 1, None);
        assert!(!m.has(1, 1));
        assert_eq!(m.size(), 0);
    }

    #[test]
    fn delete_and_clear() {
        let mut m: SparseMatrix<i32> = SparseMatrix::new();
        m.insert(0, 0, 1);
        m.insert(0, 1, 2);
        assert!(m.delete(0, 0));
        assert!(!m.delete(9, 9));
        m.clear();
        assert_eq!(m.size(), 0);
    }

    #[test]
    fn max_row_col() {
        let mut m: SparseMatrix<i32> = SparseMatrix::new();
        assert_eq!(m.max_row(), None);
        assert_eq!(m.max_col(), None);
        m.insert(2, 7, 1);
        m.insert(5, 3, 1);
        assert_eq!(m.max_row(), Some(5));
        assert_eq!(m.max_col(), Some(7));
    }

    #[test]
    fn insert_rows_shifts_down() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "r0");
        m.insert(1, 0, "r1");
        m.insert(2, 0, "r2");
        m.insert_rows(1, 2);
        let s = snapshot(&m);
        assert_eq!(s.get(&(0, 0)), Some(&"r0"));
        assert_eq!(s.get(&(3, 0)), Some(&"r1"));
        assert_eq!(s.get(&(4, 0)), Some(&"r2"));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn insert_at_zero() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "x");
        m.insert_rows(0, 1);
        assert_eq!(m.get(1, 0), Some(&"x"));
        assert!(!m.has(0, 0));
    }

    #[test]
    fn insert_count_zero_noop() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "x");
        m.insert_rows(0, 0);
        assert_eq!(snapshot(&m).get(&(0, 0)), Some(&"x"));
    }

    #[test]
    fn delete_rows_drops_and_shifts() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "r0");
        m.insert(1, 0, "r1");
        m.insert(2, 0, "r2");
        m.insert(3, 0, "r3");
        m.delete_rows(1, 2);
        let s = snapshot(&m);
        assert_eq!(s.len(), 2);
        assert_eq!(s.get(&(0, 0)), Some(&"r0"));
        assert_eq!(s.get(&(1, 0)), Some(&"r3"));
    }

    #[test]
    fn delete_first_row() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "a");
        m.insert(1, 0, "b");
        m.delete_rows(0, 1);
        let s = snapshot(&m);
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(&(0, 0)), Some(&"b"));
    }

    #[test]
    fn insert_columns_shifts_right() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "c0");
        m.insert(0, 1, "c1");
        m.insert_columns(1, 3);
        let s = snapshot(&m);
        assert_eq!(s.get(&(0, 0)), Some(&"c0"));
        assert_eq!(s.get(&(0, 4)), Some(&"c1"));
    }

    #[test]
    fn delete_columns_drops_shifts_left() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(0, 0, "c0");
        m.insert(0, 1, "c1");
        m.insert(0, 2, "c2");
        m.delete_columns(0, 1);
        let s = snapshot(&m);
        assert_eq!(s.get(&(0, 0)), Some(&"c1"));
        assert_eq!(s.get(&(0, 1)), Some(&"c2"));
    }

    #[test]
    fn axes_independent() {
        let mut m: SparseMatrix<&str> = SparseMatrix::new();
        m.insert(5, 5, "keep");
        m.insert_rows(0, 1);
        assert_eq!(m.get(6, 5), Some(&"keep"));
        m.insert_columns(0, 1);
        assert_eq!(m.get(6, 6), Some(&"keep"));
    }

    #[test]
    fn entries_and_clone() {
        let mut m: SparseMatrix<i32> = SparseMatrix::new();
        m.insert(0, 0, 1);
        m.insert(2, 3, 2);
        let mut es: Vec<_> = m.entries().map(|(r, c, v)| (r, c, *v)).collect();
        es.sort_by_key(|&(_, _, v)| v);
        assert_eq!(es, vec![(0, 0, 1), (2, 3, 2)]);

        let mut c = m.clone_matrix();
        c.insert(0, 0, 99);
        assert_eq!(m.get(0, 0), Some(&1));
        assert_eq!(c.get(0, 0), Some(&99));
    }
}
