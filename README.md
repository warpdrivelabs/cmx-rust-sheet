# cmx-rust-sheet

> 一张空白工作表，一行行 Rust，长成一个纯后端电子表格引擎。

对标 **cmx-megasheet**（TypeScript 自研电子表格）的 `core / formula / io` 三层，
**去掉画布渲染与鼠标交互**，用 Rust 重铸为可嵌入后端服务、可批量转换、可无头出 PDF 的
纯逻辑引擎。格式、分组、浮动对象、公式、迷你图、打印 PDF、XLSX/CSV/JSON 导入导出——
除渲染绘制外一个不少。

与 TS 版**刻意相反**：不 clean-room，拥抱成熟高性能 crate 生态（serde · zip · flate2 ·
quick-xml · printpdf · rayon…），不重复造轮子，把精力全押在电子表格**领域逻辑**上。

- 设计方案：[docs/方案.html](docs/方案.html)
- 许可：Apache-2.0

## 里程碑（对齐父项目 M 序号，跳过纯渲染）

| 里程碑 | 交付 | 状态 |
|---|---|---|
| **RS-M0** | 核心数据模型：坐标 · 区域 · 稀疏矩阵 · 样式级联 · 单元格 · 表/簿 · 大纲 · 撤销/命令 | ✅ |
| RS-M2 | 编辑命令 + 撤销 + 剪贴板 + 填充 + 选区 + 词法级引用变换 | ✅ |
| RS-M3 | 公式引擎：Pratt 解析 · 依赖图 · 三色环 · 增量重算 · QM/QC/JE/FS/REF | ✅ |
| RS-M4 | 中性快照（serde，字节级 parity） | ✅ |
| RS-M7 | 数字格式串引擎 + 1900 日期序列号 | ✅ |
| RS-M8 | 函数库扩容（数学/统计/多条件/文本/日期/查找 ~55 函数）+ 命名区域 + 数组 | ✅ |
| RS-M9 | 冻结/拆分/尾部窗格（视口态） | ✅ |
| RS-M11 | 查找 · 排序 · 自动筛选 · 替换 | ✅ |
| RS-M12 | 单元格类型 · 数据验证 · 超链接 | ✅ |
| RS-M13 | 富文本 · 条件格式求值（cellValue/colorScale/dataBar/iconSet） | ✅ |
| RS-M14 | 浮动对象（图片/图表/形状）+ 批注 + 图表规格（11 类） | ✅ |
| RS-M15 | 分页 + HTML 导出 + PDF（printpdf，CJK 外部字体反超 TS） | ✅ |
| RS-M16 | XLSX 全保真往返（zip + 手写 OOXML，语义级 parity） | ✅ |
| RS-M17 | 函数库补齐到 272（math/financial/statistical/database/textref 五族） | ✅ |
| RS-M18 | 选择性粘贴（值/公式/格式 · 运算 · 转置 · 跳空） | ✅ |
| RS-M20 | 工作表保护 + 单元格锁定语义 + 可编辑判定 | ✅ |
| RS-M21 | 迷你图（Sparkline，设置 + 取数辅助） | ✅ |
| RS-M24 | 图表取数（11 类 spec + series 抽取；渲染剔除） | ✅ |
| RS-M26 | CSV 导入导出 + 数据工具（文本分列 · 删除重复 · 合并计算） | ✅ |
| 门面 | `cmx-rust-sheet` umbrella：三层重导出 + `WorkbookExt` 高层 API + `VERSION` + rayon 批量并行 | ✅ |

当前：**477 测试绿** · clippy 零警告 · fmt 干净。RS-M0→M26 全里程碑 + 门面 umbrella 落地；函数库 **272** 与 TS 对等。剩最终跨引擎 parity 硬化在途。

## 布局

```
cmx-rust-sheet/
├─ Cargo.toml              # [workspace]，统一依赖版本
├─ .cargo/config.toml      # aliyun 镜像源（同 cmx-container）
├─ docs/方案.html          # 设计方案
└─ crates/
   ├─ sheet-core/          # 数据模型 + 编辑 + 数字格式 + 条件格式 + 浮动 + 分页 + 图表取数（chrono/regex）
   ├─ sheet-formula/       # 公式引擎：词法 · 解析 · 求值 · 内置函数 · 依赖图（dep: sheet-core）
   ├─ sheet-io/            # 中性快照 · XLSX · CSV · HTML · PDF（serde/zip/quick-xml/printpdf）
   └─ cmx-rust-sheet/      # 门面 umbrella：重导出 + Workbook 高层 API + VERSION + rayon 批量
```

## 开发

```bash
cargo test --workspace                    # 单测
cargo clippy --all-targets -- -D warnings # 静态检查（零警告）
cargo fmt --check                         # 格式
```

三关验收：`cargo test` 全绿 · `clippy -D warnings` 无告警 · `fmt --check` 干净。

## 与 cmx-megasheet 的关系

中性快照（`format:"cmx-megasheet"`, `version:1`）是**单一事实源**，两个引擎共享。
跨引擎 parity：JSON 快照字节级同构（serde `preserve_order`），XLSX 语义级等价。
