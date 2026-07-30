//! 选区模型：多区选择 + 活动格 + 键盘导航几何。
//!
//! 对标 cmx-megasheet 的 SelectionModel.ts。纯几何、零 DOM——把选区的**运算**
//! （移动活动格、扩展选区、跨边界钳制、多区增补、跳边、区内回绕）抽成独立可测单元。
//!
//! Rust 移植：TS 用 `rowCount()/colCount()` 闭包读网格尺寸；这里存 `rows/cols` 字段，
//! 由调用方在网格尺寸变化时 `set_bounds` 同步（更简单，无闭包捕获）。

use crate::range::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveDir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct SelectionModel {
    ranges: Vec<Range>,
    active_row: u32,
    active_col: u32,
    rows: u32,
    cols: u32,
}

impl SelectionModel {
    pub fn new(rows: u32, cols: u32) -> Self {
        SelectionModel {
            ranges: vec![Range::new(0, 0, 1, 1)],
            active_row: 0,
            active_col: 0,
            rows: rows.max(1),
            cols: cols.max(1),
        }
    }

    /// 同步网格尺寸（行列增删后调用）。
    pub fn set_bounds(&mut self, rows: u32, cols: u32) {
        self.rows = rows.max(1);
        self.cols = cols.max(1);
    }

    /// 当前（最后一个）选区。
    pub fn primary(&self) -> Range {
        *self.ranges.last().unwrap()
    }

    pub fn ranges(&self) -> Vec<Range> {
        self.ranges.clone()
    }

    pub fn active(&self) -> (u32, u32) {
        (self.active_row, self.active_col)
    }

    fn clamp_row(&self, r: i64) -> u32 {
        r.clamp(0, (self.rows - 1) as i64) as u32
    }

    fn clamp_col(&self, c: i64) -> u32 {
        c.clamp(0, (self.cols - 1) as i64) as u32
    }

    /// 单格/区域选择（替换全部选区），活动格置于左上。
    pub fn select(&mut self, row: u32, col: u32, row_count: u32, col_count: u32) {
        let r = self.clamp_row(row as i64);
        let c = self.clamp_col(col as i64);
        self.ranges = vec![Range::new(r, c, row_count.max(1), col_count.max(1))];
        self.active_row = r;
        self.active_col = c;
    }

    /// 便捷：单格选择。
    pub fn select_cell(&mut self, row: u32, col: u32) {
        self.select(row, col, 1, 1);
    }

    /// 设选区为指定区域（活动格落区内左上）。
    pub fn select_range(&mut self, range: Range) {
        self.ranges = vec![range];
        self.active_row = self.clamp_row(range.row as i64);
        self.active_col = self.clamp_col(range.col as i64);
    }

    /// 追加一个选区（Ctrl+点击多选），活动格移到新区左上。
    pub fn add_range(&mut self, row: u32, col: u32, row_count: u32, col_count: u32) {
        let r = self.clamp_row(row as i64);
        let c = self.clamp_col(col as i64);
        self.ranges
            .push(Range::new(r, c, row_count.max(1), col_count.max(1)));
        self.active_row = r;
        self.active_col = c;
    }

    /// 只设活动格（不改选区形状）。
    pub fn set_active(&mut self, row: u32, col: u32) {
        self.active_row = self.clamp_row(row as i64);
        self.active_col = self.clamp_col(col as i64);
    }

    /// 移动活动格（方向键）：折叠为单格并移动。返回移动后的活动格。
    pub fn move_active(&mut self, dir: MoveDir) -> (u32, u32) {
        let (r, c) = self.next_cell(self.active_row, self.active_col, dir);
        self.select(r, c, 1, 1);
        (r, c)
    }

    /// 扩展选区（Shift+方向键）：活动格不动，把选区另一角朝 dir 推。
    pub fn extend(&mut self, dir: MoveDir) {
        let primary = self.primary();
        let anchor_r = self.active_row;
        let anchor_c = self.active_col;
        let float_r = if primary.row == anchor_r {
            primary.last_row()
        } else {
            primary.row
        };
        let float_c = if primary.col == anchor_c {
            primary.last_col()
        } else {
            primary.col
        };
        let (fr, fc) = self.next_cell(float_r, float_c, dir);
        let range = Range::from_corners(anchor_r, anchor_c, fr, fc);
        *self.ranges.last_mut().unwrap() = range;
    }

    /// 选中整行。
    pub fn select_row(&mut self, row: u32) {
        let r = self.clamp_row(row as i64);
        self.ranges = vec![Range::new(r, 0, 1, self.cols)];
        self.active_row = r;
        self.active_col = 0;
    }

    /// 选中整列。
    pub fn select_column(&mut self, col: u32) {
        let c = self.clamp_col(col as i64);
        self.ranges = vec![Range::new(0, c, self.rows, 1)];
        self.active_row = 0;
        self.active_col = c;
    }

    /// 全选。
    pub fn select_all(&mut self) {
        self.ranges = vec![Range::new(0, 0, self.rows, self.cols)];
        self.active_row = 0;
        self.active_col = 0;
    }

    /// 下一个单元格（钳制到网格边界，不环绕）。
    fn next_cell(&self, row: u32, col: u32, dir: MoveDir) -> (u32, u32) {
        match dir {
            MoveDir::Up => (self.clamp_row(row as i64 - 1), col),
            MoveDir::Down => (self.clamp_row(row as i64 + 1), col),
            MoveDir::Left => (row, self.clamp_col(col as i64 - 1)),
            MoveDir::Right => (row, self.clamp_col(col as i64 + 1)),
        }
    }

    // ── M10 键盘导航补全 ─────────────────────────────────

    /// Ctrl+方向键：跳到数据区边界。`is_empty(r,c)` 由调用方注入（读 sheet 值）。
    /// extend_sel=true 时扩选而非移动。
    pub fn jump_to_edge<F: Fn(u32, u32) -> bool>(
        &mut self,
        dir: MoveDir,
        is_empty: F,
        extend_sel: bool,
    ) {
        let from = if extend_sel {
            self.float_corner()
        } else {
            (self.active_row, self.active_col)
        };
        let (tr, tc) = self.compute_edge(from.0, from.1, dir, &is_empty);
        if extend_sel {
            let range = Range::from_corners(self.active_row, self.active_col, tr, tc);
            self.ranges = vec![range];
        } else {
            self.select(tr, tc, 1, 1);
        }
    }

    fn compute_edge<F: Fn(u32, u32) -> bool>(
        &self,
        row: u32,
        col: u32,
        dir: MoveDir,
        is_empty: &F,
    ) -> (u32, u32) {
        let (dr, dc): (i64, i64) = match dir {
            MoveDir::Up => (-1, 0),
            MoveDir::Down => (1, 0),
            MoveDir::Left => (0, -1),
            MoveDir::Right => (0, 1),
        };
        let max_r = (self.rows - 1) as i64;
        let max_c = (self.cols - 1) as i64;
        let in_bounds = |r: i64, c: i64| r >= 0 && r <= max_r && c >= 0 && c <= max_c;
        let r = row as i64;
        let c = col as i64;
        let (n1r, n1c) = (r + dr, c + dc);
        if !in_bounds(n1r, n1c) {
            return (row, col); // 已在边缘
        }
        let cur_empty = is_empty(row, col);
        let adj_empty = is_empty(n1r as u32, n1c as u32);
        if cur_empty || adj_empty {
            // 跳到该方向首个非空（跨过空白段）
            let (mut rr, mut cc) = (n1r, n1c);
            while in_bounds(rr, cc) && is_empty(rr as u32, cc as u32) {
                rr += dr;
                cc += dc;
            }
            if !in_bounds(rr, cc) {
                // 全空到边界：停在轴末
                return (self.clamp_row(rr - dr), self.clamp_col(cc - dc));
            }
            return (rr as u32, cc as u32);
        }
        // 当前+相邻都非空 → 跳到连续非空段末端
        let (mut rr, mut cc) = (r, c);
        loop {
            let (nr, nc) = (rr + dr, cc + dc);
            if !in_bounds(nr, nc) || is_empty(nr as u32, nc as u32) {
                break;
            }
            rr = nr;
            cc = nc;
        }
        (rr as u32, cc as u32)
    }

    fn float_corner(&self) -> (u32, u32) {
        let p = self.primary();
        let r = if p.row == self.active_row {
            p.last_row()
        } else {
            p.row
        };
        let c = if p.col == self.active_col {
            p.last_col()
        } else {
            p.col
        };
        (r, c)
    }

    /// Enter/Tab 提交后在选区内回绕。单格时退化为普通移动。返回移动后的活动格。
    /// dir: down=Enter, right=Tab。backward=Shift。
    pub fn move_in_selection(&mut self, dir: MoveDir, backward: bool) -> (u32, u32) {
        let p = self.primary();
        let multi = p.row_count > 1 || p.col_count > 1;
        if !multi {
            let move_dir = if backward {
                match dir {
                    MoveDir::Down => MoveDir::Up,
                    _ => MoveDir::Left,
                }
            } else {
                dir
            };
            let (r, c) = self.next_cell(self.active_row, self.active_col, move_dir);
            self.select(r, c, 1, 1);
            return (r, c);
        }
        // 区内相对坐标
        let mut rr = self.active_row as i64 - p.row as i64;
        let mut cc = self.active_col as i64 - p.col as i64;
        let rn = p.row_count as i64;
        let cn = p.col_count as i64;
        let step = if backward { -1 } else { 1 };
        if dir == MoveDir::Down {
            rr += step;
            if rr >= rn {
                rr = 0;
                cc += 1;
            } else if rr < 0 {
                rr = rn - 1;
                cc -= 1;
            }
            if cc >= cn {
                cc = 0;
            }
            if cc < 0 {
                cc = cn - 1;
            }
        } else {
            cc += step;
            if cc >= cn {
                cc = 0;
                rr += 1;
            } else if cc < 0 {
                cc = cn - 1;
                rr -= 1;
            }
            if rr >= rn {
                rr = 0;
            }
            if rr < 0 {
                rr = rn - 1;
            }
        }
        self.active_row = p.row + rr as u32;
        self.active_col = p.col + cc as u32;
        (self.active_row, self.active_col)
    }

    /// Home：移到当前行首列。
    pub fn move_to_row_start(&mut self) {
        self.select(self.active_row, 0, 1, 1);
    }

    /// Ctrl+Home：移到 A1。
    pub fn move_to_home(&mut self) {
        self.select(0, 0, 1, 1);
    }

    /// End / Ctrl+End：移到数据区末（缺省用轴末）。
    pub fn move_to_end(&mut self, last_row: Option<u32>, last_col: Option<u32>) {
        self.select(
            last_row.unwrap_or(self.rows - 1),
            last_col.unwrap_or(self.cols - 1),
            1,
            1,
        );
    }

    /// PageUp/PageDown：按给定行数翻页。
    pub fn page_move(&mut self, delta_rows: i64, extend_sel: bool) {
        let target = self.clamp_row(self.active_row as i64 + delta_rows);
        if extend_sel {
            let range =
                Range::from_corners(self.active_row, self.active_col, target, self.active_col);
            self.ranges = vec![range];
        } else {
            self.select(target, self.active_col, 1, 1);
        }
    }

    /// Ctrl+Space：选活动格所在整列。
    pub fn select_whole_column(&mut self) {
        self.select_column(self.active_col);
    }

    /// Shift+Space：选整行。
    pub fn select_whole_row(&mut self) {
        self.select_row(self.active_row);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(rows: u32, cols: u32) -> SelectionModel {
        SelectionModel::new(rows, cols)
    }

    #[test]
    fn defaults_a1() {
        let s = model(10, 8);
        assert_eq!(s.active(), (0, 0));
        assert_eq!(s.primary().to_a1(), "A1");
    }

    #[test]
    fn select_replaces_and_sets_active() {
        let mut s = model(10, 8);
        s.select(2, 3, 2, 2);
        assert_eq!(s.ranges().len(), 1);
        assert_eq!(s.active(), (2, 3));
        assert_eq!(s.primary().to_a1(), "D3:E4");
    }

    #[test]
    fn clamps_to_bounds() {
        let mut s = model(5, 5);
        s.select_cell(99, 99);
        assert_eq!(s.active(), (4, 4));
    }

    #[test]
    fn add_range_appends() {
        let mut s = model(10, 8);
        s.select_cell(0, 0);
        s.add_range(5, 5, 2, 2);
        assert_eq!(s.ranges().len(), 2);
        assert_eq!(s.active(), (5, 5));
    }

    #[test]
    fn move_each_direction() {
        let mut s = model(10, 8);
        s.select_cell(2, 2);
        assert_eq!(s.move_active(MoveDir::Down), (3, 2));
        assert_eq!(s.move_active(MoveDir::Right), (3, 3));
        assert_eq!(s.move_active(MoveDir::Up), (2, 3));
        assert_eq!(s.move_active(MoveDir::Left), (2, 2));
    }

    #[test]
    fn move_collapses_multicell() {
        let mut s = model(10, 8);
        s.select(0, 0, 3, 3);
        s.move_active(MoveDir::Down);
        assert!(s.primary().is_single_cell());
    }

    #[test]
    fn no_move_past_edges() {
        let mut s = model(5, 5);
        s.select_cell(0, 0);
        assert_eq!(s.move_active(MoveDir::Up), (0, 0));
        assert_eq!(s.move_active(MoveDir::Left), (0, 0));
        s.select_cell(4, 4);
        assert_eq!(s.move_active(MoveDir::Down), (4, 4));
        assert_eq!(s.move_active(MoveDir::Right), (4, 4));
    }

    #[test]
    fn extend_keeps_anchor() {
        let mut s = model(10, 8);
        s.select_cell(2, 2);
        s.extend(MoveDir::Down);
        assert_eq!(s.primary().to_a1(), "C3:C4");
        assert_eq!(s.active(), (2, 2));
        s.extend(MoveDir::Right);
        assert_eq!(s.primary().to_a1(), "C3:D4");
    }

    #[test]
    fn extend_shrinks_back() {
        let mut s = model(10, 8);
        s.select_cell(2, 2);
        s.extend(MoveDir::Down);
        s.extend(MoveDir::Down);
        assert_eq!(s.primary().to_a1(), "C3:C5");
        s.extend(MoveDir::Up);
        assert_eq!(s.primary().to_a1(), "C3:C4");
    }

    #[test]
    fn extend_above_anchor() {
        let mut s = model(10, 8);
        s.select_cell(5, 5);
        s.extend(MoveDir::Up);
        assert_eq!(s.primary().to_a1(), "F5:F6");
        assert_eq!(s.active(), (5, 5));
    }

    #[test]
    fn row_col_all() {
        let mut s = model(10, 8);
        s.select_row(3);
        assert_eq!(s.primary(), Range::new(3, 0, 1, 8));
        s.select_column(2);
        assert_eq!(s.primary(), Range::new(0, 2, 10, 1));
        s.select_all();
        assert_eq!(s.primary(), Range::new(0, 0, 10, 8));
    }

    #[test]
    fn jump_to_edge_data_end() {
        let mut s = model(20, 10);
        // A1:A3 非空
        let is_empty = |r: u32, c: u32| !((0..=2).contains(&r) && c == 0);
        s.select_cell(0, 0);
        s.jump_to_edge(MoveDir::Down, is_empty, false);
        assert_eq!(s.active(), (2, 0));
    }

    #[test]
    fn jump_from_empty_to_next() {
        let mut s = model(20, 10);
        let is_empty = |r: u32, c: u32| !(r == 5 && c == 0);
        s.select_cell(0, 0);
        s.jump_to_edge(MoveDir::Down, is_empty, false);
        assert_eq!(s.active().0, 5);
    }

    #[test]
    fn move_in_selection_wraps() {
        let mut s = model(20, 10);
        s.select_range(Range::new(0, 0, 2, 2)); // A1:B2
        s.set_active(0, 0);
        assert_eq!(s.move_in_selection(MoveDir::Down, false), (1, 0));
        assert_eq!(s.move_in_selection(MoveDir::Down, false), (0, 1));
        assert_eq!(s.move_in_selection(MoveDir::Down, false), (1, 1));
        assert_eq!(s.move_in_selection(MoveDir::Down, false), (0, 0));
    }

    #[test]
    fn home_end() {
        let mut s = model(20, 10);
        s.select_cell(5, 7);
        s.move_to_row_start();
        assert_eq!(s.active(), (5, 0));
        s.select_cell(5, 7);
        s.move_to_home();
        assert_eq!(s.active(), (0, 0));
        s.move_to_end(Some(19), Some(9));
        assert_eq!(s.active(), (19, 9));
    }

    #[test]
    fn page_move_and_whole() {
        let mut s = model(100, 10);
        s.select_cell(10, 3);
        s.page_move(20, false);
        assert_eq!(s.active().0, 30);
        s.select_whole_column();
        assert_eq!(s.primary().row_count, 100);
        assert_eq!(s.primary().col_count, 1);
        s.select_whole_row();
        assert_eq!(s.primary().col_count, 10);
        assert_eq!(s.primary().row_count, 1);
    }
}
