# 05 — 渲染引擎与 Hex 视图（虚拟滚动 / 选区 / 高亮合成 / 双向联动）

> 本文是 `geode-ui` 内部唯一允许知道"屏幕"的地方：几何、字形、色彩决策全部收敛在此 crate；**不拥有任何数据语义**（Buffer / Annotation / 结构树的真值都在下游 crate）。
> 冲突时以 `00-architecture-overview.md` 为准（§4 类型、§5.3 Layer、§6 线程模型、§7 预算）。

---

## 1 目标与非目标

### 1.1 目标

| # | 目标 | 可验证指标 |
|---|---|---|
| G1 | 100GB 文件与 1KB 文件**帧成本相同** | 每帧工作量只与可见行数相关，与 `doc_len` 无关（C1） |
| G2 | 任意时刻只绘制可见行 | 绘制行数 ∈ [40, 80]，硬上限 96 |
| G3 | 高亮合成可预测、可读、色盲可用 | 合成后文本对比度 ≥ 4.5:1，且语义**不依赖色相单通道** |
| G4 | 结构 ↔ hex 双向联动 < 5ms | 点字节 → 最深节点，100 万节点下 P99 < 5ms |
| G5 | 任何数据问题降级为视觉占位，绝不阻塞 UI | UI 线程单帧不等待 I/O，最坏退化为占位纹理 |
| G6 | 视觉状态可由快照测试锁定 | 12 个基准场景像素级可复现 |

### 1.2 非目标

| # | 非目标 | 归属 |
|---|---|---|
| N1 | 差异的**一致性判断**（哪边算不同） | 见 `09-diff-subsystem`，本文只提供并排双缓冲绘制能力 |
| N2 | Annotation 索引内部结构 | 见 `00 §5.3` 的 `AnnotationIndex` trait，本文只用不建 |
| N3 | 主题色板、字体文件、i18n 文案 | 见 `16-ui-shell-theme-i18n` |
| N4 | Inspector 的值解码规则（f16/LEB128/GUID…） | 见 `10-analysis-toolkit`，本文只负责布局与跟随光标 |
| N5 | 反汇编行 / 可视化器画布 / 显示前数据变换 | 见 `11-disassembler`、`13-visualizers`（共用本文行虚拟化 API）；变换见 `07-data-processor-graph`，按变换后字节显示但**偏移列永远是原始偏移** |

---

## 2 依赖与边界

```
app ──► geode-ui ──► geode-core（类型）
              ├──► geode-buffer(read / version / patches)   ├──► geode-annot(query / Layer 样式源)
              ├──► geode-bspec(Arc<StructureIndex> 只读快照) ├──► geode-analysis(CRC / 熵 / 直方图)
              └──► geode-search / geode-diff（仅通过命令与 Ann 交互，无编译期依赖）
```

| 方向 | 契约 | 约束 |
|---|---|---|
| 读 `geode-buffer` | `len()` / `read(range)` / `version()` | 只在 UI 线程调用，单次 ≤ 64KiB，且只读缓存未命中块 |
| 读 `geode-annot` | `query(range)` / `query_point(off)` | 每帧 ≤ 3 次区间查询（可见区间、结构覆盖、光标点） |
| 写 `geode-annot` | 仅 `Layer::Selection` / `Layer::Cursor` | 本文是这两层的**唯一写入者**（§7.2） |
| 读 `geode-bspec` | `Arc<StructureIndex>`，随 `version` 整体替换 | 只持有 `NodeRef`（索引句柄），不持有 `Node` 拥有型副本 |
| 写 Buffer | **禁止**。编辑走命令 → `04-buffer-and-edit-model` | 渲染层永不直接 mutate |
| GUI 依赖 | 只允许 `egui` / `epaint` / `wgpu`；不得被内核 crate 依赖 | 见 `00 §2` |

**铁律**：本 crate 不得出现 `for _ in 0..doc_len`、`read(0..doc_len)`、结构树全量遍历。CI 用 grep 门禁。

---

## 3 数据结构与接口

### 3.1 视图状态（★ = 可持久化）

```rust
pub struct HexView {
    pub cfg: HexViewConfig,           // ★ 见 §3.3
    pub cursor: Cursor,               // ★
    pub selection: Option<Selection>, // ★
    pub scroll: ScrollPos,            // ★
    pub mode: EditMode,               // ★ Overwrite | Insert
    layout: Layout, rows: RowCache, glyphs: GlyphAtlas,
    resolve_cache: ResolveCache, dirty: DirtySet,
}
pub struct ScrollPos { pub row: u64, pub sub: f32 }  // 行号 + 行内比例，不用单一像素值
pub struct Cursor { pub off: u64, pub nibble: Nibble }
pub enum Nibble { Whole, High, Low }
pub struct Selection { pub anchor: u64, pub head: u64, pub shape: SelShape }
pub enum SelShape { Linear, Block { cols: (u8, u8) } }
```

### 3.2 布局度量（每帧重算，纯函数）

```rust
pub struct Layout { pub row_h: f32, pub first_row: u64, pub visible_rows: u16,  // sp（已量化）/ 40..=96
    pub off_w: f32, pub hex_w: f32, pub ascii_w: f32,               // 三栏宽度
    pub cell_w: f32, pub byte_w: f32,                               // 单字符宽 / 2*cell_w + nibble_gap
    pub group_pitch: f32, pub ascii_cell_w: f32,                    // 组间间隙 / = cell_w，禁止按字宽累加
    pub ascii_glyphs: u8 }                                          // UTF-16 视图 = bpr/2
```

### 3.3 配置结构体草案（`HexViewConfig`，表即契约）

| 配置项 | 类型 | 默认值 | 取值范围 | 说明 |
|---|---|---|---|---|
| `bytes_per_row` | u8 | **16** | 1..=64 | 宽度不足时**运行时下调**但不写回配置（§6 #4） |
| `row_h` | enum | **Auto** | Auto / 12..=48 sp | Auto = `ceil(ascent+descent) + 2*row_pad`，量化到整设备像素 |
| `row_pad` | f32 | **2.0** | 0..=8 | 小字号下靠 padding 保证可点击性 |
| `font.family` | enum | **内置 JetBrains Mono 子集** | 内置 / 系统等宽 | **禁止回退到 UI 字体**：CJK fallback 会破坏列宽（`01 §3.2`） |
| `font.size` | f32 | **13.0** | 8..=28 | 与 `row_h` 解耦 |
| `address_format` | enum | **Hex / 大写 / 无前缀 / 补 0** | Hex / Dec / Relative / None | 前缀 `0x` 与相对基址由工作区决定 |
| `address_width` | enum | **Auto** | Auto / Fixed(n) | Auto = `ceil(log16(len))`，最小 6 位防抖动（§4.2） |
| `grouping` | u8 | **1** | 1 / 2 / 4 / 8 | 同时定义选择单位与双击粒度 |
| `group_order` | enum | **Big**（内存序） | Big / Little | Little 时组内字节视觉反转，偏移标签仍指最低地址 |
| `group_gap` / `nibble_gap` | f32 | **6.0 / 0.0** | 0..=16 / 0..=4 | 组间间隙；`grouping=1` 时 group_gap 即字节间隙，nibble_gap>0 时 high/low 分色（§4.5） |
| `separator` / `hex_case` | enum | **Space** / **Upper** | None / Space / Pipe / Grid；Upper / Lower | `Grid` = 画行列浅网格线（+960 条线，预算允许） |
| `ascii.enabled` | bool | **true** | | 关闭后 hex 列居中扩张 |
| `ascii.encoding` | enum | **Latin-1** | Ascii / Latin1 / Utf8 / Utf16Le / Utf16Be / Ebcdic037 | UTF-16 改变**列数**（`bpr/2` 个字符） |
| `ascii.invalid_glyph` | char | **`·`** U+00B7 | `·` / `?` / 空 | 无效序列**不显示原始 hex**（列宽会变） |
| `ascii.dim_nonprintable` | bool | **true** | | 不可打印字符降到 45% 亮度 |
| `display` | enum | **Hex** | Hex / HexII / Binary / Decimal | HexII：`..`=0x00、`  `=0x20；Binary：列宽 ×4 |
| `edit_mode` | enum | **Overwrite** | Overwrite / Insert | 见 §4.6 |
| `overlay.band` / `.bracket` | bool | **true / true** | | 行底结构色带 / 左缘括号线 |
| `overlay.degrade_depth` | u8 | **6** | 0..=12 | 超过后折叠中间层（§4.8） |
| `layers[L].enabled` / `.opacity` | bool / f32 | **全 true** / **Selection 0.85、Search 0.9、其余 1.0** | 0..=1 | 逐层开关；opacity 只作用于该层底纹强度，不参与 alpha 链（§4.7） |
| `min_contrast` | f32 | **4.5** | 3.0..=7.0 | 低于即触发前景自动修正 |
| `colorblind_safe` | bool | **false** | | true 时用 Okabe-Ito 色板 + 纹理通道 |
| `stats_debounce_ms` / `stats_max_bytes` | u16 / u64 | **150** / **64 MiB** | 50..=1000 | 统计防抖；超过上限只显示长度，不算 CRC / 熵 |

### 3.4 行缓冲

```rust
pub struct RowCache { blocks: LruMap<BlockId, BlockState>, hits: u64, misses: u64 }
pub enum BlockState {
    Ready{ver: u64, bytes: Arc<[u8]>}, Loading{job: JobId, ver: u64},
    Failed{kind: LoadFail, retries: u8, next_retry: Instant},
    Holes{ver: u64, mask: Arc<[u64]>},      // 64 字节位图标记不可读
}
pub const BLOCK: u64 = 64 * 1024;  // = 4096 行（bpr=16）≈ 68 屏；LRU_BLOCKS = 32 → 2 MiB 常驻
```

**决策：块粒度 64KiB，不是行粒度。** 一次 `read_at` 覆盖 68 屏，顺序滚动命中率 > 99.9%，`read_at` 调用被压到每 68 屏 1 次。否决行粒度（每帧 60 次小读 ≈ 1ms syscall 开销，且 LRU 失效更快）与 4KiB 页粒度（调用次数 ×16，收益为零）。

### 3.5 结构索引与反查

```rust
pub struct StructureIndex {          // 由 geode-bspec 构建，Arc 共享，不可变
    by_start: Vec<NodeRef>,          // 按 offset 升序（稳定）
    max_end:  Vec<u64>,              // 隐式线段树：i 子树内最大 end
}
pub struct NodeRef { id: NodeId, start: u64, len: u64, depth: u16, parent: Option<NodeId> }

impl StructureIndex {
    pub fn deepest_at(&self, off: u64) -> Option<NodeRef>;       // O(log n)
    pub fn covering(&self, r: ByteRange, out: &mut Vec<NodeRef>); // O(log n + k)
    pub fn units(&self) -> &[NodeRef];                           // 顶层 @ 单元（分页用）
}
```

**决策：按 `start` 排序的平坦数组 + `max_end` 隐式线段树**，不用通用区间树。理由：结构树是"一次解析、整体替换、只读"的产物，无增量插入；平坦数组缓存友好（百万节点构建 < 40ms），查询沿 `max_end` 剪枝下钻 O(log n)。否决 `intervaltree` crate（指针追逐，构建慢 3~5 倍）；否决"二分 + 线性回扫"（最坏 O(n)，点击会卡）。

### 3.6 对外接口（供 `16-ui-shell` 调用）

```rust
impl HexView {
    pub fn ui(&mut self, ui: &mut Ui, cx: &mut ViewCtx) -> ViewResponse;
    pub fn jump_to(&mut self, t: JumpTarget);          // Abs / Rel / End / Node / Unit
    pub fn select(&mut self, r: ByteRange, why: SelectSrc);
    pub fn reveal_range(&mut self, r: ByteRange);      // 最小滚动 + 上下各留 1 行
    pub fn set_structure(&mut self, idx: Arc<StructureIndex>, ver: u64);
    pub fn scroll_to_node(&mut self, id: NodeId);      pub fn frame_report(&self) -> &FrameReport;
}
pub struct FrameReport { /* 仅 perf 构建填充：stage_us[8] / rows_drawn / glyphs /
    draw_calls / ann_queries / ann_hits / cache_hit / prefetch_wait_us */ }
```

---

## 4 核心算法 / 流程

### 4.1 offset ↔ row 映射（溢出边界）

```text
row_of(off)   = off / bytes_per_row                        // u64 除法，无溢出
col_of(off)   = off % bytes_per_row
off_of(row,c) = row.checked_mul(bpr).and_then(|x| x.checked_add(c))   // 溢出 → None
first_off     = scroll.row * bpr   （checked；溢出视作 EOF 之后）
```

- **全程 u64 计算，只在最后一步转 usize**，且仅用于"行内索引"（`< 64`），不用于偏移。`off_of` 结果 ≥ `buffer.len()` → 该格渲染为空，不参与命中测试。
- 100 GiB / 16 = **6,710,886,400 行**（67.1 亿）；100 GB（十进制）/16 = 62.5 亿行。u64 无压力。
- `bytes_per_row` **允许非 2 的幂**（1/3/5/7…），因此只能整除不能移位。

### 4.2 虚拟滚动与滚动条精度

- `visible_rows = ceil(viewport_h / row_h) + 1`，硬钳 < 96；典型 52~62 行。
- **不用 `egui::ScrollArea`**：其滚动量在 `f32` 上累加，超过 ~1.6e7 px 后单次增量小于 1 ULP → "拖不动 / 跳格"（`01 §4.5`）。自绘滚动条，状态 `ScrollPos{row: u64, sub: f32}`。
- 映射：`progress = row as f64 / total_rows as f64`。精度问题**不在 f64**（整数精确到 2^53 ≈ 9e15，6.7e9 行安全），而在像素：4000 px 轨道 ÷ 67 亿行 = **1 px ≈ 1.68e6 行 ≈ 26.8 MB**。
- 由此：① **不逐行跟手**——拖动 thumb 按比例直接跳 `row = round(progress * total)`，thumb 跟手、内容跳跃；② **跳跃预览**——拖动期间顶部浮层显示目标偏移 + 该行前 8 字节 + `deepest_at` 结构路径，释放后才读取精绘；③ **粒度档位** `1x/16x/256x`（双击 thumb 切换），每像素位移折算 Δrow ÷ 档位，用于微调最后几十万行；thumb 最小高 20px。
- 语义：滚动条是**比例尺**而非"可滚列表"，UI 上用 `<total_rows> 行 / <total_bytes>` 标签明示。

| 输入 | 行为 |
|---|---|
| 滚轮 / 触控板 | 每格 3 行；同向连续且间隔 < 50ms 线性加速，上限 **30 行/格**，反向或间隔 > 200ms 归零；触控板按 sp 累积到 `sub`，满 1.0 进位，不加速 |
| PageUp / PageDown | `visible_rows - 1` 行（保 1 行重叠，不丢上下文） |
| Ctrl+↑/↓ | 段落跳转：上/下一个结构单元（`units()`）；无结构时跳到下一个 16 字节对齐空行块 |
| Home / End | 行首/行尾；Ctrl+Home/End = 文件首/尾 |
| 中键拖动 | 抓取式平移，1:1 跟随 Δy，无加速度、无惯性，越界即停 |
| Ctrl+滚轮 | 缩放 `font.size`（8..=28），`row_h` 跟随，光标行屏幕位置不变 |

**锚定原则**：滚动 / 缩放 / 改 `bytes_per_row` 时保持"光标字节的屏幕相对位置"；光标不可见则保持视口顶行不变。

### 4.3 自绘策略（Painter + wgpu）

**每帧绘制预算（bpr=16，60 行）**：

| 元素 | 数量 | 形态 |
|---|---|---|
| 偏移列 + hex 字符 | 600 + 1920 字形 + 900 空格 ≈ **3420 quad** | 图集 quad |
| ASCII 字符 | **960** 字形 | 每行 1 个 galley |
| 背景 rect | 60 行底 + 分组底纹 + 选区 ≤ 120 | RectShape |
| 结构装饰 | ≤ 60 色带 + ≤ 60 括号线 | RectShape |
| 合计 | **≈ 4400 glyph quad + ≈ 400 rect** | **目标 ≤ 8 draw call** |

- **决策：hex 列与偏移列不走文本整形，直接查字形图集出 quad。** hex 只有 22 个符号，advance 恒定；一条 galley 要走 shaping + 布局 + 缓存查找，60 行/帧 = 60 次无谓整形，且一旦字体带 kerning/ligature 列宽就漂移。图集路径 O(1) 且位置精确到 `row*cell_w`。
- **决策：ASCII 列走 galley**，需处理 UTF-8 多字节与缺字 fallback；每行 1 条，60 条/帧可接受。缓存键 `(row, version, ascii_cfg_hash, style_id)`，LRU 512 行（≈8 屏）。
- **列宽对齐**：启动时测 `"0" "F" "W" "-"` 的 `advance` 取 max，量化到 `1.0/ppp` 的整数倍。列位置一律 `x = base + n * cell_w`（乘法），**禁止 `x += w` 累加**——累加是漂移的唯一来源（`01 §3.2`）。
- **批量与图集**：全部 quad 走同一张 atlas 纹理与同一 blend state，前景色作顶点属性（不打断批次）→ epaint 自动合批；**硬要求整帧只用 1 张字体图集纹理**（ASCII 与 hex 共用同一字体）。图集 2048×2048 R8，key = `(font_id, codepoint, ppb_bucket)`，桶 ∈ {1.0,1.5,2.0,3.0}；满则重建并整帧重绘（罕见）。
- **脏行 vs 整帧**：行 Shape 缓存在 `row_shapes: LruMap<RowKey, Vec<Shape>>`，`RowKey = (row, version, cfg_hash, resolve_token)`。平时每帧只把可见行 Shape 克隆进 shape list（memcpy，不是重建）。整帧重建仅当：主题 / `bytes_per_row` / 字体 / DPI / `version()` / 视口宽度变化 / 图集重建。100 万字节选区变化只脏可见 ~60 行 + 选区两端行。

### 4.4 行缓冲与数据获取

```text
每帧：① 依 scroll 算 rows_in_view → 涉及的 BlockId 集合
② 逐块判定：Ready 且 ver == buffer.version() → 命中；
            Ready 但 ver 过期 → 转 Loading 并提交读 Job；缺失 / Failed → 见 §6 #1
③ 预取：[first_block-1, last_block+1] 的缺失块合并为一个连续区间读（前后各 1 块 = ±68 屏）
④ 绘制：命中 → 真实字节；Loading 且有 ghost（上版本同偏移内容）→ ghost @ alpha 0.25 + "读取中"角标；
        Loading 且无 ghost → 斜纹占位（10% 亮度，**不画 `??` 假装有数据**）；
        静止 80ms 仍未到 → 保持占位，重绘由 Job 完成事件驱动，不轮询
```

- UI 线程唯一允许的同步读：`Ready` 命中块的切片拷贝（≤ 64KiB，~5µs）。异步读走 `00 §5.4` Job，返回值携带 `requested_version`；主线程 drain 时校验 `buffer.version()` 不符即丢弃（`00 §6.3`）。
- **滚动期低精度 / 静止后精修**：指针持续滚动（120ms 内有位移）时只做占位与 ghost，且每帧最多发起 2 个块读（防任务风暴）；静止 `settle_ms=80` 后一次性补齐 ±1 屏。理由：滚动中的读取结果多会被下一帧丢弃，纯浪费带宽。

### 4.5 光标与选区

- `Cursor{off, nibble}`：hex 列有 `High/Low` 两个可辨位置，ASCII 列恒为 `Whole`。视觉双通道：当前 nibble 用"底色高亮 + 同字节另一 nibble 降 35% 亮度"；插入模式光标改为 **2px 空心框占半格**（与覆盖模式实心块区分）。
- `Selection{anchor, head}`，保留方向；复制时统一取 `min..max`。

| 操作 | 语义 |
|---|---|
| 单击 | 设 cursor，清空选区 |
| Shift+单击 | `head = off`，保留 anchor |
| 拖拽 | `head` 跟随指针；指针出视口上下沿 → 自动滚动，速度 `clamp(dist_px / row_h, 1, 30)` 行/帧，越远越快 |
| 双击 | 选中该偏移**所属最深结构节点**的区间；无结构时选中一个 `grouping` 单位 |
| 三击 | 选中整行；四击 = 全选 |
| Ctrl+A | 全选（`0..len`），100GB 下不读任何数据 |
| Alt+拖拽 | 块状矩形选区 |

- **块状矩形选区：做，但不是默认拖拽。** 理由：多记录定长数据的列提取（16 列里取第 5~8 列）是真实高频需求，且它表达的是纯视觉概念，不需要数据语义。约束：`SelShape::Block` **不参与** Buffer 线性编辑原语（编辑时按行优先展开为不连续区间列表），只影响复制与高亮；进 Insert 模式自动降级为线性并提示。否决"块选区为默认拖拽"：与 Shift 扩展、自动滚动组合时语义冲突严重。
- **超长选区裁剪**：从不物化选区字节列表。每帧对可见 60 行做 `selection ∩ row_range` 求交（O(60)），只画相交矩形。100 万字节选区 = 6.25 万行，绘制成本与 100 字节选区**完全相同**。
- **选区统计防抖**：`len` O(1) 立即显示；CRC32 / 熵 / 分布走 `geode-analysis` Job，触发条件 = 选区变化后静止 `150ms`；`len > 64MiB` 只显示长度 + "统计已跳过"（可命令强制）。结果回主线程校验 `version + selection_epoch`，任一变动即丢弃。

### 4.6 编辑输入模型

| 场景 | 行为 |
|---|---|
| hex 输入 `0-9A-F` | 写当前 nibble → 自动推进 `High → Low → 下一字节 High`；行尾自动跨行（不强制滚动） |
| 非法字符 | **拒绝且不消费**：不写 Buffer，120ms 红框脉冲 + 状态栏"该列只接受 0-9 A-F"。不静默忽略 |
| ASCII 输入 | 按 `ascii.encoding` 编码：单字节编码写 1 字节并前进；UTF-8/16 多字节字符整体写 `n` 字节，要求 `n` 字节连续可选（遇行尾或选区边界则拒绝并提示） |
| 覆盖模式 / 插入模式 | 覆盖为默认：不改长度，到 EOF **拒绝继续**（不自动扩展文件）并提示；插入走 `insert` 原语，长度变化 → 偏移相关缓存按 `version` 全失效（`04 §5`） |
| Delete / Backspace（覆盖模式） | **不修改数据，只移动光标**（Delete 前进 1 字节，Backspace 后退 1 字节）。**否决"覆盖为 0x00"**：它把用户以为无损的"删除"变成不可见的破坏性写入，违反 C2，且与 patch 语义混淆。需要填 0 时用显式命令 `Fill 0x00`（可撤销、显示为 patch） |
| Delete / Backspace（插入模式） | `remove(1 字节)`；有选区则删整个选区（单次 `remove`，单条撤销记录） |
| 粘贴 | 见下表，识别后写入**一个** `EditId`（整段一次事务） |
| 输入聚合 | 连续 nibble/字符合并为一个撤销单元：空闲 **500ms**、或光标跳转 / 选区变化 / 模式切换 / 失焦 / 切换视图 即 `checkpoint()`；同类输入连续超过 5s 强制切分（防挂机连环合并） |

粘贴识别顺序（首个匹配即用，全不匹配按原文作 ASCII 写入）：**C/Rust 数组**（含 `0x`、`,` 分隔、被 `{}`/`[]` 包裹）→ **Base64**（长度 %4==0、字符集仅 `A-Za-z0-9+/=`，且含 ≥2 个 `+/` 或长度 > 64）→ **Hex 文本**（仅 `[0-9A-Fa-f\s]` 且去空白后偶数长）→ **原始文本**。

歧义消解：`"ABCD"` 同时像 hex 与文本 → **弹一次二选一**（可"记住本会话"）；`"deadbeef"` 这类全 hex 且长度 ≥ 8 → 默认按 hex（手输文本极少是纯 hex 且长度成组）。

### 4.7 高亮合成算法（重点）

**z 序（低 → 高，固定 = `Layer` 枚举声明序）**：`Value < Search < Structure < Analysis < Selection < Bookmark < Diff < Yara < Ai < Cursor`（共 10 层，与 00 §5.3 的 `Layer` 声明序一致；`Analysis` 承载熵段、坏区、字符串密度这类机器侧标注）。同层按 `Ann.priority` 升序，再按 `AnnId` 稳定排序。

**为什么不能简单 alpha 叠加**：两层 50% 透明（搜索黄 + 结构蓝）叠出灰绿，语义消失；三层以上变脏灰，结果依赖叠加次序，用户无法预测"这里被谁标了"。对比度按 `L = 0.2126R + 0.7152G + 0.0722B`（先 sRGB→线性化）、`ratio = (L1+0.05)/(L2+0.05)` 计算。因此：

```text
resolve(cell)，遍历 10 层中命中的 ann：
1. base = 从最高 z 向下第一个 bg.a()==255 的层；无 → 主题默认底（patch / 未加载区用各自角色底）
2. base 之下仍有 bg 的层：不做 alpha 混合，改为把 base 色沿 HSL 压暗 —— h,s 保留 base，
   l -= 0.05 × 层序差（累计上限 0.20）→ "同色系压暗"：底下的东西可感知，但不产生新色相
3. 该层 opacity < 1.0：l 按 opacity 线性插值回默认底（同样只动 l）
4. 装饰（不占底色）：下划线 1px（Search）/ 左带 3px（Yara）/ 顶边 1px（Diff）/ 斜纹（Ai 低置信）
   —— 各占固定像素位置，多装饰共存不互相覆盖
5. fg = 最高 z 层给出的 fg；无 → 主题默认 fg（base 亮度 > 0.6 时取深色）
6. 对比度校验：contrast(fg, base) < min_contrast → fg 沿 L 轴向黑/白二分修正（≤8 次，兜底纯黑或纯白）
```

- **否决**：① 直接 alpha 叠加（脏色）；② 按 priority 加权混色（不可预测、对比度失控）；③ "永远固定深色前景"（丢掉 Value 层用色表达"可打印/控制/零字节"的能力）。
- **色盲友好**：`colorblind_safe=true` 切 Okabe-Ito 8 色板，且**每层必须有非色相冗余通道**（形状/纹理/位置，见步骤 4）；结构深度用**同色相亮度阶梯**（L 62%→28%，步 6%）而非彩虹，灰度仍可辨。
- **逐层 UI**：图层面板每行 = `[复选框] 层名 [不透明度] [色板槽] [Alt+1..9]`；关层 = resolve 跳过该层（Ann 保留避免重算），不允许改名/重排（z 序是规范）。缓存键 `(version, ann_epoch, theme_epoch, sel_epoch, cfg_hash)`，任一变全量失效（960 cell 重算 < 0.8ms）。

### 4.8 结构 ↔ hex 双向联动

**结构树 → hex**：① `reveal_range(node.start, node.len)`——已可见则不滚，否则最小滚动（顶/底对齐取位移小者），上下各留 1 行；② 选中该区间（`Selection` 层，`why=Node`），节点加入 `pinned_nodes`（≤ 8，淘汰最旧），pinned 节点画 2px 虚线轮廓（区别于普通选区底纹）；③ 边界线：首字节右侧 3px 竖线、末字节左侧 3px，跨行时偏移列加 `[` `]`。

**hex → 结构树**：

1. `deepest_at(off)`（O(log n)，`max_end` 剪枝）。
2. 「最深」判定：候选集取 `depth` 最大 → `len` 最小 → `start` 最大（后声明者优先）。
3. 命中 `Compound` → 结构树展开到其**父路径**并选中；命中叶子 → 只选中，不展开父（避免 300 层深树全展开）。
4. 未命中任何节点 → 结构树不动，Inspector 仍按光标偏移解码。**联动不能变成"无结构就无响应"**。

**Inspector 跟随**：光标移动 → 150ms 防抖 → `10-analysis-toolkit` 解码全部类型，并高亮与节点 `type_name` 匹配的那一行；防抖期间显示旧值 + 50% 亮度（防闪烁）。

**结构覆盖渲染（三通道并用）**：

| 通道 | 视觉 | 用途 |
|---|---|---|
| 底部色带 | 行底 2px，色 = 深度亮度，x 范围 = 节点在本行的字节子区间 | "这行有哪些结构、边界在哪" |
| 左侧括号 | 左缘竖线，`x = off_col_right + 2 + min(depth,3)*2` | 嵌套层次，跨行连续 |
| hover tooltip | `File.header.pe[3].name` + `type_name` + 值 + `[start,end)` + 诊断 | 精确信息，不占屏 |

**深嵌套降级（`degrade_depth=6` 起）**：① `depth ≤ 6` 全画；② `6 < depth ≤ 12` 只画最外 3 层 + 最近 2 层，中间折叠为一条中性色带（灰 40%），括号缩进上限 3 档；③ `depth > 12` 只画最外 1 层 + 光标/选中所在 1 层；④ 任何情况每行色带 ≤ 8、括号线 ≤ 4。理由：2px 色带在 60 行/屏上超过 8 层即为噪声，用户要的是"我在哪一层"而非"全部层"。

### 4.9 视图模式

| 模式 | 实现 | 归属 |
|---|---|---|
| 连续 hexdump | 默认；行地址 = `row * bpr`，滚动轴 = 全文 | 本文 |
| 按结构节点分页 | 页 = `units()[i]`；行地址 = `node.start + row*bpr`，滚动钳在 `[start,end)`；页脚 `page i/n` | 本文提供能力，导航 UI 见 `16` |
| 只显示选区 | `visible_range = selection`，行从选区首字节起算，边界行显示部分单元格 | 本文 |
| 并排 diff | 暴露 `draw_rows(painter, layout, left, right, row_map)`；两 view 共用 `Layout`，中缝 1px；`row_map` 与不一致判定来自 `09` | 本文只画 |

**约束**：所有模式共用同一 `Layout` + `RowCache` + `resolve`，**不允许**为某模式写第二份绘制路径；差异只体现为行地址映射 `fn row_to_off(mode, row) -> Option<u64>`。

---

## 5 性能预算与基准

### 5.1 每帧阶段预算（60 行 / bpr=16 / 10 万 Ann / 含结构树面板）

| 阶段 | p50 | p99 | 违反时的动作 |
|---|---|---|---|
| 输入 + 视口几何 + 布局（行/列几何、可见行集） | 0.45ms | **1.0ms** | 告警：布局里混入了非 O(行) 计算 |
| 标注区间查询（≤3 次） | **0.15ms** | **0.5ms** | 降级为只查 Structure 层 |
| 高亮合成（≤960 cell） | 0.3ms | 0.8ms | 关闭 Ai / Yara 的逐 cell 装饰 |
| 文本整形 / 图集取字 | **0.6ms** | **2.0ms** | 缩小 galley 缓存，或 ASCII 列改图集路径 |
| Shape 构造 + 提交 | 1.5ms | **4.0ms** | 关闭 Grid 分隔线、关闭结构括号 |
| 其它 UI（结构树/面板/状态栏） | 2.0ms | 4.0ms | 结构树虚拟化（`16`） |
| **合计** | **~5.0ms** | **< 16ms（硬上限 25ms）** | 三项同时超 → 自动降级并提示 |

### 5.2 基准场景（进 `bench/`，CI 门禁，退化 > 5% 红灯；阈值口径见 21 §4.3）

| 场景 | 条件 | 预算 |
|---|---|---|
| 滚动帧时间 | 100GB、bpr=16、3 层高亮、结构树展开；以及 bpr=64（4 倍字形） | p50 < 16ms / p99 < 25ms；bpr=64 放宽到 p99 < 30ms（**已知例外**，§6 #4） |
| 跳转 | 100GB、随机 1000 次 `jump_to(Abs)` | 每次 < 5ms |
| 选区拖拽 / 全选首帧 | 100 万字节选区每帧移动；1GB `Ctrl+A` | < 16ms/帧；< 8ms（不读数据） |
| 结构反查 / 高亮合成 | 100 万节点随机 1000 次 `deepest_at`；960 cell × 9 层全命中 | 每次 < 2µs；合成 < 1ms |
| 图集重建 | 1024 字形、DPI 切换 | < 30ms（一次性，不计入滚动帧） |

### 5.3 性能面板

`F12` 开关；`feature="perf-panel"` 构建，release 默认编入但未开启时只写 8 个 `u16`（零旁路开销）。内容：① 帧时间瀑布图（8 阶段堆叠，最近 300 帧）；② `draw_calls / glyphs / rows_drawn`；③ `RowCache` 命中率与 miss 来源（首次/预取/版本失效）；④ `ann_queries / hits`；⑤ galley 缓存命中率与淘汰数；⑥ 掉帧计数（> 16.6ms）与最长帧；⑦ 一键导出 JSON 附 bug 报告。面板自身开销 < 0.2ms（固定容量环形缓冲，不分配）。

---

## 6 失败模式与降级

| # | 失败 | 检测 | 降级行为 |
|---|---|---|---|
| 1 | `read_at` 返回 `Err` / 超时 | Job 结果 | 块标 `Failed`，指数退避 50/200/800ms；3 次后停手 → 该行斜纹 + 角标"不可读"；不弹模态，不影响其它行 |
| 2 | `size` 变小（截断 / volatile）或存在空洞（`Caps.holes`） | 每 500ms `poll_change` + `len()` 比对；块内 `read` 部分失败 | 立即 clamp 光标与选区，超出部分画 EOF 区并一次性提示"文件已缩小至 N 字节"；空洞按 64 字节位图存 `Holes`，画红灰斜纹 + `!`（与"加载中"斜纹区分） |
| 3 | 字体缺字（UTF-16 / EBCDIC / CJK） | 图集 miss + fallback 链 | 逐字 fallback；fallback 宽度 ≠ `cell_w` → 渲染 `invalid_glyph` 并在 tooltip 显示 `U+XXXX`；缺字率 > 5% 顶部提示条建议切 HexII |
| 4 | 超长单行（bpr=64）宽度不足 | 布局期 `hex_w > avail_w` | **不做横向滚动**（破坏"行"概念且自绘横滚体验差）；运行时把绘制 `bpr` 降到最大可行值（32→16），偏移列后加 `⌁` 标记 + 提示"已按 32 字节/行显示"；**不写回配置** |
| 5 | 极端 DPI（0.75x / 4x） | `pixels_per_point` 变化 | 几何走 sp → 设备像素取整；`row_h` 取整后若裁切字形则 +1px；图集按 `ppb_bucket` 分桶，最多 4 份（< 16MB） |
| 6 | 结构树数百万节点 | `covering()` > 5ms/次 | 只画顶层 `units()` 色带，关 bracket 与深度阶梯；结构树面板自行虚拟化（`16`）；`deepest_at` 仍 O(log n) 不受影响 |
| 7 | Ann 爆炸（YARA 10 万命中） | 单行命中 > 32 | 按 `priority` 截断，行首显示 `+N` 角标；hover 展开列表 |
| 8 | `version` 不匹配（陈旧 Job 结果） | 主线程 drain 校验 | 静默丢弃 + 重新提交（`00 §6.3`），不报错 |
| 9 | 图集纹理创建失败 / GPU 设备丢失 | wgpu error scope | 重建图集重试 1 次；仍失败 → 关闭字形图集，退化"纯矩形块"模式（每字节 2 个 8×12 色块），状态栏报错一次 |
| 10 | 光标/选区越界（编辑后残留）或视口高度极小 | 每帧绘制前 clamp / `visible_rows` | `ByteRange::clamp(len)`（`00 §9.1`），选区变空即清空；视口 < 2 行时强制按 2 行绘制 |

统一原则（`00 §9.3`）：降级优于失败，所有提示都是"可关闭提示条 + 状态栏一行"，不用模态框打断滚动。

---

## 7 与其它子系统的契约

### 7.1 读（每帧）

| 来源 | 调用 | 频率 |
|---|---|---|
| `geode-buffer` | `len()` / `version()` / `read(range)` | 前两者各 1 次（缓存失效判据）；`read` 仅 miss 与预取，异步为主 |
| `geode-annot` | `query(visible_range)` | 1 次 |
| `geode-annot` 或 `StructureIndex::covering` | 结构覆盖 | 1 次（取实测更快者，见 Q2） |
| `geode-annot` | `query_point(cursor.off)` | ≤ 1 次（光标未动则用缓存） |
| 主题 | `ThemeSnapshot`（`Arc`，只读） | 仅变更时 |

### 7.2 写

| 目标 | 写什么 | 约束 |
|---|---|---|
| `geode-annot` | `Layer::Selection` / `Layer::Cursor` | 本 crate 是**唯一写入者**；每帧最多一次 upsert（先 `remove` 旧 Ann 再 `insert`），持 `AnnId` 避免整层 clear；多视图时 `tag` 带 `ViewId`，非活动视图不写这两层 |
| 自身 | `HexView`（滚动/光标/选区/配置） | 序列化给 `17-project-and-persistence`，字段带 `#[serde(default)]` |
| 命令 | `EditCommand{ overwrite / insert / remove }` → `04` | **只发命令**，从不直接 mutate Buffer |
| 事件 | `UiEvent::Perf(FrameReport)` | 仅 perf 构建 |

### 7.3 硬边界

- **不得**在渲染层物化"选区 → 字节 Vec"（10 万行以上必崩内存）；**不得**缓存 `Node` 拥有型副本，只持 `NodeRef`。
- **不得**依赖 `geode-search` 类型：搜索结果以 Ann 出现，渲染层不认识"命中"概念。
- **不得**用 `f32` 表示偏移或行号（仅用于像素与 alpha）。
- `Layer::Cursor` 优先级最高：对比度校验最后一步以光标为准，保证光标在任何高亮下可见。

---

## 8 配置项与默认值（§3.3 之外的补充）

| 键 | 默认 | 说明 |
|---|---|---|
| `view.default_mode` / `.overscan_rows` | `Continuous` / `0` | 初始模式；overscan 只预取不预画，> 0 破坏 G2，仅供调试 |
| `view.prefetch_blocks` | `1` | 可见块前后各预取几块（±68 屏） |
| `view.wheel_lines` / `.wheel_accel_max` | `3` / `30` | 滚轮每格行数 / 加速上限 |
| `view.scroll_step_preset` / `.thumb_min_px` | `[1,16,256]` / `20` | 滚动条粒度档位 / thumb 最小高度 |
| `view.settle_ms` / `.ghost_alpha` | `80` / `0.25` | 静止判定（触发精修）/ 陈旧内容透明度 |
| `view.caret_blink_ms` | `530` | 系统标准值 |
| `edit.input_merge_ms` / `.input_merge_max_ms` | `500` / `5000` | 输入聚合窗口 / 强制切分（§4.6） |
| `edit.illegal_pulse_ms` / `.paste_ambig_prompt` | `120` / `true` | 非法输入脉冲 / `ABCD` 类歧义是否弹选择 |
| `edit.delete_fill_byte` | `null` | 覆盖模式 Delete 的填充字节；`null` = 不写（§4.6 决策） |
| `overlay.max_bands_per_row` / `.max_brackets` / `.pinned_max` | `8` / `4` / `8` | 色带 / 括号缩进 / 固定节点上限 |
| `statistics.entropy_window` / `a11y.min_target_px` / `.reduce_motion` | `4096` / `18` / `false` | 熵滑窗 / 最小可点击行高 / 关闭平滑滚动与闪烁 |

- 配置版本 `hexview.cfg = 3`；缺失字段取默认（向前兼容），未知字段忽略但记 warn。
- **预设**：`Compact`（bpr=16, size=11, row_pad=0）、`Default`、`Wide`（bpr=32, size=15）、`Analysis`（bpr=16, grouping=4, overlay 全开）、`LowVision`（size=18, row_pad=4, min_contrast=7.0）。

---

## 9 测试策略

| 层 | 手段 | 覆盖 |
|---|---|---|
| 单元 | 表驱动 + proptest + 内联断言 | `row_of`/`col_of`/`off_of` 的 u64 边界、非 2 幂 `bpr`、EOF clamp、`visible_rows` 钳制；`off_of(row_of(o), col_of(o)) == o`（∀ o < len, bpr ∈ 1..=64）；`resolve` 对任意层组合对比度 ≥ `min_contrast`；滚动条映射单调、端点精确、thumb ≥ 20px |
| 快照 + 像素回归 | **`egui_kittest`**（固定 `pixels_per_point=1.0`、内置字体）+ RGBA 逐像素比对 | 12 场景：Default / bpr=1 / bpr=64 / HexII / Binary / Decimal / UTF-16LE / EBCDIC / 深嵌套 12 层 / 100 万字节选区 / 插入模式 / 块选区；同平台容差 0，跨平台比"行内色彩直方图差异 < 0.1%"（规避字体光栅化差异） |
| 基准 | `criterion` + 帧内 stage 计时 | §5.2 全部场景进 CI 门禁 |
| DPI / 字体 | 注入**故意缺 CJK 与部分 Latin** 的假字体 | 不 panic、列宽不变、缺字走 `invalid_glyph`、fallback 计数正确 |
| 压力 / 集成 | 100 万节点索引 + 每帧 `deepest_at`；无头跑 100GB 稀疏文件 | 反查 < 2µs 且降级路径被触发；滚动 10s 帧时间分布、`read_at` 调用 < 200 次 |
| fuzz | 粘贴识别器（hex / C 数组 / Base64）、Latin-1 / EBCDIC 解码 | 任意 UTF-8 输入不 panic、不产生超长输出、识别结果稳定；解码只产生 `invalid_glyph` |

---

## 10 开放问题

| # | 问题 | 备选 | 倾向 |
|---|---|---|---|
| Q1 | `bytes_per_row` 变更是否需确认（破坏性视图变更） | 立即生效 / 弹确认 | 立即生效 + 3s 内可 `Ctrl+Z` 撤回视图变更 |
| Q2 | 结构区间查询走 `AnnotationIndex::query`（Ann 化）还是直接 `StructureIndex::covering`（跳过 Ann） | 前者统一 / 后者快 5~10 倍 | 实测决定；倾向后者，代价是 Structure 层样式走独立路径 |
| Q3 | 块选区在插入模式下的语义（长度变化后列范围还成立吗） | 降级线性 / 保留列范围 + 提示 | 降级为线性（列语义依赖定长假设） |
| Q4 | 是否支持多光标 | 支持 / 不支持 | v1 不做：会让 Cursor 层、输入聚合、撤销分组同时复杂化 |
| Q5 | 100GB 下滚动条是否需要"大纲刻度"（顶层单元跳转标记） | 需要 / 不需要 | 需要，用 `units()` 生成，成本 O(units) |
| Q6 | UTF-16 是独立视图模式还是同视图内切换编码列 | 独立 / 同视图 | 同视图切换，但字符数变化必须显式提示 |
| Q7 | 对比度修正会破坏"层语义色"（用户认的红色被改黑） | 修正 / 不修正 | 修正，但保留装饰通道（下划线/左带）传达语义，色作辅助 |
| Q8 | `Painter` 能否撑住 5000 quad/帧，还是需 wgpu paint callback 实例化 | 纯 Painter / 自定义 callback | 先纯 Painter（预估 < 4ms）；p99 超标再做实例化 callback，接口已预留 |
| Q9 | 节点 `#[color]` 属性与主题 layer 色的优先级 | 属性优先 / 主题优先 | 属性优先，但受 `min_contrast` 与色盲模式二次约束 |
