# 07 — 数据处理器节点图（显示前变换）

> 上游契约：见 00-architecture-overview §5.1（`Provider`/`Caps`）、§5.4（`Job`/`CancelToken`）、§7（性能预算）、§8（术语：**Graph/Node 图** 与 BinSpec 的 **Node** 严格区分）。
> 本文定义"视图看到的字节 ≠ 源字节"这件事的全部机制。**不**定义编辑（见 04-buffer-and-edit-model）、**不**定义渲染（见 05-render-and-hexview）。

---

## 1 目标与非目标

处理器图回答一个问题：**磁盘上是密文，用户想看明文，但绝不改动那一个字节。**

### 1.1 核心不变量（违反即缺陷）

| # | 不变量 | 落地手段 |
|---|---|---|
| I1 | **只影响"视图看到的字节"，绝不修改源** | 类型层面落实：节点拿到的 reader 是 `&dyn SourceRead`（**没有** `write_at` 方法），处理器图 crate 不依赖 `geode-buffer`、不 import 任何写接口 |
| I2 | **变换确定性**：同输入 + 同配置 → 同输出，无随机、无时间依赖 | 缓存、块索引、跳转全部建立在"同一 view 偏移永远对应同一源偏移"之上；节点实现里禁止 `rand`/`now()`（代码审查项） |
| I3 | **视图坐标稳定**：源版本不变时 `view_off` 恒定；源一变，全部视图失效 | 缓存键含 `source_version`；`Resized`/`Replaced` → 整图缓存清空、索引失效 |
| I4 | 一切诊断同时带 **view 区间 + source 区间** | `ProcError` 必须两个都填，缺失即编译不过 |
| I5 | 密钥不落盘、不进日志、不进 `Debug` | `SecretKey` 复用 06 的 `Secret` 语义；`Debug` 手写为 `REDACTED` |

### 1.2 能力矩阵（编辑相关，必须有明确答案）

| 变换类别 | 随机读 | 反算写（unmap） | 视图内编辑 | 索引内存 | BinSpec 解析 | 导出副本 |
|---|---|---|---|---|---|---|
| 1:1 可逆定长（XOR/位反转/字节序）| O(1) | 精确 | ✅ 写回源 | 0 | ✅ | ✅ |
| 1:N / N:1 可逆（解压/抽取/转置）| O(log n) + 块内 | 精确（块内）| ⚠️ 仅"同长覆盖"且块已构建 | ≈ 输出/2048 | ✅ | ✅ |
| 不可逆 / 未知长度（未知大小解压）| 退化顺序 | 不可能 | ❌ 只读 | 水位 + 索引 | ✅（只读）| ✅ |
| 多输入（拼接 / 三源 XOR）| O(log m) | 仅单段可写 | ❌ 只读 | 段表 | ✅ | ✅ |

**决策**：v1 的"视图内编辑"仅在 `MapQuality::Identity | Exact` 且全链节点 `reversible == true` 时开启，写成 `unmap → 写源`。**否决**对任意图做反算：CBC 模式下改一个明文块需要重加密并重写整个密码块，且下游若有 padding 依赖会整体错位，语义无法向用户解释。

### 1.3 非目标

- **否决处理器图承载"加密后再保存"**：加密+写回属于 04 的保存事务与一个独立的"导出加密副本"功能，不是图的职责。
- **否决图内做 YARA/搜索**：搜索是 08 的引擎，图只提供 `ViewMap` 让它读。
- **否决任意用户脚本立即上线**：自定义节点（WASM/BinSpec）标为远期，且必须走沙箱预算（见 02-dsl-spec-draft §7.2）。
- **否决"透明美化"**（如自动识别并解压）：图必须由用户显式建立，UI 只在打开时**建议**（一次性提示条），绝不自动应用加密/解压。

---

## 2 依赖与边界

`geode-proc` 依赖 `geode-core`、`geode-provider`（只读接口）、`geode-bspec`（远期，自定义节点用）；不依赖 `geode-buffer`、`geode-ui`。

| 外部 crate | 用途 | 备注 |
|---|---|---|
| `aes` / `ctr` / `cbc`（RustCrypto）| AES-CTR / AES-CBC 解密 | 只用经过审计的常量时间实现，**不自己写分组密码** |
| `rc4` | RC4 keystream | 仅为兼容遗留格式；UI 标注"RC4 已不安全" |
| `flate2` / `lz4_flex` / `zstd` | zlib/gzip/lz4/zstd | 统一包一层 `BlockDecoder` 接口 |
| `zip` / `tar` | 归档成员枚举 | 只用索引与偏移，不用它的读取路径 |
| `base64` / `ascii85` | 编解码 | 流式 API |

**上下边界**

| 方向 | 契约 |
|---|---|
| 上游 05/08/09 | 只通过 `ViewMap` + `read_view(range)` 访问数据，不接触节点实现 |
| 上游 02（BinSpec）| 给它一个"视图坐标的 reader"，`Node.offset` 即视图偏移 |
| 上游 04（Buffer）| 只能通过 `unmap` 提议写回；写回的真实落盘由 04 决定 |
| 下游 06（Provider）| 只读，`SourceRef::Root` |

---

## 3 数据结构与接口

### 3.1 坐标系与引用

```rust
pub struct NodeId(pub u32);
pub struct PortIdx(pub u8);

pub enum SourceRef {
    Root,                    // 文档的原始 Provider
    Node(NodeId, PortIdx),   // 某节点的第 n 个输出端口（分支预览 / 多输出）
}

/// 视图坐标系里的一个区间 → 若干源区间。
/// 返回空 vec = "该段尚未构建 / 不可映射"，上层据此触发按需构建。
pub trait ViewMap: Send + Sync {
    fn map_range(&self, view: ByteRange) -> Vec<(SourceRef, ByteRange)>;
    fn unmap(&self, src: (SourceRef, ByteRange)) -> Option<ByteRange>;
    fn quality(&self) -> MapQuality;
    fn view_len(&self) -> SizeInfo;      // 复用 06 的 SizeInfo
    fn built_upto(&self) -> u64;         // 顺序构建水位（Identity/Exact 恒为 view_len）
    fn ensure(&self, view: ByteRange, ctx: &JobCtx) -> Result<(), ProcError>;  // 按需构建
}

pub enum MapQuality { Identity, Exact, BlockIndexed, OneWay }
```

**`map_range` 返回 `Vec` 而非单值**的三个真实原因：① 拼接/多输入时一个 view 区间横跨多个源；② 块索引未建好时只返回已确定的部分（`Vec` 可能比请求短）；③ holes 源会切成可读段。**返回值必须满足**：所有返回区间的 `len` 之和 ≤ 请求 `len`，`unmap` 端不做此保证。

**`unmap` 的歧义处理**：同一源字节可能出现在多个视图位置（如"重复引用"节点）。此情形 `unmap` 返回 **canonical（首个）**位置，并通过 `MapQuality` 之外的另一个查询 `fn unmap_ambiguous(&self, ..) -> bool` 暴露歧义；上层写回时若为 true 则拒绝编辑。

### 3.2 节点元数据与 trait

```rust
pub struct NodeMeta {
    pub fan_in: u8,            // 输入端口数 1..=8
    pub fan_out: u8,           // 输出端口数（纯 1:1 节点为 1；归档遍历为 N）
    pub reversibility: Rev,    // Exact | BlockIndexed | OneWay
    pub needs_full_buffer: bool,   // 必须看到全部输入才能产出（转置、正则替换）
    pub streamable: bool,          // 能否逐块产出（决定能否"边建索引边可读"）
    pub block_size: u32,           // 建议索引块大小（字节），0 = 不适用
    pub max_output: u64,           // 该节点的输出上限（解压炸弹的第一道闸）
}

pub trait ProcNode: Send + Sync {
    fn meta(&self) -> NodeMeta;
    /// 建立本节点的地址映射（相对其输入端口），1:1 节点返回 IdentityMap
    fn build_map(&self, inputs: &[Arc<dyn ViewMap>], ctx: &JobCtx) -> Result<Arc<dyn ViewMap>, ProcError>;
    /// 逐块变换：in_block 是**对齐到 block_size 的源块**，out 是输出缓冲
    fn transform_block(&self, in_port: PortIdx, in_block: &[u8], src_off: u64,
                       out: &mut [u8]) -> Result<usize, ProcError>;
    /// 1:1 可逆节点的组内置换（如 4 字节字节序翻转），默认无操作
    fn permute(&self, _buf: &mut [u8], _group_off: u64) {}
}
```

### 3.3 图结构

```rust
pub struct NodeGraph {
    nodes: SlotMap<NodeId, Node>,     // 稳定 id，重排不失效
    edges: Vec<Edge>,                 // 端口级连线
    version: u64,
}
pub struct Node { id: NodeId, kind: NodeKind, cfg: NodeConfig, meta: NodeMeta,
                  name: String, bypassed: bool, ui_pos: (f32, f32) }
pub struct Edge { from: (NodeId, PortIdx), to: (NodeId, PortIdx) }
pub enum NodeKind { Xor, AesCtr, AesCbc, Rc4, Rot, BitReverse, ByteSwap, NibbleSwap,
                    Transpose, Decompress { algo: Algo }, Archive { fmt: ArchiveFmt },
                    Concat, Slice, RegexReplace, Base64Dec, Ascii85Dec, Script }
```

**坐标系链**：每个节点持有"输出坐标系 → 其输入端口坐标系"的 `ViewMap`。整体映射是这条链的**惰性合成**：`ComposedMap { outer, inner }`，`map_range` 逐级下推到 `SourceRef::Root`。合成深度上限 **16**（超过则 UI 拒绝新增节点并提示"处理器链过深"），原因是每级都是 `O(log n)` 或 `O(1)`，16 级后单次映射耗时仍在 µs 量级。

### 3.4 缓存

```rust
pub struct NodeCache { budget: usize /* 默认 8MB */, lru: LruMap<BlockKey, Arc<Block>> }
pub struct BlockKey { node: NodeId, out_port: PortIdx, block_idx: u64, src_version: u64 }
```

块大小 = `max(meta.block_size, 4KB)`，默认 64KB。命中率靠"滚动方向的下一块预取"保证。

---

## 4 核心算法/流程

### 4.1 四类映射方案（本文技术核心）

**(A) 1:1 可逆定长变换 —— 不需要索引表。**

关键洞察：XOR / 位反转 / 字节序翻转 / 半字节交换 / ROT 全部是**偏移恒等的组内置换**。即 `src_off == view_off` 恒成立，只是读出来的字节要按"组"重排。

- `map_range(v) = [(Root, v)]`，`quality = Identity`；`unmap` 是恒等。
- 真正的变换发生在 `transform_block` + `permute(buf, group_off)`：读一组对齐字节（4 字节组的翻转要读 `[g*4, g*4+4)`）再重排。
- **收益**：零索引内存、零构建时间、O(1) 随机访问、`unmap` 精确。这就是"1GB 视图滚动无额外延迟"能成立的全部原因。
- 注意：**字节序翻转是 1:1**（4 字节组的置换），**不是** N:1，容易搞错。

**(B) 1:N / N:1（解压、抽取、转置）—— 块索引表。**

```rust
struct BlockEntry { view_start: u64, view_len: u32, src_off: u64, src_len: u32 }  // 32 字节
struct BlockIndex { entries: Vec<BlockEntry>, built_upto: u64, stride: u32 }
```

- **每 64KB 输出块**记一条（`view_len` 是输出长度，`src_len` 是消耗的输入长度）。查找用二分 `O(log n)`，块内查偏移 O(1)。
- **增量构建**：从 `built_upto` 顺序推进，每产出一个块就 append 一条并把 `built_upto` 前移。`ensure(view_off)` 只构建到目标块为止，随即返回，其余交后台继续。
- **内存上限**：1GB 输出 / 64KB = 16384 条 × 32B = **512KB**；`index_max_entries = 262144`（≈8MB 索引，约 16GB 输出）。超限后 **`stride *= 4` 转稀疏锚点**：只保留每 `stride` 块的首条，随机读从最近锚点顺序解码 ≤ `stride` 块（4 块 = 256KB 输出，zstd 约 1ms，可接受）。**否决"加倍块大小重建"**：重建要重跑整条解压流，成本远高于稀疏锚点。
- **随机读到未构建块**：`map_range` 返回空 vec → 上层显示"构建中"占位（不是错误）→ 发起 `BuildIndexJob(target)`：**优先构建目标块**，其次补 `[built_upto, target)`，最后后台继续推进。预取窗口 2MB。
- 典型：zlib/gzip/lz4/zstd 解压、tar/zip 成员抽取、按行/按记录切分。

**(C) 不可逆 / 未知长度 —— 顺序构建 + 上限保护。**

- 只能顺序推进，`built_upto` 是唯一水位；随机访问 `view_off > built_upto` 退化为"从水位顺序解码到目标"，UI 必须显示进度并允许取消，**不得**表现为"卡住"。
- `max_output`（默认 4GiB，可配）与压缩比监控（> 1000:1 预警）是解压炸弹的两道闸；触顶 → `ProcError::OutputLimit` → 节点标红，已产出部分**保留可读**。
- 反向映射：`unmap` 在已构建区内仍可精确回答（索引里有 `src_off`），只是不能保证"下一个字节"是哪，所以编辑一律禁止。

**(D) 多输入 —— 区间拼接表。**

```rust
struct ConcatMap { segs: Vec<Seg> }   // Seg { view_start, len, port: PortIdx, src_off }
```

- 拼接：`segs` 按输入端口顺序堆叠，长度 = 各输入 `view_len` 之和。
- **三源 XOR**：长度 = 各源 `min`；`map_range(v)` 返回三条 1:1 区间（每端口一条），`transform_block` 读三次 XOR 一次。`unmap` 只能回单源（选端口 0 为 canonical），因此**禁止编辑**。
- 段表用二分查 `O(log m)`；`m` 上限 4096 段（超出提示用户改用"切片"节点分层）。

### 4.2 DAG 执行模型

```
build(graph) -> Result<GraphRuntime, Vec<ProcError>>:
  1. 端口 arity 校验（每条边两端端口存在且在 fan_in/fan_out 范围内）
  2. 环检测（Kahn 拓扑排序；有剩余节点即成环）
  3. 类型/尺寸校验（见 §4.7）
  4. 拓扑序，自底向上 build_map；每级 map 与上游合成 ComposedMap
  5. 计算 estimated_peak_memory，超预算则拒绝启用并指明最费的节点
```

- **分支预览**：一个中间节点可有多个下游（`fan_out` 允许 ≥ 1 的连线）。每个下游在**自己的坐标系**里各自建 map，共享同一份 `NodeCache`（`BlockKey` 含 `out_port`，节点输出本身只算一次）。
- **执行是拉取式（pull）**：视图请求 `read_view(range)` → 逆向遍历到 Root → 逐节点 `transform_block` → 逐级写回。没有"全图跑一遍"的概念，因此打开图是 O(1)，不随数据量增长。
- **每节点输出缓存 LRU 8MB**：一次滚动的可见区（约 4KB~2MB）远小于 8MB，所以滚动时命中率 > 95%；缓存总量默认 64MB（8 个活跃节点），超出按"最久未用节点"整体收缩。

### 4.3 内置节点清单

| 节点 | 映射类 | 可逆 | 随机读 | 关键决策 |
|---|---|---|---|---|
| XOR（1..16 字节循环密钥）| A | Exact | O(1) | `permute` 不适用，`transform_block` 用 u64 宽处理（8 字节/次，> 4GB/s）|
| AES-CTR | A | Exact | **O(1)** | CTR 是流密码，`counter = block#(off/16)`，偏移直接可算，天然随机访问 |
| AES-CBC（解密）| A | Exact | **O(1)** | 关键点：**CBC 解密是并行的**（块 n 只需密文块 n-1 做 IV），所以随机访问成立；CBC **加密**才是串行的，故本节点只做解密方向 |
| RC4 | B | BlockIndexed | 慢 | keystream 依赖全部前置字节，索引块 64KB；首次跳远处需从最近锚点重算（100MB 约 0.5s，UI 显示进度）。UI 标注"RC4 已不安全" |
| ROT(n) / 位反转 / 半字节交换 | A | Exact | O(1) | 全部是组内置换 |
| 字节序翻转 2/4/8 | A | Exact | O(1) | 组内索引 `v - v%k + (k-1-v%k)` |
| 转置（矩阵重排 w×h）| A 或 B | Exact | O(1) | 给定 `cols` 时映射是解析式（`src = (v%cols)*rows + v/cols`），**不需要表**；`cols` 未知 → `needs_full_buffer = true` |
| 解压 zlib/gzip/lz4/zstd | B | BlockIndexed | 慢 | 统一 `BlockDecoder`；输出块 64KB；`streamable = true` |
| 解压 + 归档遍历 zip/tar | B × N | BlockIndexed | 慢 | zip 用中心目录建成员索引（可随机），tar 必须顺序扫；**每个成员一个输出端口**，UI 上是"树 + 下拉切换"而不是 N 条线 |
| 拼接 Concat | D | Exact | O(log m) | 段表；每段记录 `(port, src_off, len)` |
| 切片 Slice | A | Exact | O(1) | `src = v + start`，只是一个平移 |
| 正则替换（显示层）| C | OneWay | 顺序 | 跨块匹配 → `needs_full_buffer`，`max_output` 强制；**明确告知用户"结果不可逆、不可编辑"** |
| Base64 / ASCII85 解码 | A | Exact | O(1) | 定长组（4→3 / 5→4），偏移可解析式换算，**无需索引** |
| 自定义脚本（BinSpec / WASM）| 取决于脚本 | 未知 | 未知 | 远期；强制 `max_output` + 步骤预算；声明 `OneWay` 直到证明可逆 |

### 4.4 节点图 UI 的数据模型与校验

- 模型：`SlotMap<NodeId, Node>` + `Vec<Edge>`；节点 id 稳定（删除不复用），所以撤销/重做只需 diff `(nodes, edges)` 集合。
- 编辑期校验（每次连线/改参数立即跑，**不等启用**）：

| 校验 | 失败表现 |
|---|---|
| 端口 arity（`fan_in`/`fan_out`）| 连线高亮红，悬浮说明"该节点只接受 1 个输入" |
| 类型/尺寸：解压节点的输入必须可 `streamable` 读，拼接要求每个输入 `view_len` 已知（`Unknown` 只能放在最后一段）| 在**具体端口**上放三角形警告图标 |
| 成环 | 连线动作本身被拒绝（物理上画不上去），并提示"处理器图必须是有向无环图" |
| 随机可达性 | 若用户要求"随机访问"（默认要求）而链上有 `OneWay` 节点 → 黄条"此图不支持随机跳转，跳转将触发顺序解码"，可一键切换为"允许慢路径" |
| 内存预算 `estimated_peak_memory` | 超 1GB 时红条指明最费的节点，建议改为稀疏锚点 |

- **错误定位**：`ProcError { node: NodeId, port: PortIdx, view_range, src_range, kind, .. }` 直接映射到图上一个红框 + 端口旁的数字徽章；同时在 hex 视图标红对应的 view 区间。这条要求来自 01-competitor-feature-research §3.5(d) 的教训：报错不指向具体位置等于没报错。

### 4.5 性能关键路径

1. **拉取式 + 块缓存**：滚动只触碰可见区，`Identity` 链命中后就是 `memcpy` + 组内置换。
2. **索引构建必须后台**：`BuildIndexJob` 走 job 系统，UI 每帧只 drain 进度事件（00 §5.4），**绝不在 UI 线程上建索引**。
3. **进度与取消**：`CancelToken` 粒度 = 每解码一个块检查一次（64KB），响应 < 100ms。
4. **预取**：按滚动方向预取 2MB 视图数据，块索引与数据块两级都预取；预取任务优先级低于前台读。

---

## 5 性能预算与基准

| 场景 | 预算 | 条件 | 对齐 00 §7 |
|---|---|---|---|
| 1:1 变换（XOR）1GB 视图滚动 | **无额外延迟**（Δ 帧时间 < 1ms）| 可见区 4KB，命中缓存 | 00 §7 滚动 < 16ms |
| XOR / 位反转吞吐 | > 4GB/s | u64 宽，单线程 | |
| AES-CTR 吞吐（AES-NI）| > 3GB/s | | 低于硬件极限即视为实现缺陷 |
| 解压索引构建 100MB 输出 | **< 3s** | zstd 单线程 ~400MB/s、zlib ~150MB/s → 3× 余量 | 需求硬指标 |
| 块索引内存 | 1GB 输出 = 512KB | 64KB 块 | |
| 随机跳转（已构建区）| < 5ms | 任意图深 ≤ 16 | 00 §7 跳转 < 5ms |
| 随机跳转（未构建解压区，向后 10MB）| < 120ms 并显示进度 | 100MB/s 解码 | 允许慢，不允许卡死 |
| RC4 远跳（从最近锚点重算 ≤ 4 块）| < 5ms | | |
| 每节点缓存 | 8MB（默认），全图 64MB | 8 活跃节点 | 支撑"内存 < 500MB" |
| 图启用（build + 建图）| < 20ms | 不含索引构建 | 与 02 编译预算同量级 |
| 视图搜索（1:1 链）| 退让到源吞吐的 60% | 800MB/s → 480MB/s | 见 §7 的搜索决策 |
| 视图搜索（解压链）| 首次受索引构建限制，之后同 1:1 | 必须显示"正在构建索引" | |

---

## 6 失败模式与降级

| 场景 | 可检测性 | 行为 |
|---|---|---|
| **密钥错误** | **完全不可检测**（输出仍是"有效字节"）| 不报错，但 UI 必须常驻提示"密钥正确性无法验证"；提供**启发式**建议：对前 4KB 算熵与 magic 命中率，可疑时给黄条"解密结果无已知魔数，密钥可能不正确"；节点面板永远显示"预览前 256 字节" |
| 解压失败（数据损坏）| 可检测 | 已解出的块**保留可读**，从失败点起标为不可读纹理，节点标红 + 精确 view/src 双区间；不整体失败（对齐 00 §9.3 降级优先）|
| 解压炸弹 | 可检测 | 两道闸：`max_output`（默认 4GiB）+ 压缩比 > 1000:1 预警；触顶停止并保留已产出部分 |
| 节点循环 | 编辑期不可能（连线被拒）| 若从项目文件加载到环 → 拒绝启用该图，保留图结构供用户修 |
| 节点 panic | 用 `catch_unwind` 在 job 边界捕获 | 该节点标红，其余节点继续；图可部分可用 |
| 缓存 OOM | 可检测 | LRU 收缩；极端情况降级为"无缓存直通"（变慢但正确）|
| `needs_full_buffer` 但输入过大 | 可检测 | 拒绝启用并给出"当前输入 2GB，该节点需要全量缓冲（上限 512MB）" |
| 源 `Resized`/`Replaced` | 可检测 | 整图缓存与索引全清，`view_len` 重新计算，UI 提示"源已变化，处理器图正在重建" |
| 索引构建被取消 | 正常路径 | 保留 `built_upto` 水位，下次继续，不重头 |

---

## 7 与其它子系统的契约

| 子系统 | 决策 | 理由与 UI 提示 |
|---|---|---|
| **hex 视图（05）** | 视图坐标 = 处理器图输出坐标。偏移列默认显示 **view 偏移**，悬浮/副列显示 source 偏移；提供开关切换 | 用户关心的是"明文里的那个字节"，但分析报告要能引用源地址。**否决"只显示源偏移"**：会让"看到 0x00 却标着 0x1F3A"这种矛盾 |
| **搜索（08）** | **默认在源（`Source`）空间搜**；切到视图搜索必须显式操作，且要求该节点链保长 + 可逆 `OffsetMap` + 确定性，否则菜单项置灰并常驻提示（见 08 §3.4、ADR 0011） | 视图搜索听起来更直觉，但必须先建完整索引（首次 ~3s，RC4 与解压类节点的索引成本可能等于全量解码），且一旦映射不可逆，命中就无法回链到字节、破坏 C3。**裁决：默认 Source**；切到视图时提示本次结果不能直接回填 patch |
| **BinSpec（02）** | 在**视图**上解析，`Node.offset` 是视图坐标；每个 Node 额外携带映射后的 source 区间用于导出报告 | 结构描述的是明文格式。实现上给 BinSpec 一个 `ViewReader`，它只暴露 `read_at`，天然看不到源 |
| **Diff（09）** | 在**视图**上比较（两侧各自应用自己的处理器图），UI 明确标注"两侧均已应用处理器图" | 用户关心明文差异；若两侧都是确定性 1:1 变换，结果与源上比较等价，但语义更清楚。**否决在源上比较**：密文 diff 对用户无意义 |
| **保存（04）** | 提供"导出变换后数据为副本"（`ExportJob`）：流式顺序读视图 → 写新文件，**源永不变**（I1）| 导出走与 hex 保存相同的"临时文件 + 原子 rename"。导出前必须估算大小，未知时提示 |
| **YARA（12）/ 分析（10）** | 可选"在视图上"运行，默认在源上 | YARA 扫描密文没意义，但用户可能想扫原始字节；给开关而不是替他决定 |
| **项目持久化（17）** | 图结构、节点 cfg、`ui_pos`、`bypassed` 全存；**密钥不存** | 见 §8 |

---

## 8 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `proc.cache.per_node_bytes` | `8388608` | 8MB/节点 |
| `proc.cache.total_bytes` | `67108864` | 64MB 全图 |
| `proc.index.block_size` | `65536` | 索引块 64KB |
| `proc.index.max_entries` | `262144` | 超限转稀疏锚点 |
| `proc.index.sparse_stride` | `4` | 稀疏模式 stride 倍数 |
| `proc.prefetch_bytes` | `2097152` | 2MB |
| `proc.max_output_bytes` | `4294967296` | 4GiB，解压炸弹第一道闸 |
| `proc.compress_ratio_warn` | `1000` | 压缩比预警阈值 |
| `proc.max_chain_depth` | `16` | 合成深度上限 |
| `proc.max_concat_segments` | `4096` | 段表上限 |
| `proc.full_buffer_limit` | `536870912` | `needs_full_buffer` 节点的输入上限 512MB |
| `proc.allow_random_access_required` | `true` | 默认拒绝 `OneWay` 链启用随机访问 |
| `proc.view_edit_enabled` | `true` | 仅对 `Identity`/`Exact` 全链生效 |
| `proc.key_storage` | `"memory"` | `memory` / `keychain`；`keychain` 用系统钥匙串（服务名 `hex.procgraph/<docid>/<nodeid>`），**永不写入项目文件** |

**持久化**：图随项目文件保存（见 17-project-and-persistence），格式为 `{ nodes: [{id, kind, cfg, pos, bypassed}], edges: [...] }`；`cfg` 中凡是密钥字段一律写成 `{"$secret": "<node_id>"}` 占位，恢复时向用户索取或从钥匙串取。**否决"加密后存项目里"**：项目文件是用户会随手分享的东西。

---

## 9 测试策略

| 类别 | 内容 |
|---|---|
| **可逆性对拍**（最重要）| proptest 生成 1000 组随机字节 + 随机参数，对每个 `reversible` 节点断言 `inverse(transform(x)) == x`；对整条链断言 `unmap(map(r)) ⊇ r` |
| **映射表与输出一致性** | 用两种方式取同一段视图：① 走 `map_range` + 随机读；② 全量顺序变换后切片。断言逐字节相等。fuzz 输入：随机 `(view_off, len)`、随机块边界、随机空洞 |
| **AES 向量** | NIST SP800-38A 的 CTR/CBC 测试向量；CBC 随机访问专项：直接解第 n 块 vs 从 0 顺序解，结果必须一致（验证"只用密文前一块做 IV"）|
| **解压炸弹** | 构造 1KB→10GB 的 zstd 样本，断言在 `max_output` 处停止、内存不超预算、已产出部分可读 |
| **索引边界** | 块边界处的读（`len` 跨越 2~3 个块）、`built_upto` 恰好等于请求起点、取消后恢复 |
| **图校验** | 环、端口越界、`fan_in` 不符、`Unknown` 长度输入接在拼接中间、链深 17 → 全部给出精确到端口的错误 |
| **fuzz** | 节点 cfg 反序列化（项目文件来自不可信来源）；任意图结构输入 `build()` 不得 panic |
| **基准** | §5 全表进 `bench/proc/`；滚动基准必须包含"经 1:1 链"与"经解压链（已建索引）"两种 |

**测试设施**：`FakeNode`（可注入延迟、失败、错误长度）用来测框架而不测算法；真实节点各自独立测试。

---

## 10 开放问题

1. **视图内编辑（反算写）v1 是否上线**？只对 1:1 链看起来安全，但"用户改了明文、我们改了密文"这件事的心理模型仍需验证；备选是 v1 完全只读、只提供"导出副本后编辑"。
2. **块索引持久化**：解压索引能否缓存在项目目录（下次打开省 3s）？风险是源变了但索引没变 → 必须绑 `(size, mtime, 前 4KB 哈希)`。值得做但需要清理策略。
3. **RC4 大偏移的体验**：100MB 处跳转要重算 0.5s。是否值得引入"keystream 预计算文件"（把 100MB keystream 落盘，256MB 磁盘换随机访问）？还是一个"慢但诚实"的进度条就够？
4. **多输出节点（zip/tar 成员）**在 UI 上如何表达：树 + 下拉切换（当前倾向），还是允许 N 条出边（图会变糊）？
5. **自定义节点的运行时**：WASM（安全、可跨语言、但有拷贝开销）vs BinSpec 子集（零拷贝、但表达力弱）。倾向 v1 只做 WASM，且必须先有 02 的 `Budget` 基础设施。
6. **`needs_full_buffer` 的 512MB 上限**是否太低？正则替换大文件是真实场景，可能需要"分块 + 重叠窗口"的近似模式（会漏跨块匹配，需显式告知）。
7. **视图搜索的性能预算**：00 §7 的 800MB/s 是源搜索指标，视图搜索要不要单独立一条预算？（当前只在本文 §5 给了"退让到 60%"）
8. **密钥内存清零**：`SecretKey` 是否需要 `mlock` + 显式 `zeroize`？Rust 的 `zeroize` 只能防编译器优化，防不住 swap。倾向做 `zeroize`，`mlock` 列为可选。
