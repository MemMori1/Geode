# 04 — 缓冲区与编辑模型（piece table / 撤销 / patch / 保存事务）

> 本文定义 `geode-buffer` crate 的内部结构与语义。它是全系统唯一的可写状态，**上线后改动成本 = 重写全部下游**。
> 硬契约来自 `00-architecture-overview`（§4 类型、§5.2 `ByteBuffer`、§6 线程、§7 预算、§8 术语、§9 原则）。本文只能加严；冲突以 00 为准。

---

## 0. 决策摘要

| 决策点 | 选定 | 理由 | 否决了什么 |
|---|---|---|---|
| piece 容器 | **计数型 B-tree**（fanout 64 / 叶 64 piece） | 100 万 piece 只有 3 层，区间遍历是顺序内存访问 | implicit treap、rope/AVL、`Vec<Piece>`、skip list |
| 静态 + 动态数据 | **placeholder**（`Arc<dyn Provider>` + src_off + len，最小 4KB）/ **literal**（`Arc<[u8]>` + 视图 off/len） | 打开 O(1)、内存 O(改动量)；4KB = 页缓存与 IO 粒度；literal 不可变 + 视图化 ⇒ split 零拷贝 | 1 字节粒度窗口、整文件预读、每片 `Vec<u8>`、分块 `ChunkedVec` |
| COW 粒度 | **树节点**（`Arc<Node>`），literal 块不可变 | 快照 = 指针 +1，O(1) | 页级 / 区间级 COW |
| 默认模式 | **overwrite（不改长度）**；insert/remove 必须显式 | hex 99% 的编辑是改值；长度变化是所有下游的失效源 | 默认插入模式 |
| 撤销 | **操作日志 + 大操作 COW 快照混合** | 小步省内存、大步省时间，两者都只与改动量成正比 | 纯日志、纯快照 |
| patch 的 old | ≤4MB 存全文，超过存视图 + blake3 摘要 | 不得突破"原大小 + 改动×2"（00 §7） | 一律全文、一律摘要 |
| 保存 | temp + fsync + 原子 rename + journal | 原文件任何时刻都完整（C2） | 原地覆写、先删后写 |
| 读用户文件 | **pread + LRU 页缓存，不对可变文件 mmap** | SIGBUS 不可捕获，躲开比捕获便宜 | mmap + 信号处理（附录 A） |

---

## 1 目标与非目标

### 1.1 目标
| # | 目标 | 验收 |
|---|---|---|
| G1 | 打开 100GB O(1)：不读数据、不建索引，树里只有 1 个 placeholder | 打开 100GB 稀疏文件 < 1ms |
| G2 | 随机读 O(log N)，与文件大小无关（C1） | 100GB 上定位 < 1µs |
| G3 | 编辑/撤销单步 < 1ms（00 §7） | 1M piece 上 p99 < 1ms |
| G4 | 内存 O(改动量 + placeholder 数)；4GB 只读 < 500MB（00 §7） | `bench/buffer/mem` |
| G5/G6 | 任何崩溃点都不损坏原文件（C2）；所有编辑可回链到原文件区间（C3） | 故障注入（§9.3）+ patch 导出与映射对拍 |

### 1.2 非目标
- **不做渲染**（可见区怎么画、高亮怎么合成）→ `05-render-and-hexview`；**不做 Provider 实现**（页缓存/生命周期）→ `06-data-providers`，本文只消费 00 §5.1。
- **不做搜索/解析/diff**：它们是本文只读的消费者（00 §6 规则 3）。**不做结构重映射**：插入/删除后 BinSpec 节点的偏移调整不在本文，本文只交付 affected 区间与映射函数（§7.2）。

### 1.3 已否决且不再讨论
- **整文件 `Vec<u8>` / gap buffer**：前者 100GB 直接死、插入 memmove 后半段、撤销复制整文件（01 §3.1）；后者只对光标邻近编辑有效、随机跳转仍 memmove、且无法表达 placeholder。
- **rope / `Arc<str>` 式**：为文本设计、叶按字符计，对"100GB placeholder + 8 字节 literal"这种极不均块分布退化。**原地覆写原文件**：写一半崩 = 数据全废，违反 C2。

---

## 2 依赖与边界

### 2.1 上下游
| 方向 | crate | 用到的契约 |
|---|---|---|
| 下 | `geode-core` / `geode-provider` | `Offset`/`ByteRange`/`EditId`/`Diag`（00 §4）；"越界即裁剪，不 panic"（00 §9.1）；`read_at`/`poll_change`/`caps`/`SizeInfo`（00 §5.1） |
| 上 | `geode-ui`（05/16） | `ByteBuffer`（00 §5.2）、`PatchSet` 可视化、`SaveState` |
| 上 | `geode-bspec` / `geode-search` / `geode-diff` / `geode-analysis` | dirty 区间（02 §7.4）、只读 `Snapshot` + `version()` |
| 上 | `geode-project` | journal 路径与崩溃恢复（17） |

**铁律**：`geode-buffer` 不依赖任何 GUI crate（00 §2），也不得依赖 `geode-bspec`/`geode-search`（否则成环）。

### 2.2 并发边界（对齐 00 §6）
1. 编辑只在 UI 线程 → 本文所有 `&mut self` 方法单线程、无锁。
2. 后台 Job 只读 `Arc<Snapshot>`：`Snapshot` = `Arc<Node>` + `len` + `version`，`Clone` 为 O(1)，只读侧无锁。
3. 保存 Job 读**保存起点**的快照；保存期间允许继续编辑（§4.6）。
4. 后台结果回主线程校验 `version()`（00 §6 规则 3）。
5. **不属于本文的状态**：`AnnotationIndex`（00 §5.3）、结构树、View 状态都在 Buffer 外；Buffer 只给 `version()` 与区间映射 ⇒ 标注/书签/搜索结果**不需要在每次编辑时重写**（§7.2）。

---

## 3 数据结构与接口

### 3.1 容器选型：为什么是计数型 B-tree

| 容器 | 插删 | 按偏移定位 | 按偏移区间遍历 | 每 piece 额外开销 | 实现复杂度 | 缓存友好 | 最坏情况 |
|---|---|---|---|---|---|---|---|
| **计数 B-tree（选定）** | O(log N) | O(log N) | O(log N + k/64)，顺序扫叶 | ~0（piece 内联叶片） | 中高，900~1300 行 | ★★★★★ 叶 64×32B=2KB 连续 | 树高恒为 ⌈log₆₄ N⌉ |
| implicit treap | O(log N) 期望 | O(log N) | O(log N + k)，指针跳 | 16B 指针 + priority | 低，~200 行 | ★★ 每层一次 miss | 随机优先级 ⇒ 同序列不同树 |
| rope / AVL | O(log N) | O(log N) | O(log N + k) | 24~32B | 中高，旋转分支多 | ★★ 深度 log₂N ≈ 20 | 定位 20 次 miss |
| `Vec<Piece>` 分段表 | **O(N)** | O(log N) | O(log N + k) | 0 | 极低 | ★★★★★ | 10 万片后平均 memmove 1.6MB/次 |
| skip list / RRB 持久化向量 | O(log N) 期望 | O(log N) | O(log N + k) | 24B / 高（节点带大小表） | 低 / 高 | ★★ / ★★★ | 概率性；RRB 为多版本分叉设计，我们只要单链 COW |

**论证**：
1. **N 的量级决定一切**。100GB 的典型使用是 10⁴~10⁶ 次编辑。扇出 64 时树高 = ⌈log₆₄ 10⁶⌉ = 4；treap/AVL 是 log₂ ≈ 20。定位 = 4 次指针跳 + 叶内扫 64 个累计长度（SIMD 友好）≈ **200ns**，而 20 次指针追逐 ≈ 1.5µs。这条直接决定 G2/G3。
2. **确定性是测试资产，`Vec<Piece>` 已被量化否决**。treap 的随机优先级破坏"同一编辑序列必产生同一棵树"，而快照等价性、撤销折叠、fuzz 复现都靠它；`Vec<Piece>` 则是 10 万片 × 32B = 3.2MB 描述子，文件头附近编辑平均 memmove 1.6MB，100GB 下编辑点分散（1000 次/分）⇒ 1.6GB/s 纯浪费带宽。
3. **大文件 + 大量小 piece（B-tree 的真天敌）**：不是树高，而是**叶子碎片化**——每叶只剩 1~2 个 piece 时树高退化为 log₂N。三重防御：(a) 叶装载率下限 25%，低于即 rebalance；(b) 强制合并相邻 piece（§4.4）；(c) piece 数软上限告警 + 局部 defrag（§8）。配合 §3.3 的最小 placeholder 规则，**piece 数与编辑次数解耦**。
4. **代价诚实说明**：B-tree 是候选里实现最复杂的。用 property test + `check_invariants()` 每步校验（§9.3）把这份复杂度关进笼子，是唯一值得付的税。

### 3.2 结构与签名

```rust
// literal 与 placeholder 统一成"可寻址字节窗口" ⇒ split 无需 memcpy
enum Source { Lit(Arc<[u8]>), Win(Arc<dyn Provider>) }

struct Piece { src: Source, src_off: u64, len: u64 }   // 不变量 I1: len > 0

const FANOUT: usize = 64; const LEAF_PIECES: usize = 64;   // 内部节点子数 / 叶子 piece 上限（装载率 ≥ 25%）
enum Node {
    Leaf  { pieces: ArrayVec<Piece, LEAF_PIECES>, bytes: u64 },
    Inner { kids: ArrayVec<Arc<Node>, FANOUT>, cum: ArrayVec<u64, FANOUT>, bytes: u64 },  // cum[i]=kids[0..=i] 累计字节 ⇒ 定位对 cum 二分，无需排序键
}
pub struct PieceBuffer {
    root: Option<Arc<Node>>, len: u64,     // None 仅表示空 buffer
    version: u64,                          // 每 commit +1（undo/redo/reload/patch 应用也算；维护操作不算）
    mode: EditMode,                        // Overwrite（默认）| Insert
    history: History, patches: PatchSet,   // §4.3 / §4.5（派生视图，增量维护）
    src: Option<Arc<dyn Provider>>,        // reload / old 视图 / 映射
    policy: BufferPolicy,                  // §8
}
```

### 3.3 placeholder 最小尺寸：为什么是 4KB
- **描述子成本与语法噪音**：`Piece` = Source(16B) + 2×u64 = **32B**。若允许 1 字节 placeholder，1GB 被切碎成 10⁸ 片 ⇒ 3.2GB 描述子 + 10⁸ 次 O(log N) split，最坏放大 **32 倍**；而且每个低于阈值的 placeholder 片段都等价于一条"有语义的原文件边界"——用户只改 8 字节时，这些边界没有语义，纯粹是碎片。
- **取 4KB 的依据**：4KB = 主流页大小（x86-64/aarch64；Apple Silicon 16KB 也 ≥ 4KB，4KB 是安全下界），也是 `pread` + 页缓存的最小有用单位——一次 `read_at` 固定成本 1~2µs，读 1 字节与读 4KB 一样贵，窗口更小省不下 I/O。
- **三条配套规则（消除病态结构的唯一手段）**：
  - **R1 降级**：任何操作后若某 placeholder 片段 `len < 4096`，整段读入并替换为 literal（≤4KB 内存/次）⇒ 保证所有 placeholder ≥ 4KB。
  - **R2 禁止升级**：literal 永不"变回" placeholder——原文件内容一旦被改，provider 上仍是旧值，升级会静默出错。
  - **R3 相邻 literal 合并**：同叶相邻两 literal，若 `Arc::strong_count == 1` 且 `src_off` 相邻 ⇒ 原地拼接（单次 ≤ `coalesce_max_bytes` = 1MB，超阈值保留分片）。
- **piece 数上界（对"1 字节 literal / 1 字节 placeholder 交替"的证伪）**：设改动簇（连续编辑区）为 C。R3 使簇内反复编辑不增加片段；每簇最多切碎原文件 2 处，切出的碎片若 <4KB 立即被 R1 literal 化（不再产生新边界）。故 **在 R1 与 R3 同时生效时**，piece 数 ≤ O(C + 改动字节/4KB)，与编辑次数无关（**这条界是 R1 的产物，不是自然成立的**：实测关掉 R1 时 1 万次编辑产生 16,823 片 = 界的 191 倍，见 `docs/spikes/S1-placeholder-fragmentation.md`）。反例：min=1 且无 R1 时，"0x1000 改 1 字节、0x1001 改 1 字节…"产生交替结构，1MB → 50 万片（16MB 描述子），即 01 §3.1 坑 4（Tatham 文末警告）；加上 R1+R3 后同一场景 = 16 个 64 KiB literal + 少量边界片（**不是 1 片**：literal 合并上限 `MAX_LITERAL = 64 KiB`，故下限是 `文件大小/64 KiB + C` 片），0 个 placeholder。

### 3.4 literal 容器与 COW 粒度
| 方案 | 结论 |
|---|---|
| 每片 `Vec<u8>` | 否决：不可共享 ⇒ 快照必须深拷贝，O(1) 快照失效 |
| **每 literal 块 `Arc<[u8]>`** | **选定**：不可变 + 引用计数共享；split = 两个 `(Arc, off, len)` 视图，零拷贝 |
| 分块 `Vec<Arc<[u8;4096]>>` | 否决：只有单个 literal > 64MB 才需要，而那时走临时文件 + `TempFileProvider` |

- **COW 粒度 = 整个 literal 块**（不是页、不是区间）。前提是 literal 块**永不修改**：任何写入都是"split 出边界 → 用新 literal 替换中段"，不存在"有人正在读时被改写"，故无需页级 COW。`Arc::strong_count == 1` 允许原地扩容（R3 的优化）；`> 1`（被快照引用）必须 copy —— 这正是快照期间的正确行为。
- **超大粘贴出口**：单次编辑 literal > `paste_to_tempfile_threshold`（64MB）时不进内存，写入临时文件并生成 `Win(TempFileProvider)`，复用同一套机制。

### 3.5 三个原语对树的变换与复杂度

| 原语 | 树变换 | 长度 | 复杂度 |
|---|---|---|---|
| `overwrite(at, data)` | split(at) → split(at+len) → 中段替换为 1 个 literal → R1/R3 | 不变（越界 append，§4.2） | O(log N) |
| `insert(at, data)` | split(at) → 插入 literal → 与左右尝试 R3 | +len | O(log N) |
| `remove(range)` | split(start) → split(end) → 删中段 → 左右 placeholder 尝试 re-join（§4.2 的"缝"） | −len | O(log N) |

**split/join 从不复制字节**（两片共享同一 `Source` 与相邻 `src_off`）——这是"剪切/粘贴任意大块 O(log N)"（01 §3.1）的基础。

| 操作 | 承诺 |
|---|---|
| 打开 | **O(1)**，实测 < 1ms（树里 1 个 placeholder） |
| 编辑（三个原语） | **O(log N)** |
| 随机读 1 字节 / 区间读 | **O(log N)** ≈ 200ns @ 1M piece / **O(log N + k)**（k = 字节数）+ 每连续 Win 段 1 次 `read_at` |
| 保存 | **O(N_bytes + N_pieces)**，顺序写 |
| 内存 | **O(改动字节 + 4KB×placeholder 数 + 32B×piece 数)** |
| undo / redo | **O(1)** 取快照 + O(log N) 重排 |

### 3.6 对外读接口

```rust
#[derive(Clone)]   // Clone = O(1)
pub struct Snapshot { root: Option<Arc<Node>>, len: u64, version: u64 }
impl Snapshot {
    pub fn len(&self) -> u64;   pub fn version(&self) -> u64;
    pub fn read(&self, range: ByteRange, out: &mut [u8]) -> Result<(), BufferError>;   // 跨 piece 拼接（§4.1）
    pub fn cursor(&self, at: u64) -> PieceCursor<'_>;                                  // 流式，供保存/搜索
    pub fn read_slices(&self, range: ByteRange, f: impl FnMut(&[u8])) -> Result<(), BufferError>;  // 零拷贝：Win 片借页缓存（§10 Q2）
}
pub struct PieceCursor<'a> { /* 位置 = 叶索引 + 叶内下标，推进不重下钻 */ }
impl<'a> PieceCursor<'a> {
    pub fn remaining(&self) -> u64;
    pub fn next_chunk(&mut self, out: &mut [u8]) -> Result<usize, BufferError>;   // 一段物理连续 piece 的字节数
}
```

---

## 4 核心算法/流程

### 4.1 读取路径

```
read(range, out):
  1. r = range.clamp(self.len)                 // 永不 panic（00 §9.1）
  2. 定位 r.start → (叶, 叶内下标, 片内偏移)：cum[] 二分下钻，O(log N)
  3. 循环拷贝直到 out 满或 r.end：Lit → extend_from_slice（memcpy >8GB/s）；Win → 见 4.1.1
     推进 = 叶内下标 +1（PieceCursor，不重新下钻）
  4. 短读视为并发异常（附录 A.3 末条），重试 ≤2 次后返回 ProviderFailed
```
**4.1.1 placeholder 读取与页缓存**：`Win` 片走 `Provider::read_at` → 06 的页缓存（4KB 页、LRU 上限 256MB、`preadv` 合批）。**连续 Win 合并**：拷贝前向前看，若下一片同为 Win、`Arc::ptr_eq` 同 provider、`src_off` 连续 ⇒ 合成一次 `read_at`。
**跨 100 个 piece 的连续读优化**：(a) **PieceCursor**——位置是"叶 + 叶内下标"，推进是 `i += 1`，100 片总代价 **O(log N + 100)** 而非 100×O(log N)；(b) **syscall 合批**——连续 Win 段收进 `SmallVec<[ReadSeg; 8]>`，≥4 段用 `preadv` 一次提交；(c) **预取**——消费到片尾 < 32KB 且下一片是 Win 时提交 256KB best-effort 异步预取（失败静默），服务"跳转后连续翻页"；(d) **零拷贝**——渲染走 `read_slices`，无中间缓冲，这是滚动 16ms 预算的关键路径（00 §7）。

### 4.2 编辑原语语义（hex 默认必须是覆盖模式）

| 模式 | 语义 | 何时进入 |
|---|---|---|
| **`Overwrite`（默认）** | `len()` 不变；光标在 EOF 处继续输入则**追加**（`overwrite_past_eof = Append`，用户预期"在末尾接着打字"），严格模式 `Reject` 返回 `OutOfBounds` | 默认 |
| `Insert` | 显式进入（Insert 键 / 菜单 / 粘贴时选择） | 必须用户显式 |
| `remove` | 仅在 insert 模式或显式删除命令下可用，同样返回 `EditId` | 必须用户显式 |

**为什么默认覆盖**：hex 99% 的编辑是"改某个字段的值"；长度变化会让结构树、搜索结果、标注、书签、diff 锚点全部失效（01 §4 坑 6、02 §7.4）。把破坏性操作放进显式模式符合 00 §9.4「默认安全」。否决了"默认插入"和"覆盖/插入自动判断"（不可预测）。

**`remove` 产生的"缝"**：从 placeholder 挖掉 `[a,b)` 后，左右两片覆盖 `[s,a)` 与 `[b,s+len)`，在 source 上**不连续**，无法 re-join——这是信息损失，不是缺陷。缓解：任一侧剩余 < 4KB ⇒ R1 降级（两侧合计 ≤ 64KB 时把另一侧也读入）；两侧都 ≥ 4KB ⇒ 保留两个 Win 片；反复删/插累积的 Win 片由 `defragment()` 局部 literal 化（§4.4），不是全文件。

### 4.3 连续 nibble 输入、最小分裂、撤销/重做

**nibble 合并**：hex 输入是半字节级——`A` `F` 先写高 4 位（读原字节→改高半字节→写 1 字节），再写低 4 位。不合并的话 1 字节 = 2 个 `EditOp` + 2 次树变换 + 2 条 patch + 2 个撤销步，用户按一次 Ctrl+Z 只退半个字节，会被当成 bug。**规则**：`group_key` 相同且距上次提交 < `nibble_group_window_ms`（400ms）⇒ 合入同一 step；`group_key = (Nibble, at)`，即**同一偏移的高/低半字节**才合并，换偏移即新组；撤销一步退整字节，patch 的 `old` 只留首次原值。连续字符输入同理（`group_key = (GeodeType, at)`，`at` 递增即同组）；insert 模式下连续输入合并成一次 `Insert`，避免"每字符一个 piece"。拖拽填充与大块粘贴用 checkpoint 包裹，绝不按字符合并。
**最小分裂策略**：不机械 split 到 1 字节；`overwrite` 只在 `at` 与 `at+len` 两处 split，且 split 零拷贝 ⇒ 分裂点数只与编辑区间端点数成正比。

```rust
enum EditOp {
    Overwrite { at: u64, old: Arc<[u8]>, new: Arc<[u8]> },
    Insert { at: u64, data: Arc<[u8]> },
    Remove { at: u64, data: PieceView },          // 被删内容用视图表达，可跨多片，**不读数据**
}
struct Step { ops: ArrayVec<EditOp, 64>, pre: Option<Arc<Node>>, post: Option<Arc<Node>>,
              label: Option<String>, mem: u64, version_after: u64 }   // ops>64 或大操作 ⇒ pre/post 存 O(1) 快照
pub struct History { undo: VecDeque<Step>, redo: Vec<Step>, mem: u64,
                     group: Option<(GroupKey, Instant)>, external_barrier: Option<usize> }   // §4.7 屏障
```

- **纯反向操作**（`ops ≤ 64` 且总字节 ≤ 64KB）：内存 ≈ 改动字节。
- **`pre = Some(root.clone())` + ops 只存 affected 区间**（大块替换/删除/超大粘贴）：快照 O(1)，内存 = 被丢弃的子树 ≈ 改动量，未触及部分共享。
- **折叠**（同区域连续编辑 > 64 步时丢中间步，保留最早 `pre` 与最新 `post`）：防"逐字节改 1MB"产生 100 万步。

**为什么快照是 O(1)，以及"无限撤销"的内存上限**：节点不可变 + `Arc`，`root.clone()` 只是引用计数 +1；之后的编辑走 COW，只复制"根→修改点"路径上的 4 个节点（≈4×2KB），未触及的子树被新旧两棵树共享 ⇒ 快照**时间**恒定、**空间**只与改动区成正比。这是"无限撤销"与"100GB 文件"能同时成立的唯一原因——共享在**节点**级，不在字节级。上限 `undo_mem_limit = min(512MB, max(64MB, file_len × 2%))`，另有 `undo_min_steps = 200`（避免"大文件一开就只剩 3 步"）。超限则从栈底开始丢，直到 `mem ≤ 75% × limit`（滞后水位防抖动）。**保留 checkpoint**：`label.is_some()` 且 protected 的步不可丢；若丢弃将触及它，改用 **collapse**——把它与其后一步合并成一个粗粒度步（语义变成"撤到该 checkpoint"），保住用户语义，UI 提示"更早的撤销历史已丢弃"。

**分组**：① 显式 `checkpoint(label)`（00 §5.2）强制成组且 protected；② 时间聚合：同 `group_key` 且间隔 <400ms；③ 输入聚合：nibble / 连续字符 / 连续粘贴；④ 隐式硬边界：保存、patch 应用、reload、外部修改、切换 EditMode。
**撤销栈与 patch 一致性**：单一真相是 piece 树；`PatchSet` 是**增量维护**的派生视图（commit 正向更新、undo 反向更新），绝不重算（O(N) 不可接受）。`debug_assertions` 下每 256 次操作做一次"从树全量重建 == 当前 patch 表"的比对。不变量 I5：`PatchSet.version == buffer.version`。

### 4.4 维护性操作（碎片治理）
| 操作 | 粒度 | 触发 | 进撤销？ |
|---|---|---|---|
| `coalesce()` | 叶级 | 每次编辑后顺带 | 否（不改字节） |
| `defragment(预算)` | 局部区间 | `piece_count > max(4×簇数, 65536)` 且系统空闲 | 否（可取消，预算 64MB；只改表示不改字节 ⇒ 不进撤销栈，但使所有 piece→偏移 缓存失效，且必须响应 `CancelToken`：100ms 内，00 §5.4） |

### 4.5 Patch 管理

```rust
pub struct PatchSet { spans: Vec<Patch>, version: u64 }   // 按 seq（编辑顺序）排列的重放日志
pub struct Patch { orig: ByteRange, kind: PatchKind,      // orig = 编辑发生时的坐标（C3 回链）
                   old: OldBytes, new: Arc<[u8]> }        // cur 由长度增量前缀和导出，不落字段
pub enum OldBytes {
    Full(Arc<[u8]>),                                            // ≤4MB 全文
    View { src: Source, off: u64, len: u64, digest: [u8; 16] },  // 大区间：视图 + blake3 前 16B
}
```

**old 存全文还是摘要（内存权衡）**：一律全文 ⇒ 撤销/导出/写回自包含、provider 挂了也不怕，但改动散布全文件时 old 总量 = 原文件大小，故只用于 ≤4MB 区间；一律摘要 ⇒ 内存极省，但撤销强依赖 provider 可读、违反 §6 降级要求，**否决**；**选定混合**（小改动零依赖、大改动省内存，代价是两套路径）。
**关键性质与相邻合并**：patch 按 `orig` 区间**归并**，同一原区间反复修改只保留**最初**的 old ⇒ `old` 总量 ≤ min(原文件大小, 改动区间总长)，**不随编辑次数增长**。`patch_old_full_limit = 4MB` 的依据：一屏可见区 ≈2KB，典型 patch 几十字节~几十 KB，4MB 让 99% 的 patch 自包含；另有全局 `patch_old_budget = 128MB`，超限把最老的 `Full` 降级为 `View`。**只有等长替换（Overwrite/Fill）允许归并**：两条 patch 在 `orig` 上严格向后、gap ≤ 16 字节 ⇒ 合并（gap 内未改字节同时拼进 old 与 new，保证仍是连续区间替换）；16 = hex 视图一行内的典型编辑跨度，避免同一行两次编辑显示成两个色块。**Insert/Remove 一律不归并**——它们会改变后续坐标，拼起来既不是合法区间替换，也无法用 `orig` 顺序重放；重叠编辑（`at < last.orig.end()`）同样不归并，否则会拼出重复字节。`PatchSet` 因此是**重放日志**（按 seq 应用），不是排序后的 diff 表；`cur` 由长度增量前缀和 `O(log P)` 导出。
**可视化接口（供 05 上色）**：`query(range, out) -> O(log n + k)`（每帧调用）、`query_point(at)`、`is_dirty()`、`total_bytes()`（状态栏"改动 N 字节"）。05 只需三种颜色 + 未保存指示，不需要 old/new 内容。
**单 patch 操作**：`revert_patch(idx)` 仅当该 patch 的 `cur` 区间未被后续编辑叠加时可精确回滚，否则返回 `PatchObstructed` + 可回滚子区间，UI 提示"该修改已被后续编辑覆盖"；`apply_patch(idx)` 转成 `EditOp` 走正常编辑通道（可撤销、进 patch 表）；`export(idx|all, fmt)`。

| 导出格式 | 偏移上限 | 单记录上限 | 长度变化 | EOF 记录 | 结论 |
|---|---|---|---|---|---|
| IPS（24-bit） | **16MB**（0xFFFFFF） | 0xFFFF | **不支持**（纯等长替换） | 末尾 3 字节 `45 4F 46` | >16MB 或含 Insert/Remove ⇒ 拒绝导出 |
| IPS32 | 4GB | 0xFFFF | **不支持** | 无 | >4GB 或含 Insert/Remove ⇒ 拒绝导出 |
| `.geopatch`（自有二进制：大端定长帧 + 每记录 CRC32） | u64 | 64MiB（`formats.geopatch.max_record_bytes`） | 支持 | `FinalLen` 记录 | **默认格式**（字节级布局见 `23 §3.6`；早稿的 CBOR 方案已否决） |

EOF 记录只出现在记录边界（由长度字段决定），本实现统一在文件末尾追加，不与数据混淆；连续字节按 0xFFFF 拆分并与相邻记录合并以减少记录数。

### 4.6 保存事务

```rust
pub enum SaveState { Idle, Preparing, Writing { job: JobId }, Committing, Failed(SaveError) }
/// 同步完成：快照 + 预检 + 写 journal（<5ms）；写盘交给 Job
pub fn begin_save(&mut self, target: &Path, opts: &SaveOptions) -> Result<SaveHandle, BufferError>;
```

| # | 步骤 | 失败分支 |
|---|---|---|
| 1 | **冻结快照** `snap = self.snapshot()`（O(1)），记 `base_version`，**不锁编辑** | — |
| 2 | **预检**：目标可写、是否同一文件（`dev/ino`）、剩余空间 ≥ 预估 ×1.05（稀疏按实体化估，附录 A.2） | `Failed`，什么都没动 |
| 3 | **写 journal** `<target>.geojournal`（同目录）：`{target, temp, base_version, patch 摘要, 原文件头尾 4KB blake3, Pending}`，`fsync`，≤64KB | `Failed`，未写 temp |
| 4 | **写临时文件并落盘** `<dir>/.<name>.geode-<pid>-<rnd>.tmp`（同目录 ⇒ 同文件系统）：顺序展开 piece 树，**固定 4MB 写缓冲**，Win 片直接 `read_at` 进缓冲，满即 `write_all` ⇒ 内存与文件大小无关；随后 `fsync(temp)` + Unix `fsync(parent_dir)`（否则 rename 后的目录项可能丢） | 删 temp，`Failed` |
| 5 | **元数据保留**：`fchmod(mode)`、`fchown`（失败 warn）、xattr 复制（≤64 条、单条 ≤64KB，失败 warn）、POSIX ACL 随 `system.posix_acl_access` xattr 一起走。mtime **不回填**（会让外部修改检测误判），改为把保存后的 `(dev, ino, size, mtime, 采样哈希)` 记为**新基线**。Windows 用 `ReplaceFileW(REPLACEFILE_WRITE_THROUGH)`（天然保留 ACL/属性），回退 `MoveFileExW(REPLACE_EXISTING)` | 权限失败 warn 后继续（避免"因 ACL 保存不了"） |
| 6 | **原子提交** `rename(temp, target)` | 见下表 |
| 7 | **收尾与 `.bak`**：删 journal 前先 `fsync` 目录（维持不变式「journal 存在 ⇒ temp 可能不完整」）；更新基线；若 `version() != base_version` ⇒ 脏标记**不清零**，UI 显示"已保存到 v{base}，仍有未保存修改"。`backup_mode = HardLink`（默认）在提交**前** `link(target, target.bak)`：O(1)、不占空间、不拷贝 100GB；不支持硬链接（FAT/exFAT/部分网络盘）⇒ 降级 `Copy`（仅当 size ≤ 2GB）⇒ 否则 `Off` 并提示 | journal 删失败 ⇒ 下次启动自清；备份失败不阻止保存（warn） |

| rename 失败 | 处理 |
|---|---|
| `EXDEV`（跨设备） | 目标目录解析到了别的文件系统（符号链接/挂载点）：按 `target` 的**真实**父目录重建 temp 再 rename；仍失败 ⇒ `Failed(CrossDevice)`，保留 temp 并提示"另存为" |
| `EBUSY` / `EACCES` / Windows sharing violation | `Failed(TargetBusy)` ⇒ UI 三选：关闭占用程序 / 另存为 / 重试 |
| `ENOSPC` / `EROFS` | 删 temp，`Failed(NoSpace)` / `Failed(ReadOnly)`（磁盘满时仍可把 patch 导出为 `.geopatch` 到别处，数据不丢） |

**保存期间的编辑锁定**：否决"全锁"（100GB 保存需数十秒，锁 UI 不可接受）；选定**快照 + 允许编辑 + 版本比对**（默认 `editable_during_save = true`）：O(1) 快照让用户无感，结束后仍能正确标记"还有未保存改动"。"增量二次保存队列"列入 §10 Q4，v1 不做。
**保存中断的恢复（journal）**：启动扫描 `<workspace>/journal/*` 与目标目录。temp 存在且 size/摘要与 journal 一致 ⇒ 提供"恢复上次未完成的保存"（可把 temp 认领为目标文件）；temp 缺失 ⇒ 删 journal 并提示"上次保存未写入任何数据，原文件完好"。journal 只存元数据，数据在 temp 里；若 patch 压缩后 ≤16MB 也写入 journal，使"原文件被误删"仍可救回。

### 4.7 外部修改检测
基线 `FileStamp { dev, ino, size, mtime_ns, sampled_blake3 }`，采样 = 头 4KB + 尾 4KB + 8 个均匀点各 4KB（总 40KB，**O(1)**），打开时与每次保存后更新。检测：窗口获得焦点必查（`stat` ≈2µs），后台每 `external_check_interval_ms`（5s）一次 `stat`；**永不**做全量内容比对。

| 观测 | 判定 | 行为 |
|---|---|---|
| `(dev, ino, size, mtime)` 全同 | Unchanged | 最热路径，0 次读盘 |
| 仅 mtime 变，size 与采样哈希同 | Touched | 静默更新基线（大概率 touch/权限操作） |
| size 变、采样哈希变、或 dev/ino 变（外部 atomic rename 覆盖） | **Conflict** | 见下 |

| 冲突交互 | 行为 |
|---|---|
| **Reload**（无未保存改动时默认直接执行） | 重建 buffer（`version+1`），保留 undo 历史但插入 `ExternalChange` 屏障步：撤销**不得跨过**该屏障（否则会把别人的改动"撤销"成我的）；历史中 `EditOp` 的 `orig` 坐标统一标 stale |
| **Keep mine** | 基线更新为磁盘当前状态；保存时覆盖外部改动。保存前二次确认 + 强制生成 `.bak` |
| **Save as** | 走 `begin_save` 到新路径，原文件不动 |

`caps.volatile` 的 provider（进程内存/UDP）**不参与检测**（它们永远在变，00 §5.1）。**Windows 文件被锁**：打开时用 `FILE_SHARE_READ|WRITE|DELETE` 避免我们锁住别人；保存用 `ReplaceFileW`（可替换被他人以 DELETE 共享打开的文件）；目标被独占打开 ⇒ sharing violation ⇒ §4.6 步骤 6 分支。

---

## 5 性能预算与基准

### 5.1 与 00 §7 的对应
| 00 §7 条目 | 本文负责的部分 | 本文的额外交付 |
|---|---|---|
| 打开 4GB < 200ms；跳转 100GB < 5ms | `open` 只建 1 个 placeholder；`Snapshot` 定位 | **< 1ms**（余量全留给 provider/UI）；定位 < 1µs；5ms 几乎全给纹理上传 |
| 撤销/重做 < 1ms | `History::undo` | p99 < 1ms @ 1M piece |
| 保存 1GB > 500MB/s | 顺序展开 + 4MB 缓冲 | 页缓存命中 >1GB/s；冷盘受 IO 限制 |
| 内存 < 500MB @ 4GB 只读 | 1 placeholder + 页缓存 256MB | buffer 自身 < 8KB |
| 滚动 16ms | `read_slices` 零拷贝 | 2KB/帧 = 1 次借用或 1 次 `read_at` |

### 5.2 新增基准与内存模型（`bench/buffer/`）
| 基准 | 场景 | 预算 |
|---|---|---|
| `open_100gb` | 100GB 稀疏 | < 1ms |
| `random_read` / `seq_read` | 1M piece，10 万次随机 1 字节读 / 连读 64MB | p50 < 500ns、p99 < 2µs / > 1.5GB/s |
| `edit_1m_pieces` | 随机 overwrite 1 字节 ×10 万 | p99 < 1ms，piece 增长 ≤ 1.15× |
| `nibble_typing` | 连续 1 万半字节 | 最终 piece ≤ 8；undo 步 = 5000 |
| `undo_large_replace` | 替换 64MB 后 undo | < 1ms，内存增量 ≈64MB（不是 128MB） |
| `save_1gb` | 1GB + 10 万 patch | > 500MB/s，峰值内存 < 32MB |
| `patch_query` / `defrag` | 10 万 patch 查 4KB；100 万碎片 | < 5µs；64MB 预算内 < 2s 且可取消 |

**内存模型（即 00 §7 的"改动×2"）**：`bytes = Σ literal 字节 + 4KB×被降级 placeholder 数 + 32B×piece 数 + 页缓存(≤256MB，独立预算) + 撤销历史(≤undo_mem_limit) + patch old 全文(≤patch_old_budget)`。2× 的来源：一次 overwrite 同时存在"新 literal"与"撤销记录里的 old"各一份 ⇒ 2× 是可达的紧界。
**反模式（CI 要能自动发现）**：piece 数随编辑**次数**线性增长（R1/R3 失效）；跨片读反复重下钻（应为游标推进）；保存期间内存随文件大小增长；patch 表每次编辑后全量 rebuild。

---

## 6 失败模式与降级

| 失败 | 表现 | 降级行为 |
|---|---|---|
| Provider 读失败（IO 错/SSH 断连/进程内存无权限）；Provider 超时（`provider_read_timeout_ms` = 5s） | `read` 返回 `BufferError::ProviderFailed{range, kind}`，**不 panic**（00 §5.1）；该片标 `degraded`，本次读报错 | literal 区**照常可编辑**（编辑不依赖 provider）；不可读区在 05 渲染为"不可读"纹理，patch 标 `unknown_old`；连续 3 次超时 ⇒ provider 进 `offline`，buffer 变"部分可读"，编辑继续 |
| **不可读区上的撤销** | 仍需可用 | `EditOp::Remove{PieceView}` 只记**视图**不读数据（§4.5 混合策略的第二个红利）⇒ provider 挂了也能撤销 |
| 尺寸变小（`poll_change = Resized(smaller)`） | `src_off+len > new_size` 的 Win 片标 invalid | 读该区间返回 `Hole` 而非错乱旧数据；**不**自动 reload（§4.7） |
| 不可写（`caps.writable = false`）/ 默认只读打开（00 §9.4） | 只读模式 | 编辑原语返回 `NotWritable`；UI **灰掉**命令（不静默失败）；用户显式"允许编辑"后才可写 |
| OOM / 超大操作 | 单次 literal >256MB 拒绝 | >64MB 自动转 `TempFileProvider`（§3.4），内存不爆 |
| 保存失败（§4.6 分支）/ 磁盘写满 | `Failed(...)`；原文件**从不被触碰** | 重试/另存为；patch 摘要 ≤16MB 时仍可导出 `.geopatch` 到别处，数据不丢 |
| journal 残留（上次崩溃） | 启动扫描 | temp 完整 ⇒ 提供恢复；缺失 ⇒ 删 journal + 提示"原文件完好" |

**总原则**（00 §9.3）：一次 provider 故障只让"那一段字节"不可读，绝不让文档不可编辑、不可保存（另存为永远可行）。

---

## 7 与其它子系统的契约

### 7.1 读写矩阵
| 子系统 | 读 | 失效协议 |
|---|---|---|
| `05` 渲染 | `Snapshot`/`PieceCursor`/`read_slices`/`patches().query` | `version` 变 ⇒ 只失效**可见区**行缓存，不做全量失效 |
| `08` 搜索；`09` diff / `geode-annot` | `cursor` 流式 + `version`；`patches()` + §7.2 映射 | 结果带 `version`，不匹配即丢（00 §6 规则 3）；标注**永远存原文件坐标**，不因编辑重写索引 |
| `02` BinSpec | `read` + dirty 区间 | overwrite ⇒ 只重跑相交单元；insert/remove ⇒ 全量失效（02 §7.4） |
| `06` provider | 持有 `Arc<dyn Provider>` | `poll_change` 由 buffer 轮询；4KB 页缓存与 `page_ref` 借用是 06 的责任 |
| `17` 项目/恢复 | journal 路径与格式 | 崩溃恢复扫描 |

### 7.2 区间映射（C3 的落地）
```rust
pub enum MapResult { At(u64), Removed, Unmapped }
impl PieceBuffer {
    pub fn map_orig_to_new(&self, orig: u64) -> MapResult;   // O(log P)，P = patch 数
    pub fn map_new_to_orig(&self, cur: u64) -> MapResult;
}
```
标注、书签、结构节点的 `ByteRange` 一律存**原文件坐标**，渲染时换算 ⇒ overwrite（不改长度）时零成本；insert/remove 时只是 patch 表变化，索引本身不动。
**版本协议**：`version()` 在**每次 commit** 时 +1（不是每个 `EditOp`）；undo/redo/reload/patch 应用也 +1；维护性操作（coalesce/defrag）**不**变。所有异步结果必须带版本（00 §9.2）。

---

## 8 配置项与默认值

| 配置 | 默认 | 范围 | 改错的后果 |
|---|---|---|---|
| `btree_fanout` / `leaf_max_pieces` | 64 / 64 | 32~128 / 16~128 | 改小 ⇒ 树高↑；改大 ⇒ 节点跨页、定位两次 miss；<16 装载率失控 |
| `min_placeholder_bytes` | **4096** | 1024~65536 | **低于 4096 引发 piece 爆炸**（§3.3） |
| `coalesce_max_bytes` | 1MB | 64KB~16MB | 太大 ⇒ 单次合并拷贝卡帧 |
| `paste_to_tempfile_threshold` / `max_literal_bytes` | 64MB / 256MB | 8MB~1GB / 32MB~2GB | 太小 ⇒ 临时文件满天飞；太大 ⇒ OOM |
| `default_edit_mode` / `overwrite_past_eof` / `open_readonly_default` | **Overwrite** / Append / true | — | 默认 Insert 会让所有下游频繁失效；只读默认是 00 §9.4 的安全前提 |
| `nibble_group_window_ms` | 400 | 100~1500 | 太大 ⇒ 不同字节被并进一个撤销步 |
| `undo_mem_limit` | `min(512MB, max(64MB, 2%×len))` | 32MB~4GB | 太小 ⇒ 撤销步数骤减 |
| `undo_min_steps` / `undo_low_watermark` | 200 / 0.75 | 20~5000 / 0.5~0.9 | 水位 1.0 ⇒ 每步都触发回收（抖动） |
| `patch_old_full_limit` / `patch_old_budget` / `patch_merge_gap` | 4MB / 128MB / 16 | 64KB~64MB / 16MB~1GB / 0~256 | 太大 ⇒ patch 表吃满内存、假连续色块 |
| `save_buffer_bytes` / `editable_during_save` | 4MB / true | 256KB~64MB | 缓冲太大 ⇒ 保存峰值内存高；禁编辑 ⇒ 大文件保存时 UI 冻结 |
| `save_fsync` / `save_fsync_dir` | true / true | — | 关掉 ⇒ 掉电可能丢文件（仅测试） |
| `backup_mode` / `backup_size_limit` | **HardLink** / 2GB | — | Copy 会拷贝 100GB |
| `journal_enabled` / `journal_max_bytes` | true / 64KB | — | 关掉 ⇒ 保存中断不可恢复 |
| `verify_after_save` | SampledBlake3 | None / Size / SampledBlake3 | None ⇒ 静默写坏 |
| `external_check_interval_ms` / `external_sample_bytes` | 5000 / 40KB | 1s~60s / 8KB~1MB | 采样太大 ⇒ 检测变慢，破坏 O(1) |
| `editable_during_save` | true | — | false ⇒ 大文件保存时 UI 冻结 |
| `defrag_piece_soft_limit` / `provider_read_timeout_ms` | 1_000_000 / 5000 | — | 前者只告警，不拒绝编辑 |

---

## 9 测试策略

### 9.1 与 `Vec<u8>` 黄金模型对拍（最重要的一条）
参考实现 `ModelBuffer = Vec<u8>` + `Vec<Vec<u8>>` 撤销栈。生成器 `op_gen(seed) -> Vec<Op>`，`Op ∈ {Overwrite, Insert, Remove, Undo, Redo, Checkpoint, ReadRandom, SaveRoundtrip, Defrag}`，权重偏向边界：`0`、`len`、`len−1`、跨 4KB 阈值的切点、刚好小于/等于 `min_placeholder_bytes` 的长度。每步断言：`len()` 相等 → 随机 8 次区间读逐字节比对 → `patch_set` 应用后能重建出当前内容。规模：CI 每次 10⁵ 步，nightly 10⁷ 步；操作序列自身可收缩（长度二分 + 单步简化），不依赖框架默认 shrinker。

### 9.2 Property test（proptest）
| # | 性质 |
|---|---|
| P1 | `read(any range) == model[range]`（裁剪后） |
| P2 | `Σ piece.len == len()`；所有 `piece.len > 0`（空 buffer 除外） |
| P3 | 任意 placeholder `len ≥ min_placeholder_bytes` 或被标 `degraded` |
| P4 | 无"相邻且可合并"的 piece（coalesce 后） |
| P5 | `undo^k ∘ redo^k ≡ id`（k ≤ 历史深度） |
| P6 | `map_new_to_orig(map_orig_to_new(x)) == x`（未删除点） |
| P7 | `PatchSet` 增量维护 ≡ 从树全量重建 |
| P8 | 保存 → 重开 ⇒ 字节完全相同（含 mtime/权限保留断言） |

**树不变量检查器**：`check_invariants() -> Result<(), Violation>`，`debug_assertions` 下**每次编辑后**运行：`cum` 单调且 `last == bytes`、`Inner.bytes == Σ kid.bytes`、装载率 ≥25%、所有叶子深度相同、`Arc::strong_count` 在 drop 后回落（防泄漏）、无零长 piece、无相邻可合并 piece。

### 9.3 Fuzz、故障注入与集成
1. **目标 A** 任意字节流 → 解码成操作序列 → 执行，断言不 panic、无 UB、不变量保持（把输入当"恶意用户输入"）；**目标 B 保存事务故障注入**（failpoints：temp 写到一半失败、`fsync` 失败、`rename` 失败、`fsync(dir)` 失败、`SIGKILL`），断言原文件 blake3 **逐字节不变**——这是 C2 唯一的机器可验证形式；**目标 C** patch 导出/导入 round-trip，含 IPS 的 16MB 边界与 0xFFFF 拆分边界。
2. **集成与基准**：打开 → 编辑 → 保存 → 重开 → 与模型比对（端到端）；外部修改三种策略各一条用例（可控 mtime）；只读/`EBUSY`/`ENOSPC`/`EXDEV` 用 mock `SaveSink` 模拟，不依赖真实文件系统。跨平台矩阵：Linux（ext4/xfs）、macOS（APFS）、Windows（NTFS），FAT/exFAT 的硬链接降级单列一条。§5.2 基准全部接入 CI，退化 >5% 红灯（阈值口径统一见 21 §4.3）（00 §7「没有基准的功能等于没有功能」）。

---

## 10 开放问题

| # | 问题 | 候选 | 需要什么数据 |
|---|---|---|---|
| Q1 | 叶子 rebalance 策略 | 惰性 + 阈值触发（倾向）/ 每次编辑后严格维护 | `edit_1m_pieces` 上 piece 数与 p99 曲线 |
| Q2 | `read_slices` 零拷贝能否落地 | 需 06 提供 `page_ref(off) -> Arc<Page>` | 能否在不改 00 §5.1 `Provider` 签名的前提下做到；否则退回拷贝并重算滚动预算 |
| Q3 | 全零 run 的空洞保真（附录 A.2） | `SEEK_HOLE` 保真 / 一律实体化 | 100GB 稀疏镜像的真实保存耗时与占用 |
| Q4 | 保存期间二次编辑的增量续写 | v1 不做；v2 用"patch 游标续写" | 用户在大文件保存时是否真的继续编辑 |
| Q5 | `ExternalChange` 屏障的历史语义；撤销 collapse 后 UI 如何呈现"一段历史已合并" | 屏障（当前）/ 清空历史；显示为粗粒度步并标注 | 与 17 崩溃恢复交互时的用户预期；可用性测试 |
| Q6 | nibble 合并窗口 400ms 是否合适 | 400ms / 按"同一字节未离开光标"判定 | 真实打字速度分布 |
| Q8 | 是否支持多版本 buffer 分叉（"假如改成这样"实验） | 当前否决，但 B-tree 可平滑演进而 RRB 不易 | 是否进入路线图 |

---

## 附录 A — 安全坑位（独立一节，必读）

### A.1 mmap 的文件被外部截断 → SIGBUS
**成因链（精确）**：`mmap` 只建立虚拟映射，物理页在**首次访问时**由内核从文件填充。若映射建立后文件被外部 `truncate` 变小（或 NFS 上被别的客户端截断），访问超出新 EOF 的页时内核无法填充 ⇒ 向进程发 `SIGBUS(BUS_ADRERR)`。
**为什么"捕获并恢复"不可行**：信号处理器里只能做 async-signal-safe 的事；`panic!` 与 `siglongjmp` 跳出信号上下文都是 UB；即便捕获也无法可靠区分"是我们哪个映射、哪个偏移"。**结论：否决一切 signal-based 恢复方案。**

| 措施 | 说明 |
|---|---|
| **不对用户可变文件 mmap（主防线）** | 用户文件一律 `pread` + LRU 页缓存（06）。越界返回 `Err`，永不 SIGBUS。代价：一次 syscall（~1µs）+ 一次拷贝，用 `preadv` 合批与 `read_slices` 借用抵消 |
| mmap 仅用于"我们独占创建"的文件 | 保存用临时文件、我们自己的只读快照缓存 |
| 若某 provider 必须 mmap（01 §3.1 的 `LocalFile(mmap)`） | 持 fd；每次**批处理**前 `fstat` 校验 `size` 未变，处理期间接受"中途变化 ⇒ 本次批处理作废"；禁止长时间无校验的随机访问 |
| 最后防线（仅用于崩溃上报） | 装 `SIGBUS` handler，只做"标记 + `_exit(70)`"，**不做恢复** |
| Windows 等价物 | 映射期间别人 `SetEndOfFile` ⇒ 访问触发 `EXCEPTION_IN_PAGE_ERROR`（SEH），同一结论：不用 mmap 读可变文件。反过来，**我们**映射别人的文件会让对方 `SetEndOfFile` 失败（`ERROR_USER_MAPPED_FILE`）——这是"不 mmap 用户文件"的第三个理由（另两个：SIGBUS 不可捕获、我们从不 truncate 原文件故无需 mmap 自保） |

### A.2 位宽、超大文件与稀疏空洞
- 全程 `Offset(u64)`（00 §4），**禁用 `usize`**：32 位上 `usize` 是 32 位，`len()` 会静默截断；仅支持 64 位目标（`#[cfg(not(target_pointer_width="64"))] compile_error!`）。
- 系统调用层受 `off_t`（i64）限制：`file_len > i64::MAX` 时打开即失败（`FileTooLarge`），不尝试、不截断。
- 所有偏移运算用 `checked_add`/`checked_sub`：溢出返回 `Err(OutOfBounds)`，绝不 wrap。
- **稀疏文件**：读无坑（`pread` 对空洞返回 0，POSIX 保证）；保存才是坑——顺序展开写会把稀疏文件**实体化**（100GB 稀疏 → 100GB 实占）。v1 策略：识别 **≥1MB 的全零 run 且原文件同位置也是空洞**（`SEEK_HOLE`/`SEEK_DATA`，Linux/macOS；不支持则降级"不保真"）⇒ `seek` 跳过 + 末尾 `ftruncate` 补尺寸；Windows 用 `FSCTL_SET_ZERO_DATA`。保存结果必须写明 `holes_preserved: bool`，不能保真时提示"保存后文件将占用完整大小"。`caps.holes` 的 provider（进程内存）空洞读取失败 ⇒ 渲染"不可读"（00 §5.1），对该区编辑仍允许（写入 literal，覆盖空洞）。

### A.3 保存路径与尺寸竞态的其它坑
- **目标路径是符号链接**：必须解析真实路径，在**真实文件的父目录**建 temp，否则 `rename` 会替换链接本身而不是目标文件（经典丢数据方式）。
- 临时文件名可预测 ⇒ `pid` + 128 位随机 + `O_EXCL|O_NOFOLLOW` 创建，防符号链接攻击。
- 不请求提权（不 sudo、不 UAC）；权限不足只提供"另存为"。`SIGKILL` 残留的 `.geode-*.tmp` 启动时清理 7 天前的（且不触碰有匹配 journal 的）。
- **永不原地写原文件**是本文唯一需要逐条代码审查的红线。
- **尺寸竞态（TOCTOU）**："读 `size` → 分配缓冲 → 读"之间文件可变。规则：**先读后校验**——`read_at` 短读即视为并发变化（`ShortRead`），重试 ≤2 次，仍不一致 ⇒ `ProviderFailed`。全程无 `unwrap`/`expect`（00 §9.1）。
