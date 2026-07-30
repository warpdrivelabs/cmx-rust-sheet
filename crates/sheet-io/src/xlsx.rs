//! XLSX 导入导出（OOXML SpreadsheetML）。对标 cmx-megasheet 的 io/xlsx.ts。
//!
//! 经中性快照中转：export = workbook_to_json → OOXML zip；import = OOXML → 快照 → workbook_from_json。
//! 按 **CMX 报表实际用到的样式子集**手写 OOXML（方案：XLSX 保真以 CMX 子集为验收范围）：
//!  - 单元格值（数字/字符串/布尔）、公式（f + 缓存 v）、合并 mergeCells
//!  - 行高 rows[@ht]、列宽 cols[@width]、隐藏 hidden、多级分组 outlineLevel + <outlinePr>
//!  - 样式：字体(b/i/u/sz/name/color)、填充 fgColor、对齐 h/v、numFmt、边框 → styles.xml
//!  - 多 sheet、活动 sheet、冻结窗格 <pane>、自动筛选 <autoFilter>、页面设置 <pageSetup>
//!
//! Rust 移植取舍：TS 用手写 zip.ts + 正则扫 XML；这里用 `zip` crate 做容器、`quick-xml`（escape）+
//! 手写字符串做 OOXML 生成、regex 做导入解析（对齐 TS 的轻量正则路线，避免全 DOM/SAX 复杂度）。

use std::collections::BTreeMap;
use std::io::{Cursor, Read, Write};
use std::sync::OnceLock;

use regex::Regex;
use zip::write::SimpleFileOptions;
use zip::{ZipArchive, ZipWriter};

use sheet_core::address::{col_to_label, format_addr, parse_addr};
use sheet_core::cell::CellValue;
use sheet_core::outline::OutlineAxis;
use sheet_core::style::{BorderEdge, BorderLineStyle, Borders, HAlign, Style, VAlign};
use sheet_core::workbook::Workbook;

use crate::snapshot::{
    workbook_from_json, workbook_to_json, CellSnapshot, NumPair, OutlineGroupSnapshot,
    SheetSnapshot, WorkbookSnapshot,
};

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn unesc(s: &str) -> String {
    // 数字实体
    static DEC: OnceLock<Regex> = OnceLock::new();
    static HEX: OnceLock<Regex> = OnceLock::new();
    let dec = DEC.get_or_init(|| Regex::new(r"&#(\d+);").unwrap());
    let hex = HEX.get_or_init(|| Regex::new(r"&#x([0-9a-fA-F]+);").unwrap());
    let mut out = s
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    out = dec
        .replace_all(&out, |c: &regex::Captures| {
            c[1].parse::<u32>()
                .ok()
                .and_then(char::from_u32)
                .map(|ch| ch.to_string())
                .unwrap_or_default()
        })
        .into_owned();
    out = hex
        .replace_all(&out, |c: &regex::Captures| {
            u32::from_str_radix(&c[1], 16)
                .ok()
                .and_then(char::from_u32)
                .map(|ch| ch.to_string())
                .unwrap_or_default()
        })
        .into_owned();
    out.replace("&amp;", "&")
}

/// #rgb / #rrggbb → OOXML ARGB（8 位）。
fn to_argb(color: &str) -> String {
    let c = color.trim().trim_start_matches('#');
    let full = if c.len() == 3 && c.chars().all(|x| x.is_ascii_hexdigit()) {
        c.chars().flat_map(|x| [x, x]).collect::<String>()
    } else {
        c.to_string()
    };
    if full.len() == 6 && full.chars().all(|x| x.is_ascii_hexdigit()) {
        return format!("FF{}", full.to_uppercase());
    }
    if full.len() == 8 && full.chars().all(|x| x.is_ascii_hexdigit()) {
        return full.to_uppercase();
    }
    "FF000000".to_string()
}

/// OOXML ARGB → #rrggbb（丢 alpha）。
fn from_argb(argb: Option<&str>) -> Option<String> {
    let c = argb?.trim();
    if c.len() == 8 && c.chars().all(|x| x.is_ascii_hexdigit()) {
        return Some(format!("#{}", c[2..].to_lowercase()));
    }
    if c.len() == 6 && c.chars().all(|x| x.is_ascii_hexdigit()) {
        return Some(format!("#{}", c.to_lowercase()));
    }
    None
}

fn h_to_xlsx(h: HAlign) -> &'static str {
    match h {
        HAlign::Left => "left",
        HAlign::Center => "center",
        HAlign::Right => "right",
        HAlign::Fill => "fill",
        HAlign::Justify => "justify",
        HAlign::CenterContinuous => "centerContinuous",
    }
}
fn v_to_xlsx(v: VAlign) -> &'static str {
    match v {
        VAlign::Top => "top",
        VAlign::Middle => "center",
        VAlign::Bottom => "bottom",
    }
}
fn h_from_xlsx(s: &str) -> Option<HAlign> {
    Some(match s {
        "left" => HAlign::Left,
        "center" => HAlign::Center,
        "right" => HAlign::Right,
        "fill" => HAlign::Fill,
        "justify" => HAlign::Justify,
        "centerContinuous" => HAlign::CenterContinuous,
        _ => return None,
    })
}
fn v_from_xlsx(s: &str) -> Option<VAlign> {
    Some(match s {
        "top" => VAlign::Top,
        "center" => VAlign::Middle,
        "bottom" => VAlign::Bottom,
        _ => return None,
    })
}
fn border_to_xlsx(b: BorderLineStyle) -> &'static str {
    match b {
        BorderLineStyle::None => "none",
        BorderLineStyle::Thin => "thin",
        BorderLineStyle::Medium => "medium",
        BorderLineStyle::Thick => "thick",
        BorderLineStyle::Dashed => "dashed",
        BorderLineStyle::Dotted => "dotted",
        BorderLineStyle::Double => "double",
    }
}
fn border_from_xlsx(s: &str) -> BorderLineStyle {
    match s {
        "medium" => BorderLineStyle::Medium,
        "thick" => BorderLineStyle::Thick,
        "dashed" | "mediumDashed" => BorderLineStyle::Dashed,
        "dotted" => BorderLineStyle::Dotted,
        "double" => BorderLineStyle::Double,
        _ => BorderLineStyle::Thin,
    }
}

// ── styles.xml 构造：去重 font/fill/border/numFmt → cellXfs 索引 ──
struct StyleRegistry {
    fonts: Vec<String>,
    fills: Vec<String>,
    borders: Vec<String>,
    num_fmts: Vec<(u32, String)>,
    num_fmt_by_code: BTreeMap<String, u32>,
    xfs: Vec<String>,
    xf_by_key: BTreeMap<String, usize>,
    next_num_fmt_id: u32,
}

impl StyleRegistry {
    fn new() -> Self {
        StyleRegistry {
            fonts: vec!["<font><sz val=\"11\"/><name val=\"Calibri\"/></font>".to_string()],
            fills: vec![
                "<fill><patternFill patternType=\"none\"/></fill>".to_string(),
                "<fill><patternFill patternType=\"gray125\"/></fill>".to_string(),
            ],
            borders: vec!["<border><left/><right/><top/><bottom/><diagonal/></border>".to_string()],
            num_fmts: Vec::new(),
            num_fmt_by_code: BTreeMap::new(),
            xfs: vec![
                "<xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/>"
                    .to_string(),
            ],
            xf_by_key: BTreeMap::new(),
            next_num_fmt_id: 164,
        }
    }

    fn dedupe(arr: &mut Vec<String>, xml: String) -> usize {
        if let Some(i) = arr.iter().position(|x| *x == xml) {
            return i;
        }
        arr.push(xml);
        arr.len() - 1
    }

    fn intern(&mut self, style: &Style) -> usize {
        if style.is_empty() {
            return 0;
        }
        let font_id = self.intern_font(style);
        let fill_id = self.intern_fill(style);
        let border_id = self.intern_border(style);
        let num_fmt_id = self.intern_num_fmt(style);
        let align = align_attrs(style);
        let unlocked = style.locked == Some(false);
        let key = format!(
            "{num_fmt_id}|{font_id}|{fill_id}|{border_id}|{align}|{}",
            if unlocked { "u" } else { "" }
        );
        if let Some(&hit) = self.xf_by_key.get(&key) {
            return hit;
        }
        let apply_font = if font_id != 0 { " applyFont=\"1\"" } else { "" };
        let apply_fill = if fill_id != 0 { " applyFill=\"1\"" } else { "" };
        let apply_border = if border_id != 0 {
            " applyBorder=\"1\""
        } else {
            ""
        };
        let apply_num = if num_fmt_id != 0 {
            " applyNumberFormat=\"1\""
        } else {
            ""
        };
        let apply_align = if !align.is_empty() {
            " applyAlignment=\"1\""
        } else {
            ""
        };
        let apply_prot = if unlocked {
            " applyProtection=\"1\""
        } else {
            ""
        };
        let mut inner = String::new();
        if !align.is_empty() {
            inner.push_str(&format!("<alignment {align}/>"));
        }
        if unlocked {
            inner.push_str("<protection locked=\"0\"/>");
        }
        let body = if inner.is_empty() {
            "/>".to_string()
        } else {
            format!(">{inner}</xf>")
        };
        self.xfs.push(format!(
            "<xf numFmtId=\"{num_fmt_id}\" fontId=\"{font_id}\" fillId=\"{fill_id}\" borderId=\"{border_id}\" xfId=\"0\"{apply_num}{apply_font}{apply_fill}{apply_border}{apply_align}{apply_prot}{body}"
        ));
        let idx = self.xfs.len() - 1;
        self.xf_by_key.insert(key, idx);
        idx
    }

    fn intern_font(&mut self, s: &Style) -> usize {
        if s.bold.is_none()
            && s.italic.is_none()
            && s.underline.is_none()
            && s.strikethrough.is_none()
            && s.font_size.is_none()
            && s.font_family.is_none()
            && s.fore_color.is_none()
        {
            return 0;
        }
        let mut f = String::from("<font>");
        if s.bold == Some(true) {
            f.push_str("<b/>");
        }
        if s.italic == Some(true) {
            f.push_str("<i/>");
        }
        if s.underline == Some(true) {
            f.push_str("<u/>");
        }
        if s.strikethrough == Some(true) {
            f.push_str("<strike/>");
        }
        f.push_str(&format!(
            "<sz val=\"{}\"/>",
            sheet_core::numstr::num_to_string(s.font_size.unwrap_or(11.0))
        ));
        if let Some(fc) = &s.fore_color {
            f.push_str(&format!("<color rgb=\"{}\"/>", to_argb(fc)));
        }
        f.push_str(&format!(
            "<name val=\"{}\"/>",
            esc(s.font_family.as_deref().unwrap_or("Calibri"))
        ));
        f.push_str("</font>");
        Self::dedupe(&mut self.fonts, f)
    }

    fn intern_fill(&mut self, s: &Style) -> usize {
        if let Some(bg) = &s.back_color {
            let f = format!(
                "<fill><patternFill patternType=\"solid\"><fgColor rgb=\"{}\"/><bgColor indexed=\"64\"/></patternFill></fill>",
                to_argb(bg)
            );
            return Self::dedupe(&mut self.fills, f);
        }
        0
    }

    fn intern_border(&mut self, s: &Style) -> usize {
        let Some(borders) = &s.borders else {
            return 0;
        };
        let edge = |side: &str, e: &Option<BorderEdge>| -> String {
            match e {
                Some(edge) if edge.style != BorderLineStyle::None => format!(
                    "<{side} style=\"{}\"><color rgb=\"{}\"/></{side}>",
                    border_to_xlsx(edge.style),
                    to_argb(&edge.color)
                ),
                _ => format!("<{side}/>"),
            }
        };
        let b = format!(
            "<border>{}{}{}{}<diagonal/></border>",
            edge("left", &borders.left),
            edge("right", &borders.right),
            edge("top", &borders.top),
            edge("bottom", &borders.bottom)
        );
        Self::dedupe(&mut self.borders, b)
    }

    fn intern_num_fmt(&mut self, s: &Style) -> u32 {
        let Some(code) = &s.formatter else {
            return 0;
        };
        if let Some(&hit) = self.num_fmt_by_code.get(code) {
            return hit;
        }
        let id = self.next_num_fmt_id;
        self.next_num_fmt_id += 1;
        self.num_fmts.push((id, code.clone()));
        self.num_fmt_by_code.insert(code.clone(), id);
        id
    }

    fn to_xml(&self) -> String {
        let num_fmts_xml = if self.num_fmts.is_empty() {
            String::new()
        } else {
            format!(
                "<numFmts count=\"{}\">{}</numFmts>",
                self.num_fmts.len(),
                self.num_fmts
                    .iter()
                    .map(|(id, code)| format!(
                        "<numFmt numFmtId=\"{id}\" formatCode=\"{}\"/>",
                        esc(code)
                    ))
                    .collect::<String>()
            )
        };
        format!(
            "{XML_DECL}<styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">{num_fmts_xml}<fonts count=\"{}\">{}</fonts><fills count=\"{}\">{}</fills><borders count=\"{}\">{}</borders><cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs><cellXfs count=\"{}\">{}</cellXfs><cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles></styleSheet>",
            self.fonts.len(), self.fonts.concat(),
            self.fills.len(), self.fills.concat(),
            self.borders.len(), self.borders.concat(),
            self.xfs.len(), self.xfs.concat()
        )
    }
}

fn align_attrs(s: &Style) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(h) = s.h_align {
        parts.push(format!("horizontal=\"{}\"", h_to_xlsx(h)));
    }
    if let Some(v) = s.v_align {
        parts.push(format!("vertical=\"{}\"", v_to_xlsx(v)));
    }
    if s.word_wrap == Some(true) {
        parts.push("wrapText=\"1\"".to_string());
    }
    if let Some(tr) = s.text_rotation {
        if tr != 0.0 {
            let r = tr.trunc() as i64;
            parts.push(format!(
                "textRotation=\"{}\"",
                if r >= 0 { r } else { 90 - r }
            ));
        }
    }
    if let Some(ind) = s.indent {
        if ind > 0.0 {
            parts.push(format!("indent=\"{}\"", ind.trunc() as i64));
        }
    }
    if s.shrink_to_fit == Some(true) {
        parts.push("shrinkToFit=\"1\"".to_string());
    }
    parts.join(" ")
}

// ── 共享字符串表 ──
#[derive(Default)]
struct SharedStrings {
    list: Vec<String>,
    by_str: BTreeMap<String, usize>,
}

impl SharedStrings {
    fn intern(&mut self, s: &str) -> usize {
        if let Some(&hit) = self.by_str.get(s) {
            return hit;
        }
        let i = self.list.len();
        self.list.push(s.to_string());
        self.by_str.insert(s.to_string(), i);
        i
    }
    fn to_xml(&self) -> String {
        format!(
            "{XML_DECL}<sst xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" count=\"{}\" uniqueCount=\"{}\">{}</sst>",
            self.list.len(),
            self.list.len(),
            self.list
                .iter()
                .map(|s| format!("<si><t xml:space=\"preserve\">{}</t></si>", esc(s)))
                .collect::<String>()
        )
    }
}

/// 快照某格样式展开为完整 Style（styleName 引用在共享 styleSheet 里展平）。
fn resolve_cell_style(style: Option<&Style>, wb: &WorkbookSnapshot) -> Style {
    let Some(style) = style else {
        return Style::default();
    };
    match &style.style_name {
        None => {
            let mut s = style.clone();
            s.style_name = None;
            s
        }
        Some(name) => {
            let base = wb
                .styles
                .as_ref()
                .and_then(|m| m.get(name))
                .cloned()
                .unwrap_or_default();
            let mut rest = style.clone();
            rest.style_name = None;
            let mut out = base;
            out.overlay(&rest);
            out.style_name = None;
            out
        }
    }
}

// ── 顶层导出 ──

/// 工作簿 → XLSX 字节。
pub fn export_xlsx(wb: &Workbook) -> Vec<u8> {
    snapshot_to_xlsx(&workbook_to_json(wb))
}

/// 中性快照 → XLSX 字节（.xlsx = OOXML 部件的 ZIP）。
pub fn snapshot_to_xlsx(snap: &WorkbookSnapshot) -> Vec<u8> {
    let mut styles = StyleRegistry::new();
    let mut sst = SharedStrings::default();
    let active = snap.active_sheet.unwrap_or(0);

    // ── 图表 / 迷你图部件规划 ──────────────────────────────
    // 每个 sheet 若有图表 → 一个 drawingN.xml；图表按全簿累计编号 chartM.xml。
    // draw_plan[sheet_i] = Some((drawing_index, [chart_global_index...])) 或 None。
    let mut draw_plan: Vec<Option<(usize, Vec<usize>)>> = Vec::with_capacity(snap.sheets.len());
    let mut chart_parts: Vec<String> = Vec::new(); // chart_parts[m] = chartM+1.xml 内容
    let mut drawing_parts: Vec<(usize, String, String)> = Vec::new(); // (drawing_idx, drawing.xml, rels.xml)
    let mut next_drawing = 1usize;
    for s in &snap.sheets {
        let charts = crate::xlsx_drawing::sheet_charts(s);
        if charts.is_empty() {
            draw_plan.push(None);
            continue;
        }
        let anchors = crate::xlsx_drawing::chart_anchors(s);
        let mut local_charts: Vec<usize> = Vec::new();
        for spec in charts.iter() {
            let global_idx = chart_parts.len(); // 0-based
            chart_parts.push(crate::xlsx_drawing::chart_xml(&s.name, spec, s));
            local_charts.push(global_idx);
        }
        // drawing xml 里 twoCellAnchor 参数 = (rid_i, from_row, from_col, to_row, to_col)。
        let anchor_for_xml: Vec<(usize, u32, u32, u32, u32)> = anchors
            .iter()
            .enumerate()
            .map(|(li, a)| (li + 1, a.0, a.1, a.2, a.3))
            .collect();
        let dxml = crate::xlsx_drawing::drawing_xml(&anchor_for_xml);
        let drels = crate::xlsx_drawing::drawing_rels_xml(local_charts.len());
        drawing_parts.push((next_drawing, dxml, drels));
        draw_plan.push(Some((next_drawing, local_charts)));
        next_drawing += 1;
    }

    let sheet_xmls: Vec<String> = snap
        .sheets
        .iter()
        .enumerate()
        .map(|(i, s)| {
            // 工作簿级冻结注入活动 sheet
            let (fr, fc) = if i == active {
                (snap.frozen_row_count, snap.frozen_col_count)
            } else {
                (0, 0)
            };
            // 有图表 → worksheet 里加 <drawing rId1>（每 sheet 的 drawing 关系固定 rId1）。
            let drawing_rid = draw_plan[i].as_ref().map(|_| 1u32);
            sheet_to_xml(s, snap, &mut styles, &mut sst, fr, fc, drawing_rid)
        })
        .collect();

    let sheets_xml = snap
        .sheets
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let name = if s.name.is_empty() {
                format!("Sheet{}", i + 1)
            } else {
                s.name.clone()
            };
            format!(
                "<sheet name=\"{}\" sheetId=\"{}\" r:id=\"rId{}\"/>",
                esc(&name),
                i + 1,
                i + 1
            )
        })
        .collect::<String>();
    let book_views = if active > 0 {
        format!("<bookViews><workbookView activeTab=\"{active}\"/></bookViews>")
    } else {
        String::new()
    };
    let workbook_xml = format!(
        "{XML_DECL}<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">{book_views}<sheets>{sheets_xml}</sheets></workbook>"
    );

    let mut wb_rel_parts: Vec<String> = snap
        .sheets
        .iter()
        .enumerate()
        .map(|(i, _)| {
            format!(
                "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{}.xml\"/>",
                i + 1,
                i + 1
            )
        })
        .collect();
    let style_rid = snap.sheets.len() + 1;
    let sst_rid = snap.sheets.len() + 2;
    wb_rel_parts.push(format!("<Relationship Id=\"rId{style_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>"));
    wb_rel_parts.push(format!("<Relationship Id=\"rId{sst_rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings\" Target=\"sharedStrings.xml\"/>"));
    let workbook_rels = format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{}</Relationships>",
        wb_rel_parts.concat()
    );

    let sheet_overrides = snap
        .sheets
        .iter()
        .enumerate()
        .map(|(i, _)| {
            format!(
                "<Override PartName=\"/xl/worksheets/sheet{}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>",
                i + 1
            )
        })
        .collect::<String>();
    // 图表 / drawing 的 Content_Types Override。
    let drawing_overrides = drawing_parts
        .iter()
        .map(|(idx, _, _)| {
            format!(
                "<Override PartName=\"/xl/drawings/drawing{idx}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/>"
            )
        })
        .collect::<String>();
    let chart_overrides = (1..=chart_parts.len())
        .map(|m| {
            format!(
                "<Override PartName=\"/xl/charts/chart{m}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.drawingml.chart+xml\"/>"
            )
        })
        .collect::<String>();
    let content_types = format!(
        "{XML_DECL}<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>{sheet_overrides}<Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/><Override PartName=\"/xl/sharedStrings.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml\"/>{drawing_overrides}{chart_overrides}</Types>"
    );

    let root_rels = format!(
        "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>"
    );

    // 组 zip
    let mut buf = Vec::new();
    {
        let mut zw = ZipWriter::new(Cursor::new(&mut buf));
        let opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let put = |zw: &mut ZipWriter<Cursor<&mut Vec<u8>>>, name: &str, data: &str| {
            zw.start_file(name, opts).unwrap();
            zw.write_all(data.as_bytes()).unwrap();
        };
        put(&mut zw, "[Content_Types].xml", &content_types);
        put(&mut zw, "_rels/.rels", &root_rels);
        put(&mut zw, "xl/workbook.xml", &workbook_xml);
        put(&mut zw, "xl/_rels/workbook.xml.rels", &workbook_rels);
        put(&mut zw, "xl/styles.xml", &styles.to_xml());
        put(&mut zw, "xl/sharedStrings.xml", &sst.to_xml());
        for (i, xml) in sheet_xmls.iter().enumerate() {
            put(&mut zw, &format!("xl/worksheets/sheet{}.xml", i + 1), xml);
            // 有图表的 sheet → worksheet rels 指向其 drawing（固定 rId1）。
            if let Some((drawing_idx, _)) = &draw_plan[i] {
                let ws_rels = format!(
                    "{XML_DECL}<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"../drawings/drawing{drawing_idx}.xml\"/></Relationships>"
                );
                put(
                    &mut zw,
                    &format!("xl/worksheets/_rels/sheet{}.xml.rels", i + 1),
                    &ws_rels,
                );
            }
        }
        // drawing 部件 + 其 rels。
        for (idx, dxml, drels) in &drawing_parts {
            put(&mut zw, &format!("xl/drawings/drawing{idx}.xml"), dxml);
            put(
                &mut zw,
                &format!("xl/drawings/_rels/drawing{idx}.xml.rels"),
                drels,
            );
        }
        // chart 部件（全簿累计编号）。
        for (m, cxml) in chart_parts.iter().enumerate() {
            put(&mut zw, &format!("xl/charts/chart{}.xml", m + 1), cxml);
        }
        zw.finish().unwrap();
    }
    buf
}

fn sheet_to_xml(
    snap: &SheetSnapshot,
    wb: &WorkbookSnapshot,
    styles: &mut StyleRegistry,
    sst: &mut SharedStrings,
    frozen_rows: u32,
    frozen_cols: u32,
    drawing_rid: Option<u32>,
) -> String {
    // 按行分组
    let mut by_row: BTreeMap<u32, Vec<&CellSnapshot>> = BTreeMap::new();
    for cell in &snap.cells {
        by_row.entry(cell.r).or_default().push(cell);
    }
    let row_height: BTreeMap<u32, f64> = snap.row_heights.iter().map(|p| (p.0, p.1)).collect();
    let hidden_rows: std::collections::HashSet<u32> = snap.hidden_rows.iter().copied().collect();
    let col_width: BTreeMap<u32, f64> = snap.col_widths.iter().map(|p| (p.0, p.1)).collect();
    let hidden_cols: std::collections::HashSet<u32> = snap.hidden_cols.iter().copied().collect();

    // 大纲分组：从快照重建 OutlineAxis（复用其 level 派生 + 折叠隐藏逻辑），
    // 供 XLSX 的 <row/col outlineLevel hidden collapsed> 与 <outlinePr summaryBelow/Right>。
    let row_axis = rebuild_outline(&snap.row_outlines);
    let col_axis = rebuild_outline(&snap.col_outlines);
    let summary_below = snap.summary_below.unwrap_or(true);
    let summary_right = snap.summary_right.unwrap_or(true);
    // 折叠隐藏的行/列（并入显式 hidden 集）。
    let row_collapse_hidden = row_axis.hidden_indices(summary_below);
    let col_collapse_hidden = col_axis.hidden_indices(summary_right);

    let last_col = col_to_label(snap.col_count.saturating_sub(1));
    let dimension = format!("A1:{last_col}{}", snap.row_count);

    // 列
    let mut col_indices: Vec<u32> = col_width
        .keys()
        .copied()
        .chain(hidden_cols.iter().copied())
        .chain(col_collapse_hidden.iter().copied())
        .chain((0..snap.col_count).filter(|&c| col_axis.detail_level_at(c, summary_right) > 0))
        .collect();
    col_indices.sort_unstable();
    col_indices.dedup();
    let cols_xml = if col_indices.is_empty() {
        String::new()
    } else {
        let parts: String = col_indices
            .iter()
            .map(|&c| {
                let w_attr = col_width
                    .get(&c)
                    .map(|w| format!(" width=\"{:.2}\" customWidth=\"1\"", w / 7.0))
                    .unwrap_or_else(|| " width=\"8.43\"".to_string());
                let level = col_axis.detail_level_at(c, summary_right);
                let lvl_attr = if level > 0 {
                    format!(" outlineLevel=\"{level}\"")
                } else {
                    String::new()
                };
                let hidden = hidden_cols.contains(&c) || col_collapse_hidden.contains(&c);
                let h_attr = if hidden { " hidden=\"1\"" } else { "" };
                // collapsed 标在被折叠组的汇总列（summaryRight 决定其在明细右/左）。
                let collapsed_attr = if col_axis.is_collapse_boundary(c, summary_right) {
                    " collapsed=\"1\""
                } else {
                    ""
                };
                format!(
                    "<col min=\"{}\" max=\"{}\"{w_attr}{lvl_attr}{h_attr}{collapsed_attr}/>",
                    c + 1,
                    c + 1
                )
            })
            .collect();
        format!("<cols>{parts}</cols>")
    };

    // 行
    let mut row_nums: Vec<u32> = by_row
        .keys()
        .copied()
        .chain(row_height.keys().copied())
        .chain(hidden_rows.iter().copied())
        .chain(row_collapse_hidden.iter().copied())
        .chain((0..snap.row_count).filter(|&r| row_axis.detail_level_at(r, summary_below) > 0))
        .collect();
    row_nums.sort_unstable();
    row_nums.dedup();
    let mut rows_xml = String::new();
    for r in row_nums {
        let mut cells: Vec<&CellSnapshot> = by_row.get(&r).cloned().unwrap_or_default();
        cells.sort_by_key(|c| c.c);
        let ht_attr = row_height
            .get(&r)
            .map(|ht| {
                format!(
                    " ht=\"{}\" customHeight=\"1\"",
                    sheet_core::numstr::num_to_string(*ht)
                )
            })
            .unwrap_or_default();
        let level = row_axis.detail_level_at(r, summary_below);
        let lvl_attr = if level > 0 {
            format!(" outlineLevel=\"{level}\"")
        } else {
            String::new()
        };
        let hidden = hidden_rows.contains(&r) || row_collapse_hidden.contains(&r);
        let hid_attr = if hidden { " hidden=\"1\"" } else { "" };
        let collapsed_attr = if row_axis.is_collapse_boundary(r, summary_below) {
            " collapsed=\"1\""
        } else {
            ""
        };
        let cells_xml: String = cells
            .iter()
            .map(|cell| cell_to_xml(cell, r, snap, wb, styles, sst))
            .collect();
        rows_xml.push_str(&format!(
            "<row r=\"{}\"{ht_attr}{lvl_attr}{hid_attr}{collapsed_attr}>{cells_xml}</row>",
            r + 1
        ));
    }

    // 合并
    let merge_xml = if snap.spans.is_empty() {
        String::new()
    } else {
        let merges: String = snap
            .spans
            .iter()
            .map(|s| {
                let a = format_addr(s.row, s.col);
                let b = format_addr(s.row + s.row_count - 1, s.col + s.col_count - 1);
                format!("<mergeCell ref=\"{a}:{b}\"/>")
            })
            .collect();
        format!(
            "<mergeCells count=\"{}\">{merges}</mergeCells>",
            snap.spans.len()
        )
    };

    // <drawing>（图表锚点部件引用）——须在 mergeCells 之后、extLst 之前。
    let drawing_xml = match drawing_rid {
        Some(rid) => format!(
            "<drawing xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId{rid}\"/>"
        ),
        None => String::new(),
    };
    // 迷你图 <extLst>（放 worksheet 末）。
    let ext_lst = crate::xlsx_drawing::sparkline_ext_lst(&snap.name, snap);

    format!(
        "{XML_DECL}<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">{}{}<dimension ref=\"{dimension}\"/>{cols_xml}<sheetData>{rows_xml}</sheetData>{}{merge_xml}{}{drawing_xml}{ext_lst}</worksheet>",
        sheet_pr_xml(summary_below, summary_right, &row_axis, &col_axis),
        sheet_views_xml(frozen_rows, frozen_cols),
        sheet_protection_xml(snap),
        page_setup_xml(snap)
    )
}

/// 从快照分组列表重建 OutlineAxis（复用其 level 派生 + 折叠隐藏逻辑）。
fn rebuild_outline(groups: &[OutlineGroupSnapshot]) -> OutlineAxis {
    let mut axis = OutlineAxis::new();
    for g in groups {
        axis.group(g.start, g.count);
        if g.collapsed {
            axis.set_collapsed(g.start, true);
        }
    }
    axis
}

/// 逆运算 `level_at`：从逐行/列 outlineLevel 重建嵌套分组段。
/// 对每一层 ℓ∈[1,max]，取「level ≥ ℓ」的连续段各成一组（正确嵌套下即还原原分组）。
/// `collapsed` 为标了 `collapsed="1"` 的汇总边界索引；某组含此边界则置 collapsed。
fn levels_to_groups(
    levels: &[(u32, u32)],
    collapsed: &[u32],
    summary_after: bool,
) -> Vec<OutlineGroupSnapshot> {
    if levels.is_empty() {
        return Vec::new();
    }
    let level_of: std::collections::HashMap<u32, u32> = levels.iter().copied().collect();
    let max_level = levels.iter().map(|&(_, l)| l).max().unwrap_or(0);
    let max_idx = levels.iter().map(|&(i, _)| i).max().unwrap_or(0);
    let collapsed_set: std::collections::HashSet<u32> = collapsed.iter().copied().collect();
    let mut out: Vec<OutlineGroupSnapshot> = Vec::new();
    for lvl in 1..=max_level {
        let mut start: Option<u32> = None;
        // 扫到 max_idx+1 以在末尾收尾。
        for i in 0..=max_idx + 1 {
            let here = level_of.get(&i).copied().unwrap_or(0) >= lvl;
            match (here, start) {
                (true, None) => start = Some(i),
                (false, Some(s)) => {
                    // 段 [s, i)。OOXML 明细段 → 加回汇总格得整组区间。
                    let (g_start, g_count) = if summary_after {
                        (s, (i - s) + 1) // 汇总在明细后一格
                    } else {
                        (s - 1, (i - s) + 1) // 汇总在明细前一格
                    };
                    let is_collapsed =
                        (g_start..g_start + g_count).any(|x| collapsed_set.contains(&x));
                    out.push(OutlineGroupSnapshot {
                        start: g_start,
                        count: g_count,
                        collapsed: is_collapsed,
                    });
                    start = None;
                }
                _ => {}
            }
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then(a.count.cmp(&b.count)));
    out
}

/// `<sheetPr><outlinePr .../></sheetPr>`：仅当有大纲分组或汇总方位非默认时才写。
/// Excel 默认 summaryBelow=1/summaryRight=1；本项目模型同默认，故只在为 false 时显式写 0。
fn sheet_pr_xml(
    summary_below: bool,
    summary_right: bool,
    row_axis: &OutlineAxis,
    col_axis: &OutlineAxis,
) -> String {
    let has_outline = !row_axis.list().is_empty() || !col_axis.list().is_empty();
    if !has_outline && summary_below && summary_right {
        return String::new();
    }
    let sb = if summary_below { "1" } else { "0" };
    let sr = if summary_right { "1" } else { "0" };
    format!("<sheetPr><outlinePr summaryBelow=\"{sb}\" summaryRight=\"{sr}\"/></sheetPr>")
}

fn sheet_protection_xml(snap: &SheetSnapshot) -> String {
    let Some(p) = &snap.protection else {
        return String::new();
    };
    if !p.enabled {
        return String::new();
    }
    // OOXML 属性语义：值 1=禁止。allow* 为放行→取反。
    let sort = if p.allow_sort == Some(true) {
        " sort=\"0\""
    } else {
        " sort=\"1\""
    };
    let filter = if p.allow_filter == Some(true) {
        " autoFilter=\"0\""
    } else {
        " autoFilter=\"1\""
    };
    let fmt = if p.allow_format_cells == Some(true) {
        " formatCells=\"0\""
    } else {
        " formatCells=\"1\""
    };
    let ins = if p.allow_insert_delete == Some(true) {
        ""
    } else {
        " insertRows=\"1\" insertColumns=\"1\" deleteRows=\"1\" deleteColumns=\"1\""
    };
    format!("<sheetProtection sheet=\"1\" objects=\"1\" scenarios=\"1\"{sort}{filter}{fmt}{ins}/>")
}

fn sheet_views_xml(frozen_rows: u32, frozen_cols: u32) -> String {
    if frozen_rows == 0 && frozen_cols == 0 {
        return String::new();
    }
    let top_left = format!("{}{}", col_to_label(frozen_cols), frozen_rows + 1);
    let active_pane = if frozen_rows > 0 && frozen_cols > 0 {
        "bottomRight"
    } else if frozen_rows > 0 {
        "bottomLeft"
    } else {
        "topRight"
    };
    format!(
        "<sheetViews><sheetView workbookViewId=\"0\"><pane xSplit=\"{frozen_cols}\" ySplit=\"{frozen_rows}\" topLeftCell=\"{top_left}\" activePane=\"{active_pane}\" state=\"frozen\"/></sheetView></sheetViews>"
    )
}

fn page_setup_xml(snap: &SheetSnapshot) -> String {
    let Some(ps) = &snap.page_setup else {
        return String::new();
    };
    let paper = match ps.paper_size.as_deref() {
        Some("Letter") => 1,
        Some("Legal") => 5,
        Some("A3") => 8,
        _ => 9,
    };
    let orient = match ps.orientation {
        Some(sheet_core::worksheet::Orientation::Landscape) => "landscape",
        _ => "portrait",
    };
    let m = ps
        .margins
        .map(|m| (m.left / 72.0, m.right / 72.0, m.top / 72.0, m.bottom / 72.0))
        .unwrap_or((0.5, 0.5, 0.5, 0.5));
    let margins_xml = format!(
        "<pageMargins left=\"{}\" right=\"{}\" top=\"{}\" bottom=\"{}\" header=\"0.3\" footer=\"0.3\"/>",
        sheet_core::numstr::num_to_string(m.0),
        sheet_core::numstr::num_to_string(m.1),
        sheet_core::numstr::num_to_string(m.2),
        sheet_core::numstr::num_to_string(m.3)
    );
    let scale_attr = ps
        .scale
        .map(|s| format!(" scale=\"{}\"", sheet_core::numstr::num_to_string(s)))
        .unwrap_or_default();
    let fit_attr = ps
        .fit_to_pages
        .map(|f| format!(" fitToWidth=\"{}\" fitToHeight=\"{}\"", f.width, f.height))
        .unwrap_or_default();
    format!("{margins_xml}<pageSetup paperSize=\"{paper}\" orientation=\"{orient}\"{scale_attr}{fit_attr}/>")
}

fn cell_to_xml(
    cell: &CellSnapshot,
    row: u32,
    _sheet: &SheetSnapshot,
    wb: &WorkbookSnapshot,
    styles: &mut StyleRegistry,
    sst: &mut SharedStrings,
) -> String {
    let cref = format_addr(row, cell.c);
    let resolved = resolve_cell_style(cell.s.as_ref(), wb);
    let s = styles.intern(&resolved);
    let s_attr = if s != 0 {
        format!(" s=\"{s}\"")
    } else {
        String::new()
    };

    // 公式格
    if let Some(f) = &cell.f {
        let (v_xml, t_attr) = match &cell.v {
            Some(CellValue::Number(n)) => (
                format!("<v>{}</v>", sheet_core::numstr::num_to_string(*n)),
                "",
            ),
            Some(CellValue::Bool(b)) => (format!("<v>{}</v>", if *b { 1 } else { 0 }), ""),
            Some(CellValue::Text(t)) if !t.is_empty() => {
                (format!("<v>{}</v>", esc(t)), " t=\"str\"")
            }
            _ => (String::new(), ""),
        };
        return format!(
            "<c r=\"{cref}\"{s_attr}{t_attr}><f>{}</f>{v_xml}</c>",
            esc(f)
        );
    }

    match &cell.v {
        None => {
            if s != 0 {
                format!("<c r=\"{cref}\"{s_attr}/>")
            } else {
                String::new()
            }
        }
        Some(CellValue::Text(t)) if t.is_empty() => {
            if s != 0 {
                format!("<c r=\"{cref}\"{s_attr}/>")
            } else {
                String::new()
            }
        }
        Some(CellValue::Number(n)) => format!(
            "<c r=\"{cref}\"{s_attr}><v>{}</v></c>",
            sheet_core::numstr::num_to_string(*n)
        ),
        Some(CellValue::Bool(b)) => {
            format!(
                "<c r=\"{cref}\"{s_attr} t=\"b\"><v>{}</v></c>",
                if *b { 1 } else { 0 }
            )
        }
        Some(CellValue::Text(t)) => {
            let si = sst.intern(t);
            format!("<c r=\"{cref}\"{s_attr} t=\"s\"><v>{si}</v></c>")
        }
    }
}

// ── 导入：OOXML → 中性快照（正则扫描）──

/// XLSX 字节 → 工作簿。
pub fn import_xlsx(bytes: &[u8]) -> Workbook {
    workbook_from_json(&xlsx_to_snapshot(bytes))
}

/// XLSX 字节 → 中性快照。
pub fn xlsx_to_snapshot(bytes: &[u8]) -> WorkbookSnapshot {
    let files = unzip(bytes);
    let get = |p: &str| -> Option<String> {
        files
            .get(p)
            .map(|d| String::from_utf8_lossy(d).into_owned())
    };

    let sst = parse_shared_strings(get("xl/sharedStrings.xml").as_deref());
    let styles = parse_styles(get("xl/styles.xml").as_deref());

    let workbook_xml = get("xl/workbook.xml").unwrap_or_default();
    let rels_xml = get("xl/_rels/workbook.xml.rels").unwrap_or_default();
    let mut rel_target: BTreeMap<String, String> = BTreeMap::new();
    for caps in re_rel().captures_iter(&rels_xml) {
        let a = &caps[1];
        if let (Some(id), Some(target)) = (attr(a, "Id"), attr(a, "Target")) {
            let t = target
                .trim_start_matches('/')
                .trim_start_matches("xl/")
                .to_string();
            rel_target.insert(id, t);
        }
    }

    let mut sheets: Vec<SheetSnapshot> = Vec::new();
    for caps in re_sheet().captures_iter(&workbook_xml) {
        let a = &caps[1];
        let name = attr(a, "name")
            .map(|n| unesc(&n))
            .unwrap_or_else(|| format!("Sheet{}", sheets.len() + 1));
        let rid = attr(a, "r:id").or_else(|| attr(a, "id"));
        let target = rid
            .and_then(|r| rel_target.get(&r).cloned())
            .unwrap_or_else(|| format!("worksheets/sheet{}.xml", sheets.len() + 1));
        if let Some(sheet_xml) = get(&format!("xl/{target}")) {
            sheets.push(parse_sheet_xml(&sheet_xml, &name, &sst, &styles));
        }
    }

    if sheets.is_empty() {
        sheets.push(SheetSnapshot {
            name: "Sheet1".to_string(),
            row_count: 40,
            col_count: 12,
            cells: Vec::new(),
            ..Default::default()
        });
    }

    let mut snap = WorkbookSnapshot {
        format: crate::snapshot::SNAPSHOT_FORMAT.to_string(),
        version: 1,
        sheets,
        ..Default::default()
    };
    if let Some(caps) = re_active_tab().captures(&workbook_xml) {
        if let Ok(n) = caps[1].parse::<usize>() {
            if n > 0 {
                snap.active_sheet = Some(n);
            }
        }
    }
    // 活动 sheet 的 <pane> 冻结提升到工作簿级
    let active_idx = snap.active_sheet.unwrap_or(0);
    let active = snap
        .sheets
        .get(active_idx)
        .or_else(|| snap.sheets.iter().find(|s| s.frozen_pane.is_some()));
    if let Some(fp) = active.and_then(|s| s.frozen_pane) {
        snap.frozen_row_count = fp.0;
        snap.frozen_col_count = fp.1;
    }
    snap
}

fn unzip(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let Ok(mut archive) = ZipArchive::new(Cursor::new(bytes)) else {
        return out;
    };
    for i in 0..archive.len() {
        let Ok(mut file) = archive.by_index(i) else {
            continue;
        };
        let name = file.name().to_string();
        let mut data = Vec::new();
        if file.read_to_end(&mut data).is_ok() {
            out.insert(name, data);
        }
    }
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let re = cached_re(&format!("{}=\"([^\"]*)\"", regex::escape(name)));
    re.captures(tag).map(|c| c[1].to_string())
}

/// 缓存编译过的正则（按 pattern 字符串），避免循环内重复编译（clippy regex_creation_in_loops）。
fn cached_re(pattern: &str) -> std::sync::Arc<Regex> {
    use std::sync::Mutex;
    static CACHE: OnceLock<Mutex<BTreeMap<String, std::sync::Arc<Regex>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut guard = cache.lock().unwrap();
    if let Some(re) = guard.get(pattern) {
        return re.clone();
    }
    let re = std::sync::Arc::new(Regex::new(pattern).unwrap());
    guard.insert(pattern.to_string(), re.clone());
    re
}

fn re_rel() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<Relationship\s+([^/>]*)/>").unwrap())
}
fn re_sheet() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<sheet\s+([^/>]*)/>").unwrap())
}
fn re_active_tab() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"<workbookView[^>]*activeTab="([^"]*)""#).unwrap())
}

fn parse_shared_strings(xml: Option<&str>) -> Vec<String> {
    let Some(xml) = xml else {
        return Vec::new();
    };
    static SI: OnceLock<Regex> = OnceLock::new();
    static T: OnceLock<Regex> = OnceLock::new();
    let si_re = SI.get_or_init(|| Regex::new(r"(?s)<si>(.*?)</si>").unwrap());
    let t_re = T.get_or_init(|| Regex::new(r"(?s)<t[^>]*>(.*?)</t>").unwrap());
    si_re
        .captures_iter(xml)
        .map(|si| {
            t_re.captures_iter(&si[1])
                .map(|t| unesc(&t[1]))
                .collect::<String>()
        })
        .collect()
}

/// 解析后的 styles.xml：cellXfs 索引 → Style。
struct ParsedStyles {
    xf: Vec<Style>,
}

fn builtin_numfmt(id: u32) -> Option<&'static str> {
    match id {
        1 => Some("0"),
        2 => Some("0.00"),
        3 => Some("#,##0"),
        4 => Some("#,##0.00"),
        9 => Some("0%"),
        10 => Some("0.00%"),
        44 => Some("#,##0.00"),
        _ => None,
    }
}

fn parse_styles(xml: Option<&str>) -> ParsedStyles {
    let Some(xml) = xml else {
        return ParsedStyles {
            xf: vec![Style::default()],
        };
    };

    // numFmts
    let mut num_fmt_by_id: BTreeMap<u32, String> = BTreeMap::new();
    let nf_re = Regex::new(r"<numFmt\s+([^/>]*)/>").unwrap();
    for c in nf_re.captures_iter(xml) {
        if let (Some(id), Some(code)) = (attr(&c[1], "numFmtId"), attr(&c[1], "formatCode")) {
            if let Ok(id) = id.parse::<u32>() {
                num_fmt_by_id.insert(id, unesc(&code));
            }
        }
    }

    // fonts
    let mut fonts: Vec<Style> = Vec::new();
    let fonts_block = block(xml, "fonts").unwrap_or_default();
    let font_re = Regex::new(r"(?s)<font>(.*?)</font>").unwrap();
    for c in font_re.captures_iter(&fonts_block) {
        let f = &c[1];
        let mut fp = Style::default();
        if f.contains("<b/>") || f.contains("<b>") || f.contains("<b ") {
            fp.bold = Some(true);
        }
        if f.contains("<i/>") || f.contains("<i>") || f.contains("<i ") {
            fp.italic = Some(true);
        }
        if f.contains("<u/>") || f.contains("<u>") || f.contains("<u ") {
            fp.underline = Some(true);
        }
        if f.contains("<strike/>") || f.contains("<strike>") || f.contains("<strike ") {
            fp.strikethrough = Some(true);
        }
        if let Some(sz) = cached_re(r#"<sz val="([^"]*)""#).captures(f) {
            fp.font_size = sz[1].parse::<f64>().ok();
        }
        if let Some(name) = cached_re(r#"<name val="([^"]*)""#).captures(f) {
            fp.font_family = Some(unesc(&name[1]));
        }
        if let Some(color) = cached_re(r#"<color[^>]*rgb="([^"]*)""#).captures(f) {
            fp.fore_color = from_argb(Some(&color[1]));
        }
        fonts.push(fp);
    }

    // fills（solid → backColor）
    let mut fills: Vec<Option<String>> = Vec::new();
    let fills_block = block(xml, "fills").unwrap_or_default();
    let fill_re = Regex::new(r"(?s)<fill>(.*?)</fill>").unwrap();
    for c in fill_re.captures_iter(&fills_block) {
        let body = &c[1];
        let pt = cached_re(r#"patternType="([^"]*)""#)
            .captures(body)
            .map(|m| m[1].to_string());
        let fg = cached_re(r#"<fgColor[^>]*rgb="([^"]*)""#)
            .captures(body)
            .map(|m| m[1].to_string());
        if pt.as_deref() == Some("solid") {
            fills.push(from_argb(fg.as_deref()));
        } else {
            fills.push(None);
        }
    }

    // borders
    let mut borders: Vec<Option<Borders>> = Vec::new();
    let borders_block = block(xml, "borders").unwrap_or_default();
    let border_re = Regex::new(r"(?s)<border[^>]*>(.*?)</border>").unwrap();
    for c in border_re.captures_iter(&borders_block) {
        borders.push(parse_border_xml(&c[1]));
    }

    // cellXfs
    let mut xf: Vec<Style> = Vec::new();
    let xfs_block = block(xml, "cellXfs").unwrap_or_default();
    let xf_re = Regex::new(r"(?s)<xf\s+([^>]*?)(?:/>|>(.*?)</xf>)").unwrap();
    for c in xf_re.captures_iter(&xfs_block) {
        let attrs = &c[1];
        let inner = c.get(2).map(|m| m.as_str()).unwrap_or("");
        let mut st = Style::default();
        let font_id = attr(attrs, "fontId")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let fill_id = attr(attrs, "fillId")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let border_id = attr(attrs, "borderId")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        let num_fmt_id = attr(attrs, "numFmtId")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if font_id != 0 {
            if let Some(f) = fonts.get(font_id) {
                st.overlay(f);
            }
        }
        if fill_id != 0 {
            if let Some(Some(bg)) = fills.get(fill_id) {
                st.back_color = Some(bg.clone());
            }
        }
        if border_id != 0 {
            if let Some(Some(b)) = borders.get(border_id) {
                st.borders = Some(b.clone());
            }
        }
        if num_fmt_id != 0 {
            if let Some(code) = num_fmt_by_id.get(&num_fmt_id) {
                st.formatter = Some(code.clone());
            } else if let Some(code) = builtin_numfmt(num_fmt_id) {
                st.formatter = Some(code.to_string());
            }
        }
        // alignment
        if let Some(al) = cached_re(r"<alignment\s+([^/>]*)/>").captures(inner) {
            let al = &al[1];
            if let Some(h) = attr(al, "horizontal").as_deref().and_then(h_from_xlsx) {
                st.h_align = Some(h);
            }
            if let Some(v) = attr(al, "vertical").as_deref().and_then(v_from_xlsx) {
                st.v_align = Some(v);
            }
            if attr(al, "wrapText").as_deref() == Some("1") {
                st.word_wrap = Some(true);
            }
            if let Some(ind) = attr(al, "indent").and_then(|s| s.parse::<f64>().ok()) {
                if ind > 0.0 {
                    st.indent = Some(ind);
                }
            }
            if attr(al, "shrinkToFit").as_deref() == Some("1") {
                st.shrink_to_fit = Some(true);
            }
        }
        if inner.contains("locked=\"0\"") {
            st.locked = Some(false);
        }
        xf.push(st);
    }
    if xf.is_empty() {
        xf.push(Style::default());
    }
    ParsedStyles { xf }
}

fn block(xml: &str, tag: &str) -> Option<String> {
    let re = cached_re(&format!(r"(?s)<{tag}[^>]*>(.*?)</{tag}>"));
    re.captures(xml).map(|c| c[1].to_string())
}

fn parse_border_xml(inner: &str) -> Option<Borders> {
    let mut b = Borders::default();
    let mut any = false;
    for side in ["left", "right", "top", "bottom"] {
        let re = cached_re(&format!(
            r#"<{side}\s+style="([^"]*)"[^>]*>(?:<color[^>]*rgb="([^"]*)")?"#
        ));
        if let Some(m) = re.captures(inner) {
            let edge = BorderEdge {
                style: border_from_xlsx(&m[1]),
                color: from_argb(m.get(2).map(|x| x.as_str()))
                    .unwrap_or_else(|| "#000".to_string()),
            };
            match side {
                "left" => b.left = Some(edge),
                "right" => b.right = Some(edge),
                "top" => b.top = Some(edge),
                _ => b.bottom = Some(edge),
            }
            any = true;
        }
    }
    if any {
        Some(b)
    } else {
        None
    }
}

fn parse_sheet_xml(xml: &str, name: &str, sst: &[String], styles: &ParsedStyles) -> SheetSnapshot {
    let mut cells: Vec<CellSnapshot> = Vec::new();
    let mut max_row = 0u32;
    let mut max_col = 0u32;
    let mut row_heights: Vec<NumPair> = Vec::new();
    let mut hidden_rows: Vec<u32> = Vec::new();
    // 大纲：逐行/列的 outlineLevel（0=无组）+ collapsed 汇总边界，用于重建分组。
    let mut row_levels: Vec<(u32, u32)> = Vec::new();
    let mut row_collapsed: Vec<u32> = Vec::new();

    let sheet_data = block(xml, "sheetData").unwrap_or_default();
    let row_re = Regex::new(r"(?s)<row\s+([^>]*?)(?:/>|>(.*?)</row>)").unwrap();
    let c_re = Regex::new(r"(?s)<c\s+([^>]*?)(?:/>|>(.*?)</c>)").unwrap();
    let f_re = Regex::new(r"(?s)<f[^>]*>(.*?)</f>").unwrap();
    let v_re = Regex::new(r"(?s)<v>(.*?)</v>").unwrap();
    let is_re = Regex::new(r"(?s)<is>.*?<t[^>]*>(.*?)</t>.*?</is>").unwrap();

    for rm in row_re.captures_iter(&sheet_data) {
        let r_attrs = &rm[1];
        let Some(r_idx) = attr(r_attrs, "r").and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let r_idx = r_idx - 1;
        max_row = max_row.max(r_idx);
        if attr(r_attrs, "customHeight").as_deref() == Some("1") {
            if let Some(ht) = attr(r_attrs, "ht").and_then(|s| s.parse::<f64>().ok()) {
                row_heights.push(NumPair(r_idx, ht));
            }
        }
        if attr(r_attrs, "hidden").as_deref() == Some("1") {
            hidden_rows.push(r_idx);
        }
        if let Some(lvl) = attr(r_attrs, "outlineLevel").and_then(|s| s.parse::<u32>().ok()) {
            if lvl > 0 {
                row_levels.push((r_idx, lvl));
            }
        }
        if attr(r_attrs, "collapsed").as_deref() == Some("1") {
            row_collapsed.push(r_idx);
        }
        let row_inner = rm.get(2).map(|m| m.as_str()).unwrap_or("");
        for cm in c_re.captures_iter(row_inner) {
            let c_attrs = &cm[1];
            let c_inner = cm.get(2).map(|m| m.as_str()).unwrap_or("");
            let Some(cref) = attr(c_attrs, "r") else {
                continue;
            };
            let Some(coord) = parse_addr(&cref) else {
                continue;
            };
            max_col = max_col.max(coord.col);
            let t = attr(c_attrs, "t");
            let s_idx = attr(c_attrs, "s")
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(0);
            let mut cell = CellSnapshot {
                r: coord.row,
                c: coord.col,
                v: None,
                f: None,
                s: None,
                rich: None,
            };
            if let Some(fm) = f_re.captures(c_inner) {
                cell.f = Some(unesc(&fm[1]));
            }
            if let Some(is_m) = is_re.captures(c_inner) {
                cell.v = Some(CellValue::Text(unesc(&is_m[1])));
            } else if let Some(vm) = v_re.captures(c_inner) {
                let raw = &vm[1];
                cell.v = Some(match t.as_deref() {
                    Some("s") => CellValue::Text(
                        raw.parse::<usize>()
                            .ok()
                            .and_then(|i| sst.get(i).cloned())
                            .unwrap_or_default(),
                    ),
                    Some("b") => CellValue::Bool(raw == "1"),
                    Some("str") => CellValue::Text(unesc(raw)),
                    _ => {
                        if raw.is_empty() {
                            CellValue::Text(String::new())
                        } else {
                            match raw.parse::<f64>() {
                                Ok(n) => CellValue::Number(n),
                                Err(_) => CellValue::Text(unesc(raw)),
                            }
                        }
                    }
                });
            }
            if let Some(st) = styles.xf.get(s_idx) {
                if !st.is_empty() {
                    cell.s = Some(st.clone());
                }
            }
            if cell.v.is_some() || cell.f.is_some() || cell.s.is_some() {
                cells.push(cell);
            }
        }
    }

    // cols
    let mut col_widths: Vec<NumPair> = Vec::new();
    let mut hidden_cols: Vec<u32> = Vec::new();
    let mut col_levels: Vec<(u32, u32)> = Vec::new();
    let mut col_collapsed: Vec<u32> = Vec::new();
    if let Some(cols_block) = block(xml, "cols") {
        let col_re = Regex::new(r"<col\s+([^/>]*)/>").unwrap();
        for cm in col_re.captures_iter(&cols_block) {
            let a = &cm[1];
            let min = attr(a, "min")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1)
                - 1;
            let max = attr(a, "max")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(1)
                - 1;
            let width = attr(a, "width").and_then(|s| s.parse::<f64>().ok());
            let hidden = attr(a, "hidden").as_deref() == Some("1");
            let custom = attr(a, "customWidth").as_deref() == Some("1");
            let level = attr(a, "outlineLevel")
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let collapsed = attr(a, "collapsed").as_deref() == Some("1");
            for c in min..=max {
                if custom {
                    if let Some(w) = width {
                        col_widths.push(NumPair(c, (w * 7.0).round()));
                    }
                }
                if hidden {
                    hidden_cols.push(c);
                }
                if level > 0 {
                    col_levels.push((c, level));
                }
                if collapsed {
                    col_collapsed.push(c);
                }
                max_col = max_col.max(c);
            }
        }
    }

    // merges
    let mut spans: Vec<sheet_core::worksheet::Span> = Vec::new();
    if let Some(merge_block) = block(xml, "mergeCells") {
        let merge_re = Regex::new(r#"<mergeCell\s+ref="([^"]*)""#).unwrap();
        for mm in merge_re.captures_iter(&merge_block) {
            let refstr = &mm[1];
            let mut it = refstr.split(':');
            let a = it.next().unwrap_or("");
            let b = it.next().unwrap_or(a);
            if let (Some(ca), Some(cb)) = (parse_addr(a), parse_addr(b)) {
                spans.push(sheet_core::worksheet::Span {
                    row: ca.row.min(cb.row),
                    col: ca.col.min(cb.col),
                    row_count: ca.row.abs_diff(cb.row) + 1,
                    col_count: ca.col.abs_diff(cb.col) + 1,
                });
                max_row = max_row.max(ca.row).max(cb.row);
                max_col = max_col.max(ca.col).max(cb.col);
            }
        }
    }

    // dimension 兜底
    if let Some(dim) = Regex::new(r#"<dimension\s+ref="([^"]*)""#)
        .unwrap()
        .captures(xml)
    {
        let d = &dim[1];
        let end = d.split(':').next_back().unwrap_or(d);
        if let Some(e) = parse_addr(end) {
            max_row = max_row.max(e.row);
            max_col = max_col.max(e.col);
        }
    }

    cells.sort_by(|a, b| a.r.cmp(&b.r).then(a.c.cmp(&b.c)));

    // 大纲汇总方位 <outlinePr summaryBelow summaryRight>（缺省 Excel 均为 1=后）。
    let (summary_below, summary_right) = if let Some(op) = Regex::new(r"<outlinePr\s+([^>]*?)/>")
        .unwrap()
        .captures(xml)
    {
        (
            attr(&op[1], "summaryBelow").as_deref() != Some("0"),
            attr(&op[1], "summaryRight").as_deref() != Some("0"),
        )
    } else {
        (true, true)
    };
    // 从逐行/列 outlineLevel 重建分组段（collapsed 汇总边界 → 该组折叠）。
    let row_outlines = levels_to_groups(&row_levels, &row_collapsed, summary_below);
    let col_outlines = levels_to_groups(&col_levels, &col_collapsed, summary_right);

    let mut snap = SheetSnapshot {
        name: name.to_string(),
        row_count: (max_row + 1).max(1),
        col_count: (max_col + 1).max(1),
        cells,
        spans,
        row_heights,
        col_widths,
        hidden_rows,
        hidden_cols,
        row_outlines,
        col_outlines,
        summary_below: if summary_below { None } else { Some(false) },
        summary_right: if summary_right { None } else { Some(false) },
        ..Default::default()
    };

    // 冻结窗格
    if let Some(pane) = Regex::new(r"<pane\s+([^>]*?)/>").unwrap().captures(xml) {
        let x = attr(&pane[1], "xSplit")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        let y = attr(&pane[1], "ySplit")
            .and_then(|s| s.parse::<u32>().ok())
            .unwrap_or(0);
        if x > 0 || y > 0 {
            snap.frozen_pane = Some((y, x));
        }
    }
    // 工作表保护 <sheetProtection>
    if let Some(prot) = cached_re(r"<sheetProtection\s+([^/>]*)/?>").captures(xml) {
        let a = &prot[1];
        if a.contains("sheet=\"1\"") {
            snap.protection = Some(sheet_core::worksheet::SheetProtection {
                enabled: true,
                allow_sort: Some(a.contains("sort=\"0\"")),
                allow_filter: Some(a.contains("autoFilter=\"0\"")),
                allow_format_cells: Some(a.contains("formatCells=\"0\"")),
                allow_insert_delete: None,
                allow_select_locked: None,
            });
        }
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheet_core::style::StyleSheet;
    use sheet_core::worksheet::Worksheet;

    fn sample() -> Workbook {
        let mut wb = Workbook::empty();
        let mut ss = StyleSheet::new();
        ss.define(
            "hdr",
            Style {
                bold: Some(true),
                back_color: Some("#dfe8ff".into()),
                h_align: Some(HAlign::Center),
                ..Default::default()
            },
        );
        ss.define(
            "money",
            Style {
                formatter: Some("#,##0.00".into()),
                h_align: Some(HAlign::Right),
                ..Default::default()
            },
        );
        let mut s1 = Worksheet::with_size("资产表", 20, 6);
        s1.style_sheet = ss.clone();
        s1.set_column_width(0, 210.0);
        s1.set_row_height(0, 34.0);
        s1.set_value(0, 0, Some("某公司 资产负债表".into()));
        s1.set_style(
            0,
            0,
            Some(Style {
                style_name: Some("hdr".into()),
                ..Default::default()
            }),
        );
        s1.add_span(0, 0, 1, 4);
        s1.set_value(1, 0, Some("货币资金".into()));
        s1.set_value(1, 1, Some(620000.into()));
        s1.set_style(
            1,
            1,
            Some(Style {
                style_name: Some("money".into()),
                ..Default::default()
            }),
        );
        s1.set_value(2, 0, Some("应收账款".into()));
        s1.set_value(2, 1, Some(388000.into()));
        s1.set_value(3, 0, Some("合计".into()));
        s1.set_style(
            3,
            0,
            Some(Style {
                bold: Some(true),
                ..Default::default()
            }),
        );
        s1.set_formula(3, 1, "=SUM(B2:B3)");
        s1.set_style(
            3,
            1,
            Some(Style {
                style_name: Some("money".into()),
                ..Default::default()
            }),
        );
        s1.set_computed_value(3, 1, Some(1008000.into()));
        s1.set_value(4, 0, Some("达标".into()));
        s1.set_style(
            4,
            0,
            Some(Style {
                italic: Some(true),
                underline: Some(true),
                fore_color: Some("#c00000".into()),
                ..Default::default()
            }),
        );
        s1.set_value(4, 1, Some(true.into()));
        s1.set_column_visible(5, false);
        wb.append_sheet(s1);

        let mut s2 = Worksheet::with_size("利润表", 10, 4);
        s2.style_sheet = ss.clone();
        s2.set_value(0, 0, Some("营业收入".into()));
        s2.set_value(0, 1, Some(3200000.into()));
        s2.set_style(
            0,
            1,
            Some(Style {
                borders: Some(Borders {
                    bottom: Some(BorderEdge {
                        style: BorderLineStyle::Medium,
                        color: "#333333".into(),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        );
        wb.append_sheet(s2);
        wb.set_active_sheet_index(1);
        wb
    }

    #[test]
    fn valid_zip_with_all_parts() {
        let bytes = export_xlsx(&sample());
        let files = unzip(&bytes);
        for part in [
            "[Content_Types].xml",
            "_rels/.rels",
            "xl/workbook.xml",
            "xl/_rels/workbook.xml.rels",
            "xl/styles.xml",
            "xl/sharedStrings.xml",
            "xl/worksheets/sheet1.xml",
            "xl/worksheets/sheet2.xml",
        ] {
            assert!(files.contains_key(part), "missing {part}");
        }
    }

    #[test]
    fn xml_parts_have_declaration_and_balanced_tags() {
        let files = unzip(&export_xlsx(&sample()));
        for (name, data) in &files {
            if !name.ends_with(".xml") {
                continue;
            }
            let xml = String::from_utf8_lossy(data);
            assert!(xml.starts_with("<?xml"), "{name} no decl");
            assert_eq!(
                xml.matches('<').count(),
                xml.matches('>').count(),
                "{name} unbalanced"
            );
        }
    }

    #[test]
    fn round_trip_values() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let s1 = wb.sheet_by_name("资产表").unwrap();
        assert_eq!(s1.get_value(1, 0), Some("货币资金".into()));
        assert_eq!(s1.get_value(1, 1), Some(620000.into()));
        assert_eq!(s1.get_value(4, 1), Some(true.into()));
    }

    #[test]
    fn round_trip_formulas() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let s1 = wb.sheet_by_name("资产表").unwrap();
        assert_eq!(s1.get_formula(3, 1), "SUM(B2:B3)");
        assert_eq!(s1.get_value(3, 1), Some(1008000.into()));
    }

    #[test]
    fn round_trip_merges() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let span = wb.sheet_by_name("资产表").unwrap().get_span(0, 0).unwrap();
        assert_eq!(
            (span.row, span.col, span.row_count, span.col_count),
            (0, 0, 1, 4)
        );
    }

    #[test]
    fn round_trip_geometry() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let s1 = wb.sheet_by_name("资产表").unwrap();
        assert_eq!(s1.get_row_height(0), 34.0);
        assert!((s1.get_column_width(0) - 210.0).abs() <= 8.0);
        assert!(!s1.is_column_visible(5));
    }

    #[test]
    fn round_trip_font_styles() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let st = wb.sheet_by_name("资产表").unwrap().get_style(4, 0).unwrap();
        assert_eq!(st.italic, Some(true));
        assert_eq!(st.underline, Some(true));
        assert_eq!(st.fore_color.as_deref(), Some("#c00000"));
    }

    #[test]
    fn round_trip_named_style_flattened() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let hdr = wb.sheet_by_name("资产表").unwrap().get_style(0, 0).unwrap();
        assert_eq!(hdr.bold, Some(true));
        assert_eq!(hdr.h_align, Some(HAlign::Center));
        assert_eq!(hdr.back_color.as_deref(), Some("#dfe8ff"));
        let money = wb.sheet_by_name("资产表").unwrap().get_style(1, 1).unwrap();
        assert_eq!(money.formatter.as_deref(), Some("#,##0.00"));
        assert_eq!(money.h_align, Some(HAlign::Right));
    }

    #[test]
    fn round_trip_borders() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        let st = wb.sheet_by_name("利润表").unwrap().get_style(0, 1).unwrap();
        let bottom = st.borders.unwrap().bottom.unwrap();
        assert_eq!(bottom.style, BorderLineStyle::Medium);
        assert_eq!(bottom.color, "#333333");
    }

    #[test]
    fn round_trip_sheet_order_active() {
        let wb = import_xlsx(&export_xlsx(&sample()));
        assert_eq!(wb.sheet(0).unwrap().name(), "资产表");
        assert_eq!(wb.sheet(1).unwrap().name(), "利润表");
        assert_eq!(wb.active_sheet_index(), 1);
    }

    #[test]
    fn shared_and_inline_strings() {
        let snap = WorkbookSnapshot {
            format: crate::snapshot::SNAPSHOT_FORMAT.to_string(),
            version: 1,
            sheets: vec![SheetSnapshot {
                name: "S".into(),
                row_count: 3,
                col_count: 2,
                cells: vec![
                    CellSnapshot {
                        r: 0,
                        c: 0,
                        v: Some("共享串".into()),
                        f: None,
                        s: None,
                        rich: None,
                    },
                    CellSnapshot {
                        r: 1,
                        c: 0,
                        v: Some(123.into()),
                        f: None,
                        s: None,
                        rich: None,
                    },
                ],
                ..Default::default()
            }],
            ..Default::default()
        };
        let bytes = snapshot_to_xlsx(&snap);
        let back = xlsx_to_snapshot(&bytes);
        let c00 = back.sheets[0]
            .cells
            .iter()
            .find(|c| c.r == 0 && c.c == 0)
            .unwrap();
        assert_eq!(c00.v, Some("共享串".into()));
        let c10 = back.sheets[0]
            .cells
            .iter()
            .find(|c| c.r == 1 && c.c == 0)
            .unwrap();
        assert_eq!(c10.v, Some(123.into()));
    }

    #[test]
    fn empty_workbook_round_trips() {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::new("Sheet1"));
        let back = import_xlsx(&export_xlsx(&wb));
        assert_eq!(back.sheet_count(), 1);
        assert!(back.active_sheet().is_some());
    }

    #[test]
    fn round_trip_freeze_pane() {
        let mut wb = Workbook::empty();
        wb.append_sheet(Worksheet::with_size("S", 20, 10));
        wb.freeze_panes(2, 1);
        let back = import_xlsx(&export_xlsx(&wb));
        assert_eq!(back.viewport().frozen_row_count, 2);
        assert_eq!(back.viewport().frozen_col_count, 1);
    }

    #[test]
    fn round_trip_protection() {
        // M20：保护 + 解锁格随 XLSX 往返
        use sheet_core::worksheet::SheetProtection;
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("S", 10, 10);
        ws.set_value(0, 0, Some("x".into()));
        ws.set_style(
            0,
            0,
            Some(Style {
                locked: Some(false),
                ..Default::default()
            }),
        );
        ws.set_protection(Some(SheetProtection {
            enabled: true,
            ..Default::default()
        }));
        wb.append_sheet(ws);
        let back = import_xlsx(&export_xlsx(&wb));
        let s2 = back.sheet(0).unwrap();
        assert!(s2.is_protected());
        assert_eq!(s2.get_style(0, 0).and_then(|st| st.locked), Some(false));
        assert!(s2.can_edit_cell(0, 0));
        assert!(!s2.can_edit_cell(1, 1));
    }

    #[test]
    fn round_trip_row_outline_three_levels() {
        // M99 ②：三级行分组 summaryBelow=false（汇总在组首）随 XLSX 往返。
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("行分组", 12, 4);
        for r in 0..9 {
            ws.set_value(r, 0, Some(format!("r{r}").into()));
        }
        ws.summary_below = false;
        ws.row_outlines.group(2, 7); // 合计 2..8
        ws.row_outlines.group(3, 3); // 甲 3..5
        ws.row_outlines.group(6, 3); // 乙 6..8
        wb.append_sheet(ws);

        // 导出 sheet XML 应含 outlineLevel + <outlinePr summaryBelow="0">。
        let files = unzip(&export_xlsx(&wb));
        let xml = String::from_utf8_lossy(&files["xl/worksheets/sheet1.xml"]).to_string();
        assert!(xml.contains("summaryBelow=\"0\""), "缺 outlinePr");
        assert!(xml.contains("outlineLevel=\"2\""), "缺三级 outlineLevel");

        let back = import_xlsx(&export_xlsx(&wb));
        let s2 = back.sheet(0).unwrap();
        assert_eq!(s2.row_outlines.list().len(), 3, "行分组 3 组");
        assert_eq!(s2.row_outlines.max_level(), Some(1), "三级 maxLevel=1");
        assert!(!s2.summary_below, "summaryBelow=false 保持");
        assert!(
            s2.column_outlines.list().is_empty(),
            "行分组 sheet 无列分组"
        );
    }

    #[test]
    fn round_trip_col_outline_and_collapsed() {
        // M99 ③：列分组 summaryRight=false + 折叠态随 XLSX 往返。
        let mut wb = Workbook::empty();
        let mut ws = Worksheet::with_size("列分组", 4, 10);
        for c in 0..8 {
            ws.set_value(0, c, Some(format!("c{c}").into()));
        }
        ws.summary_right = false;
        ws.column_outlines.group(1, 7); // 全年 1..7
        ws.column_outlines.group(2, 3); // 上半年 2..4
        ws.column_outlines.group(5, 3); // 下半年 5..7
        ws.column_outlines.set_collapsed(2, true); // 折叠上半年
        ws.apply_outline_visibility();
        wb.append_sheet(ws);

        let back = import_xlsx(&export_xlsx(&wb));
        let s2 = back.sheet(0).unwrap();
        assert_eq!(s2.column_outlines.list().len(), 3, "列分组 3 组");
        assert_eq!(s2.column_outlines.max_level(), Some(1));
        assert!(!s2.summary_right, "summaryRight=false 保持");
        // 折叠态还原：上半年组（start=2）应仍折叠。
        let collapsed = s2
            .column_outlines
            .list()
            .iter()
            .find(|g| g.start == 2)
            .map(|g| g.collapsed);
        assert_eq!(collapsed, Some(true), "折叠态随往返保持");
    }
}
