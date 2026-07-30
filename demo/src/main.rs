//! M99 演示主程序：后端搭出六 sheet 工作簿 → 导出 `.xlsx`（Excel 可打开）+ 中性 `.json`。
//!
//! 运行：`cargo run -p m99-demo`（产物默认落在 `demo/out/`，可传一个目录参数覆盖）。
//! 对标 cmx-mega-sheet `demo/verify-m99.mjs` 的「构建→导出→回读校验」闭环，纯后端无浏览器。

use std::path::PathBuf;

use cmx_rust_sheet::WorkbookExt;
use m99_demo::build_m99_workbook;

fn main() -> std::io::Result<()> {
    // 产物目录：默认 demo/out（相对本 crate），可用 argv[1] 覆盖。
    let out_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("out"));
    std::fs::create_dir_all(&out_dir)?;

    let wb = build_m99_workbook();
    println!("M99 六 sheet 已在后端构建：");
    for (i, s) in wb.sheets().iter().enumerate() {
        let charts = s
            .list_floating_objects()
            .iter()
            .filter(|o| matches!(o.kind, sheet_core::worksheet::FloatingKind::Chart))
            .count();
        let sparks = s.list_sparklines().len();
        let r_groups = s.row_outlines.list().len();
        let c_groups = s.column_outlines.list().len();
        println!(
            "  [{i}] {:16} · 迷你图 {sparks} · 行组 {r_groups} · 列组 {c_groups} · 图表 {charts}",
            s.name()
        );
    }

    // 导出 XLSX + 中性 JSON。
    let xlsx_path = out_dir.join("m99-multi-sheet.xlsx");
    let json_path = out_dir.join("m99-multi-sheet.json");
    let xlsx = wb.to_xlsx();
    std::fs::write(&xlsx_path, &xlsx)?;
    std::fs::write(&json_path, wb.to_json_string(true))?;

    println!(
        "\n已导出：\n  XLSX  {}  ({} bytes)\n  JSON  {}",
        xlsx_path.display(),
        xlsx.len(),
        json_path.display()
    );

    // 回读自检：重新从 XLSX 装载，确认多 sheet 与分组结构无损。
    let back = cmx_rust_sheet::Workbook::from_xlsx(&xlsx);
    let rg = &back.sheets()[1];
    let cg = &back.sheets()[2];
    // 图表/迷你图是「只写不回读」（Excel 渲染，但我方 importer 不解析 chart XML 回模型），
    // 故直接数 zip 里的 chart 部件与 sheet1 的迷你图组，反映 .xlsx 里真实有什么。
    let (chart_parts, spark_groups) = count_xlsx_visuals(&xlsx);
    println!(
        "\n回读自检：sheet 数 {} · ②行组 {} · ③列组 {} · ⑥图表部件 {} · ①迷你图组 {}",
        back.sheet_count(),
        rg.row_outlines.list().len(),
        cg.column_outlines.list().len(),
        chart_parts,
        spark_groups,
    );
    println!(
        "完成。用 Excel / WPS / Numbers 打开 {} 查看多 sheet 页签、分组大纲、图表与迷你图。",
        xlsx_path.display()
    );
    Ok(())
}

/// 数 .xlsx 里的图表部件数（xl/charts/chartN.xml）与 sheet1 的迷你图组数。
fn count_xlsx_visuals(xlsx: &[u8]) -> (usize, usize) {
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(xlsx)) else {
        return (0, 0);
    };
    let charts = zip
        .file_names()
        .filter(|n| n.starts_with("xl/charts/chart") && n.ends_with(".xml"))
        .count();
    let sparks = {
        use std::io::Read;
        let mut xml = String::new();
        if zip
            .by_name("xl/worksheets/sheet1.xml")
            .ok()
            .and_then(|mut f| f.read_to_string(&mut xml).ok())
            .is_some()
        {
            xml.matches("<x14:sparklineGroup ").count()
                + xml.matches("<x14:sparklineGroup>").count()
        } else {
            0
        }
    };
    (charts, sparks)
}
