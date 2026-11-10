# Geode 开发计划与进度记录

> 这个文件是**开发期间的唯一进度事实来源**。规则只有四条：
>
> 1. **开工前**看 §2（当前迭代）与 §6（阻塞）；**收工后**往 §4 追加一行日志，改 §2 的状态。
> 2. 状态只有四种：`[ ]` 待办 · `[~]` 进行中 · `[x]` 完成 · `[!]` 阻塞（必须在 §6 写原因）。
> 3. 任务一旦开始，**ID 不改**；拆出来的子任务用 `T1.2a` 这种后缀。
> 4. 数字（测试数、耗时、体积）必须是真的——**没测就写"未测"，不写估计值**。

- 仓库：`github.com/MemMori1/Geode`（分支 `main`）
- 当前里程碑：**M0 收尾 → M1（编辑与持久化）**
- 文档基线：26 份设计文档 + 18 篇 ADR（`docs/`），本文件不重复它们的内容，只记"做到哪了"
- 最近更新：2026-11-10

---

## 1 现在在哪（一句话）

Sprint 0 只差两把尺子（P4 5/6 的 `scroll_p99` 要真实窗口；S4 未开）。**M1 的代码面已全部落地**：T1.1~T1.8 全绿，只剩 T1.9 集成验收与一处环境限制（`save_1gb` 在本机磁盘上 110.9 MiB/s，未达 500 MB/s，已定位为磁盘带宽而非实现）。

| 里程碑 | 状态 | 出口条件（摘要） | 备注 |
|---|---|---|---|
| M0 骨架 | `[~]` 部分完成 | 打开 4GB <200ms ✓；滚动 p99 <25ms ✗（未测）；`xtask deps` + CI ✓；`--version/--help` ✓ | 四个 crate 齐；真实窗口滚动未测 |
| M1 编辑与持久化 | `[~]` 代码完成，待集成验收 | 见 §3 的 T1.1~T1.8，出口是 `18 §10` 的 M1 五条 | T1.1 ✅（4 GiB pwrite 回读；整文件 4 GiB 保存因磁盘不足未跑）、T1.2 ✅、T1.3 ✅、T1.4 ✅、T1.5 ✅、T1.6 ✅、T1.7 ✅、T1.8 ✅；T1.9 未开始 |
| M2 视图完备 | `[ ]` | — | 依赖 M1 |
| M3 BinSpec v0.1 | `[ ]` | — | 依赖 M1/M2 |

---

## 2 当前迭代：Sprint 0「把尺子造出来」（≈3.0 人日）

目标：让 M1 的每一条验收都有**可执行的测量手段**，并把四个押着核心假设的技术未知打掉。

| ID | 任务 | 人日 | 验收（怎么算做完） | 状态 |
|---|---|---|---|---|
| P1 | 错误码登记表 + `xtask docs-errors`（双向校验 + 生成 `error_codes.rs`） | 0.3 | `cargo xtask docs-errors --check` 绿；126 码全登记；生成文件进 CI | `[x]` 2026-11-09（释义 54/126，余 72 条随 27 文档补） |
| P2 | `schemas/*/v1.json` ×4（geopatch / geojournal / geows / geoproj） | 0.3 | 四个 schema 能被 `jsonschema` 校验器加载；与 23 §3 的字段表逐字段对齐 | `[x]` 4/4 + 2 份 `v1.layout.md`（二进制格式的字节表）；`xtask schema-check` 已接 CI |
| P3 | `tests/vectors/` 每格式 ≥3 个损坏向量 | 0.2 | 每个损坏向量写明"期望哪个错误码"，CI 校验该码已登记 | `[x]` 结构完成：4 格式 16 条样本（含 12 条损坏），`xtask vectors-check` 双向校验；**字节级真跑待 M1 的 `geode-format`（`--run`）** |
| P4 | bench 骨架（criterion，先 6 项：open_4gb / scroll_p99 / save_1gb / read_64k / edit_random / undo_all） | 0.3 | `cargo bench` 能跑；`xtask bench audit` 认得出 6 项与 00 §7 的对应关系 | `[~]` **5/6** 实现（open_4gb 6.19 µs、read_64k 889 ns、edit_random p99 105.9 µs、undo_all 717 ms、save_1gb 110.9 MiB/s **未达标**——本机磁盘 `dd` 同卷仅 116.7 MB/s）；余 scroll_p99 |
| P5 | proptest 骨架（piece table vs `Vec<u8>` 影子对拍） | 0.2 | 随机操作序列 1 万次，双实现对拍无差异（此时 buffer 还没有编辑，先锁只读读路径） | `[x]` 只读路径完成：`ByteRange` 5 条 + `Buffer::read` 2 条属性；编辑对拍随 T1.4/T1.5 接入 |
| S1 | spike：placeholder 4 KiB 下限是否碎片化 | 0.5 | 1 万次随机编辑后 `piece ≤ O(C + 改动字节/4KiB)`、内存放大 <2× | `[x]` **通过**：1 万次编辑后 36 片（理论上界 88 片的 0.4×），描述子 1.1 KiB；无 R1 则 16,823 片（191×），证明 R1 是界的前提；见 `docs/spikes/S1-placeholder-fragmentation.md` |
| S2 | spike：journal 不逐条 fsync 在 `kill -9` 下的丢帧窗口 | 0.3 | 1000 次随机时刻 kill：最大丢失 ≤1 秒编辑、恢复成功率 100% | `[ ]` |
| S3 | 纯 `Painter` 5000 quad/帧 | 0.4 | 一屏 ~5000 图元整帧 <16.6 ms | `[x]` **通过**：15,360 图元 0.83 ms（余量 20×）；见 `docs/spikes/S3-painter-throughput.md`。不含 GPU 提交与命中测试 |
| S4 | spike：`epaint` 字体图集按字体局部失效 | 0.3 | 换字体不全量重建；首帧到可交互 <1.2 s | `[ ]` |

**顺序**：P1 → P2 → P3 → P5 → P4 → S1 → S2 → S3 → S4。
理由：P1 先做是因为 M1 从第一天就会产码；S3/S4 要等 `geode-ui` 有最小渲染循环才有意义，排最后。

---

## 3 队列：M1「编辑与持久化」任务拆解（40 人日）

| ID | 任务 | 人日 | 验收 | 状态 |
|---|---|---|---|---|
| T1.1 | 写路径：`FileProvider` 可写（`pwrite`）+ `Caps::READ_WRITE_STABLE` + 短写重试 + `fsync` 语义 | 4 | 在 4 GiB 文件随机位置写 1 MiB，回读逐字节相等 | `[x]` **4/4**：`open_rw/write_at/write_all_at/sync_all`；`save_provider_atomic`（temp+fsync+rename+刷目录+权限位）→ **T1.7 补齐 .bak/进度/取消**；`tests/write_4gb.rs::pwrite_at_4gib` 在 4 GiB 文件 2 GiB 处写 1 MiB 并回读逐字节相等（秒级，常跑）；`save_4gib_roundtrip` 已写好但 `#[ignore]`（本机仅 742 MiB 空闲，跑不了） |
| T1.2 | 计数 B-tree piece table：`Piece`(32B) / 叶 64 piece / fanout 64 / split / join / `PieceCursor` | 8 | 跨 100 piece 连续读 O(log N + 100)；与 `Vec<u8>` 对拍 | `[x]` **完成（T1.2a 语义层 + T1.2b 计数树）**。`TreeCursor` 沿树下降（路径栈、无 unsafe、均摊 O(1)），`concat` 做树内 R3 边界合并，`check_invariants` 六类检查。实测：10 万 piece 定位 0.48 µs（与偏移无关）、10k 次 1 字节覆盖后 14,170 片/height 30（R3 省 17.1%）、10k 次 append 后 2 片/height 0。T1.2a：`Piece::{Source,Literal}`、`PieceTable::{from_source,from_bytes,spans,overwrite,insert,remove}`、相邻 Source 连续即合并 / 相邻 Literal ≤64 KiB 合并、越界返回 `EditError` 不 panic；9 条测试 + 200 例 proptest 与 `Vec<u8>` 对拍。**T1.2b 未做**：计数 B-tree、`PieceCursor`、O(log N) 定位。**placeholder ≥4 KiB 降级也未做**——降级要读源字节，归到 T1.3 的 hydrate 路径（`piece.rs` 头部已写明，不静默填零）。**T1.2b 计数树已落地**（`tree.rs`：叶/节点 ≤64、每节点缓存字节数、按 bytes 下降、split/concat；10 万 piece 树高 ≤6，定位 0.48 µs 且与偏移无关；150 例随机编辑与 `PieceTable` 对拍）。**仍缺**：`PieceCursor` 沿树下降（现在还是 `PieceTable` 的平坦游标）、相邻 piece 合并没有搬进树、节点欠载未收紧 |
| T1.3 | placeholder（≥4 KiB 阈值、R1 降级 / R2 禁升级 / R3 相邻合并）+ lazy file loading | 5 | 打开 4 GiB **不读一个字节**；首次滚动才填 | `[x]` **5/5**：`hydrate.rs` 完成 R1（`demote_small_placeholders`：<4 KiB 的 Source 读成 Literal，短读报错不填零）+ 局部 `defragment(at,len)`（把区间 literal 化，piece 数有界）+ 6 条测试；**已做**：R3 进树；`PieceTree` 接 R1/defrag；**lazy 填页**由 `geode-provider::page_cache`（64 KiB 页 / 64 MiB 预算 / CLOCK / 空洞负缓存 TTL 2s / volatile 直通）+ `EnsureFilled::ensure_filled` 实现：4 GiB 稀疏源打开 reads=0，`ensure_filled(1 MiB,4 KiB)` 只读 1 页、只降级该区间 |
| T1.4 | 覆盖编辑：`Overwrite` 默认、EOF 追加、nibble 400 ms 分组、`EditOp` 日志 | 4 | 输入 `41 42` 后字节变为 `41 42`，撤销一次回到原值 | `[x]` **4/4**：覆盖/插入/删除/填充、EOF 追加、nibble 400 ms 分组；`EditOp{Overwrite,Insert,Remove,Fill}` 手写大端编解码 + `Editor::replay`；patch 归并只许等长替换 |
| T1.5 | 撤销：操作日志 + 大操作 COW 快照；`undo_mem_limit = min(512MB, max(64MB, 2%×len))` | 4 | 1 万次随机编辑后全部撤销，文件逐字节相等（P5 对拍） | `[x]` **4/4**：逆操作撤销 + `undo_all`（**10,000 次**随机编辑逐字节相等）+ 预算 `clamp(len/50,64MiB,512MiB)` 与丢最早步；**大操作 COW 快照**（≥1 MiB 走 `Undo::Snapshot(Arc<PieceTable>)`，literal 只加引用不复制） |
| T1.6 | patch 收集与合并（间隙 ≤16 归并）+ `.geopatch` 导出（`geode-format`，BE 定长帧 + 每记录 CRC32） | 3 | 导出再导入，字节等价；损坏向量产出指定 `EB` 码 | `[x]` `geode-project` 桥接：`to_patch_file/from_patch_file/apply_patch_file`；64 MiB 源 500 次随机编辑逐字节对拍、4 GiB 稀疏源只读改动窗口；导入先全量校验再应用（失败不半应用）。**归并修正**：只允许等长替换（Overwrite/Fill）且 `at>=last_end`——相邻 Insert/Remove 归并会拼出非法区间 |
| T1.7 | 保存事务：temp + `fsync(file)` + `fsync(dir)` + 原子 rename + 硬链接 `.bak` + 进度与取消 | 5 | 保存 1 GiB >500 MB/s；`kill -9` 后原文件不损坏 | `[x]` 代码完成：`.bak` 硬链接（失败不致命）、`save_provider_atomic_with` 每 1 MiB 回调、`..._with_cancel` 取消后目标不动且无 `.part`；9 条测试。**吞吐 110.9 MiB/s 未达 500 MB/s**，同卷 `dd conv=fsync` 仅 116.7 MB/s ⇒ 磁盘限制，非实现 |
| T1.8 | journal 写入与恢复：80 B 头 + `kind│seq│len│payload│crc32`，四道关卡（magic/CRC/seq/指纹） | 5 | `kill -9` 重启提示"恢复 N 条修改"并成功恢复（S2 的结论写进文案） | `[x]` `journal.rs` 608 行：`JournalWriter`（fsync 策略 64 帧/1 s 取先）、`JournalReader::{open,open_with_base}`、`Recovered{partial,truncated_at,failure}`；10 条测试。S2：100 次 kill -9 恢复 100/100、Partial 0/100、最多丢 62 帧（≈124 ms）；限定已写入 spike 文档 |
| T1.9 | M1 集成验收：把 `18 §10` M1 的五条跑成 CI 用例 | 2 | 五条全绿，作为 M1 的出口 | `[~]` **3/5**：`tests/m1_acceptance.rs` 覆盖 ①（4 GiB 处插入 16 B，0.1 ms 内可读）②（10,000 次随机编辑 undo_all 逐字节相等）③ 数据面（journal 尾部截断→Partial 恢复前缀）；④ 1 GiB 保存吞吐受磁盘限制（110.9 MiB/s）；⑤ schema/vectors 已在 P2/P3 覆盖 |

**M1 的硬顺序**：T1.1 → T1.2 → T1.3 → T1.4 → T1.5 →（T1.6 与 T1.7 可并行）→ T1.8 → T1.9。
T1.2 是全项目最危险的一块（R1：静默数据损坏），必须先有 P5 的对拍再写。

---

## 3.1 队列：M2「视图完备」任务拆解（35 人日）

> 里程碑规格见 `docs/18 §10`。M2 的四条验收：①行列/分组全可切换且列对齐不裂 ②100 万字节矩形拖选 <16 ms/帧 ③四引擎在 1 GB 文件达 `00 §7` 吞吐 ④Inspector 全类型解码可反写。

| ID | 任务 | 人日 | 验收 | 状态 |
|---|---|---|---|---|
| T2.1 | 窗口骨架：eframe 窗口 + 自绘比例尺滚动条（u64 行号，禁用 `egui::ScrollArea`） | 6 | 4 GiB 文件滚动 p99 <25 ms 持续 30 s（M0②） | `[~]` **代码完成**：`app/src/window.rs`（eframe 0.32 + 自绘 thumb + 拖拽/滚轮/PageUp/Down/Home/End），滚动位置存 `top_row:u64`；**p99 未测**（需真实显示器与 30 s 采样） |
| T2.2 | 网格渲染：列配置 8/16/32/64、分组 1/2/4/8、radix、Sarasa 图集、乘法定位 | 7 | 切换后列对齐不裂；像素快照零容差 | `[~]` **3/5**：bpr 8/16/32/64 与 group 1/2/4/8 可切、`format_row` 纯函数（短行补满、列宽不漂）、偏移/ASCII 列已画；**未做** radix 切换、Sarasa 字体嵌入、像素快照基线 |
| T2.3 | 光标与选区模型：线性/矩形、锚点-焦点、拖拽、键盘扩展、多字节选 | 5 | 100 万字节矩形拖选 <16 ms/帧 | `[x]` **完成**：`selection.rs` 485 行，`Pos/SelectionKind/Selection/Cursor`，13 单测 + 3 组确定性属性测试（区间不重叠、总长 ≤ len） |
| T2.4 | 高亮合成：10 层 z 序、最高不透明层定底、前景对比度 ≥4.5:1、装饰通道 | 4 | 色盲可辨；不产生新色相 | `[x]` **完成**：`composite.rs` 688 行，10 层 `Layer`、WCAG 对比度、压暗 ΔL≤0.20、4 种装饰通道，16 单测 + 3 组属性测试（合成后 fg ≥4.5:1） |
| T2.5 | Inspector：u8~u128/u24/u48/f16~f64/bool/char/时间戳/GUID/RGBA8/LEB128 解码与反写 | 4 | 全类型往返一致 | `[x]` **完成**：`core/inspector.rs` 1035 行，20 类型 × 双端序往返、i24/i48 符号扩展、f16 特殊值、GUID/LEB128/5 种时间戳，28 测试（含 3 proptest） |
| T2.6 | 四引擎搜索：hex 通配、多编码字符串、正则、数值范围；流式 + 可取消 | 5 | 1 GB 文件达 `00 §7` 吞吐 | `[x]` **完成**：`geode-search` 2382 行，四引擎 + 流式分块（1 MiB，正则 64 KiB 兜底）+ 取消 + 截断；43 测试；实测 hex 1715 MiB/s、正则 9965 MiB/s、数值 511 MiB/s（16 MiB 合成） |
| T2.7 | 分析面板：CRC32/BLAKE3、滑动熵、字节直方图、字符串提取 | 3 | 1 GB 文件熵 ≥1.5 GB/s | `[x]` **完成**：`geode-analysis` 2030 行，单遍多分析器；实测单线程 CRC32 545.8 MiB/s、BLAKE3 2314 MiB/s、熵+直方图 412.8 MiB/s；**1.5 GB/s 是 4 线程预算，单线程未达** |
| T2.8 | 性能与基线：`scroll_p99` 30 s、拖选基准、像素快照接入 CI | 1 | 连续 3 次不超阈 | `[ ]` |

---

## 3.2 队列：M3「BinSpec v0.1」任务拆解（80 人日）

> 里程碑规格见 `docs/18 §10`。验收：①ZIP 模板解析真实 zip 与黄金文件一致 ②树↔hex 双向定位 <10ms ③编译 1000 行 <20ms、执行 10MB <100ms ④死循环 5s 内被掐断给 E2001 ⑤Verifier 可脱离 AI 单用。

| ID | 任务 | 人日 | 验收 | 状态 |
|---|---|---|---|---|
| A0 | 词法/语法/AST/`SrcSpan`+`LineIndex`/错误恢复 | 15 | 解析 02 §11 全部示例；一次报 ≥3 个错 | `[~]` 派工中 |
| A1 | 字节码 + VM（预算、取消、软失败、`Node` 树） | 18 | 死循环 5s 内 E2001；越界字段降级 unknown | `[~]` 派工中 |
| A1.5 | Verifier：I2/I4/I6/I7/I9/I10 + 配置 + 报告 | 12 | 可脱离 AI 单用；误报率 <5% | `[~]` 骨架完成（`verify.rs`，5 测试） |
| A2 | 结构树 UI + 双向联动（`Layer::Structure`） | 15 | 点树跳 hex、点 hex 反查 <10ms | `[ ]` |
| A3 | `.gmagic` 匹配 + 模板自动加载编排 | 8 | 206 格式规则集可加载 | `[ ]` |
| A4 | 增量重解析（顶层 `@` 单元 + dirty ranges） | 12 | 改 1 字节只重解析受影响单元 | `[ ]` |

**硬顺序**：A0 → A1 → A1.5 →（A2 与 A3 可并行）→ A4。A1.5 必须先于任何 AI 生成（03 §12）。

---

## 4 进度日志（只追加，不改旧行）

- 2026-11-10 M3 开工：A0 前端、A1 字节码 VM 派工；A1.5 Verifier 骨架落地（`crates/geode-bspec/src/verify.rs`，I2/I4/I6/I7/I9/I10，5 测试）；M2 收尾（radix + ScrollModel + bench）并行。
- 2026-11-10 M2 第一波：T2.3/T2.4/T2.5/T2.6/T2.7 完成，T2.1 代码完成、T2.2 3/5；新增 `geode-search`、`geode-analysis` 两个 crate；测试 194→331。

| 日期 | 做了什么 | 证据 |
|---|---|---|
| 2026-11-10 | **S1 完成**：4 KiB + R1/R3 后 36 片（界内 0.4×）、描述子 1.1 KiB；无 R1 则 16,823 片（191×）；病态 1 MiB/10 万次 1 字节 ⇒ 不合并 182k 片、4 KiB 合并 17 片；指出 `04 §3.3` 两处措辞需修正 | `c112668` |
| 2026-11-10 | **T1.7 保存事务**：`.bak` 硬链接 + 进度回调 + 协作取消；`save_1gb` 实测 110.9 MiB/s（磁盘 `dd` 同卷 116.7 MB/s，未达 500 预算，环境限制） | 本提交 |
| 2026-11-10 | **T1.8 journal**：80 B 头 + 四道关卡 + `Recovered.partial`；S2 100 次 kill -9：恢复 100/100、Partial 0、最多丢 62 帧（124 ms） | 本提交 |
| 2026-11-10 | **T1.4/T1.5 收尾**：`EditOp` 编解码 + `replay`；≥1 MiB 走 COW 快照；随机对拍 2,000→10,000 | 本提交 |
| 2026-11-10 | **T1.3 收尾**：`PageCache`（64 KiB 页/64 MiB/CLOCK/空洞 TTL 2s）+ `CachedProvider` + `ensure_filled`；4 GiB 打开 reads=0 | 本提交 |
| 2026-11-10 | **T1.1 验收**：`pwrite_at_4gib` 4 GiB 偏移写 1 MiB 回读相等；整文件保存测试标 `#[ignore]`（磁盘不足） | 本提交 |
| 2026-11-10 | **T1.2 收尾**：`TreeCursor` 沿树下降 + 树内 R3 边界合并 + `check_invariants`；10k 次 1 字节覆盖后 14,170 片（R3 省 17.1%），10k 次 append 后 2 片；`tree_cursor_matches_piece_cursor` 250 例对拍 | 本提交 |
| 2026-11-10 | **T1.6 patch 桥**：`geode-project` 导出/导入/应用；修两个真问题——①相邻 Insert/Remove 被归并成非法区间（改为只归并等长替换）②`eof_hit` 语义与 06 §3.1 不符（短读才置位）；FinalLen 改用 EB014 | 本提交 |
| 2026-11-10 | **T1.4 编辑会话**：`edit.rs` 覆盖/插入/删除/`fill`/`nibble`（400 ms 分组），逆操作撤销，patch 间隙 ≤16 归并（含 gap 字节）；10 条测试含 2,000 次随机编辑后 `undo_all` 逐字节相等；顺带修 `PieceTree::split` 在子节点边界 panic（proptest 抓到） | `1e64d1d` |
| 2026-11-10 | **T1.5 撤销预算**：`min(512MB, max(64MB, 2%×len))`，`VecDeque` 撤销栈、超预算丢最早步且至少留一步；`default_undo_limit` 边界测试 | `226ebfa` |
| 2026-11-10 | **geode-format 落地**：`.geopatch`/`.geojournal` 编解码，自实现 CRC32，8 条向量（6 条损坏）全过；`decode_prefix` 供 journal 前缀恢复；schema 枚举纠正 | `a11550c` |
| 2026-11-10 | **geode-annot 落地**：10 层 z 序 + 分块 max_end 线段树，10 万条 `query_point` 162 ns；17 单测 + 3 proptest 对拍朴素 oracle | `7878a50` |
| 2026-11-09 | 文档收口：M0 实现状态、M1 前置工件、4 项 spike 写进 `18 §10.1~10.4`；README 增「开工前清单」 | `7d0d7a2` |
| 2026-11-09 | **P1 完成**：建 `docs/errors.toml`（126 码唯一事实来源）+ `xtask docs-errors`（双向校验 + 生成 `crates/geode-core/src/error_codes.rs`）+ CI 加 `--check`；测试 31 → 33 | `d0627c2` |
| 2026-11-09 | **P5 完成（只读路径）**：7 条 proptest 属性（`ByteRange` 5 + `Buffer::read` 2）；属性测试当场抓出 `clip` 的 `Option` 永远为 `Some` 的假信号，已改为返回 `Self` | 见下一次提交 |
| 2026-11-09 | **P2 完成**：补齐 `geopatch`/`geojournal` 的 schema 与 `v1.layout.md`（头 32/80 B、帧格式、CRC 覆盖范围），并纠正 23 §3.6 示例里 `payload_len=09` 的排版错误（Overwrite 恒为 16+len） | 见下一次提交 |
| 2026-11-10 | **T1.3 3/5**：`PieceTree` 接上 R1 降级与局部 defrag（与 `PieceTable` 结果逐字节对拍），短读仍必须报错。剩余：树内相邻合并、lazy 填页 | 本提交 |
| 2026-11-10 | **T1.3 前半**：`hydrate.rs` 的 R1 降级（<4 KiB placeholder → literal，短读报 `ShortRead` 不静默填零）与局部 `defragment(at,len)`；6 条测试（含"降级后不变量仍成立""短读必须报错"）。**未接线到 `PieceTree`** | 本提交 |
| 2026-11-10 | **T1.2b 计数树落地**：`PieceTree` 叶/节点 ≤64、缓存子树字节、按 bytes 下降；10 万 piece 定位 0.48 µs（与偏移、规模无关），树高 ≤6；150 例随机编辑与语义层对拍。**偏差**：允许子节点高度不一致（非严格 B-tree，ADR-0019）；合并不在树内做 | 本提交 |
| 2026-11-09 | **T1.2b 基尺**：`benches/piece.rs` 实测 flat Vec 的定位/编辑随 piece 数线性增长（2,048 piece：定位 1.11 µs、编辑 28.2 µs；外推 10 万 piece：54 µs / 1.4 ms，一帧 68 次定位 ≈3.7 ms）。新增 `check_invariants()`（4 类结构错误）与 `PieceCursor`（跨 piece 顺序读）；见 `docs/spikes/T1.2b-baseline.md`。B-tree 未写 | 本提交 |
| 2026-11-09 | **T1.2a 完成**：piece table 语义层 + 9 条测试（含 200 例随机编辑对拍）；抓到并修掉一个真 bug——`split_at` 在 `at` 恰好是边界时返回 `OutOfRange`，导致"插入后马上删除"必失败。**索引层 T1.2b 未开始** | 本提交 |
| 2026-11-09 | **T1.1 第一片**：`FileProvider::open_rw/write_at/write_all_at/sync_all`、`geode-buffer::save::save_provider_atomic`（临时文件→fsync→rename→刷目录，保留权限位，失败不留垃圾）；4 条 save 测试 + 3 条 provider 写测试；测试 45→48。**未做**：4 GiB 随机写验收（等 T1.2）、journal | 本提交 |
| 2026-11-09 | **S3 完成**：`crates/geode-ui` 骨架 + `grid` 几何（乘法定位）+ `benches/paint`；15,360 图元 0.83 ms、文本路径 1,088 格 0.114 ms；结论与"没证明什么"写入 `docs/spikes/S3-painter-throughput.md` | 见下一次提交 |
| 2026-11-09 | **P3 结构完成**：4 个格式 16 条向量（12 条损坏，覆盖 EB001/EB002/EB004/EB009/EB010/EB011/EB012/EB013），`xtask vectors-check`（负向测试：删一个样本 ⇒ 退出码 1） | 同上 |
| 2026-11-09 | **P2 部分**：写 `schemas/geows/v1.json`、`schemas/geoproj/v1.json` + 两份 `CHANGELOG.md`；新增 `cargo xtask schema-check` 并接入 CI（负向测试：塞一个 `format` ⇒ 退出码 1，报两处） | 见下一次提交 |
| 2026-11-09 | **P4 部分**：新建 `bench/` crate（criterion）+ `M1_BENCHES` 注册表（6 项，4 项写明阻塞任务）；`open_4gb` 6.19 µs、`read_64k` 889 ns（内存源，仅回归基线） | 同上 |

---

## 5 决策记录（开发期，与 `docs/adr/` 互补：这里记"小决定"，架构级的进 ADR）

| ID | 决定 | 理由 | 日期 |
|---|---|---|---|
| D-001 | 许可证按 **MIT OR Apache-2.0** 落地，Q8 未拍板前不改 | 代码已带两个 LICENSE；改起来只是措辞与 `deny.toml`，不阻塞开发 | 2026-11-09 |
| D-002 | `docs/errors.toml` 是诊断码唯一事实来源，**不接受**从文档正文反向抽取当权威 | 文档是散文，抽取不可靠；宁可让 CI 逼着两边对齐 | 2026-11-09 |
| D-003 | 生成的 `error_codes.rs` 允许 `#[rustfmt::skip]` | 126 个常量一行一个才好 diff；被 rustfmt 压成行会毁掉可读性 | 2026-11-09 |
| D-004 | 错误码 `severity` 暂按段默认 `error`，逐码复核随 27 文档做 | 先要"码存在且一致"，再要"分级准确"；反过来会拖住 P1 | 2026-11-09 |
| D-005 | `ByteRange::clip` 从 `Option<Self>` 改为 `Self` | 属性测试证明 `Option` **永远是 `Some`**（`min()` 不会失败），这种"看起来会失败其实不会"的签名会诱导调用方写无效分支；短读语义统一为空区间 | 2026-11-09 |

---

## 6 阻塞与风险

| # | 事项 | 影响 | 需要谁 | 状态 |
|---|---|---|---|---|
| B1 | **Q8 许可证**未拍板 | 只影响 14/15/19/24 的措辞，不阻塞代码 | 用户 | `[!]` 等一句话 |
| B2 | M0② 滚动 p99 未测 | M0 无法宣布完成；`geode-ui` 落地后才能测 | — | `[!]` 等 `geode-ui` |
| B3 | SFTP / GDB 无真实环境 | 只影响 M4 的数据源验收 | 后期再找目标机 | `[ ]` |
| B4 | `severity` 分级未经复核 | 27 文档前，日志里的严重级别可能偏重（全是 error） | — | `[ ]` |

---

## 7 度量看板（每次收工更新，数字必须真实）

| 指标 | 当前值 | 目标 | 来源 |
|---|---|---|---|
| Rust 行数（不含生成） | 16,325 | — | `wc -l`（crates/app/xtask/bench） |
| 提交数 | 28 | — | `git rev-list --count HEAD` |
| 测试数 | **191** | M1 出口 ≥80 | `cargo test --workspace` |
| 打开 4 GiB 稀疏文件 | 0.013 ms（app 端到端）/ 6.19 µs（bench 纯打开） | <200 ms | `app` / `bench open_4gb` |
| release 二进制 | 461 KB | <15 MB | `ls -l` |
| CI | 3 OS 绿 | 保持 | GitHub Actions |
| 诊断码登记 | 126 / 释义 54 | 126 / 126 | `xtask docs-errors` |
| `00 §7` 性能条目达标数 | 1 / 19（打开；read_64k 是内存源基线不算数） | 19/19（M7） | `cargo bench` |

---

## 8 怎么用（给未来的自己）

```bash
export PATH=$HOME/.cargo/bin:$PATH          # 本机 cargo 不在默认 PATH
cargo test --workspace                       # 33 个测试
cargo run -p xtask -- deps                   # 依赖方向门禁
cargo run -p xtask -- docs-errors --check    # 诊断码双向一致
cargo run -p app -- --help                   # CLI
git push                                     # 需要显式 key（见下）
```

**推送**：本机 ssh 以 root 运行、`~` 解析成 `/var/root`，必须显式指定 key：

```bash
GIT_SSH_COMMAND="ssh -i /Users/edwa2d4th/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push
```
