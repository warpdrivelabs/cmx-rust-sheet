//! m99_demo —— M99 多 sheet 综合场景的**后端构建器**。
//!
//! 对标 cmx-mega-sheet `demo/specs/spec-m99.js` 的 setup：用 cmx-rust-sheet 在**纯后端**
//! （无渲染、无 DOM）搭出六个工作表，覆盖迷你图 / 行多级分组 / 列多级分组 / 行列同时分组 /
//! 字体·背景·边框 / 图表 11 类，然后可整簿导出为 `.xlsx`（Excel 可打开）+ 中性 `.json`。
//!
//! [`build_m99_workbook`] 被 demo 主程序与集成测试共用，保证「演示所见 = 测试所验」。

use sheet_core::style::{BorderEdge, BorderLineStyle, Borders, HAlign, Style, StyleSheet, VAlign};
use sheet_core::worksheet::{
    ChartOptions, ChartSpec, ChartType, FloatingKind, FloatingObject, ObjAnchor, RegionRect,
    Sparkline, SparklineType, Worksheet,
};
use sheet_core::{CellValue, Workbook};

/// 便捷：细边框（四边同 style/color）。
fn edge(style: BorderLineStyle, color: &str) -> BorderEdge {
    BorderEdge {
        style,
        color: color.to_string(),
    }
}

/// 四边同款边框。
fn box_border(style: BorderLineStyle, color: &str) -> Borders {
    Borders {
        top: Some(edge(style, color)),
        bottom: Some(edge(style, color)),
        left: Some(edge(style, color)),
        right: Some(edge(style, color)),
        ..Default::default()
    }
}

/// 命名样式表（对齐 spec 的 wb.styleSheet.define）。
fn build_style_sheet() -> StyleSheet {
    let mut ss = StyleSheet::new();
    ss.define(
        "title",
        Style {
            bold: Some(true),
            font_size: Some(15.0),
            h_align: Some(HAlign::Center),
            ..Default::default()
        },
    );
    ss.define(
        "hdr",
        Style {
            bold: Some(true),
            back_color: Some("#cdd9f5".into()),
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
    ss.define(
        "total",
        Style {
            bold: Some(true),
            formatter: Some("#,##0.00".into()),
            h_align: Some(HAlign::Right),
            back_color: Some("#dce6fb".into()),
            ..Default::default()
        },
    );
    ss.define(
        "sub",
        Style {
            formatter: Some("#,##0.00".into()),
            h_align: Some(HAlign::Right),
            back_color: Some("#eef3fd".into()),
            ..Default::default()
        },
    );
    ss
}

/// 便捷：设值 + 命名样式。
fn put_styled(ws: &mut Worksheet, r: u32, c: u32, v: impl Into<CellValue>, style_name: &str) {
    ws.set_value(r, c, Some(v.into()));
    ws.set_style(
        r,
        c,
        Some(Style {
            style_name: Some(style_name.to_string()),
            ..Default::default()
        }),
    );
}

/// 便捷：设值 + 内联样式。
fn put(ws: &mut Worksheet, r: u32, c: u32, v: impl Into<CellValue>, style: Style) {
    ws.set_value(r, c, Some(v.into()));
    ws.set_style(r, c, Some(style));
}

/// 单格数据区（1 行 × n 列），迷你图取数用。
fn dr(row: u32, col: u32, n: u32) -> RegionRect {
    RegionRect::new(row, col, 1, n)
}

// ── ① 迷你图 ──────────────────────────────────────────────
fn sheet_sparkline(ss: &StyleSheet) -> Worksheet {
    let mut sp = Worksheet::with_size("① 迷你图", 24, 12);
    sp.style_sheet = ss.clone();
    sp.set_column_width(0, 90.0);
    sp.set_column_width(1, 150.0);
    put_styled(&mut sp, 0, 0, "趋势", "hdr");
    put_styled(&mut sp, 0, 1, "迷你图", "hdr");
    let trend = [3.0, 7.0, 4.0, 9.0, 6.0, 11.0, 8.0, 14.0];
    let winloss = [1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0, -1.0];
    for (i, v) in trend.iter().enumerate() {
        sp.set_value(0, 3 + i as u32, Some((*v).into()));
    }
    for (i, v) in winloss.iter().enumerate() {
        sp.set_value(1, 3 + i as u32, Some((*v).into()));
    }
    for (i, v) in [20.0, 45.0, 30.0, 55.0, 40.0].iter().enumerate() {
        sp.set_value(2, 3 + i as u32, Some((*v).into())); // column/pie 源
    }
    sp.set_value(3, 3, Some(72.0.into())); // bullet 实际值
    for (i, n) in ["折线", "柱", "输赢", "KPI", "填充", "微饼"]
        .iter()
        .enumerate()
    {
        sp.set_value(2 + i as u32, 0, Some((*n).into()));
    }
    for r in 2..=7 {
        sp.set_row_height(r, 26.0);
    }
    // 六格迷你图（B 列 = col 1）。
    sp.set_sparkline(
        2,
        1,
        Sparkline {
            sparkline_type: SparklineType::Line,
            data_range: dr(0, 3, 8),
            markers: Some(true),
            high_low: Some(true),
            ..spark_default()
        },
    );
    sp.set_sparkline(
        3,
        1,
        Sparkline {
            sparkline_type: SparklineType::Column,
            data_range: dr(2, 3, 5),
            ..spark_default()
        },
    );
    sp.set_sparkline(
        4,
        1,
        Sparkline {
            sparkline_type: SparklineType::Winloss,
            data_range: dr(1, 3, 8),
            negative_color: Some("#e15759".into()),
            ..spark_default()
        },
    );
    sp.set_sparkline(
        5,
        1,
        Sparkline {
            sparkline_type: SparklineType::Bullet,
            data_range: dr(3, 3, 1),
            target: Some(60.0),
            color: Some("#59a14f".into()),
            ..spark_default()
        },
    );
    sp.set_sparkline(
        6,
        1,
        Sparkline {
            sparkline_type: SparklineType::Area,
            data_range: dr(0, 3, 8),
            ..spark_default()
        },
    );
    sp.set_sparkline(
        7,
        1,
        Sparkline {
            sparkline_type: SparklineType::Pie,
            data_range: dr(2, 3, 5),
            ..spark_default()
        },
    );
    sp
}

fn spark_default() -> Sparkline {
    Sparkline {
        sparkline_type: SparklineType::Line,
        data_range: RegionRect::new(0, 0, 1, 1),
        color: None,
        negative_color: None,
        markers: None,
        high_low: None,
        first_last: None,
        target: None,
    }
}

// ── ② 行·多级分组（三级嵌套，汇总在首）──────────────────────
fn sheet_row_group(ss: &StyleSheet) -> Worksheet {
    let mut rg = Worksheet::with_size("② 行·多级分组", 30, 8);
    rg.style_sheet = ss.clone();
    rg.set_column_width(0, 200.0);
    rg.set_column_width(1, 130.0);
    rg.set_column_width(2, 130.0);
    put_styled(&mut rg, 0, 0, "区域 / 客户 · 应收明细", "title");
    rg.add_span(0, 0, 1, 3);
    rg.set_row_height(0, 30.0);
    put_styled(&mut rg, 1, 0, "项目", "hdr");
    put_styled(&mut rg, 1, 1, "期初", "hdr");
    put_styled(&mut rg, 1, 2, "期末", "hdr");
    // 行2 合计 / 行3 华东小计 / 行4-5 客户 / 行6 华南小计 / 行7-8 客户
    let rows: [(&str, &str, f64, f64); 7] = [
        ("合计", "total", 8300.0, 9200.0),
        ("华东 小计", "sub", 4300.0, 4800.0),
        ("上海A公司", "money", 2600.0, 2900.0),
        ("杭州B公司", "money", 1700.0, 1900.0),
        ("华南 小计", "sub", 4000.0, 4400.0),
        ("广州C公司", "money", 2100.0, 2300.0),
        ("深圳D公司", "money", 1900.0, 2100.0),
    ];
    for (i, (name, st, a, b)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        if *st == "money" {
            rg.set_value(r, 0, Some((*name).into()));
        } else {
            put(
                &mut rg,
                r,
                0,
                *name,
                Style {
                    bold: Some(true),
                    ..Default::default()
                },
            );
        }
        put_styled(&mut rg, r, 1, *a, st);
        put_styled(&mut rg, r, 2, *b, st);
    }
    rg.summary_below = false;
    rg.row_outlines.group(2, 7); // level 0：合计 2..8
    rg.row_outlines.group(3, 3); // level 1：华东 3..5
    rg.row_outlines.group(6, 3); // level 1：华南 6..8
    rg.apply_outline_visibility();
    rg
}

// ── ③ 列·多级分组（三级嵌套，汇总在左）──────────────────────
fn sheet_col_group(ss: &StyleSheet) -> Worksheet {
    let mut cg = Worksheet::with_size("③ 列·多级分组", 12, 16);
    cg.style_sheet = ss.clone();
    cg.set_column_width(0, 120.0);
    put_styled(&mut cg, 0, 0, "项目", "hdr");
    let col_hdr = ["全年合计", "上半年", "Q1", "Q2", "下半年", "Q3", "Q4"];
    for (i, h) in col_hdr.iter().enumerate() {
        cg.set_column_width(1 + i as u32, 96.0);
        put_styled(&mut cg, 0, 1 + i as u32, *h, "hdr");
    }
    let data_rows: [(&str, [f64; 7]); 3] = [
        (
            "营业收入",
            [3200.0, 1570.0, 750.0, 820.0, 1630.0, 800.0, 830.0],
        ),
        (
            "营业成本",
            [1980.0, 960.0, 460.0, 500.0, 1020.0, 510.0, 510.0],
        ),
        ("毛利", [1220.0, 610.0, 290.0, 320.0, 610.0, 290.0, 320.0]),
    ];
    for (i, (name, vals)) in data_rows.iter().enumerate() {
        let r = 1 + i as u32;
        put(
            &mut cg,
            r,
            0,
            *name,
            Style {
                bold: Some(true),
                ..Default::default()
            },
        );
        for (j, v) in vals.iter().enumerate() {
            let sn = if j == 0 {
                "total"
            } else if j == 1 || j == 4 {
                "sub"
            } else {
                "money"
            };
            put_styled(&mut cg, r, 1 + j as u32, *v, sn);
        }
    }
    cg.summary_right = false;
    cg.column_outlines.group(1, 7); // level 0：全年 1..7
    cg.column_outlines.group(2, 3); // level 1：上半年 2..4
    cg.column_outlines.group(5, 3); // level 1：下半年 5..7
    cg.apply_outline_visibility();
    cg
}

// ── ④ 行列·同时分组（双轴各三级）──────────────────────────
fn sheet_dual_group(ss: &StyleSheet) -> Worksheet {
    let mut dg = Worksheet::with_size("④ 行列·同时分组", 20, 16);
    dg.style_sheet = ss.clone();
    dg.set_column_width(0, 130.0);
    put_styled(&mut dg, 0, 0, "行列同时多级分组", "title");
    dg.add_span(0, 0, 1, 5);
    dg.set_row_height(0, 28.0);
    put_styled(&mut dg, 1, 0, "行 \\ 列", "hdr");
    let dc_hdr = ["合计", "A小计", "A-1", "A-2", "B小计", "B-1", "B-2"];
    for (i, h) in dc_hdr.iter().enumerate() {
        dg.set_column_width(1 + i as u32, 80.0);
        put_styled(&mut dg, 1, 1 + i as u32, *h, "hdr");
    }
    let dr_hdr = ["合计", "甲小计", "甲-1", "甲-2", "乙小计", "乙-1", "乙-2"];
    for (i, name) in dr_hdr.iter().enumerate() {
        let r = 2 + i as u32;
        put(
            &mut dg,
            r,
            0,
            *name,
            Style {
                bold: Some(true),
                ..Default::default()
            },
        );
        for c in 1..=7u32 {
            put_styled(&mut dg, r, c, ((i as u32 + 1) * 100 + c) as f64, "money");
        }
    }
    dg.summary_below = false;
    dg.summary_right = false;
    dg.row_outlines.group(2, 7);
    dg.row_outlines.group(3, 3);
    dg.row_outlines.group(6, 3);
    dg.column_outlines.group(1, 7);
    dg.column_outlines.group(2, 3);
    dg.column_outlines.group(5, 3);
    dg.apply_outline_visibility();
    dg
}

// ── ⑤ 字体 · 背景色 · 边框 ────────────────────────────────
fn sheet_style(ss: &StyleSheet) -> Worksheet {
    let mut st = Worksheet::with_size("⑤ 字体·背景·边框", 26, 10);
    st.style_sheet = ss.clone();
    for c in 0..4 {
        st.set_column_width(c, 150.0);
    }
    put_styled(&mut st, 0, 0, "字体 / 背景色 / 边框 样式集", "title");
    st.add_span(0, 0, 1, 4);
    st.set_row_height(0, 30.0);

    // 区块 A：字体
    put_styled(&mut st, 2, 0, "字体", "hdr");
    st.add_span(2, 0, 1, 4);
    let fonts: [(&str, &str); 4] = [
        ("宋体 Serif", "SimSun, serif"),
        ("黑体 Sans", "SimHei, sans-serif"),
        ("楷体 Kai", "KaiTi, serif"),
        ("等宽 Mono", "Consolas, monospace"),
    ];
    for (i, (label, family)) in fonts.iter().enumerate() {
        put(
            &mut st,
            3,
            i as u32,
            *label,
            Style {
                font_family: Some((*family).to_string()),
                font_size: Some(14.0),
                ..Default::default()
            },
        );
    }
    put(
        &mut st,
        4,
        0,
        "加粗 Bold",
        Style {
            bold: Some(true),
            font_size: Some(15.0),
            ..Default::default()
        },
    );
    put(
        &mut st,
        4,
        1,
        "斜体 Italic",
        Style {
            italic: Some(true),
            font_size: Some(15.0),
            ..Default::default()
        },
    );
    put(
        &mut st,
        4,
        2,
        "下划线",
        Style {
            underline: Some(true),
            font_size: Some(15.0),
            ..Default::default()
        },
    );
    put(
        &mut st,
        4,
        3,
        "删除线",
        Style {
            strikethrough: Some(true),
            font_size: Some(15.0),
            ..Default::default()
        },
    );
    put(
        &mut st,
        5,
        3,
        "旋转 45°",
        Style {
            text_rotation: Some(45.0),
            font_size: Some(13.0),
            ..Default::default()
        },
    );
    st.set_row_height(5, 40.0);

    // 区块 B：背景色 + 前景色
    put_styled(&mut st, 7, 0, "背景色 / 前景色", "hdr");
    st.add_span(7, 0, 1, 4);
    let swatches: [(&str, &str, &str); 8] = [
        ("红", "#ffffff", "#e15759"),
        ("橙", "#ffffff", "#f28e2b"),
        ("绿", "#ffffff", "#59a14f"),
        ("蓝", "#ffffff", "#4e79a7"),
        ("浅黄", "#7a5c00", "#fff3cd"),
        ("浅绿", "#0f5132", "#d1e7dd"),
        ("浅蓝", "#084298", "#cfe2ff"),
        ("浅灰", "#333333", "#e9ecef"),
    ];
    for (i, (label, fg, bg)) in swatches.iter().enumerate() {
        let r = 8 + (i / 4) as u32;
        let c = (i % 4) as u32;
        put(
            &mut st,
            r,
            c,
            *label,
            Style {
                back_color: Some((*bg).to_string()),
                fore_color: Some((*fg).to_string()),
                h_align: Some(HAlign::Center),
                bold: Some(true),
                ..Default::default()
            },
        );
    }
    st.set_row_height(8, 28.0);
    st.set_row_height(9, 28.0);

    // 区块 C：边框
    put_styled(&mut st, 11, 0, "边框", "hdr");
    st.add_span(11, 0, 1, 4);
    let bc = "#3b6cff";
    let center_box = |style: BorderLineStyle, color: &str| Style {
        h_align: Some(HAlign::Center),
        borders: Some(box_border(style, color)),
        ..Default::default()
    };
    put(
        &mut st,
        12,
        0,
        "细 thin",
        center_box(BorderLineStyle::Thin, bc),
    );
    put(
        &mut st,
        12,
        1,
        "中 medium",
        center_box(BorderLineStyle::Medium, bc),
    );
    put(
        &mut st,
        12,
        2,
        "粗 thick",
        center_box(BorderLineStyle::Thick, bc),
    );
    put(
        &mut st,
        12,
        3,
        "双线 double",
        center_box(BorderLineStyle::Double, bc),
    );
    put(
        &mut st,
        13,
        0,
        "虚线 dashed",
        center_box(BorderLineStyle::Dashed, "#e15759"),
    );
    put(
        &mut st,
        13,
        1,
        "点线 dotted",
        center_box(BorderLineStyle::Dotted, "#e15759"),
    );
    put(
        &mut st,
        13,
        2,
        "仅下边框",
        Style {
            h_align: Some(HAlign::Center),
            borders: Some(Borders {
                bottom: Some(edge(BorderLineStyle::Thick, "#59a14f")),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    put(
        &mut st,
        13,
        3,
        "对角线",
        Style {
            h_align: Some(HAlign::Center),
            borders: Some(Borders {
                diagonal_down: Some(edge(BorderLineStyle::Thin, "#333333")),
                diagonal_up: Some(edge(BorderLineStyle::Thin, "#333333")),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    st.set_row_height(12, 32.0);
    st.set_row_height(13, 32.0);
    // 靠右对齐一格演示 vAlign（补 VAlign 用到，避免 unused import 抱怨）。
    st.set_style(
        13,
        2,
        Some(Style {
            h_align: Some(HAlign::Center),
            v_align: Some(VAlign::Middle),
            borders: Some(Borders {
                bottom: Some(edge(BorderLineStyle::Thick, "#59a14f")),
                ..Default::default()
            }),
            ..Default::default()
        }),
    );
    st
}

// ── ⑥ 图表集锦（11 类）────────────────────────────────────
fn sheet_charts(ss: &StyleSheet) -> Worksheet {
    let mut ch = Worksheet::with_size("⑥ 图表集锦", 46, 16);
    ch.style_sheet = ss.clone();
    put_styled(&mut ch, 0, 0, "图表集锦 · 11 类", "title");
    ch.add_span(0, 0, 1, 4);
    // 通用类别 + 两系列数据 A2:C6
    let cgrid: [(&str, f64, f64); 5] = [
        ("月", 0.0, 0.0), // 表头行（"销售"/"成本" 见下）
        ("1月", 30.0, 18.0),
        ("2月", 45.0, 25.0),
        ("3月", 38.0, 22.0),
        ("4月", 52.0, 30.0),
    ];
    // 表头行文本
    ch.set_value(1, 0, Some("月".into()));
    ch.set_value(1, 1, Some("销售".into()));
    ch.set_value(1, 2, Some("成本".into()));
    for (r, (label, a, b)) in cgrid.iter().enumerate().skip(1) {
        ch.set_value(1 + r as u32, 0, Some((*label).into()));
        ch.set_value(1 + r as u32, 1, Some((*a).into()));
        ch.set_value(1 + r as u32, 2, Some((*b).into()));
    }
    // OHLC 数据 E2:H5（stock）
    let ohlc: [[f64; 4]; 4] = [
        [0.0, 0.0, 0.0, 0.0], // 表头（O/H/L/C 见下）
        [10.0, 14.0, 9.0, 12.0],
        [12.0, 15.0, 10.0, 11.0],
        [11.0, 13.0, 10.0, 13.0],
    ];
    for (c, h) in ["O", "H", "L", "C"].iter().enumerate() {
        ch.set_value(1, 4 + c as u32, Some((*h).into()));
    }
    for (r, row) in ohlc.iter().enumerate().skip(1) {
        for (c, v) in row.iter().enumerate() {
            ch.set_value(1 + r as u32, 4 + c as u32, Some((*v).into()));
        }
    }

    let cdr = RegionRect::new(1, 0, 5, 3); // A2:C6（含表头行/列）
    let at = |r1, c1, r2, c2| ObjAnchor {
        from_row: r1,
        from_col: c1,
        to_row: r2,
        to_col: c2,
        from_dx: None,
        from_dy: None,
        to_dx: None,
        to_dy: None,
    };
    // 11 类图表，各锚不同格区。
    let specs: [(
        &str,
        ChartType,
        ObjAnchor,
        RegionRect,
        bool,
        bool,
        ChartOptions,
    ); 11] = [
        (
            "柱状 column",
            ChartType::Column,
            at(7, 0, 14, 3),
            cdr,
            true,
            true,
            opt_legend(),
        ),
        (
            "条形 bar",
            ChartType::Bar,
            at(7, 4, 14, 7),
            cdr,
            true,
            true,
            opt_legend(),
        ),
        (
            "折线 line",
            ChartType::Line,
            at(7, 8, 14, 11),
            cdr,
            true,
            true,
            opt_legend_labels(),
        ),
        (
            "面积 area",
            ChartType::Area,
            at(7, 12, 14, 15),
            cdr,
            true,
            true,
            opt_legend(),
        ),
        (
            "饼图 pie",
            ChartType::Pie,
            at(15, 0, 22, 3),
            cdr,
            true,
            true,
            opt_legend(),
        ),
        (
            "甜甜圈 doughnut",
            ChartType::Doughnut,
            at(15, 4, 22, 7),
            cdr,
            true,
            true,
            opt_legend(),
        ),
        (
            "散点+趋势 scatter",
            ChartType::Scatter,
            at(15, 8, 22, 11),
            cdr,
            true,
            false,
            opt_trend(),
        ),
        (
            "气泡 bubble",
            ChartType::Bubble,
            at(15, 12, 22, 15),
            cdr,
            true,
            false,
            ChartOptions::default(),
        ),
        (
            "雷达 radar",
            ChartType::Radar,
            at(23, 0, 30, 3),
            cdr,
            true,
            true,
            opt_legend(),
        ),
        (
            "K线 stock",
            ChartType::Stock,
            at(23, 4, 30, 7),
            RegionRect::new(1, 4, 4, 4),
            true,
            false,
            ChartOptions::default(),
        ),
        (
            "组合双轴 combo",
            ChartType::Combo,
            at(23, 8, 30, 11),
            cdr,
            true,
            true,
            opt_combo(),
        ),
    ];
    for (i, (title, kind, anchor, data_range, row_hdr, col_hdr, options)) in
        specs.into_iter().enumerate()
    {
        ch.add_floating_object(FloatingObject {
            id: format!("chart-{i}"),
            kind: FloatingKind::Chart,
            anchor,
            src: None,
            chart: Some(ChartSpec {
                chart_type: kind,
                data_range,
                title: Some(title.to_string()),
                first_row_header: Some(row_hdr),
                first_col_header: Some(col_hdr),
                options: Some(options),
            }),
            shape: None,
            z: Some(i as f64),
        });
    }
    ch
}

fn opt_legend() -> ChartOptions {
    ChartOptions {
        legend: Some(true),
        ..Default::default()
    }
}
fn opt_legend_labels() -> ChartOptions {
    ChartOptions {
        legend: Some(true),
        data_labels: Some(true),
        ..Default::default()
    }
}
fn opt_trend() -> ChartOptions {
    ChartOptions {
        trendline: Some("linear".into()),
        ..Default::default()
    }
}
fn opt_combo() -> ChartOptions {
    ChartOptions {
        series_types: Some(vec!["column".into(), "line".into()]),
        secondary_axis: Some(vec![1]),
        data_labels: Some(true),
        ..Default::default()
    }
}

/// 搭出 M99 六 sheet 工作簿（对齐 spec-m99.js 的 setup，纯后端无渲染）。
pub fn build_m99_workbook() -> Workbook {
    let ss = build_style_sheet();
    let mut wb = Workbook::empty();
    wb.append_sheet(sheet_sparkline(&ss));
    wb.append_sheet(sheet_row_group(&ss));
    wb.append_sheet(sheet_col_group(&ss));
    wb.append_sheet(sheet_dual_group(&ss));
    wb.append_sheet(sheet_style(&ss));
    wb.append_sheet(sheet_charts(&ss));
    wb.set_active_sheet_index(0);
    wb
}
