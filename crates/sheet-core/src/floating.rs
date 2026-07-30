//! 浮动对象几何（M14）。纯函数：双格锚点 → 屏幕矩形 + 命中/句柄/批注标记。
//! 对标 cmx-megasheet 的 render/overlay/FloatingLayout.ts。
//!
//! 「无渲染」重解读：几何换算（锚点→矩形、命中判定、句柄位置）是**计算件**，留 sheet-core。
//! 关键复用 M9：对象位置经 get_cell_rect 回调锚定——滚动/缩放/冻结象限跟随全自动成立。
//! 只依赖一个 `get_cell_rect(row,col)→ScreenRect` 回调（由调用方的几何提供者注入）。

use crate::worksheet::ObjAnchor;

/// 屏幕矩形。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// 缩放句柄名（8 向）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandleName {
    Nw,
    N,
    Ne,
    E,
    Se,
    S,
    Sw,
    W,
}

/// 一个缩放句柄：名 + 中心点。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Handle {
    pub name: HandleName,
    pub x: f64,
    pub y: f64,
}

/// 批注标记三角（右上角）顶点 + 尺寸。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CommentMarker {
    pub x: f64,
    pub y: f64,
    pub size: f64,
}

/// 双格锚点 → 屏幕矩形。fromCell 左上+fromDx/Dy 为左上角，toCell 左上+toDx/Dy 为右下角。
/// zoom 把内容像素偏移缩放到屏幕。get_cell_rect 由调用方注入（SheetGeometry.getCellRect 子集）。
pub fn resolve_object_rect<F: Fn(u32, u32) -> ScreenRect>(
    anchor: &ObjAnchor,
    get_cell_rect: F,
    zoom: f64,
) -> ScreenRect {
    let from = get_cell_rect(anchor.from_row, anchor.from_col);
    let to = get_cell_rect(anchor.to_row, anchor.to_col);
    let x1 = from.x + anchor.from_dx.unwrap_or(0.0) * zoom;
    let y1 = from.y + anchor.from_dy.unwrap_or(0.0) * zoom;
    let x2 = to.x + anchor.to_dx.unwrap_or(0.0) * zoom;
    let y2 = to.y + anchor.to_dy.unwrap_or(0.0) * zoom;
    ScreenRect {
        x: x1.min(x2),
        y: y1.min(y2),
        width: (x2 - x1).abs(),
        height: (y2 - y1).abs(),
    }
}

/// 点是否落在对象矩形内。
pub fn hit_object(x: f64, y: f64, rect: &ScreenRect) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

/// 8 个缩放句柄位置（对象选中时）。
pub fn resize_handles(rect: &ScreenRect) -> Vec<Handle> {
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
    let cx = x + w / 2.0;
    let cy = y + h / 2.0;
    let r = x + w;
    let b = y + h;
    vec![
        Handle {
            name: HandleName::Nw,
            x,
            y,
        },
        Handle {
            name: HandleName::N,
            x: cx,
            y,
        },
        Handle {
            name: HandleName::Ne,
            x: r,
            y,
        },
        Handle {
            name: HandleName::E,
            x: r,
            y: cy,
        },
        Handle {
            name: HandleName::Se,
            x: r,
            y: b,
        },
        Handle {
            name: HandleName::S,
            x: cx,
            y: b,
        },
        Handle {
            name: HandleName::Sw,
            x,
            y: b,
        },
        Handle {
            name: HandleName::W,
            x,
            y: cy,
        },
    ]
}

/// 命中缩放句柄（半径 tol 屏幕像素）。返回句柄名或 None。
pub fn hit_handle(x: f64, y: f64, rect: &ScreenRect, tol: f64) -> Option<HandleName> {
    resize_handles(rect)
        .into_iter()
        .find(|h| (x - h.x).abs() <= tol && (y - h.y).abs() <= tol)
        .map(|h| h.name)
}

/// 批注标记三角（右上角小三角）顶点。
pub fn comment_marker(cell_rect: &ScreenRect) -> CommentMarker {
    let size = 7.0_f64
        .min(cell_rect.width * 0.4)
        .min(cell_rect.height * 0.5);
    CommentMarker {
        x: cell_rect.x + cell_rect.width - size,
        y: cell_rect.y,
        size,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 等宽网格几何：col 60px、row 20px，加 (40,20) 表头偏移。滚动/冻结由调用方模拟。
    fn cell_rect(
        scroll_x: f64,
        scroll_y: f64,
        frozen_r: u32,
        frozen_c: u32,
    ) -> impl Fn(u32, u32) -> ScreenRect {
        move |row: u32, col: u32| {
            let sx = if col < frozen_c { 0.0 } else { scroll_x };
            let sy = if row < frozen_r { 0.0 } else { scroll_y };
            ScreenRect {
                x: 40.0 + col as f64 * 60.0 - sx,
                y: 20.0 + row as f64 * 20.0 - sy,
                width: 60.0,
                height: 20.0,
            }
        }
    }

    #[test]
    fn anchor_to_rect() {
        let anchor = ObjAnchor {
            from_row: 1,
            from_col: 1,
            to_row: 4,
            to_col: 4,
            from_dx: None,
            from_dy: None,
            to_dx: None,
            to_dy: None,
        };
        let rect = resolve_object_rect(&anchor, cell_rect(0.0, 0.0, 0, 0), 1.0);
        assert_eq!(rect.x, 100.0); // 40 + 60
        assert_eq!(rect.y, 40.0); // 20 + 20
        assert_eq!(rect.width, 180.0); // 3 cols × 60
        assert_eq!(rect.height, 60.0); // 3 rows × 20
    }

    #[test]
    fn follows_scroll() {
        let anchor = ObjAnchor {
            from_row: 5,
            from_col: 5,
            to_row: 8,
            to_col: 8,
            from_dx: None,
            from_dy: None,
            to_dx: None,
            to_dy: None,
        };
        let ra = resolve_object_rect(&anchor, cell_rect(0.0, 0.0, 0, 0), 1.0);
        let rb = resolve_object_rect(&anchor, cell_rect(120.0, 100.0, 0, 0), 1.0);
        assert!(rb.x < ra.x);
        assert!(rb.y < ra.y);
        assert_eq!(rb.width, ra.width);
        assert_eq!(rb.height, ra.height);
    }

    #[test]
    fn follows_freeze() {
        let anchor = ObjAnchor {
            from_row: 0,
            from_col: 0,
            to_row: 2,
            to_col: 1,
            from_dx: None,
            from_dy: None,
            to_dx: None,
            to_dy: None,
        };
        let r1 = resolve_object_rect(&anchor, cell_rect(200.0, 200.0, 3, 2), 1.0);
        let r2 = resolve_object_rect(&anchor, cell_rect(500.0, 500.0, 3, 2), 1.0);
        assert_eq!(r1.x, r2.x); // 冻结区不随滚动
        assert_eq!(r1.y, r2.y);
    }

    #[test]
    fn hit_and_handles() {
        let rect = ScreenRect {
            x: 100.0,
            y: 40.0,
            width: 180.0,
            height: 60.0,
        };
        assert!(hit_object(150.0, 70.0, &rect));
        assert!(!hit_object(50.0, 70.0, &rect));
        assert_eq!(resize_handles(&rect).len(), 8);
        assert_eq!(hit_handle(100.0, 40.0, &rect, 5.0), Some(HandleName::Nw));
        assert_eq!(hit_handle(280.0, 100.0, &rect, 5.0), Some(HandleName::Se));
        assert_eq!(hit_handle(150.0, 70.0, &rect, 5.0), None);
    }

    #[test]
    fn comment_marker_top_right() {
        let m = comment_marker(&ScreenRect {
            x: 100.0,
            y: 40.0,
            width: 60.0,
            height: 20.0,
        });
        assert!(m.x > 100.0);
        assert_eq!(m.y, 40.0);
        assert!(m.size > 0.0);
    }
}
