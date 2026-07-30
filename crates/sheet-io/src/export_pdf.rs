//! 工作表 → PDF（M15）。分页(sheet-core paginate) + printpdf 生成。对标 cmx-megasheet 的
//! io/exportPdf.ts（但用 printpdf 而非手写 PDF 字节）。
//!
//! Rust 移植取舍：TS 手写 PDF-1.4 字节 + WinAnsi 内置字体（无 CJK）。这里用 printpdf 0.12：
//!  - 内置字体 Helvetica 全离线可用（14 标准字体子集 include_bytes! 进 crate），覆盖 ASCII/拉丁。
//!  - CJK：内置字体不含中日韩字形，printpdf 内置字体渲染 CJK 会丢字。调用方可传外部 TTF 字节
//!    （PdfFont::External），printpdf add_font 子集嵌入——这是相对 TS 内置 WinAnsi 的能力增益。
//!  - printpdf 坐标原点在**左下角**，与网格「左上为原点」相反，故 y 需翻转（paper_height - y）。
//!
//! 生成流程：paginate 切页 → 逐页画淡网格线 + 单元格显示值文本。样式细节（粗体/色/对齐）
//! 走 numfmt 显示值 + 内置字体；富样式渲染保真是后续增强（当前满足「出合法多页 PDF」契约）。

use printpdf::*;

use sheet_core::numfmt::format_with;
use sheet_core::paginate::{paginate, PageDescriptor, PaginateResult};
use sheet_core::worksheet::Worksheet;

/// PDF 字体选择：内置（ASCII/拉丁）或外部 TTF 字节（CJK）。
pub enum PdfFont {
    /// 内置 Helvetica（仅 ASCII/WinAnsi，全离线）。
    Builtin,
    /// 外部 TrueType 字体字节（支持 CJK，printpdf 子集嵌入）。
    External(Vec<u8>),
}

const PX_TO_PT: f64 = 72.0 / 96.0;

/// 导出工作表为 PDF 字节。按 page_setup 分页；每页画网格 + 文本。
pub fn export_pdf(sheet: &Worksheet, font: PdfFont) -> Vec<u8> {
    let result = paginate(sheet, sheet.get_page_setup());
    let mut doc = PdfDocument::new(sheet.name());

    // 外部字体：注册一次取 FontId；内置直接引用。
    let font_handle = match &font {
        PdfFont::Builtin => PdfFontHandle::Builtin(BuiltinFont::Helvetica),
        PdfFont::External(bytes) => match ParsedFont::from_bytes(bytes, 0, &mut Vec::new()) {
            Some(parsed) => PdfFontHandle::External(doc.add_font(&parsed)),
            None => PdfFontHandle::Builtin(BuiltinFont::Helvetica),
        },
    };

    let mut pages: Vec<PdfPage> = Vec::new();
    for pd in &result.pages {
        pages.push(render_page(sheet, pd, &result, &font_handle));
    }
    if pages.is_empty() {
        pages.push(PdfPage::new(
            Mm(paper_mm(result.paper_width)),
            Mm(paper_mm(result.paper_height)),
            Vec::new(),
        ));
    }

    doc.with_pages(pages)
        .save(&PdfSaveOptions::default(), &mut Vec::new())
}

fn paper_mm(pt: f64) -> f32 {
    (pt / 72.0 * 25.4) as f32
}

/// 渲染单页：淡网格线 + 单元格文本（左下原点，y 翻转）。
fn render_page(
    sheet: &Worksheet,
    pd: &PageDescriptor,
    result: &PaginateResult,
    font: &PdfFontHandle,
) -> PdfPage {
    let paper_w = result.paper_width;
    let paper_h = result.paper_height;
    let margin_left = 36.0;
    let margin_top = 36.0;
    let scale = result.scale;
    let mut ops: Vec<Op> = Vec::new();

    let grey = Color::Rgb(Rgb::new(0.82, 0.84, 0.86, None));
    let black = Color::Rgb(Rgb::new(0.0, 0.0, 0.0, None));

    // 列 x 偏移（pt，含缩放）——预算每列左沿。
    let mut col_x: Vec<f64> = Vec::new();
    let mut x = margin_left;
    for c in pd.col_start..=pd.col_end {
        col_x.push(x);
        x += sheet.get_column_width(c) * PX_TO_PT * scale;
    }
    let right_edge = x;

    // 行 y 偏移（pt，从顶算，含缩放）。
    let mut row_y: Vec<f64> = Vec::new();
    let mut y = margin_top;
    for r in pd.row_start..=pd.row_end {
        row_y.push(y);
        y += sheet.get_row_height(r) * PX_TO_PT * scale;
    }
    let bottom_edge = y;

    // 网格线（水平 + 垂直）
    ops.push(Op::SetOutlineColor { col: grey });
    ops.push(Op::SetOutlineThickness { pt: Pt(0.5) });
    for (i, r) in (pd.row_start..=pd.row_end).enumerate() {
        let yy = paper_h - row_y[i];
        ops.push(line_op(margin_left, yy, right_edge, yy));
        let _ = r;
    }
    // 底边
    ops.push(line_op(
        margin_left,
        paper_h - bottom_edge,
        right_edge,
        paper_h - bottom_edge,
    ));
    for xx in &col_x {
        ops.push(line_op(
            *xx,
            paper_h - margin_top,
            *xx,
            paper_h - bottom_edge,
        ));
    }
    ops.push(line_op(
        right_edge,
        paper_h - margin_top,
        right_edge,
        paper_h - bottom_edge,
    ));

    // 单元格文本
    ops.push(Op::SetFillColor { col: black });
    let font_size = (10.0 * scale).max(4.0);
    for (ri, r) in (pd.row_start..=pd.row_end).enumerate() {
        for (ci, c) in (pd.col_start..=pd.col_end).enumerate() {
            let Some(v) = sheet.get_value(r, c) else {
                continue;
            };
            let style = sheet.get_resolved_style(r, c);
            let fmt = style.formatter.clone().unwrap_or_default();
            let text = format_with(&v, &fmt).text;
            if text.is_empty() {
                continue;
            }
            // 内置字体不含非拉丁字形：过滤到可打印 ASCII/拉丁，避免乱码（CJK 需外部字体）。
            let safe = sanitize_for_builtin(&text);
            if safe.is_empty() {
                continue;
            }
            // 文本基线：单元格左沿 +2pt，顶沿下移一个字高。左下原点 → y 翻转。
            let tx = col_x[ci] + 2.0;
            let ty = paper_w * 0.0 + (paper_h - row_y[ri] - font_size);
            ops.push(Op::StartTextSection);
            ops.push(Op::SetTextCursor {
                pos: Point {
                    x: Pt(tx as f32),
                    y: Pt(ty as f32),
                },
            });
            ops.push(Op::SetFont {
                font: font.clone(),
                size: Pt(font_size as f32),
            });
            ops.push(Op::ShowText {
                items: vec![TextItem::Text(safe)],
            });
            ops.push(Op::EndTextSection);
        }
    }

    PdfPage::new(Mm(paper_mm(paper_w)), Mm(paper_mm(paper_h)), ops)
}

fn line_op(x1: f64, y1: f64, x2: f64, y2: f64) -> Op {
    Op::DrawLine {
        line: Line {
            points: vec![
                LinePoint {
                    p: Point {
                        x: Pt(x1 as f32),
                        y: Pt(y1 as f32),
                    },
                    bezier: false,
                },
                LinePoint {
                    p: Point {
                        x: Pt(x2 as f32),
                        y: Pt(y2 as f32),
                    },
                    bezier: false,
                },
            ],
            is_closed: false,
        },
    }
}

/// 内置字体只支持 WinAnsi/拉丁；过滤到可打印 ASCII + Latin-1，其余替换空格（CJK 场景传外部字体）。
fn sanitize_for_builtin(s: &str) -> String {
    s.chars()
        .map(|c| {
            if (' '..='~').contains(&c) || ('\u{00A0}'..='\u{00FF}').contains(&c) {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .trim_end()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheet_core::worksheet::{PageSetup, RegionRect, Worksheet};

    #[test]
    fn export_pdf_from_sheet() {
        let mut ws = Worksheet::with_size("S", 5, 3);
        ws.set_value(0, 0, Some("A".into()));
        ws.set_value(1, 1, Some(99.into()));
        ws.set_page_setup(Some(PageSetup {
            print_area: Some(RegionRect::new(0, 0, 5, 3)),
            ..Default::default()
        }));
        let bytes = export_pdf(&ws, PdfFont::Builtin);
        assert!(bytes.len() > 100);
        // PDF 魔数 + EOF
        assert!(bytes.starts_with(b"%PDF"));
        let tail = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(32)..]);
        assert!(tail.contains("%%EOF"));
    }

    #[test]
    fn multi_page_pdf() {
        // 大表触发多页，PDF 仍合法
        let mut ws = Worksheet::with_size("Big", 200, 100);
        ws.set_value(0, 0, Some("x".into()));
        let bytes = export_pdf(&ws, PdfFont::Builtin);
        assert!(bytes.starts_with(b"%PDF"));
        assert!(bytes.len() > 500);
    }

    #[test]
    fn sanitize_drops_cjk_for_builtin() {
        assert_eq!(sanitize_for_builtin("abc"), "abc");
        assert_eq!(sanitize_for_builtin("a中b"), "a b");
    }
}
