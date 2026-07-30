//! 区域/工作表 → 自包含 HTML 文档（M15）。对标 cmx-megasheet 的 io/exportHtml.ts。
//!
//! 生成 `<table>` + 内联样式（字体/粗斜/对齐/背景/边框），叠加 M13 条件格式底色，合并格用
//! rowspan/colspan。复用 numfmt::format_with（显示值）与 condfmt::evaluate_rules（CF 底色/字色）。
//! 纯逻辑、零 DOM（拼字符串）。

use sheet_core::condfmt::evaluate_rules;
use sheet_core::numfmt::format_with;
use sheet_core::worksheet::{RegionRect, Worksheet};

/// HTML 导出选项。
#[derive(Debug, Clone, Default)]
pub struct ExportHtmlOptions {
    /// 导出区域（缺省=全表）。
    pub range: Option<RegionRect>,
    /// 是否输出完整 HTML 文档（<html><head>…）；false=只 <table> 片段。
    pub full_document: bool,
    /// 文档标题。
    pub title: Option<String>,
    /// 是否画网格线（默认 true）。
    pub gridlines: Option<bool>,
}

/// 导出工作表为 HTML。
pub fn export_html(sheet: &Worksheet, opts: &ExportHtmlOptions) -> String {
    let g = opts.range.unwrap_or(RegionRect::new(
        0,
        0,
        sheet.row_count(),
        sheet.column_count(),
    ));
    let gridlines = opts.gridlines != Some(false);
    let overlays = evaluate_rules(sheet, sheet.list_conditional_rules());

    let mut cols = String::new();
    for c in g.col..g.col + g.col_count {
        cols.push_str(&format!(
            "<col style=\"width:{}px\">",
            sheet.get_column_width(c).round() as i64
        ));
    }
    let mut rows = String::new();
    for r in g.row..g.row + g.row_count {
        let mut cells = String::new();
        let h = sheet.get_row_height(r).round() as i64;
        for c in g.col..g.col + g.col_count {
            // 合并区非左上跳过
            if let Some(span) = sheet.get_span(r, c) {
                if span.row != r || span.col != c {
                    continue;
                }
            }
            let overlay = overlays.get(&(r, c));
            cells.push_str(&cell_html(sheet, r, c, overlay));
        }
        rows.push_str(&format!("<tr style=\"height:{h}px\">{cells}</tr>"));
    }

    let border_css = if gridlines {
        "border-collapse:collapse;"
    } else {
        ""
    };
    let table = format!(
        "<table style=\"{border_css}font-family:Arial,sans-serif;font-size:13px\"><colgroup>{cols}</colgroup><tbody>{rows}</tbody></table>"
    );
    if !opts.full_document {
        return table;
    }
    let title = escape_html(opts.title.as_deref().unwrap_or(sheet.name()));
    format!(
        "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>{title}</title></head><body>{table}</body></html>"
    )
}

fn cell_html(
    sheet: &Worksheet,
    r: u32,
    c: u32,
    overlay: Option<&sheet_core::condfmt::CondFormatOverlay>,
) -> String {
    let mut style = sheet.get_resolved_style(r, c);
    if let Some(ov) = overlay {
        if let Some(ov_style) = &ov.style {
            style = sheet_core::style::merge_style(Some(&style), Some(ov_style));
        }
    }
    let value = sheet.get_value(r, c);
    let cell_val = value.unwrap_or(sheet_core::cell::CellValue::Text(String::new()));
    let fmt = style.formatter.clone().unwrap_or_default();
    let result = format_with(&cell_val, &fmt);

    let mut css: Vec<String> = Vec::new();
    if style.bold == Some(true) {
        css.push("font-weight:bold".into());
    }
    if style.italic == Some(true) {
        css.push("font-style:italic".into());
    }
    if style.underline == Some(true) {
        css.push("text-decoration:underline".into());
    }
    if let Some(h) = style.h_align {
        css.push(format!("text-align:{}", h_align_css(h)));
    }
    if let Some(v) = style.v_align {
        css.push(format!("vertical-align:{}", v_align_css(v)));
    }
    let bg = overlay
        .and_then(|o| o.fill.clone())
        .or_else(|| style.back_color.clone());
    if let Some(bg) = bg {
        css.push(format!("background-color:{bg}"));
    }
    let fg = result.color.clone().or_else(|| style.fore_color.clone());
    if let Some(fg) = fg {
        css.push(format!("color:{fg}"));
    }
    if let Some(fs) = style.font_size {
        css.push(format!(
            "font-size:{}px",
            sheet_core::numstr::num_to_string(fs)
        ));
    }
    // 边框
    if let Some(borders) = &style.borders {
        for (side, edge) in [
            ("top", &borders.top),
            ("right", &borders.right),
            ("bottom", &borders.bottom),
            ("left", &borders.left),
        ] {
            if let Some(b) = edge {
                css.push(format!("border-{side}:1px solid {}", b.color));
            }
        }
    } else {
        css.push("border:1px solid #d0d4da".into());
    }

    let mut attrs = String::new();
    if let Some(span) = sheet.get_span(r, c) {
        if span.row_count > 1 {
            attrs.push_str(&format!(" rowspan=\"{}\"", span.row_count));
        }
        if span.col_count > 1 {
            attrs.push_str(&format!(" colspan=\"{}\"", span.col_count));
        }
    }
    format!(
        "<td{attrs} style=\"{};padding:1px 4px\">{}</td>",
        css.join(";"),
        escape_html(&result.text)
    )
}

fn h_align_css(h: sheet_core::style::HAlign) -> &'static str {
    use sheet_core::style::HAlign::*;
    match h {
        Left | Fill => "left",
        Center | CenterContinuous => "center",
        Right => "right",
        Justify => "justify",
    }
}

fn v_align_css(v: sheet_core::style::VAlign) -> &'static str {
    use sheet_core::style::VAlign::*;
    match v {
        Top => "top",
        Middle => "middle",
        Bottom => "bottom",
    }
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheet_core::style::{HAlign, Style};
    use sheet_core::worksheet::{
        CondFormatOperator, CondFormatType, CondValue, ConditionalRule, Worksheet,
    };

    fn sheet() -> Worksheet {
        Worksheet::with_size("S", 100, 30)
    }

    #[test]
    fn basic_table_inline_style() {
        let mut ws = sheet();
        ws.set_value(0, 0, Some("标题".into()));
        ws.set_style(
            0,
            0,
            Some(Style {
                bold: Some(true),
                back_color: Some("#ffff00".into()),
                ..Default::default()
            }),
        );
        ws.set_value(1, 0, Some(42.into()));
        let html = export_html(
            &ws,
            &ExportHtmlOptions {
                range: Some(RegionRect::new(0, 0, 2, 2)),
                full_document: true,
                ..Default::default()
            },
        );
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<table"));
        assert!(html.contains("font-weight:bold"));
        assert!(html.contains("background-color:#ffff00"));
        assert!(html.contains("标题"));
        assert!(html.contains("42"));
    }

    #[test]
    fn fragment_mode_no_html() {
        let mut ws = sheet();
        ws.set_value(0, 0, Some("x".into()));
        let html = export_html(
            &ws,
            &ExportHtmlOptions {
                range: Some(RegionRect::new(0, 0, 1, 1)),
                full_document: false,
                ..Default::default()
            },
        );
        assert!(!html.contains("<!DOCTYPE"));
        assert!(html.starts_with("<table"));
    }

    #[test]
    fn merged_rowspan_colspan() {
        let mut ws = sheet();
        ws.set_value(0, 0, Some("M".into()));
        ws.add_span(0, 0, 2, 2);
        let html = export_html(
            &ws,
            &ExportHtmlOptions {
                range: Some(RegionRect::new(0, 0, 2, 2)),
                ..Default::default()
            },
        );
        assert!(html.contains("rowspan=\"2\""));
        assert!(html.contains("colspan=\"2\""));
    }

    #[test]
    fn conditional_format_bg_in_html() {
        let mut ws = sheet();
        ws.set_value(0, 0, Some(100.into()));
        ws.add_conditional_rule(ConditionalRule {
            range: RegionRect::new(0, 0, 1, 1),
            rule_type: CondFormatType::CellValue,
            operator: Some(CondFormatOperator::Gt),
            value1: Some(CondValue::Number(50.0)),
            value2: None,
            style: Some(Style {
                back_color: Some("#ff0000".into()),
                ..Default::default()
            }),
            colors: None,
            bar_color: None,
            icon_set: None,
        });
        let html = export_html(
            &ws,
            &ExportHtmlOptions {
                range: Some(RegionRect::new(0, 0, 1, 1)),
                ..Default::default()
            },
        );
        assert!(html.contains("background-color:#ff0000"));
    }

    #[test]
    fn align_css_maps() {
        assert_eq!(h_align_css(HAlign::Center), "center");
    }
}
