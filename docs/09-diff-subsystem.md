# 09 — Diff 子系统（字节级与结构级）

> 状态：设计文档，未实现。上级契约见 `00-architecture-overview`，冲突以 00 为准。
> 本文**拥有**：字节级 diff 算法与参数、`EditScript`/`OffsetMap` 模型、结构级 diff、diff 结果缓存、apply-patch 的语义。
> 本文**不涉及**：并排/内联视图的渲染（见 05-render-and-hexview）、`PatchSet` 与保存事务（见 04-buffer-and-edit-model）、`Layer::Diff` 的内部实现（见 00 §5.3）。

---

## 1 目标与非目标

| # | 目标 | 验收 |
|---|---|---|
| G1 | 100MB vs 100MB **< 5s**，内存 **< 800MB** | 00 §7 + §5 |
| G2 | 产出的 `EditScript` 满足强不变量：**把脚本应用到左文件必得右文件** | 属性测试（§9.3） |
| G3 | 提供**双向偏移映射**，使插入一个字节后不会"后面全错位"（01 §3.6 的核心痛点） | 并排视图中 1 字节插入只染 1 字节 |
| G4 | 结构级 diff 输出**人可读字段表**（字段名/左值/右值/偏移） | BinSpec 解析成功即产出 |
| G5 | 结果可缓存复用，退出再进不重算 | §7.2 |
| G6 | 全程后台 + 可取消（100ms），UI 线程零占用 | 00 §6 |

**非目标（含否决理由）**

- **不用全量 Myers**：纯 Myers O(ND) 在"两个 100MB 且大量不同"的文件上 D 可达 10^8，直接不可用（01 §3.6 已指出）。本文采用**两阶段**（§4.1）：滚动哈希锚点 + 只在缺口上做精细 diff。
- **不做字节级三方合并**：见 §4.6——字节级"哪边对"没有语义，三方只在结构级/文件级远期成立。
- **不做"语义 diff"**（理解格式后判断"哪些字段是等价重编码"），那是结构级 diff 的 v2 延伸。
- **diff 不做编辑**：任何写操作必须回到 04 的 patch 事务（§4.5.6）。

## 2 依赖与边界

| 上游 | 用途 |
|---|---|
| `geode-core` | `ByteRange`/`Offset`/`Diag`/`CancelToken`（00 §4） |
| `geode-buffer` | 两侧只读快照 + `version()`；`patches()` 用于缓存 key（00 §5.2） |
| `geode-provider` | `read_at` / `Caps{seekable, volatile}` / `poll_change()`（00 §5.1） |
| `geode-bspec` | 结构级 diff 的 `Node` 树（02 §7.3） |
| `geode-annot` | `Layer::Diff` 标注（00 §5.3） |
| `memmap2` | 只读映射（不可映射时退回 `read_at` 分块，§4.8） |
| `xxhash-rust` | xxh3_64（强哈希） |
| `rayon` | 哈希分块并行 |

**下游**：`geode-ui`（并排/内联视图，§4.5）、`geode-project`（diff 会话持久化）、`04` 的 `PatchSet`（apply-patch 出口）。

**铁律**：① 不依赖 GUI crate；② 两侧数据全程**只读**，写入必须走 04 事务；③ 结果带 `(left_version, right_version)`，回主线程校验，不匹配丢弃重来（00 §6 规则 3）；④ 映射时**不整读文件**（G1 内存靠这个）；⑤ 每 64KiB / 每个超块检查 `CancelToken`。

## 3 数据结构与接口

```rust
pub struct DiffRequest { pub left: Side, pub right: Side, pub mode: DiffMode, pub params: DiffParams }
pub struct Side { pub doc: DocId, pub version: u64, pub provider_fp: Fingerprint }
pub enum DiffMode { Bytes, Structure { template: TemplateId, origin: u64 } }

pub enum EditOp {
    Equal   { left: u64, right: u64, len: u64 },
    Replace { left: u64, right: u64, left_len: u64, right_len: u64 },
    Delete  { left: u64, len: u64 },
    Insert  { right: u64, len: u64 },
}
pub struct DiffResult {
    pub ops: Vec<EditOp>,       // 按 left 升序（Insert 按 right 插入点定位）
    pub map: OffsetMap,
    pub stats: DiffStats,       // hunks / equal_bytes / changed_left / changed_right
    pub level: DiffLevel,       // Refined | BlockLevel(降级，§6)
    pub cache_key: Option<CacheKey>,
}
```

**偏移映射**用**分段表 + 二分**（不用区间树：段天然有序不重叠）：

```rust
pub struct MapSeg { pub left_start: u64, pub right_start: u64, pub len: u64, pub kind: MapKind }
pub enum MapKind { Equal, Gap }        // Gap: 该段内只有"最近锚点"可用，不能精确映射
pub struct OffsetMap { segs: Vec<MapSeg> }   // 1 万 hunk → 1 万段，40B/段 = 400KB

pub enum MapHit { Exact(u64), InGap { nearest: u64, skew: u64 }, Unmapped }
impl OffsetMap {
    pub fn l2r(&self, off: u64) -> MapHit;   // O(log segs)
    pub fn r2l(&self, off: u64) -> MapHit;
    pub fn seg_at_left(&self, off: u64) -> Option<&MapSeg>;
}
```

`OffsetMap` 是**同步滚动与高亮的基础**（§4.5），也是"apply patch 到另一侧"的定位依据。`InGap` 不是一个错误：并排视图在 Gap 段用"空白填充行"呈现，这就是"插入 1 字节只染 1 字节"的实现方式。

## 4 核心算法/流程

### 4.1 阶段一：滚动哈希分块对齐（rsync 风格）

**块大小自适应**（默认 32，范围 16~1KB）：

| 较大文件 | 16 字节 | ≤16MB | ≤256MB | ≤1GB | >1GB |
|---|---|---|---|---|---|
| `block_size` | 16 | 32（默认） | 64 | 128 | 256~1024 |

依据：块越小 → 锚点分辨率越高（能检测到的最小插入是 1 字节，因为滚动是逐字节的；但**块小 → 哈希表条目多**）。100MB / 64B = 1.6M 条目 × 16B ≈ 26MB，可接受；1KB 块用于 >1GB 场景把表压到同级。**块大小不改变可检测的最小编辑**（逐字节滚动），只影响"等长随机区的锚点密度"。

```
左文件建表（并行）：对每个 block-aligned 块 b[i] = left[i·bs, (i+1)·bs)
    weak   = adler-32(b[i])                         // 可滚动（弱，用于快速筛）
    strong = xxh3_64(b[i])                          // 强，抗碰撞
    表: HashMap<(weak, strong), SmallVec<u32, 2>>   // 键 → 左块下标（≤ max_key_occ 个）
    低信息块过滤：若同一 (weak,strong) 出现 > max_key_occ(32) 次 → 标记为非信息块，
    从表中剔除（全零/全同区块的哈希撞车风暴就在这一步被消灭，§4.4）

右文件滚动扫描（逐字节）：
    w = adler32(right[0..bs])；逐字节丢首字节、加尾字节更新 w（O(1)）
    查表：w 命中候选 → 算 xxh3_64 确认 → 得到锚点 (left_off, right_off)
    锚点确认后向两侧**逐字节扩展**（memcmp 前进/后退），最大化 equal run
    扩展完从 run 末尾继续滚动（跳过已匹配区）
    每个锚点记入 anchors: Vec<(l_off, r_off, len)>
```

**为什么可滚动 + 逐字节**：这就是**"滑动锚点搜索"**，它天然解决"偏移平移导致全文件错位"——在头部插入 1 字节后，右文件的所有左块内容出现在 `+1` 偏移处，逐字节滚动会在第 2 个字节命中，随后一次性扩展出整段 Equal。**否决了"块对齐扫描 + 偏移差修正"**：那块永远对不上，且需要 O(N²) 试偏移。

**并行化**：左表构建按 8MB 超块 rayon 并行（各块独立）；右滚动天然串行，但按 `P` 个超块并行启动（每片自带 `bs` 预热字节以初始化滚动窗口），合并时按 `r_off` 排序并丢弃重叠锚点。

### 4.2 阶段二：只在缺口上做精细 diff

缺口 = 相邻锚点之间的未匹配区间 `(l0..l1, r0..r1)`。按规模选算法：

| 缺口规模（两侧 max） | 算法 | 复杂度 |
|---|---|---|
| ≤ 16 KiB 且 `min(l,r) ≥ 1` | **Myers（线性空间，D 上限 = 缺口/2）** | O(D·(N+M)) |
| ≤ 64 KiB | **Patience/Histogram**，以 8 字节"词"为单元递归 | O((N+M)·log) |
| > 64 KiB 或细化预算耗尽 | **整段标 Replace（块级精度）** | O(1) |

**全局细化预算**：`max_total_refine_bytes = 32 MiB`（左右合计）。超出后剩余缺口一律标 Replace 并 `level = BlockLevel`——这是 G1 的护栏：最坏情况仍以 O(N) 结束。**否决了"无预算递归"**：那正是 Myers 退化的来源。

**复杂度汇总**：阶段一 O(N) 时间 + O(N/bs) 空间（左右各一份，26MB@100MB）；阶段二受 `max_total_refine_bytes` 硬约束，≈ 107/16 = O(6.7M) 个 D-step，实测量级 < 500ms。**总 O(N) + 有界非线性项。**

### 4.3 阶段三元信息：把 Equal run 切细

锚点 + 缺口过后，`ops` 里可能出现超长 Equal 段（中间被插入打乱又重新对齐的部分被合并成大 Replace）。**只对 Replace 段做尾/首修饰**：若 `Replace{left, right, llen, rlen}` 中两侧存在 ≥ 16 字节的公共前缀/后缀，则把它拆成 `Equal + Replace' + Equal`，重复 ≤ 2 次。收益：把"整块红"变成"只染真正差异"，视觉噪声骤降。

### 4.4 退化场景与对策（必读）

| 场景 | 为什么坏 | 对策 |
|---|---|---|
| **完全不同的两个文件** | 阶段一无锚点 → 1 个巨型缺口 → 若进阶段二就是 Myers 地狱 | 缺口 > 64KiB 即整段 Replace，输出 1 个 Replace op，**耗时 O(N)，瞬间返回**。这是"不退化"而非"不出错" |
| **全 0 / 全同字节文件** | 所有块哈希相同 → 表中键碰撞风暴（每条查找候选数千） | `max_key_occ = 32`：超过即判为低信息块并从表中剔除。剔除后仍能正确输出（回落为偏移对齐 + 字节比较），只是锚点少 |
| **头部插入 1 字节（全文件平移）** | 块对齐方案会判定"全文件都变了" | **逐字节滚动锚点**天然解决（§4.1），输出 1 个 Insert + 1 个超长 Equal |
| **长段重复数据**（两个相同的大段在不同位置） | 滚动会命中错误候选，锚点错配 → 映射抖动 | 候选确认后做双向扩展并取**最长 run**；并对"同一左块被匹配两次"做保护：一个左块只能被消费一次（贪心、按右文件顺序） |
| **只有末尾追加** | 无 | 首锚点即覆盖全文，1 个 Equal + 1 个 Insert |
| **两侧长度差 > 50% 且不同** | 锚点稀疏 | 走"完全不同的两个文件"路径（块级 Replace），非失败 |

### 4.5 结构级 diff

**输入**：两侧各一棵 BinSpec `Node` 树（02 §7.3），**必须用同一模板版本**（`template_id` + `origin` 一致；不一致则告警并禁用结构级）。

**节点身份判定**：`(name, type_name, 相对路径)`。相对路径用"父路径 + 兄弟序号区间"，这样同名同类型数组元素可区分。

```
diff_node(l, r):
  1. 两侧都是 Compound → 对 children 做序列比对（LCS）：
     - 键 = (name, type_name)；同名同类型视为候选同源
     - 规模小（|children| ≤ 1024）：Patience LCS（唯一键优先，O(k log k)）
     - 超 1024：先用"键的多重集差分"分离能唯一配对的，再对剩余连续段递归
  2. 配对成功的子节点递归下钻
  3. 只在左侧出现的子树 → Removed；只在右侧 → Added
  4. 两侧都是叶子（标量/bytes/string）：
     - value 不同 → FieldDiff { kind: ValueChanged }（左值/右值/两侧偏移）
     - value 相同但 offset 不同 → FieldDiff { kind: Shifted }（"值没变，只是位置漂了"）
  5. 一方 Unresolved（02 §S1 软失败）→ kind: Unresolved（不当作差异，只在表里标灰）
```

**输出（字段级差异表）**：`字段路径 / 字段名 / 左值 / 右值 / 左偏移 / 右偏移 / 差异类型(ValueChanged|Shifted|Added|Removed|Unresolved)`。

**相对字节 diff 的价值**：① 人可读——"`width: 1920 → 1280`"而不是"0x1A..0x1E 变了"；② **不受字节插入影响**——节点按 `name` 匹配而非按 offset，插入 4 字节后不会全盘错位（这正是 01 §3.6 说的"结构化 diff 基本没人做"的空档）；③ 可过滤噪声（padding/checksum 字段可被模板属性标记为 ignorable）。**否决"把结构级作为唯一 diff"**：模板覆盖不到的区域（文件尾、未知块）会丢失信息，故结构级**叠加**在字节级之上，字节级始终是底稿。

**失败场景**：模板解析失败（magic 不匹配 / 预算耗尽）→ 无结构 diff，退化为纯字节 diff + 一条可关闭提示"模板不适用，仅显示字节差异"；一侧解析成功一侧失败 → 只显示成功侧的结构树 + 字节 diff；结构未覆盖的区间标 `Uncovered`。

### 4.6 三方 diff（远期）

`base/left/right`。**为什么难**：字节级三方合并需要"哪一侧是对的"这一判断，而这在字节上没有语义——两个改动落在同一字节区间时，唯一正确的做法是让用户裁决，但字节级甚至无法确定"这是同一处改动"。所以：

- 字节级只提供**冲突检测**（`left` 与 `right` 的 hunk 在 base 坐标上相交 → 标 Conflict），不提供自动合并。
- 真正的三方合并只在**结构级**成立（字段粒度上有语义），且需要 `base` 与两侧用同一模板。
- 近期折中（v1.5 可做）：算两次独立 2-way（base→left、base→right），用 base 坐标求交集区间高亮冲突。成本是 2 倍 diff，但语义清楚。

### 4.7 差分呈现与 apply-patch 所需的数据（UI 契约）

```rust
pub struct Hunk { pub op_range: Range<u32>,   // ops 下标区间
                  pub left: ByteRange, pub right: ByteRange, pub kind: HunkKind }
pub struct DiffView { pub hunks: Vec<Hunk>, pub map: OffsetMap, pub stats: DiffStats }

/// 并排视图：按"对齐行"取数，O(log h)
pub struct AlignedRow { pub left: Option<u64>, pub right: Option<u64>, pub kind: RowKind }
impl DiffView { pub fn rows(&self, first: u64, count: u32, bytes_per_row: u16) -> Vec<AlignedRow>; }
```

- **并排（side-by-side）**：滚动轴是**对齐坐标系**（不是各自的文件偏移）——由 `EditScript` 展开的"对齐行"序列定义。这样插入段两侧都不漂移。`rows(first, count)` 二分定位，O(log h + count)。
- **内联（inline）**：只需 `hunks` + 当前文档的 `OffsetMap`，用 `Layer::Diff` 标色，并在 `Delete` 段插入"虚拟行"（"此处少 12 字节"）。内联不要求 `AlignedRow`。
- **同步滚动**：`map.l2r(off)`；`Exact(r)` 直接对应行，`InGap{nearest, skew}` 显示在最近锚点行 + 一条"±N 字节偏差"提示。**否决"两侧各自滚动 + 事后修正"**：会有可见的抖动。
- **上一处/下一处**：对 `hunks` 二分（按 `left.start`），到边界循环。
- **差异计数**：`hunks` 数 / `changed_left` / `changed_right` / `equal_bytes`，UI 显示为 `12 处差异（左 340 B / 右 352 B）`。
- **apply as patch**：把某一侧 hunk 区间应用为 patch 到另一侧，产出 04 的 `PatchSet`：
  - `Replace` → `remove(left_range)` + `insert(left_start, right_bytes)`
  - `Delete` → `remove(left_range)`；`Insert` → `insert(left_start, right_bytes)`
  - **约束**：只能在 hunk 边界应用（整 hunk 或整段 Equal）；一次应用 = 一个 `checkpoint` = 一个撤销单元；字节内容必须从**右侧快照**读入而非从 `EditScript` 重建（脚本不含字节）。
  - 提供"应用此差异"/"应用全部从左到右"/"反向应用"三个动作。

### 4.8 分块、内存与 I/O 策略

- **不整读**：优先 `memmap2` 只读映射（100MB × 2 映射开销 ≈ 0，页面按需缺页）；判定规则与 06-data-providers §4.3 一致：`Caps.seekable == false` 或 `volatile == true` 时禁用 mmap，退回 `read_at` 分块（4MB 块 + 06 §4.10 的统一分块缓存，LRU 保留 4 块 = 16MB/侧）。
- **mmap 安全**（01 §3.1 坑 1）：打开时记录 `size`；每个超块读取前 `poll_change()`，`Resized`/`Unmapped` 即中止 Job。**不依赖 SIGBUS 捕获**（Rust 无法安全地在 SIGBUS 后继续），因此 `volatile` 源永远走 `read_at`。
- **分块并行哈希**：左文件按 8MB 超块 rayon 并行算 `(weak, strong)` 序列；右文件滚动按 `P` 片并行（每片 `bs` 预热字节）。
- **峰值内存**（100MB 对 100MB）：哈希表 26MB×1（只有左表）+ 左右超块缓冲 8MB×8 线程 = 64MB + `ops`/`map`（1 万 hunk ≈ 0.5MB）+ 结构树（若开结构级，10MB 文件典型 5~20MB）。**典型峰值 ≈ 150MB，硬上限 800MB 留给"块级降级前的最坏碎片"与结构级大树。**

## 5 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 100MB vs 100MB（10% 差异） | **< 5s**（目标 3s） | 00 §7 |
| 100MB vs 100MB（完全相同） | < 400ms | 指纹相同则直接短路（§7.2） |
| 100MB vs 100MB（完全不同） | < 2s | 1 个 Replace，块级 |
| 4GB vs 4GB | < 60s（目标 45s） | 块大小自动升到 256 |
| 内存峰值 | < 800MB（典型 < 200MB） | 已回填 00 §7「Diff 内存峰值 < 800MB」 |
| 单 hunk 细化 | < 50ms | 16KiB 缺口 |
| 并排视图取 60 行 | < 1ms | 任意 hunk 数（二分） |
| 取消延迟 | < 100ms p99 | |

数据：`bench/data/` 生成器（相同对、1 字节插入对、10% 随机改写对、完全不同对、全零对、4GB 稀疏对）；CI 门 `elapsed <= budget * 1.1`，内存用分配器统计（`dhat` 或 `stats_alloc`）。

## 6 失败模式与降级

| # | 失败 | 检测 | 处理 |
|---|---|---|---|
| F1 | diff 期间文件被改 | 回主线程校验 `(left_version, right_version)`；超块前 `poll_change()` | 丢弃结果，**自动重试 1 次**（带 200ms backoff），仍失败则提示"文件在比对期间持续变化，请关闭外部写入" |
| F2 | 内存不足 | 分配失败 / 峰值超 `mem_budget` | **降级为块级 diff**（跳过阶段二三，只输出 Equal/Replace，`level = BlockLevel`），并在差异计数旁标"块级精度（1 块 = 64 B）" |
| F3 | 细化预算耗尽 | `max_total_refine_bytes` | 剩余缺口标 Replace + `level=BlockLevel`（不算失败） |
| F4 | 模板解析失败 | `geode-bspec` 返回错误 | 无结构 diff，退字节 diff + 可关闭提示条（§4.5） |
| F5 | 两侧模板不一致 | `template_id`/`origin` 不同 | 禁用结构级 + 提示"两侧使用了不同模板，无法字段级比对" |
| F6 | provider 不可 seek | `Caps.seekable == false` | 单趟顺序读；不支持"从末尾反向"的锚点优化，速度约降 30% |
| F7 | 缓存损坏 | header CRC32 不符 / 版本不符 | 静默丢弃重建，不报错 |
| F8 | 用户取消 | `CancelToken` | 保留已完成部分？**否**——半份 diff 会误导，直接丢弃并恢复原视图 |
| F9 | 单侧为空文件 | `len == 0` | 直出 1 个 Insert/Delete，O(1) 快捷路径 |
| F10 | 两侧相同（同指纹） | 指纹/内容比较 | 直出 0 op 的 `DiffResult`（`hunks` 空），并显示"两个文件完全相同" |

原则（00 §9 原则 3）：diff 失败绝不影响编辑器其它功能；**降级必须可见**（`level` 字段驱动 UI 上的"块级精度"标记）。

## 7 与其它子系统的契约

### 7.1 各子系统的读写边界

- **与 Buffer**：只读 `read`/快照（04 §3.7）；记录两侧 `version()`（04 §7.3 版本协议）；apply-patch 走 04 的三原语 + `checkpoint`（一个 hunk = 一个撤销单元），产物是 04 §4.6 的 `PatchSet`，**不得**绕过 patch 事务直接改 Buffer。
- **与 Provider**：`read_at` 越界返回 `Err`，diff 侧一律 `clamp`（00 §9 原则 1）；mmap 走 06-data-providers §4.3 的同一套判定（`files.mmap_threshold_mb` 见 17 §3.2）；`volatile` 源禁 mmap、禁缓存（§7.2）。
- **与 AnnotationIndex**：写 `Layer::Diff`，`AnnTag::DiffHunk(hunk_index)` 供点击跳转；清理时 `clear_layer(Layer::Diff)`。跨层 z 序由 05-render-and-hexview §4.7 固定为 `Layer` 枚举序（`Diff` 高于 `Search`/`Structure`、低于 `Yara`/`Cursor`），层内 `priority` 统一 0，hunk 类型（增/删/改）的区分靠 **色相**而非 priority。
- **与 geode-ui**：只暴露 `DiffView { hunks, map, stats }` + `rows()`；UI 不得窥探 `ops` 的内部结构（否则算法一改就崩）。渲染规则见 05-render-and-hexview。
- **与 geode-bspec**：结构级 diff 需要 `Node` 的 `(name, type_name, offset, size, value, path, errors)`；不修改 Node 树。增量重解析（02 §7.4）导致的 Node 树替换会让结构 diff 结果失效 → 用 `(template_id, buffer_version)` 判定。
- **与 geode-project**：diff 会话（两个 `DocId` + 参数 + 缓存 key）可存入工作区（17-project-and-persistence）。
- **不与 geode-search 耦合**：不共享块调度（阈值与语义不同），但共享"结果带版本号 + 校验丢弃"的规约。

### 7.2 diff 结果缓存（持久化）

**为什么必须缓存**：100MB 对的一次 diff 是 3~5 秒，用户在"两个文件间来回看"时会反复触发；100GB 级更是分钟级（本期不支持 >4GB，见 §10 Q1）。

```rust
pub struct CacheKey { pub left_fp: Fingerprint, pub right_fp: Fingerprint, pub params_hash: u64 }
pub struct Fingerprint { pub len: u64, pub mtime_ns: u128, pub sampled_xxh3: u64 }
```

- **指纹不能整文件哈希**（100GB 要几分钟）：`sampled_xxh3` = 对 `[0..1MiB)` ∪ `[len-1MiB..len)` ∪ 中间 8 个等距 64KiB 采样块的 xxh3 串联。碰撞概率对"人改过的文件"足够低；命中后仍会做**首尾 1MiB 的逐字节校验**（廉价二次确认）。
- **`params_hash`**：`(block_size, max_total_refine_bytes, mode, template_id, level)` 的稳定哈希——参数不同结果不同。
- **存哪**：平台缓存目录 `.../geode/diff/<key_hex>.dmsg`；自有紧凑格式：`magic "GDD1" | 格式版本 u16 | params_hash | crc32(header) | ops 段（定长 32B/条） | map 段`。只存 `ops`（可重算 `map`，但存了省时间）。
- **失效**：指纹不匹配 / 格式版本不符 / crc 错 / `params_hash` 不符 → 丢弃重建。**`volatile` 源不写缓存**。
- **容量**：默认上限 **2GB**、条目上限 500，LRU 淘汰（按 `last_used` 存在同目录索引文件）。单条超过 256MB 不缓存（避免大条目驱逐一切）。
- **缓存 key 里的"未保存编辑"**：若一侧 Buffer 有改动，`left_fp` 用 `(provider_fp, patches_hash)`——即 `PatchSet` 的哈希（00 §5.2 `patches()`）。这样"改一个字节再 diff"不会误命中旧缓存。
- **短路优化**：`left_fp == right_fp` 且两侧 `version` 相同 → 直接返回空 diff（F10），无需读任何字节。

## 8 配置项与默认值

落盘位置与 `schema_version` 见 17-project-and-persistence §3.2；缓存目录的清理与原子写遵守 17 §4.10。

| 键 | 默认 | 范围 | 说明 |
|---|---|---|---|
| `diff.block_size` | Auto（32） | 16~1024 / Auto | 按文件大小自适应，§4.1 |
| `diff.myers_max_gap` | 16 KiB | 1KiB~1MiB | 阶段二 Myers 上限 |
| `diff.patience_max_gap` | 64 KiB | 16KiB~1MiB | 缺口升级到 Patience 的上限 |
| `diff.max_total_refine_bytes` | 32 MiB | 1~256MiB | 全局细化预算 |
| `diff.max_key_occ` | 32 | 4~256 | 低信息块剔除阈值 |
| `diff.anchor_extend` | true | | 锚点双向逐字节扩展 |
| `diff.trim_common_affix` | true | | §4.3 前缀/后缀修饰 |
| `diff.mem_budget` | 800 MiB | 128MiB~4GiB | 超限降块级 |
| `diff.threads` | 0（自动） | 0~64 | |
| `diff.use_mmap` | Auto | Auto/Always/Never | Auto = 可 seek 且非 volatile |
| `diff.structure.enabled` | true | | 有模板时自动附加结构 diff |
| `diff.structure.max_children` | 1024 | 64~16384 | 超出走多重集差分 |
| `diff.structure.ignore_padding` | true | | 忽略 `padding` 类型字段的差异 |
| `diff.cache.enabled` | true | | |
| `diff.cache.max_bytes` | 2 GiB | 0~32GiB | 0 = 禁用 |
| `diff.cache.max_entries` | 500 | | |
| `diff.cache.max_entry_bytes` | 256 MiB | | 超大条目不缓存 |
| `diff.ui.aligned_axis` | true | | 并排滚动轴用对齐坐标（§4.7） |
| `diff.ui.auto_scroll_first_hunk` | true | | 完成后滚到第一处差异 |

## 9 测试策略

**9.1 黄金用例**：`tests/golden/*.json`（`{left_hex, right_hex, params, expected_ops}`），覆盖：1 字节头插、1 字节中插、1 字节尾插、多段替换、块边界处插入、纯删除、全等、空对空、空对非空、单字节对单字节。用**定长块（如 4 字节）**跑，使期望脚本可手算。

**9.2 强不变量（最重要的一条）**：`apply(ops, left) == right`。用随机变异（插入/删除/替换/复制段/平移 + 各种长度 0~64KiB）生成 10 万个 `(left, right)` 对，断言脚本应用后逐字节相等。**这一条能抓住 90% 的算法 bug**，且不依赖"最优 diff"（我们不追求最小编辑脚本，只追求正确 + 可读）。

**9.3 属性测试**：
- `ops` 覆盖两侧全区间、不重叠、无缝（`left` 侧连续拼满 `[0, len)`，`right` 侧同理）。
- `map.l2r(map.r2l(x)) == x`（在 Equal 段内）；`InGap` 返回的 `nearest` 一定落在某个 Gap 段内。
- 确定性：同一输入两次跑出完全相同的 `ops`（并行无关）。
- 不同 `chunk`/`threads` 配置下结果相同。

**9.4 退化用例组**（每项都是 §4.4 的回归测试）：完全随机两文件（断言 1 个 Replace 且 < 2s）、全零 vs 全零（断言 0 op）、全零 vs 全零尾部 +1 字节、头部插入后整体平移 1/7/64/1025 字节、长重复段（同一 4KB 块出现 1000 次）。

**9.5 结构级**：手写 BinSpec 模板（ZIP local header）+ 手工改字段值/插入字段/重排数组 → 断言字段级表内容；模板解析失败 → 断言退化为字节 diff 且不 panic；两侧模板不一致 → 断言禁用并给出 `Diag`。

**9.6 fuzz**：随机 `(bytes_a, bytes_b)` + 随机参数 → 断言不 panic、内存不超预算、`apply` 不变量成立；对 `OffsetMap::l2r/r2l` 做随机偏移 fuzz，断言绝不 panic 且结果单调。

**9.7 基准**：§5 全表 + 缓存命中路径（断言命中 < 100ms）、并排 `rows()` 取 60 行 < 1ms。

**9.8 集成**：diff 期间编辑一侧 → 断言结果被丢弃并重试（F1）；apply-patch 后撤销一次 → 断言完全复原（04 事务回归）；缓存写入/读取/损坏/淘汰。

## 10 开放问题

| # | 问题 | 倾向 |
|---|---|---|
| Q1 | 是否支持 > 4GB（乃至 100GB）的 diff？ | 本期上限 4GB（对齐 00 §7 的测试条件）。100GB 需要"分片 diff + 只看指定窗口"，是独立特性；倾向 v2 且只对 `Known` 尺寸源 |
| Q2 | 块大小自适应公式的最优形式？ | 现用查表（§4.1）。理论上 `bs ≈ sqrt(len)/64` 更平滑，但需基准裁决；当前表是工程近似 |
| Q3 | 是否引入 `bsdiff` 风格的"近似匹配 + delta 编码"（把插入表现为 copy+insert 指令流）？ | 对"生成可分发补丁"有价值（IPS/delta patch 导出），对"人看差异"价值低。倾向 v2，且与 `PatchSet` 导出解耦 |
| Q4 | 结构级 diff 的"节点身份"用 `(name,type,path)` 够吗？数组重排/重命名会被判为全删全增 | 可能需要"相似度匹配"（值+类型+位置加权）。需真实样本验证，暂不加启发式（宁可保守） |
| Q5 | 三方 diff 是否值得做？ | 见 §4.6：字节级只做冲突检测，结构级才做合并。客户场景不明，倾向 v1.5 做"两个 2-way + 冲突高亮"折中 |
| Q6 | 缓存格式是否复用项目的序列化格式（17）？ | 倾向独立格式 + 独立版本号：cache 是易失数据，不值得为它承担项目文件格式的兼容负担 |
| Q7 | 并排视图的"对齐行"是否要支持"按结构节点对齐"（结构级对齐轴）？ | 与 Q4 绑定；若结构级 diff 成熟，对齐轴可切到"字段对齐"，这对人读最友好，但实现复杂度显著上升 |
| Q8 | 差异计数里要不要暴露"字节相似度百分比"？ | 廉价（equal_bytes / max(len)）且用户喜欢；但要避免被误读为"文件相同度"。倾向显示但标注"按字节计" |
