# M99 多 sheet 演示（后端构建 + 导出 Excel）

对标 cmx-mega-sheet 的 `demo/specs/spec-m99.js`，用 **cmx-rust-sheet** 在纯后端
（无渲染、无浏览器）搭出六个工作表，再整簿导出为 `.xlsx`（Excel/WPS/Numbers 可直接打开）
和中性 `.json` 快照。

## 六个 sheet

| # | Sheet | 演示能力 |
|---|---|---|
| ① | 迷你图 | line/column/winloss/bullet/area/pie 七型迷你图设置（存规格，不渲染） |
| ② | 行·多级分组 | 三级嵌套行大纲，汇总在首（summaryBelow=false） |
| ③ | 列·多级分组 | 三级嵌套列大纲，汇总在左（summaryRight=false） |
| ④ | 行列·同时分组 | 双轴各三级嵌套 |
| ⑤ | 字体·背景·边框 | 字族/粗斜下删线/字号/旋转 · 8 色背景+前景 · thin/medium/thick/double/dashed/dotted/对角 边框 |
| ⑥ | 图表集锦 | column/bar/line/area/pie/doughnut/scatter/bubble/radar/stock/combo 共 11 类图表规格 |

## 运行

```bash
cargo run -p m99-demo              # 产物落在 demo/out/
cargo run -p m99-demo -- /tmp/x    # 或指定输出目录
```

产物：
- `m99-multi-sheet.xlsx` —— 六 sheet 页签 + 多级分组大纲（可见「+/-」折叠钮）+ **11 类图表** + **迷你图**。
- `m99-multi-sheet.json` —— 中性快照（迷你图原始 7 型不失真）。

## 导出保真范围

XLSX 已覆盖：**多 sheet · 值 · 命名/内联样式（字体/填充/对齐/numFmt/边框）· 合并 ·
行高列宽 · 多级分组（outlineLevel + outlinePr summaryBelow/Right）· 折叠态 ·
图表 11 类（xl/charts + drawings 双格锚）· 迷你图（worksheet x14 extLst）**。

**反超 TS 版**：cmx-megasheet 的 xlsx.ts 不导出图表/迷你图，本项目补齐了 OOXML 生成。
两点取舍：
- **图表**：11 类映射到 Excel 原生图型（bubble→scatter、combo→bar+line、doughnut→pie+holeSize）。
  图表在 .xlsx 里是「只写」——Excel 能渲染，但本项目 importer 不解析 chart XML 回模型（无损往返走 JSON）。
- **迷你图**：Excel 原生只 3 型（line/column/stacked=winloss），我们 7 型里
  area→line、bar/bullet→column、pie→column **降级到最接近原生型**；原始类型仍在 JSON 快照里不失真。

## 校验

集成测试见 [`../test/tests/m99_multi_sheet.rs`](../test/tests/m99_multi_sheet.rs)：
`cargo test -p m99-test`。断言与本演示同源（共用 `build_m99_workbook`）。
