//! 打印分页计算（M15）。纯函数：按纸张/边距/缩放把网格切成页。对标 cmx-megasheet 的 io/paginate.ts。
//!
//! 「无渲染」重解读：分页是**计算件**（算行列区间，不画像素），留 sheet-core。支持缩放或适合
//! N 页宽×M 页高（自动算缩放）；重复标题行/列每页带。坐标单位 pt（1pt=1/72 inch）。零 DOM，
//! 只依赖行高/列宽/维度回调。

use crate::worksheet::PageSetup;

/// 纸张物理尺寸（pt，纵向 w×h）。
fn paper_size(name: &str) -> (f64, f64) {
    match name {
        "A3" => (842.0, 1191.0),
        "Letter" => (612.0, 792.0),
        "Legal" => (612.0, 1008.0),
        _ => (595.0, 842.0), // A4
    }
}

const DEFAULT_MARGIN: f64 = 36.0; // 0.5"
/// px→pt（屏幕 96dpi → 72pt）。行高列宽是 px。
const PX_TO_PT: f64 = 72.0 / 96.0;

/// 单页描述。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageDescriptor {
    pub row_start: u32,
    pub row_end: u32,
    pub col_start: u32,
    pub col_end: u32,
    pub page_index: u32,
    pub page_row: u32,
    pub page_col: u32,
}

/// 标题区间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TitleRange {
    pub start: u32,
    pub end: u32,
}

/// 分页结果。
#[derive(Debug, Clone)]
pub struct PaginateResult {
    pub pages: Vec<PageDescriptor>,
    /// 应用的缩放系数（fitToPages 时自动算）。
    pub scale: f64,
    pub printable_width: f64,
    pub printable_height: f64,
    pub paper_width: f64,
    pub paper_height: f64,
    pub title_rows: Option<TitleRange>,
    pub title_cols: Option<TitleRange>,
    pub pages_wide: u32,
    pub pages_tall: u32,
}

/// 网格度量回调（Worksheet 子集）。
pub trait GridMetrics {
    fn row_height(&self, row: u32) -> f64;
    fn column_width(&self, col: u32) -> f64;
    fn row_count(&self) -> u32;
    fn column_count(&self) -> u32;
}

/// Worksheet 直接充当网格度量（便捷：paginate(&ws, ws.get_page_setup())）。
impl GridMetrics for crate::worksheet::Worksheet {
    fn row_height(&self, row: u32) -> f64 {
        self.get_row_height(row)
    }
    fn column_width(&self, col: u32) -> f64 {
        self.get_column_width(col)
    }
    fn row_count(&self) -> u32 {
        crate::worksheet::Worksheet::row_count(self)
    }
    fn column_count(&self) -> u32 {
        self.column_count()
    }
}

/// 计算分页。
pub fn paginate(grid: &dyn GridMetrics, setup: Option<&PageSetup>) -> PaginateResult {
    let empty = PageSetup::default();
    let s = setup.unwrap_or(&empty);
    let (pw, ph) = paper_size(s.paper_size.as_deref().unwrap_or("A4"));
    let landscape = s.orientation == Some(crate::worksheet::Orientation::Landscape);
    let paper_width = if landscape { ph } else { pw };
    let paper_height = if landscape { pw } else { ph };
    let m = s.margins.unwrap_or(crate::worksheet::PageMargins {
        top: DEFAULT_MARGIN,
        right: DEFAULT_MARGIN,
        bottom: DEFAULT_MARGIN,
        left: DEFAULT_MARGIN,
    });
    let printable_width = (paper_width - m.left - m.right).max(1.0);
    let printable_height = (paper_height - m.top - m.bottom).max(1.0);

    // 打印区域（缺省全维度）
    let area = s.print_area.unwrap_or(crate::worksheet::RegionRect::new(
        0,
        0,
        grid.row_count(),
        grid.column_count(),
    ));
    let r0 = area.row;
    let r1 = area.row + area.row_count.saturating_sub(1);
    let c0 = area.col;
    let c1 = area.col + area.col_count.saturating_sub(1);

    // 重复标题
    let title_rows = s
        .print_titles
        .and_then(|pt| match (pt.row_start, pt.row_end) {
            (Some(start), Some(end)) => Some(TitleRange { start, end }),
            _ => None,
        });
    let title_cols = s
        .print_titles
        .and_then(|pt| match (pt.col_start, pt.col_end) {
            (Some(start), Some(end)) => Some(TitleRange { start, end }),
            _ => None,
        });

    let row_pt = |r: u32| grid.row_height(r) * PX_TO_PT;
    let col_pt = |c: u32| grid.column_width(c) * PX_TO_PT;
    let mut total_w = 0.0;
    for c in c0..=c1 {
        total_w += col_pt(c);
    }
    let mut total_h = 0.0;
    for r in r0..=r1 {
        total_h += row_pt(r);
    }

    // 缩放：fitToPages 优先
    let mut scale = s.scale.unwrap_or(100.0) / 100.0;
    if let Some(fit) = s.fit_to_pages {
        let scale_w = if fit.width > 0 {
            printable_width * fit.width as f64 / total_w.max(1.0)
        } else {
            f64::INFINITY
        };
        let scale_h = if fit.height > 0 {
            printable_height * fit.height as f64 / total_h.max(1.0)
        } else {
            f64::INFINITY
        };
        scale = scale_w.min(scale_h);
        if !scale.is_finite() {
            scale = 1.0;
        }
        scale = scale.min(1.0); // 只缩小不放大
    }

    // 标题占位（缩放后）
    let title_rows_h = title_rows
        .map(|t| sum_range(t.start, t.end, &row_pt) * scale)
        .unwrap_or(0.0);
    let title_cols_w = title_cols
        .map(|t| sum_range(t.start, t.end, &col_pt) * scale)
        .unwrap_or(0.0);
    let body_w = (printable_width - title_cols_w).max(1.0);
    let body_h = (printable_height - title_rows_h).max(1.0);

    let col_segs = split_axis(c0, c1, &|c| col_pt(c) * scale, body_w, title_cols);
    let row_segs = split_axis(r0, r1, &|r| row_pt(r) * scale, body_h, title_rows);

    let mut pages = Vec::new();
    let mut idx = 0u32;
    for (pr, rseg) in row_segs.iter().enumerate() {
        for (pc, cseg) in col_segs.iter().enumerate() {
            pages.push(PageDescriptor {
                row_start: rseg.start,
                row_end: rseg.end,
                col_start: cseg.start,
                col_end: cseg.end,
                page_index: idx,
                page_row: pr as u32,
                page_col: pc as u32,
            });
            idx += 1;
        }
    }
    if pages.is_empty() {
        pages.push(PageDescriptor {
            row_start: r0,
            row_end: r0,
            col_start: c0,
            col_end: c0,
            page_index: 0,
            page_row: 0,
            page_col: 0,
        });
    }

    let pages_wide = (col_segs.len() as u32).max(1);
    let pages_tall = (row_segs.len() as u32).max(1);
    PaginateResult {
        pages,
        scale,
        printable_width,
        printable_height,
        paper_width,
        paper_height,
        title_rows,
        title_cols,
        pages_wide,
        pages_tall,
    }
}

fn sum_range(a: u32, b: u32, size: &dyn Fn(u32) -> f64) -> f64 {
    (a..=b).map(size).sum()
}

/// 沿一轴按可用长度切段（跳过重复标题区，其已固定占位）。
fn split_axis(
    start: u32,
    end: u32,
    size_pt: &dyn Fn(u32) -> f64,
    avail: f64,
    titles: Option<TitleRange>,
) -> Vec<TitleRange> {
    let in_title = |i: u32| titles.is_some_and(|t| i >= t.start && i <= t.end);
    // 主体起点：跳过开头标题区
    let mut body_start = start;
    while body_start <= end && in_title(body_start) {
        body_start += 1;
    }
    if body_start > end {
        return vec![TitleRange { start: end, end }];
    }

    let mut segs: Vec<TitleRange> = Vec::new();
    let mut seg_start = body_start;
    let mut acc = 0.0;
    const EPS: f64 = 0.5;
    for i in body_start..=end {
        if in_title(i) {
            continue;
        }
        let sz = size_pt(i);
        if acc > 0.0 && acc + sz > avail + EPS {
            segs.push(TitleRange {
                start: seg_start,
                end: i - 1,
            });
            seg_start = i;
            acc = 0.0;
        }
        acc += sz;
    }
    if seg_start <= end {
        segs.push(TitleRange {
            start: seg_start,
            end,
        });
    }
    if segs.is_empty() {
        vec![TitleRange {
            start: body_start,
            end,
        }]
    } else {
        segs
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worksheet::{FitToPages, Orientation, PrintTitles, RegionRect};

    struct EqGrid {
        rows: u32,
        cols: u32,
        row_h: f64,
        col_w: f64,
    }
    impl GridMetrics for EqGrid {
        fn row_height(&self, _r: u32) -> f64 {
            self.row_h
        }
        fn column_width(&self, _c: u32) -> f64 {
            self.col_w
        }
        fn row_count(&self) -> u32 {
            self.rows
        }
        fn column_count(&self) -> u32 {
            self.cols
        }
    }

    fn grid(rows: u32, cols: u32) -> EqGrid {
        EqGrid {
            rows,
            cols,
            row_h: 20.0,
            col_w: 80.0,
        }
    }

    fn setup_area(rc: u32, cc: u32) -> PageSetup {
        PageSetup {
            print_area: Some(RegionRect::new(0, 0, rc, cc)),
            ..Default::default()
        }
    }

    #[test]
    fn small_single_page() {
        let pg = paginate(&grid(5, 3), Some(&setup_area(5, 3)));
        assert_eq!(pg.pages.len(), 1);
        assert_eq!(pg.pages_wide, 1);
        assert_eq!(pg.pages_tall, 1);
    }

    #[test]
    fn large_multi_page() {
        let pg = paginate(&grid(200, 100), Some(&setup_area(200, 100)));
        assert!(pg.pages_wide > 1);
        assert!(pg.pages_tall > 1);
        assert_eq!(pg.pages.len() as u32, pg.pages_wide * pg.pages_tall);
    }

    #[test]
    fn landscape_wider() {
        let mut portrait = setup_area(10, 20);
        portrait.orientation = Some(Orientation::Portrait);
        let mut landscape = setup_area(10, 20);
        landscape.orientation = Some(Orientation::Landscape);
        let p = paginate(&grid(10, 20), Some(&portrait));
        let l = paginate(&grid(10, 20), Some(&landscape));
        assert!(l.pages_wide <= p.pages_wide);
    }

    #[test]
    fn fit_to_one_page() {
        let mut s = setup_area(200, 100);
        s.fit_to_pages = Some(FitToPages {
            width: 1,
            height: 1,
        });
        let pg = paginate(&grid(200, 100), Some(&s));
        assert_eq!(pg.pages.len(), 1);
        assert!(pg.scale < 1.0);
    }

    #[test]
    fn print_titles_repeat() {
        let mut s = setup_area(200, 100);
        s.print_titles = Some(PrintTitles {
            row_start: Some(0),
            row_end: Some(0),
            col_start: Some(0),
            col_end: Some(0),
        });
        let pg = paginate(&grid(200, 100), Some(&s));
        assert_eq!(pg.title_rows, Some(TitleRange { start: 0, end: 0 }));
        assert_eq!(pg.title_cols, Some(TitleRange { start: 0, end: 0 }));
        for p in &pg.pages {
            assert!(p.row_start > 0);
        }
    }
}
