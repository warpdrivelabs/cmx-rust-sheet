//! xlsx_drawing —— XLSX 的图表 / 迷你图 OOXML 生成（反超 cmx-megasheet：TS 版不导出这两者）。
//!
//! 三块产物，接进 [`crate::xlsx::snapshot_to_xlsx`]：
//!  1. **图表** `xl/charts/chartN.xml`（DrawingML chart）+ `xl/drawings/drawingN.xml`
//!     （twoCellAnchor 双格锚）+ 各自 rels + worksheet `<drawing>` 引用。11 类图型映射到
//!     Excel 原生 barChart/lineChart/pieChart/scatterChart/radarChart/stockChart（bubble→scatter、
//!     combo→bar+line、doughnut→pie+holeSize）。
//!  2. **迷你图** worksheet `<extLst>` 里的 x14 `<x14:sparklineGroups>`。Excel 原生只 3 型
//!     （line/column/stacked=winloss），我们 7 型里 area/bar/pie/bullet **降级到最接近原生型**
//!     （area→line、bar/bullet→column、pie→column）；原始类型仍在中性 JSON 快照里不失真。
//!
//! 坐标/引用一律用**绝对 A1**（`'Sheet 名'!$A$1:$C$3`），sheet 名含空格/中文/标点时加单引号并转义。

use sheet_core::address::{col_to_label, format_addr};
use sheet_core::worksheet::{ChartSpec, ChartType, FloatingKind, RegionRect, SparklineType};

use crate::snapshot::{SheetSnapshot, SparklineEntry};

/// 转义 XML 文本（& < > " 用于属性/正文）。
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// sheet 名 → 公式里的引用前缀。含非字母数字/下划线或以数字开头 → 加单引号并转义内部单引号。
fn quote_sheet_name(name: &str) -> String {
    let needs_quote = name.is_empty()
        || name.chars().next().is_some_and(|c| c.is_ascii_digit())
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '\u{4e00}');
    // 更稳妥：只要不是纯 ASCII 字母数字下划线即加引号（中文名一律加）。
    let safe = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && name.chars().next().is_some_and(|c| !c.is_ascii_digit());
    if safe && !needs_quote {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

/// 区域 → 绝对 A1 引用（不含 sheet 前缀），如 `$A$2:$C$6`；单格则 `$A$2`。
fn region_abs_a1(r: &RegionRect) -> String {
    let a = abs_addr(r.row, r.col);
    if r.row_count <= 1 && r.col_count <= 1 {
        return a;
    }
    let b = abs_addr(r.row + r.row_count - 1, r.col + r.col_count - 1);
    format!("{a}:{b}")
}

/// 绝对单格地址 `$A$1`。
fn abs_addr(row: u32, col: u32) -> String {
    format!("${}${}", col_to_label(col), row + 1)
}

/// 带 sheet 前缀的绝对区域引用：`'Sheet'!$A$2:$C$6`。
fn sheet_region_ref(sheet: &str, r: &RegionRect) -> String {
    format!("{}!{}", quote_sheet_name(sheet), region_abs_a1(r))
}

/// 带 sheet 前缀的绝对单格引用：`'Sheet'!$B$1`。
fn sheet_cell_ref(sheet: &str, row: u32, col: u32) -> String {
    format!("{}!{}", quote_sheet_name(sheet), abs_addr(row, col))
}

// ── 图表数据系列拆解（对齐 sheet_core::chart 的取数，但产出 A1 引用而非取值）──

/// 一条系列的引用集（名/类别/值都用 sheet 内绝对引用）。
struct SeriesRef {
    name_ref: String,
    name_lit: String,
    cat_ref: String,
    val_ref: String,
}

/// 从 ChartSpec 的 dataRange + firstRow/ColHeader 拆出「类别列 + 各值列」的引用。
/// 与渲染层 buildChartData 同构，但这里要的是 A1 引用（图表用引用而非快照值，Excel 里可联动）。
fn series_refs(sheet: &str, spec: &ChartSpec, snap: &SheetSnapshot) -> (String, Vec<SeriesRef>) {
    let g = &spec.data_range;
    let row_hdr = spec.first_row_header.unwrap_or(true);
    let col_hdr = spec.first_col_header.unwrap_or(true);
    let data_r0 = g.row + if row_hdr { 1 } else { 0 };
    let data_c0 = g.col + if col_hdr { 1 } else { 0 };
    let last_row = g.row + g.row_count - 1;

    // 类别 = 首列去表头行。
    let cat_region = RegionRect::new(data_r0, g.col, g.row_count - if row_hdr { 1 } else { 0 }, 1);
    let cat_ref = sheet_region_ref(sheet, &cat_region);

    let mut series = Vec::new();
    for c in data_c0..g.col + g.col_count {
        let name_ref = sheet_cell_ref(sheet, g.row, c);
        let name_lit = if row_hdr {
            cell_text(snap, g.row, c).unwrap_or_else(|| format!("列{c}"))
        } else {
            format!("列{c}")
        };
        let val_region = RegionRect::new(data_r0, c, (last_row - data_r0) + 1, 1);
        series.push(SeriesRef {
            name_ref,
            name_lit,
            cat_ref: cat_ref.clone(),
            val_ref: sheet_region_ref(sheet, &val_region),
        });
    }
    (cat_ref, series)
}

/// 取某格的文本（系列名字面量用；找不到→None）。
fn cell_text(snap: &SheetSnapshot, row: u32, col: u32) -> Option<String> {
    snap.cells
        .iter()
        .find(|c| c.r == row && c.c == col)
        .and_then(|c| c.v.as_ref())
        .map(|v| match v {
            sheet_core::cell::CellValue::Text(t) => t.clone(),
            sheet_core::cell::CellValue::Number(n) => sheet_core::numstr::num_to_string(*n),
            sheet_core::cell::CellValue::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        })
}

/// 一条 `<c:ser>`（含名/类别/值引用；numRef 缓存值可省，Excel 会自算）。
fn ser_xml(idx: usize, s: &SeriesRef, cat_tag: &str) -> String {
    format!(
        "<c:ser><c:idx val=\"{idx}\"/><c:order val=\"{idx}\"/>\
<c:tx><c:strRef><c:f>{name_ref}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{name_lit}</c:v></c:pt></c:strCache></c:strRef></c:tx>\
<c:{cat_tag}><c:numRef><c:f>{cat_ref}</c:f></c:numRef></c:{cat_tag}>\
<c:val><c:numRef><c:f>{val_ref}</c:f></c:numRef></c:val></c:ser>",
        name_ref = esc(&s.name_ref),
        name_lit = esc(&s.name_lit),
        cat_ref = esc(&s.cat_ref),
        val_ref = esc(&s.val_ref),
    )
}

/// 生成 `xl/charts/chartN.xml`（chartSpace）。11 类映射到原生图型。
pub fn chart_xml(sheet: &str, spec: &ChartSpec, snap: &SheetSnapshot) -> String {
    let (_cat, series) = series_refs(sheet, spec, snap);
    let legend = spec
        .options
        .as_ref()
        .and_then(|o| o.legend)
        .unwrap_or(false);
    let title = spec.title.as_deref().unwrap_or("");
    let title_xml = if title.is_empty() {
        "<c:autoTitleDeleted val=\"1\"/>".to_string()
    } else {
        format!(
            "<c:title><c:tx><c:rich><a:bodyPr/><a:p><a:r><a:t>{}</a:t></a:r></a:p></c:rich></c:tx><c:overlay val=\"0\"/></c:title><c:autoTitleDeleted val=\"0\"/>",
            esc(title)
        )
    };
    let legend_xml = if legend {
        "<c:legend><c:legendPos val=\"r\"/><c:overlay val=\"0\"/></c:legend>"
    } else {
        ""
    };
    let plot = plot_area_xml(spec, &series);
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<c:chartSpace xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<c:chart>{title_xml}<c:plotArea><c:layout/>{plot}</c:plotArea>{legend_xml}<c:plotVisOnly val=\"1\"/><c:dispBlanksAs val=\"gap\"/></c:chart></c:chartSpace>"
    )
}

/// 主图区：按类型出对应 *Chart 元素 + 轴。
fn plot_area_xml(spec: &ChartSpec, series: &[SeriesRef]) -> String {
    let ax_cat = 111111111u64;
    let ax_val = 222222222u64;
    let axes = format!(
        "<c:catAx><c:axId val=\"{ax_cat}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"{ax_val}\"/></c:catAx>\
<c:valAx><c:axId val=\"{ax_val}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:crossAx val=\"{ax_cat}\"/></c:valAx>"
    );
    let sers = |cat_tag: &str| -> String {
        series
            .iter()
            .enumerate()
            .map(|(i, s)| ser_xml(i, s, cat_tag))
            .collect::<String>()
    };
    match spec.chart_type {
        ChartType::Bar => format!(
            "<c:barChart><c:barDir val=\"bar\"/><c:grouping val=\"clustered\"/>{}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:barChart>{axes}",
            sers("cat")
        ),
        ChartType::Column => format!(
            "<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/>{}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:barChart>{axes}",
            sers("cat")
        ),
        ChartType::Line => format!(
            "<c:lineChart><c:grouping val=\"standard\"/>{}<c:marker val=\"1\"/><c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:lineChart>{axes}",
            sers("cat")
        ),
        ChartType::Area => format!(
            "<c:areaChart><c:grouping val=\"standard\"/>{}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:areaChart>{axes}",
            sers("cat")
        ),
        ChartType::Pie => format!(
            "<c:pieChart><c:varyColors val=\"1\"/>{}</c:pieChart>",
            sers("cat")
        ),
        ChartType::Doughnut => format!(
            "<c:doughnutChart><c:varyColors val=\"1\"/>{}<c:holeSize val=\"50\"/></c:doughnutChart>",
            sers("cat")
        ),
        ChartType::Scatter | ChartType::Bubble => {
            // 散点 / 气泡：Excel 用 scatterChart（气泡简化为散点，避免 bubbleChart 的三序列强约束）。
            let scatter_sers = series
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    format!(
                        "<c:ser><c:idx val=\"{i}\"/><c:order val=\"{i}\"/>\
<c:tx><c:strRef><c:f>{name_ref}</c:f><c:strCache><c:ptCount val=\"1\"/><c:pt idx=\"0\"><c:v>{name_lit}</c:v></c:pt></c:strCache></c:strRef></c:tx>\
<c:xVal><c:numRef><c:f>{cat_ref}</c:f></c:numRef></c:xVal>\
<c:yVal><c:numRef><c:f>{val_ref}</c:f></c:numRef></c:yVal></c:ser>",
                        name_ref = esc(&s.name_ref),
                        name_lit = esc(&s.name_lit),
                        cat_ref = esc(&s.cat_ref),
                        val_ref = esc(&s.val_ref),
                    )
                })
                .collect::<String>();
            format!(
                "<c:scatterChart><c:scatterStyle val=\"lineMarker\"/>{scatter_sers}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:scatterChart>\
<c:valAx><c:axId val=\"{ax_cat}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"b\"/><c:crossAx val=\"{ax_val}\"/></c:valAx>\
<c:valAx><c:axId val=\"{ax_val}\"/><c:scaling><c:orientation val=\"minMax\"/></c:scaling><c:delete val=\"0\"/><c:axPos val=\"l\"/><c:crossAx val=\"{ax_cat}\"/></c:valAx>"
            )
        }
        ChartType::Radar => format!(
            "<c:radarChart><c:radarStyle val=\"marker\"/>{}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:radarChart>{axes}",
            sers("cat")
        ),
        ChartType::Stock => {
            // K 线：用 stockChart（各系列 = O/H/L/C 折线），退化为多条 lineChart 语义最稳。
            format!(
                "<c:stockChart>{}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:stockChart>{axes}",
                sers("cat")
            )
        }
        ChartType::Combo => {
            // 组合：首系列柱 + 其余线（对齐 spec seriesTypes: [column, line]）。
            let (first, rest) = series.split_first().map(|(f, r)| (Some(f), r)).unwrap_or((None, &[]));
            let bar_part = first
                .map(|s| {
                    format!(
                        "<c:barChart><c:barDir val=\"col\"/><c:grouping val=\"clustered\"/>{}<c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:barChart>",
                        ser_xml(0, s, "cat")
                    )
                })
                .unwrap_or_default();
            let line_part = if rest.is_empty() {
                String::new()
            } else {
                let line_sers = rest
                    .iter()
                    .enumerate()
                    .map(|(i, s)| ser_xml(i + 1, s, "cat"))
                    .collect::<String>();
                format!(
                    "<c:lineChart><c:grouping val=\"standard\"/>{line_sers}<c:marker val=\"1\"/><c:axId val=\"{ax_cat}\"/><c:axId val=\"{ax_val}\"/></c:lineChart>"
                )
            };
            format!("{bar_part}{line_part}{axes}")
        }
    }
}

/// 生成 `xl/drawings/drawingN.xml`：把本 sheet 的图表按双格锚放好。
/// `anchors` = (chart 局部 rId 从1起, from_row, from_col, to_row, to_col)。
/// 用 twoCellAnchor（图表随格缩放），故不需绝对 EMU 尺寸。
pub fn drawing_xml(anchors: &[(usize, u32, u32, u32, u32)]) -> String {
    let mut body = String::new();
    for (rid_i, from_row, from_col, to_row, to_col) in anchors {
        body.push_str(&format!(
            "<xdr:twoCellAnchor><xdr:from><xdr:col>{fc}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{fr}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
<xdr:to><xdr:col>{tc}</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>{tr}</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>\
<xdr:graphicFrame macro=\"\"><xdr:nvGraphicFramePr><xdr:cNvPr id=\"{id}\" name=\"Chart {id}\"/><xdr:cNvGraphicFramePr/></xdr:nvGraphicFramePr>\
<xdr:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></xdr:xfrm>\
<a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/chart\"><c:chart xmlns:c=\"http://schemas.openxmlformats.org/drawingml/2006/chart\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"rId{rid}\"/></a:graphicData></a:graphic></xdr:graphicFrame><xdr:clientData/></xdr:twoCellAnchor>",
            fc = from_col, fr = from_row, tc = to_col, tr = to_row, id = rid_i, rid = rid_i,
        ));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<xdr:wsDr xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\">{body}</xdr:wsDr>"
    )
}

/// 生成 `xl/drawings/_rels/drawingN.xml.rels`：drawing → 各 chart。
pub fn drawing_rels_xml(chart_count: usize) -> String {
    let rels = (1..=chart_count)
        .map(|i| {
            format!(
                "<Relationship Id=\"rId{i}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/chart\" Target=\"../charts/chart{i}.xml\"/>"
            )
        })
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{rels}</Relationships>"
    )
}

// ── 迷你图（sparklines）：worksheet <extLst> 里的 x14 group ──

/// 7 型 → Excel 原生 3 型（line/column/stacked）。area→line, bar/bullet→column, pie→column。
fn native_sparkline_type(t: SparklineType) -> &'static str {
    match t {
        SparklineType::Line | SparklineType::Area => "line",
        SparklineType::Column | SparklineType::Bar | SparklineType::Bullet | SparklineType::Pie => {
            "column"
        }
        SparklineType::Winloss => "stacked",
    }
}

/// 单条迷你图的 `<x14:sparklineGroup>`（一组一格，简单直接）。
fn sparkline_group_xml(sheet: &str, entry: &SparklineEntry) -> String {
    let SparklineEntry(row, col, spec) = entry;
    let native = native_sparkline_type(spec.sparkline_type);
    let type_attr = if native == "line" {
        String::new() // line 是默认，不写 type
    } else {
        format!(" type=\"{native}\"")
    };
    let markers = if spec.markers.unwrap_or(false) {
        " markers=\"1\""
    } else {
        ""
    };
    let data_ref = sheet_region_ref(sheet, &spec.data_range);
    let loc_ref = format_addr(*row, *col);
    format!(
        "<x14:sparklineGroup{type_attr}{markers} displayEmptyCellsAs=\"gap\">\
<x14:sparklines><x14:sparkline><xm:f>{data}</xm:f><xm:sqref>{loc}</xm:sqref></x14:sparkline></x14:sparklines></x14:sparklineGroup>",
        data = esc(&data_ref),
        loc = esc(&loc_ref),
    )
}

/// 整 sheet 的迷你图 `<extLst>`（无迷你图返回空串）。放在 worksheet 元素末尾。
pub fn sparkline_ext_lst(sheet: &str, snap: &SheetSnapshot) -> String {
    if snap.sparklines.is_empty() {
        return String::new();
    }
    let groups = snap
        .sparklines
        .iter()
        .map(|e| sparkline_group_xml(sheet, e))
        .collect::<String>();
    format!(
        "<extLst><ext xmlns:x14=\"http://schemas.microsoft.com/office/spreadsheetml/2009/9/main\" uri=\"{{05C60535-1F16-4fd2-B633-F4F36F0B64E0}}\">\
<x14:sparklineGroups xmlns:xm=\"http://schemas.microsoft.com/office/excel/2006/main\">{groups}</x14:sparklineGroups></ext></extLst>"
    )
}

/// 本 sheet 是否有图表浮动对象（决定是否要 drawing 部件）。
pub fn sheet_charts(snap: &SheetSnapshot) -> Vec<&ChartSpec> {
    snap.floating_objects
        .iter()
        .filter(|o| matches!(o.kind, FloatingKind::Chart))
        .filter_map(|o| o.chart.as_ref())
        .collect()
}

/// 图表锚点（from/to 行列）——从 FloatingObject 的 ObjAnchor 取。
pub fn chart_anchors(snap: &SheetSnapshot) -> Vec<(u32, u32, u32, u32)> {
    snap.floating_objects
        .iter()
        .filter(|o| matches!(o.kind, FloatingKind::Chart))
        .filter(|o| o.chart.is_some())
        .map(|o| {
            let a = &o.anchor;
            (a.from_row, a.from_col, a.to_row, a.to_col)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sheet_core::worksheet::Sparkline;

    #[test]
    fn quote_sheet_name_rules() {
        assert_eq!(quote_sheet_name("Sheet1"), "Sheet1");
        assert_eq!(quote_sheet_name("data_2"), "data_2");
        // 中文名 / 含空格 / 含标点 → 加引号
        assert_eq!(quote_sheet_name("① 迷你图"), "'① 迷你图'");
        assert_eq!(quote_sheet_name("My Sheet"), "'My Sheet'");
        assert_eq!(quote_sheet_name("a'b"), "'a''b'"); // 内部单引号翻倍
        assert_eq!(quote_sheet_name("2024"), "'2024'"); // 数字开头
    }

    #[test]
    fn abs_a1_refs() {
        assert_eq!(abs_addr(0, 0), "$A$1");
        assert_eq!(abs_addr(4, 2), "$C$5");
        // 单格区域 → 单地址
        assert_eq!(region_abs_a1(&RegionRect::new(0, 1, 1, 1)), "$B$1");
        // 多格区域 → 区间
        assert_eq!(region_abs_a1(&RegionRect::new(1, 0, 5, 3)), "$A$2:$C$6");
    }

    #[test]
    fn native_sparkline_degradation() {
        // 7 型 → 3 原生型
        assert_eq!(native_sparkline_type(SparklineType::Line), "line");
        assert_eq!(native_sparkline_type(SparklineType::Area), "line"); // 降级
        assert_eq!(native_sparkline_type(SparklineType::Column), "column");
        assert_eq!(native_sparkline_type(SparklineType::Bar), "column"); // 降级
        assert_eq!(native_sparkline_type(SparklineType::Bullet), "column"); // 降级
        assert_eq!(native_sparkline_type(SparklineType::Pie), "column"); // 降级
        assert_eq!(native_sparkline_type(SparklineType::Winloss), "stacked");
    }

    fn spark(t: SparklineType, dr: RegionRect) -> Sparkline {
        Sparkline {
            sparkline_type: t,
            data_range: dr,
            color: None,
            negative_color: None,
            markers: None,
            high_low: None,
            first_last: None,
            target: None,
        }
    }

    #[test]
    fn sparkline_ext_lst_empty_when_none() {
        let snap = SheetSnapshot {
            name: "S".into(),
            ..Default::default()
        };
        assert_eq!(sparkline_ext_lst("S", &snap), "");
    }

    #[test]
    fn sparkline_ext_lst_emits_x14_group() {
        let mut snap = SheetSnapshot {
            name: "① 迷你图".into(),
            ..Default::default()
        };
        snap.sparklines.push(SparklineEntry(
            2,
            1,
            spark(SparklineType::Column, RegionRect::new(0, 3, 1, 8)),
        ));
        let xml = sparkline_ext_lst("① 迷你图", &snap);
        assert!(xml.contains("x14:sparklineGroups"));
        assert!(xml.contains("type=\"column\""));
        // 数据引用带引号 sheet 名 + 绝对 A1；定位格 B3。
        assert!(xml.contains("'① 迷你图'!$D$1:$K$1"), "data ref: {xml}");
        assert!(xml.contains("<xm:sqref>B3</xm:sqref>"));
    }

    fn chart_spec(kind: ChartType) -> ChartSpec {
        ChartSpec {
            chart_type: kind,
            data_range: RegionRect::new(1, 0, 5, 3),
            title: Some("T".into()),
            first_row_header: Some(true),
            first_col_header: Some(true),
            options: None,
        }
    }

    fn snap_with_grid() -> SheetSnapshot {
        use crate::snapshot::CellSnapshot;
        use sheet_core::cell::CellValue;
        let mut snap = SheetSnapshot {
            name: "Data".into(),
            ..Default::default()
        };
        // data_range = (1,0,5,3) = A2:C6；firstRowHeader → 表头行 = 0-idx row 1 (=A1 行2)。
        // 表头 B2/C2 + 类别 A3..A6（数据行）。
        for (r, c, v) in [
            (1u32, 1u32, "销售"),
            (1, 2, "成本"),
            (2, 0, "1月"),
            (3, 0, "2月"),
        ] {
            snap.cells.push(CellSnapshot {
                r,
                c,
                v: Some(CellValue::Text(v.into())),
                f: None,
                s: None,
                rich: None,
            });
        }
        snap
    }

    #[test]
    fn chart_xml_column_has_barchart_and_refs() {
        let snap = snap_with_grid();
        let xml = chart_xml("Data", &chart_spec(ChartType::Column), &snap);
        assert!(xml.contains("<c:barChart>"));
        assert!(xml.contains("<c:barDir val=\"col\"/>"));
        // 两个值系列（B、C 列）
        assert_eq!(xml.matches("<c:ser>").count(), 2);
        // 值引用绝对 + sheet 前缀（数据行 = A1 行 3..6）
        assert!(xml.contains("Data!$B$3:$B$6"), "val ref: {xml}");
        assert!(xml.contains("Data!$C$3:$C$6"));
        // 类别引用（首列去表头 = A3:A6）
        assert!(xml.contains("Data!$A$3:$A$6"));
        // 系列名字面量取自表头行（B2=销售）
        assert!(xml.contains("<c:v>销售</c:v>"));
    }

    #[test]
    fn chart_xml_type_mapping() {
        let snap = snap_with_grid();
        let has =
            |k: ChartType, needle: &str| chart_xml("Data", &chart_spec(k), &snap).contains(needle);
        assert!(has(ChartType::Bar, "<c:barDir val=\"bar\"/>"));
        assert!(has(ChartType::Line, "<c:lineChart>"));
        assert!(has(ChartType::Area, "<c:areaChart>"));
        assert!(has(ChartType::Pie, "<c:pieChart>"));
        assert!(has(ChartType::Doughnut, "<c:doughnutChart>"));
        assert!(has(ChartType::Doughnut, "<c:holeSize val=\"50\"/>"));
        assert!(has(ChartType::Scatter, "<c:scatterChart>"));
        assert!(has(ChartType::Bubble, "<c:scatterChart>")); // bubble→scatter
        assert!(has(ChartType::Radar, "<c:radarChart>"));
        assert!(has(ChartType::Stock, "<c:stockChart>"));
        // combo = barChart + lineChart
        let combo = chart_xml("Data", &chart_spec(ChartType::Combo), &snap);
        assert!(combo.contains("<c:barChart>") && combo.contains("<c:lineChart>"));
    }

    #[test]
    fn drawing_xml_two_cell_anchor() {
        let xml = drawing_xml(&[(1, 7, 0, 14, 3)]);
        assert!(xml.contains("<xdr:twoCellAnchor>"));
        assert!(xml.contains("<xdr:col>0</xdr:col>")); // from_col
        assert!(xml.contains("<xdr:row>7</xdr:row>")); // from_row
        assert!(xml.contains("<xdr:col>3</xdr:col>")); // to_col
        assert!(xml.contains("r:id=\"rId1\""));
    }

    #[test]
    fn drawing_rels_targets_charts() {
        let xml = drawing_rels_xml(3);
        assert_eq!(xml.matches("relationships/chart").count(), 3);
        assert!(xml.contains("Target=\"../charts/chart1.xml\""));
        assert!(xml.contains("Target=\"../charts/chart3.xml\""));
    }

    #[test]
    fn title_escaping() {
        let snap = snap_with_grid();
        let mut spec = chart_spec(ChartType::Column);
        spec.title = Some("A & B <x>".into());
        let xml = chart_xml("Data", &spec, &snap);
        assert!(xml.contains("A &amp; B &lt;x&gt;"));
    }
}
