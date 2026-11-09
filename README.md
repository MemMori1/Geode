# Geode 晶洞

> **一个能在 100GB 数据上流畅工作的十六进制编辑器，用 BinSpec 描述结构，用 AI 生成 BinSpec。**
>
> *外面是一块普通石头，切开里面是晶体 —— 外面是原始字节，解析后是结构。*

ImHex 证明了"hex 视图 + 结构描述语言"这条路能做成产品，但它把护城河建在了 Pattern Language 的解释器和模板生态上，而把 hex 视图本身做成了一个（相对）普通的功能。本项目的赌注是：**在数据规模上拉开一个数量级的差距（100GB 不是 4GB），并把"写模板"这件最贵的事交给 AI + 可验证的生成闭环**。

本仓库当前**只有设计文档，没有代码**。26 份文档 + 一册 ADR 构成一套自洽的规范：`00` 是契约，`01`–`18` 是各子系统的完整设计，`19`–`25` 是守门层（安全、验证、工具链、格式规范、互操作、交互），`adr/` 记录所有"为什么不用 X"。

---

## 文档地图

| # | 文档 | 内容 | 关键结论 |
|---|---|---|---|
| 00 | [架构总纲](docs/00-architecture-overview.md) | **契约**：技术栈、crate 分层、核心类型、跨子系统 trait、线程模型、性能预算、错误码登记 | 除 `geode-ui`/`app` 外禁止任何 GUI 依赖；UI 线程不做 >1ms 的工作 |
| 01 | [竞品与功能调研](docs/01-competitor-feature-research.md) | ImHex / 010 / Hex Fiend / Kaitai 拆解，功能三层分级，技术难点排序 | 不做 DSL 就只是 HxD 的复刻；缓冲区模型是唯一会逼重架构的决策 |
| 02 | [BinSpec 语言草案](docs/02-dsl-spec-draft.md) | 自研结构描述语言 v0.1：类型系统、指针语义分层、语句、属性、字节码 VM、预算、诊断 | 编译到字节码而非 AST 解释，才能精确计费与取消 |
| 03 | [AI 生成 BinSpec](docs/03-ai-dsl-generation.md) | 采样 → 生成 → 验证 → 修复闭环、置信度模型、eval | **模型只是零件，verifier 才是产品**；Verifier 必须先于 AI 落地且可单用 |
| 04 | [缓冲区与编辑模型](docs/04-buffer-and-edit-model.md) | 计数 B-tree piece table、COW、撤销、patch、保存事务、SIGBUS 坑 | 默认覆盖模式；placeholder ≥4KB；**绝不对可变文件 mmap** |
| 05 | [渲染与 Hex 视图](docs/05-render-and-hexview.md) | 虚拟滚动、68 亿行的滚动条、自绘、选区、高亮合成、双向联动 | 滚动条是"比例尺"不是列表；高亮不做 alpha 叠加 |
| 06 | [数据源 Provider](docs/06-data-providers.md) | 文件 / 磁盘 / 进程内存 / GDB / SSH / HEX-SREC / 流；统一缓存 | 短读合法；空洞是显式结果；未知长度用"探测扩展" |
| 07 | [数据处理器节点图](docs/07-data-processor-graph.md) | 显示前解密/解压、四类 view↔source 映射 | 1:1 变换退化为组内置换，零索引——这是 1GB 滚动无延迟的支点 |
| 08 | [搜索子系统](docs/08-search-subsystem.md) | 四引擎：字节通配 / 多编码字符串 / 正则 / 数值范围 | 正则跨块用 HIR `maximum_len` 定窗口，无界时续 DFA 状态 |
| 09 | [Diff 子系统](docs/09-diff-subsystem.md) | 滚动哈希锚点 + Myers/Patience 两级、双向偏移映射、结构级 diff | 纯 Myers 在 100MB 上不可用；`apply(ops,left)==right` 是主不变量 |
| 10 | [分析工具集](docs/10-analysis-toolkit.md) | 哈希、熵、字节分布、magic/MIME、Inspector 解码、报告 | 单遍多分析器；magic → 模板自动加载是全产品最重要的链路之一 |
| 11 | [反汇编视图](docs/11-disassembler.md) | Capstone 多架构、线性扫描、锚点回溯 | 变长指令集不能从任意偏移开始解码，宁可顶部留白绝不错位 |
| 12 | [YARA 扫描](docs/12-yara.md) | yara-x、规则来源与护栏、命中呈现 | 否决 libyara；恶意规则必须限超时/内存/正则复杂度 |
| 13 | [字段可视化器](docs/13-visualizers.md) | 图片/3D/时间轴/音频/矩阵，CPU 解码与 GPU 绘制分离 | 拆出 `geode-viz`（纯 CPU）；解码炸弹四道防线 |
| 14 | [插件系统与 ABI](docs/14-plugin-system.md) | C ABI、能力模型、声明式 UI、崩溃隔离 | 插件不能画任意 UI（v1）；诚实承认 v1 插件崩溃 = 应用崩溃 |
| 15 | [内容分发](docs/15-content-distribution.md) | 模板库/主题库/插件包、信任链、更新器、许可证红线 | 先静态 index.json，别造服务端；**ImHex 模板是 GPL，绝不能搬** |
| 16 | [UI 外壳 / 主题 / i18n](docs/16-ui-shell-theme-i18n.md) | 布局、命令系统、快捷键、主题三层 token、i18n、字体 | 命令注册表是菜单/工具栏/命令面板/快捷键的唯一事实来源 |
| 17 | [项目与持久化](docs/17-project-and-persistence.md) | 三层配置、项目格式、journal 崩溃恢复、密钥管理 | journal 只记 EditOp 不存文件副本；撤销栈不持久化靠 journal 重建 |
| 18 | [工程计划](docs/18-engineering-plan.md) | 目录结构、依赖矩阵、CI、测试金字塔、M0~M7、风险登记 | 400 人日 ≈ 20 人月，关键路径 240 人日；默认构建零 C 依赖 |
| 19 | [安全威胁模型](docs/19-security-threat-model.md) | 信任边界、22 行攻击面登记、TH-01~TH-22 威胁清单、防线库、漏洞响应 | 只承认进程/FFI/网络三条真隔离；同进程预算与 `catch_unwind` 只是降级不是边界 |
| 20 | [测试语料与模糊](docs/20-test-corpus-and-fuzzing.md) | 语料库规范、六类边界样本矩阵、25 个 fuzz target、I1~I14 属性不变量、triage | 语料只允许合成或可再分发；VM 挂死即 P0；CI 三层（PR 2min / 夜间 30min / 每周回灌） |
| 21 | [性能基准套件](docs/21-benchmark-suite.md) | 27 项 bench、8 份数据集造法、MAD 显著性判定、基准机噪声控制 | `00 §7` 每一行只允许一个 bench 守它；连续 3 次同向 >max(5%, 3·MAD) 才判红灯 |
| 22 | [BinSpec 工具链](docs/22-bspec-toolchain.md) | fmt / lint(L001~L032) / LSP / tree-sitter / VM 调试器 / 错误码→人话 | **lint 是 verifier 的第 0 层**；fmt 是只动空白的纯函数、幂等可测 |
| 23 | [自有格式规范](docs/23-file-formats.md) | 7 个格式的正式规范、字节级布局、schema、测试向量、EB 错误码、弃用政策 | `TooNew` 一律拒绝且不写盘；未知字段在 config 保留、在项目告警 |
| 24 | [迁移与互操作](docs/24-migration-and-interop.md) | 010 bt / hexpat / Kaitai / C header → BinSpec，统一 IR，GPL 红线，反向导出 | Kaitai 第一优先（60~75% 自动）；`.hexpat` 默认禁用、产物 `local-only` 永不入索引 |
| 25 | [交互流程与文案](docs/25-ux-flows-and-copy.md) | 6 条核心旅程、ASCII 线框、文档状态机、术语表、错误话术三段式 | 只读降级优先于拒绝打开；长任务永不阻塞输入且可取消 |
| 26 | [视觉设计](docs/26-visual-design.md) | 4dp 栅格、五级字阶、hex 单元格乘法几何、四态、48 图标、四套主题完整色表 | 否决斑马纹（与 `Value` 层抢通道）；hex 列宽永远乘法不累加 |
| — | [视觉原型](docs/img/mockup.html) | 单文件 HTML 原型：可滚动/可选字节/切主题切语言 | 双击即开，零外部依赖；给实现提供像素靶子 |
| ADR | [架构决策记录](docs/adr/README.md) | 18 篇不可变决策记录（piece table、placeholder 4KiB、否决 ScrollArea / egui C shim / 可分离窗口 / libyara…） | 只增不改；修订写新号并标"被 NNNN 取代" |

---

## 已定型的 16 条全局决策

1. **Rust + egui/wgpu 自绘**。内核 crate 与 GUI 完全解耦，依赖方向单向向下，CI 强制检查（`18 §3.2`）。
2. **缓冲区 = 计数型 B-tree piece table**，叶子 64 piece / Piece 32B，placeholder ≥4KB，COW 粒度 = 节点（快照 O(1)）。
3. **默认覆盖编辑**，insert/remove 必须显式进入。hex 的本质是改字段值，不是改长度。
4. **绝不 mmap 可变文件**（SIGBUS 不可捕获），一律 pread；mmap 仅用于"只读且大小稳定"。
5. **保存 = 临时文件 + fsync(file) + fsync(dir) + 原子 rename + journal**，任何一步失败都可恢复。
6. **所有外观高亮只有一个数据源**：`geode-annot` 的 `AnnotationIndex`，10 个图层，z 序固定（`00 §5.3`）。
7. **偏移一律 u64**，所有区间越界自动裁剪，绝不 panic。
8. **异步结果必须校验 `buffer.version()`**，陈旧结果静默丢弃。
9. **BinSpec 编译到字节码再执行**，不是为了快，是为了预算计数精确、可取消、栈深可控。
10. **Verifier 独立于 AI**，先落地、可单用（语法/执行/10 条不变量三层）。
11. **数据处理器只改"看到的字节"**，绝不改源；可写回的边界是 `MapQuality::Identity/Exact` 且全链可逆。
12. **诊断码全局登记**（`00 §12`），一段一归属，一码一语义，每条必须带 `ByteRange`。
13. **默认零遥测、零自动上传**。崩溃转储只落本地，上报必须逐次询问并先展示字段清单（`17 §8`）。
14. **安全姿态只能由用户上调，不能被项目文件下调**。安全策略字段一律不进 `.geoproj`（`19 §8`、`17 §3.4`）。
15. **lint 是 verifier 的第 0 层**，AI 与人类模板共用同一套 fmt/lint 规则与出厂等级，不允许"AI 模式放宽"（`22 §4.2`）。
16. **同进程内的预算与 `catch_unwind` 不是安全边界**，只是把"必然失败"降级为"可用"；真正的边界只有进程、FFI、网络三条（`19 §0`）。

---

## 待拍板（真正影响架构的 7 条）

这些是各子系统文档里的开放问题的**收敛结果**，其余问题属于实现期可自行决定的范围。

| ID | 问题 | 影响面 | 我的倾向 |
|---|---|---|---|
| Q1 | **"解密后的视图"里能不能直接编辑？**（反算写回源） | 07 × 04 × 08 × 09 | v1 只读 + 导出副本；写回留 v2。理由：不可逆变换下的写回会产生"编辑丢失"这类用户无法理解的失败 |
| Q2 | **Verifier / BinSpec 先做还是 hex 编辑器先做？** | 全部里程碑 | M0（只读 hex 视图）先做，M3 做 BinSpec。理由：没有能用的 hex 视图，模板跑出来也没地方看 |
| Q3 | **Capstone 的 C 层崩溃怎么兜？**（Rust 的 `catch_unwind` 抓不住段错误） | 11 | 默认同进程 + 崩溃上报；"反汇编/可视化解码在子进程"作为 v2 统一方案 |
| Q4 | **YARA 的 `pe`/`elf` 模块需要整文件缓冲**，与 100GB 流式目标冲突 | 12 | 仅在 ≤512MB 时启用模块，超限只用纯字节规则并明确提示 |
| Q5 | **正则 streaming 是否 v1** | 08 | v1 落地"有界长度用重叠窗口 / 无界降级为 64KiB 重叠 + 警告"，续 DFA 状态留 v1.1 |
| Q6 | **>4GB 的 diff 做不做** | 09 | 不做。100MB 对 100MB < 5s 是产品承诺，4GB 以上没有可用算法 |
| Q7 | **内容索引是否也签名** | 15 | v1 只签包体不签索引（靠包级签名兜底），索引被篡改最多导致"装不上" |
| Q8 | **许可证选哪个** | 14 × 15 × 19 × 24 | MIT OR Apache-2.0 双许可（Rust 生态惯例、对插件友好），模板与主题资源 CC0-1.0、文档 CC BY 4.0、插件任意但 ABI 头文件 MIT。**这一条决定后要同步改四份文档** |

另有两个需要**做 spike 才能定**的技术未知（不影响文档，但影响 05/13 的实现路径）：纯 `egui::Painter` 能否稳定扛住一屏 ~5000 quad（是否需要 wgpu 实例化 callback）；`epaint` 字体图集能否按字体局部失效（决定 CJK 字体的加载策略）。

---

## 建议阅读顺序

- **想看产品该长什么样**：01 → 03 → 05
- **想看技术上最难的地方**：04 → 02 → 07
- **想参与实现**：00 → 04 →（按你要做的子系统）→ 18
- **想看排期和风险**：18
- **想知道"凭什么说它安全"**：19 → 20
- **想知道性能红线怎么守**：21
- **想写模板**：02 → 22（工具链）→ 24（从现有生态导入）
- **想评审这个设计**：`docs/adr/`（18 篇否决理由，最快理解取舍）→ 25（用户实际看到什么）

---

## 关于名字

**Geode**（晶洞）：外面是一块普通石头，切开里面是晶体 —— 正是这个产品在做的事，而且晶洞里的晶体是**自然长出来的结构**，我们只是让它显形，不是硬雕上去的。

| 项 | 取值 |
|---|---|
| 产品 / 应用 | **Geode**，中文「晶洞」，读 /ˈdʒiː.oʊd/ |
| CLI | `geode`（`geode open fw.bin`、`geode run parser.bspec`） |
| crate 前缀 | `geode-core` / `-buffer` / `-provider` / `-annot` / `-format` / `-analysis` / `-bspec` / `-proc` / `-search` / `-diff` / `-disasm` / `-yara` / `-viz` / `-migrate` / `-project` / `-ai` / `-content` / `-plugin-host` / `-ui` / `-app` |
| 文件扩展名 | `.geows`（工作区）/ `.geoproj`（项目）/ `.geopkg`（模板包）/ `.geopatch`（补丁）/ `.geoplug`（插件）/ `.gmagic`（magic 规则） |
| 结构语言 | **BinSpec**，`.bspec` —— 不随品牌改名（语言不该被产品绑架，像 `.yaml` 不属于任何编辑器） |
| 用户目录 / 环境变量 | `~/.config/geode/`、`GEODE_CONFIG_DIR` 等 |
| 插件 ABI | `GeodePluginDesc` / `GEODEPLUG_API` / `geodeplug_entry` |

已刻意避开：不用 `.gd`（GDScript / Geometry Dash）；不用 `hex-*` 前缀（`hex` 是格式名，不是品牌名）。已知代价：搜索结果会与 [geode-sdk](https://docs.geode-sdk.org/)（Geometry Dash 模组 SDK）混在一起。

---

## 状态

- [x] 竞品调研与功能分层（01）
- [x] BinSpec 语言 v0.1 草案（02）
- [x] AI 生成方案（03）
- [x] 架构总纲与 18 个子系统设计（00、04–18）
- [x] 跨文档一致性校对（错误码段统一登记、Layer 枚举、crate 分层、键位冲突、性能预算回填）
- [x] 项目命名与全量改名（Geode，~520 处引用）
- [x] 守门层文档：安全威胁模型（19）、测试语料与模糊（20）、性能基准（21）、BinSpec 工具链（22）、自有格式规范（23）、迁移与互操作（24）、交互与文案（25）
- [x] 架构决策记录（`docs/adr/`，18 篇）
- [x] 二次一致性校对（错误码 EB/EC 段、`geode-migrate` 分层、搜索空间默认 Source、Layer 10 变体、阈值统一 5%、隐私默认 strict）
- [ ] 待拍板 8 条（见上表，Q8 许可证最急）
- [ ] M0 骨架代码

## 反目标（明确不做的事）

不做通用反编译器、不做恶意样本管理平台、不做协作编辑（v1）、不做 Web 版、不追求"能打开所有格式"（走模板库生态而不是内置一切）。
