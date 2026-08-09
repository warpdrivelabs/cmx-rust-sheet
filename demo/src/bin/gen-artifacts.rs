//! 图文并茂用：构建一张真实感的季度损益表 → 导出 HTML / PDF / XLSX / JSON。
//! 产物落在 branding-artifacts/，供功能简介文档配图。
//! 运行：cargo run -p m99-demo --bin gen-artifacts

use cmx_rust_sheet::prelude::*;
use sheet_core::style::{HAlign, Style};

fn styled(bold: bool, bg: Option<&str>, fg: Option<&str>, align: Option<HAlign>) -> Style {
    let mut s = Style::default();
    if bold {
        s.bold = Some(true);
    }
    s.back_color = bg.map(|x| x.to_string());
    s.fore_color = fg.map(|x| x.to_string());
    s.h_align = align;
    s
}

fn build() -> Workbook {
    let mut wb = Workbook::empty();
    let mut ws = Worksheet::with_size("损益表", 16, 6);
    ws.set_column_width(0, 200.0);
    for c in 1..=4 {
        ws.set_column_width(c, 110.0);
    }

    // 标题
    ws.set_value(0, 0, Some("XX 公司 · 季度损益表".into()));
    ws.set_style(0, 0, Some(styled(true, None, Some("#1a2138"), None)));

    // 表头
    let hd = ["项目", "Q1", "Q2", "Q3", "合计"];
    for (c, h) in hd.iter().enumerate() {
        ws.set_value(2, c as u32, Some((*h).into()));
        ws.set_style(
            2,
            c as u32,
            Some(styled(true, Some("#4f7cff"), Some("#ffffff"), Some(HAlign::Center))),
        );
    }

    // 明细行（项目 + 三季度值 + 合计公式）
    let rows: [(&str, [f64; 3]); 5] = [
        ("营业收入", [1200.0, 1380.0, 1510.0]),
        ("营业成本", [720.0, 810.0, 905.0]),
        ("销售费用", [160.0, 175.0, 190.0]),
        ("管理费用", [95.0, 102.0, 110.0]),
        ("研发费用", [140.0, 168.0, 205.0]),
    ];
    for (i, (name, vals)) in rows.iter().enumerate() {
        let r = 3 + i as u32;
        ws.set_value(r, 0, Some((*name).into()));
        for (k, v) in vals.iter().enumerate() {
            ws.set_value(r, 1 + k as u32, Some((*v).into()));
        }
        ws.set_formula(r, 4, &format!("=SUM(B{}:D{})", r + 1, r + 1));
    }

    // 毛利 = 收入 - 成本；净利 = 毛利 - 三费
    ws.set_value(8, 0, Some("毛利".into()));
    ws.set_style(8, 0, Some(styled(true, Some("#eef1fb"), None, None)));
    for c in 1..=3u32 {
        let col = (b'A' + c as u8) as char;
        ws.set_formula(8, c, &format!("={col}4-{col}5"));
        ws.set_style(8, c, Some(styled(true, Some("#eef1fb"), None, None)));
    }
    ws.set_formula(8, 4, "=SUM(B9:D9)");
    ws.set_style(8, 4, Some(styled(true, Some("#eef1fb"), None, None)));

    ws.set_value(9, 0, Some("净利润".into()));
    ws.set_style(9, 0, Some(styled(true, Some("#23d5c8"), Some("#08312d"), None)));
    for c in 1..=3u32 {
        let col = (b'A' + c as u8) as char;
        ws.set_formula(9, c, &format!("={col}9-{col}6-{col}7-{col}8"));
        ws.set_style(9, c, Some(styled(true, Some("#23d5c8"), Some("#08312d"), None)));
    }
    ws.set_formula(9, 4, "=SUM(B10:D10)");
    ws.set_style(9, 4, Some(styled(true, Some("#23d5c8"), Some("#08312d"), None)));

    // 毛利率（百分比格式）
    ws.set_value(11, 0, Some("毛利率".into()));
    for c in 1..=3u32 {
        let col = (b'A' + c as u8) as char;
        ws.set_formula(11, c, &format!("={col}9/{col}4"));
        let mut s = styled(false, None, Some("#7c5cff"), Some(HAlign::Center));
        s.formatter = Some("0.0%".to_string());
        ws.set_style(11, c, Some(s));
    }

    wb.append_sheet(ws);
    wb
}

fn main() -> std::io::Result<()> {
    let mut wb = build();
    let mut engine = FormulaEngine::new();
    engine.recalc_all(&mut wb);

    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("branding-artifacts");
    std::fs::create_dir_all(&out)?;

    // HTML（自包含文档，带网格线）
    let html = wb
        .export_html(0, &ExportHtmlOptions { full_document: true, gridlines: Some(true), ..Default::default() })
        .unwrap();
    std::fs::write(out.join("income-statement.html"), &html)?;

    // PDF（内置字体，ASCII 安全；此表含中文，PDF 内置字体不含 CJK，故 PDF 主要展示版式/分页能力）
    if let Some(pdf) = wb.export_pdf(0, PdfFont::Builtin) {
        std::fs::write(out.join("income-statement.pdf"), &pdf)?;
    }

    // XLSX（Excel 可打开）
    std::fs::write(out.join("income-statement.xlsx"), wb.to_xlsx())?;
    // 中性 JSON 快照
    std::fs::write(out.join("income-statement.json"), wb.to_json_string(true))?;

    // 打印几个算得的值证明公式真跑了
    let s = wb.sheet(0).unwrap();
    println!("  营业收入合计 E4 = {:?}", s.get_value(3, 4));
    println!("  毛利 Q1  B9  = {:?}", s.get_value(8, 1));
    println!("  净利润合计 E10 = {:?}", s.get_value(9, 4));
    println!("  毛利率 Q1 B12 (0.0%) = {:?}", s.get_value(11, 1));
    println!("  artifacts -> {}", out.display());
    Ok(())
}
