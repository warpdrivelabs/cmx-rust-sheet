//! M99 多 sheet 集成测试 —— 对标 cmx-mega-sheet `demo/specs/spec-m99.js` 的 `checks`。
//!
//! 覆盖两条链：
//!  1. **后端模型**：build_m99_workbook 搭出的六 sheet 结构（迷你图/行组/列组/双轴/样式/图表）
//!     与 spec 的 checks 断言逐条对齐。
//!  2. **XLSX 往返**：整簿导出 `.xlsx` 再 import 回来，多 sheet 与多级分组（outlineLevel/
//!     summaryBelow）无损——即「后端实现多 sheet + 导出 Excel」的端到端证明。

use cmx_rust_sheet::{Workbook, WorkbookExt};
use m99_demo::build_m99_workbook;
use sheet_core::worksheet::{ChartType, FloatingKind};

// ── 链 1：后端模型结构（对齐 spec.checks）─────────────────────

#[test]
fn six_sheets_present() {
    let wb = build_m99_workbook();
    assert_eq!(wb.sheet_count(), 6, "六个 sheet 齐全");
    let names: Vec<String> = wb.sheets().iter().map(|s| s.name().to_string()).collect();
    assert!(names[0].contains("迷你图"));
    assert!(names[1].contains("行·多级分组"));
    assert!(names[5].contains("图表集锦"));
}

#[test]
fn sheet1_has_six_sparklines_incl_column() {
    let wb = build_m99_workbook();
    let sp = &wb.sheets()[0];
    assert_eq!(sp.list_sparklines().len(), 6, "① 迷你图 6 格");
    let col = sp.get_sparkline(3, 1).expect("柱型迷你图在 (3,1)");
    assert_eq!(
        col.sparkline_type,
        sheet_core::worksheet::SparklineType::Column
    );
}

#[test]
fn sheet2_row_groups_three_levels_no_col_groups() {
    let wb = build_m99_workbook();
    let rg = &wb.sheets()[1];
    assert_eq!(rg.row_outlines.list().len(), 3, "② 行分组 3 组");
    assert_eq!(rg.row_outlines.max_level(), Some(1), "三级 maxLevel=1");
    assert!(rg.column_outlines.list().is_empty(), "② 无列分组");
    assert!(!rg.summary_below, "汇总在首 summaryBelow=false");
}

#[test]
fn sheet3_col_groups_three_levels_no_row_groups() {
    let wb = build_m99_workbook();
    let cg = &wb.sheets()[2];
    assert_eq!(cg.column_outlines.list().len(), 3, "③ 列分组 3 组");
    assert_eq!(cg.column_outlines.max_level(), Some(1));
    assert!(cg.row_outlines.list().is_empty(), "③ 无行分组");
    assert!(!cg.summary_right, "汇总在左 summaryRight=false");
}

#[test]
fn sheet4_dual_axis_both_three_levels() {
    let wb = build_m99_workbook();
    let dg = &wb.sheets()[3];
    assert_eq!(dg.row_outlines.max_level(), Some(1), "④ 行 maxLevel=1");
    assert_eq!(dg.column_outlines.max_level(), Some(1), "④ 列 maxLevel=1");
    assert_eq!(dg.row_outlines.list().len(), 3);
    assert_eq!(dg.column_outlines.list().len(), 3);
}

#[test]
fn sheet5_font_bg_border_rotation() {
    let wb = build_m99_workbook();
    let st = &wb.sheets()[4];
    // 字体 fontFamily（宋体 Serif 在 (3,0)）
    let font = st.get_style(3, 0).expect("字体样式");
    assert!(
        font.font_family.as_deref().unwrap_or("").contains("serif"),
        "fontFamily 生效"
    );
    // 背景+前景（红底白字在 (8,0)）
    let bg = st.get_style(8, 0).expect("背景样式");
    assert_eq!(bg.back_color.as_deref(), Some("#e15759"));
    assert_eq!(bg.fore_color.as_deref(), Some("#ffffff"));
    // 四边粗边框（(12,2)）
    let thick = st.get_style(12, 2).and_then(|s| s.borders).expect("边框");
    use sheet_core::style::BorderLineStyle;
    assert_eq!(thick.top.map(|e| e.style), Some(BorderLineStyle::Thick));
    assert_eq!(thick.right.map(|e| e.style), Some(BorderLineStyle::Thick));
    // 对角线（(13,3)）
    let diag = st
        .get_style(13, 3)
        .and_then(|s| s.borders)
        .expect("对角边框");
    assert!(diag.diagonal_down.is_some(), "对角线边框");
    // 文本旋转 45°（(5,3)）
    assert_eq!(st.get_style(5, 3).and_then(|s| s.text_rotation), Some(45.0));
}

#[test]
fn sheet6_eleven_charts_cover_eleven_types() {
    let wb = build_m99_workbook();
    let ch = &wb.sheets()[5];
    let charts: Vec<_> = ch
        .list_floating_objects()
        .into_iter()
        .filter(|o| matches!(o.kind, FloatingKind::Chart))
        .collect();
    assert_eq!(charts.len(), 11, "⑥ 图表 11 个");
    let kinds: std::collections::HashSet<ChartType> = charts
        .iter()
        .filter_map(|o| o.chart.as_ref().map(|c| c.chart_type))
        .collect();
    assert_eq!(kinds.len(), 11, "覆盖 11 类图型");
}

// ── 链 2：XLSX / JSON 往返（导出 Excel 的端到端证明）──────────

#[test]
fn xlsx_round_trip_preserves_multi_sheet_and_groups() {
    let wb = build_m99_workbook();
    let bytes = wb.to_xlsx();
    assert!(bytes.len() > 2000, "XLSX 非空");

    let back = Workbook::from_xlsx(&bytes);
    assert_eq!(back.sheet_count(), 6, "往返后仍 6 sheet");
    // 页签名保持
    assert!(back.sheets()[1].name().contains("行·多级分组"));
    // ② 行分组结构保持
    let rg = &back.sheets()[1];
    assert_eq!(rg.row_outlines.list().len(), 3, "行分组 3 组往返保持");
    assert_eq!(rg.row_outlines.max_level(), Some(1));
    assert!(!rg.summary_below, "summaryBelow 往返保持");
    // ③ 列分组结构保持
    let cg = &back.sheets()[2];
    assert_eq!(cg.column_outlines.list().len(), 3, "列分组 3 组往返保持");
    assert!(!cg.summary_right, "summaryRight 往返保持");
    // ④ 双轴分组保持
    let dg = &back.sheets()[3];
    assert_eq!(dg.row_outlines.max_level(), Some(1));
    assert_eq!(dg.column_outlines.max_level(), Some(1));
}

#[test]
fn xlsx_bytes_contain_outline_ooxml() {
    // 直接查 sheet2.xml 的 OOXML：outlineLevel + <outlinePr summaryBelow="0">。
    let wb = build_m99_workbook();
    let bytes = wb.to_xlsx();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid zip");
    use std::io::Read;
    let mut xml = String::new();
    zip.by_name("xl/worksheets/sheet2.xml")
        .expect("sheet2 exists")
        .read_to_string(&mut xml)
        .unwrap();
    assert!(xml.contains("summaryBelow=\"0\""), "含 outlinePr");
    assert!(xml.contains("outlineLevel=\"2\""), "含三级 outlineLevel");
}

#[test]
fn xlsx_bytes_contain_eleven_chart_parts() {
    // ⑥ 图表集锦 11 类 → xl/charts/chart1..11.xml + drawing + worksheet rels 全部落盘。
    let wb = build_m99_workbook();
    let bytes = wb.to_xlsx();
    let zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid zip");
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    let chart_parts = names
        .iter()
        .filter(|n| n.starts_with("xl/charts/chart") && n.ends_with(".xml"))
        .count();
    assert_eq!(chart_parts, 11, "11 个 chart 部件");
    assert!(
        names.iter().any(|n| n == "xl/drawings/drawing1.xml"),
        "有 drawing 部件"
    );
    assert!(
        names
            .iter()
            .any(|n| n == "xl/drawings/_rels/drawing1.xml.rels"),
        "drawing rels"
    );
    // ⑥ 是第 6 个 sheet（sheet6.xml），其 worksheet rels 指向 drawing。
    assert!(
        names
            .iter()
            .any(|n| n == "xl/worksheets/_rels/sheet6.xml.rels"),
        "sheet6 worksheet rels"
    );
}

#[test]
fn xlsx_bytes_contain_sparkline_extlst() {
    // ① 迷你图 6 格 → sheet1.xml 的 x14 <sparklineGroup>（7→3 型降级）。
    let wb = build_m99_workbook();
    let bytes = wb.to_xlsx();
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).expect("valid zip");
    use std::io::Read;
    let mut xml = String::new();
    zip.by_name("xl/worksheets/sheet1.xml")
        .expect("sheet1 exists")
        .read_to_string(&mut xml)
        .unwrap();
    assert!(xml.contains("x14:sparklineGroups"), "含 x14 迷你图组");
    assert_eq!(
        xml.matches("<x14:sparklineGroup ").count() + xml.matches("<x14:sparklineGroup>").count(),
        6,
        "6 组迷你图"
    );
    // 降级验证：winloss→stacked、column→column 都出现。
    assert!(xml.contains("type=\"stacked\""), "winloss→stacked");
    assert!(
        xml.contains("type=\"column\""),
        "column/bar/bullet/pie→column"
    );
}

#[test]
fn json_round_trip_preserves_charts_and_sparklines() {
    // 中性 JSON 快照比 XLSX 更全：图表 + 迷你图也往返（XLSX 暂不含 chart drawing）。
    let wb = build_m99_workbook();
    let json = wb.to_json_string(false);
    let back = Workbook::from_json(&json).expect("parse snapshot");
    assert_eq!(back.sheet_count(), 6);
    assert_eq!(back.sheets()[0].list_sparklines().len(), 6, "迷你图往返");
    let charts = back.sheets()[5]
        .list_floating_objects()
        .into_iter()
        .filter(|o| matches!(o.kind, FloatingKind::Chart))
        .count();
    assert_eq!(charts, 11, "11 图表随 JSON 往返");
}
