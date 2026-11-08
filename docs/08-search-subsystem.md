# 08 — 搜索子系统（四种引擎）

> 状态：设计文档，未实现。上级契约见 `00-architecture-overview`，冲突以 00 为准。
> 本文**拥有**：四种引擎的语义与算法、扫描框架、结果集模型、搜索空间语义、结果交付方式。
> 本文**不涉及**：结果列表的视觉呈现与高亮合成（见 05-render-and-hexview）、标注索引内部结构（见 00 §5.3）、显示前变换本身（见 07-data-processor-graph）。

---

## 1 目标与非目标

| # | 目标 | 验收 |
|---|---|---|
| G1 | 四种引擎共享一个 `Searcher` 抽象与一套扫描框架 | 新增引擎只实现 `Searcher`，不改框架 |
| G2 | 100GB 上搜索**完全后台**、零 UI 占用、取消 < 100ms | 搜索中滚动 p99 < 25ms（00 §7） |
| G3 | 结果**不复制字节**，只存区间；10 万命中 ≤ 8MB | 内存基准 |
| G4 | 吞吐达 §5 预算，退化即 CI 红灯 | `bench/search.rs` |
| G5 | 结果可回链字节区间并进入 `Layer::Search`，双向联动 | 点结果→视图定位，点高亮→列表定位 |

**非目标（含否决理由）**

- **不做索引**（倒排 / 后缀数组）：100GB 建索引的时间与磁盘代价远超收益，且 Buffer 可编辑使索引维护成本爆炸。全部引擎一律**流式单趟扫描**。否决"对反复搜索建 n-gram 索引"（v2 仅考虑 `Known` 尺寸的只读 Provider）。
- **不做模糊/相似度搜索**（属 09-diff-subsystem）。
- **不做替换的事务语义**：替换只在 04 的 `checkpoint` + patch 事务内进行，本文只定义"命中集合 → 替换请求"的形状。
- **不做跨文档搜索**（工作区级 grep），接口预留 `DocId`。
- **不做 UTF-16 数据上的正则**（§4.3.6）。

## 2 依赖与边界

| 上游 | 用途 |
|---|---|
| `geode-core` | `Offset`/`ByteRange`/`JobId`/`Diag`/`CancelToken`（00 §4） |
| `geode-buffer` | `read` + `version()`，只读快照（00 §5.2） |
| `geode-provider` | `Caps{volatile, seekable}`、`poll_change()`（00 §5.1） |
| `geode-annot` | 批量写入 `Layer::Search`（00 §5.3） |
| Job 系统 | 每次搜索 = 1 个 `Job` + `CancelToken` + progress（00 §5.4、§6） |
| 外部 crate | `memchr`、`regex`/`regex-automata`/`regex-syntax`、`encoding_rs`、`rayon`、`aho-corasick` |

**下游**：`geode-ui`（搜索面板与结果列表，§7.3）、`geode-analysis`（沿命中分布统计）、`geode-yara`（复用 `ScanDriver`，不复用 `Query`）。

**铁律**：① 不依赖任何 GUI crate（00 §2）；② 不写 Buffer，编辑只在 UI 线程（00 §6 规则 2）；③ 结果回主线程校验 `version()`，不匹配整批丢弃（00 §6 规则 3）；④ 任何扫描路径每 64KiB 检查 `CancelToken`（00 §6 规则 4 的落地手段）。

## 3 数据结构与接口

### 3.1 Query / SearchRequest

```rust
pub enum Query { Bytes(BytePattern), Text(TextQuery), Regex(RegexQuery), Numeric(NumericQuery) }

pub enum PatToken { Exact(u8), HighNibble(u8), LowNibble(u8), Any }  // 0xDE / ?E / D? / ??
pub struct BytePattern { pub tokens: Vec<PatToken>, pub ignore_case: bool }  // 半字节通配 + ignore_case → 拒绝（§6 F5）

pub struct TextQuery {
    pub text: String, pub encodings: Vec<Encoding>,   // 可多选，命中按编码标记
    pub case: CaseMode,                                // Sensitive | SimpleFold | FullFold
    pub nul_terminated: Option<u8>,                    // Some(1|2|4)：匹配后须紧跟 N 个 0
    pub max_len: Option<u32>,
}
pub struct RegexQuery { pub pattern: String, pub options: RegexOptions,
    pub overlapping: bool /*默认 false*/, pub max_match_len: u32 /*默认 1 MiB*/ }
pub struct NumericQuery { pub width: u8 /*1..=8*/, pub endian: Endian,
    pub kind: NumKind /*Int{min,max,signed} | Float{min,max,allow_nan}*/,
    pub alignment: Option<u8> /*None=任意偏移，默认*/ }

pub struct SearchRequest { pub query: Query, pub space: SearchSpace /*默认 Source，§3.4*/,
    pub scope: ByteRange, pub limit: u32 /*默认 100_000*/,
    pub mode: SearchMode, pub doc: DocId }
pub enum SearchMode { Collect, FirstOnly, CountOnly }
```

### 3.2 结果集（不复制字节）

主存储用 **SoA**：`Vec<u64> starts` + `Vec<u32> lens` + `Vec<u8> tags`，变长附属信息走侧表。单命中 **12B**（`len` 用 `u32`，前提是单次命中 ≤ `MAX_MATCH_LEN = 1 GiB`，超出截断并警告）。**10 万命中 ≤ 8MB**（主体 1.2MB + 侧表通常为空）。

```rust
pub enum MatchAux { None, Group{index:u8, range:ByteRange}, Decoded{enc:Encoding, chars:u16}, Value{bits:u64} }
pub struct SearchResults {
    pub job: JobId, pub base_version: u64,        // = buffer.version()，失效判定
    pub space: SearchSpace, pub ranges: HitRanges, pub aux: Vec<(u32, MatchAux)>,
    pub total_found: u64, pub truncated: bool, pub complete: bool,
}
```

**命中上限语义**：默认 `limit = 100_000`；达上限后仍继续扫描（为 `total_found` 准确），除非 `stop_at_limit`（默认 false——用户更常问"到底几处"）。`truncated` 时 UI 固定文案：**"结果已截断：显示前 100,000 条，共 N 处命中"**。

### 3.3 Searcher / Engine

```rust
pub trait Searcher: Send + Sync { fn compile(&self, q: &Query, cfg: &ScanConfig) -> Result<Box<dyn Engine>, SearchError>; }

pub trait Engine: Send + Sync {
    fn scan_block(&mut self, lane: &ChunkLane, out: &mut HitSink) -> Result<(), SearchError>;
    fn overlap(&self) -> u32;              // 跨块匹配的必要下界，§4.5.2
    fn crosses_boundary(&self) -> bool;    // 决定是否启用去重规则
    fn min_advance(&self) -> u32 { 1 }
}
pub struct ChunkLane<'a> {
    pub bytes: &'a [u8],    // 从 block_start 起，长度 = chunk + overlap
    pub base: u64, pub block_start: u64, pub block_len: u64, pub scope_end: u64,
}
```

### 3.4 搜索空间语义（最易困惑，必须写清）

```rust
pub enum SearchSpace { Source, Display { graph: GraphHash, view: ViewId } }
```

**决策：默认 `Source`（搜 Buffer 的字节，含 patch 后的状态）。** 理由：① 00 §C3 要求一切可回链字节区间，`Ann.range` 必须是 Buffer 偏移，否则高亮/跳转/导出/patch 都要二次映射，漏一处就错位；② 数据处理器（07）不一定**保长**（解压类无双射，位平面提取会重排），映射可能非单调甚至不存在；③ 用户直觉是"搜文件里的字节"，010/ImHex 默认也搜源数据。

**`Display` 的充要条件**（否则菜单置灰 + tooltip 说明）：`caps.len_preserving == true` ∧ 提供单调可逆 `OffsetMap` ∧ `caps.deterministic == true`。

**UI 常驻提示（不可关闭，搜索框下方 12px 灰字）**：

| space | 文案 | 列表附加列 |
|---|---|---|
| `Source` | `搜索范围：源数据（不含数据处理器视图）` | 无 |
| `Display` | `搜索范围：显示数据（经 <处理器名>；偏移已映射回源）` | 图标 `⇄` |
| 处理器图已变更 | `显示数据搜索不可用` + 输入框禁用 | — |

`Display` 下若命中映射回源后**不连续**（跨重排边界），标 `NonContiguous`：灰显、不可跳转、单独计数 `n_unmappable`。

### 3.5 结果如何进入 `Layer::Search`

1. 每条命中生成 `Ann { layer: Layer::Search, tag: AnnTag::SearchHit(i), priority: 0 }`（层内 priority 统一为 0，见 §7）。
2. **整段区间标注**（不切分），因 `query(range)` 是 O(log n + k)，不切分是 k 最小的选择。例外：命中 > 4KiB 时只标前 256B + 末 64B + 一个 `SearchEllipsis` 锚，防止单条命中撑爆区间树。
3. **批量插入**：索引写只在 UI 线程（00 §6），10 万条 ≈ 50ms，故 **4096 条/帧**（每批 < 1.5ms）；首批 1000 条在第一帧立即插入，保证马上可跳转。
4. 生命周期：插入前 `clear_layer(Layer::Search)`，只保留当前一次搜索（多结果集并存见 §10 Q2）。

## 4 核心算法/流程

### 4.1 引擎①：hex 字节序列（带通配）

**语法**：`DE AD ?? EF`，空白可选（`DEADBEEF` 也接受）。`?F` = 高半字节任意/低必须 F，`F?` 对称，`??` = 整字节任意。`ignore_case` 只对 `Exact` 生效（A↔a 即 0x20 位翻转）；**与半字节通配组合直接报错**（`?F` 忽略大小写无定义，§6 F5）。编译产物 `Vec<(mask, value)>`。

**锚点选择**（吞吐的关键）：

```
1. 找 tokens 中最长的「连续全 Exact」段 L。
2. 无任何 Exact（全通配）→ 退化路径 W1。
3. 平局裁决：段最长 → 起始最靠前 → 段首字节在 256 桶频率表中最罕见
   （频率表复用 10-analysis-toolkit §4.4 的 1-gram 直方图，缺失则跳过该条）。
4. 锚长 1 → memchr 单字节跳转；锚长 ≥2 → memmem::find（Two-Way + SIMD）。
5. 命中锚点后双向校验前缀/后缀（含掩码），16 字节一组做 (a & m) == v 的 SIMD 掩码比较。
```

**无固定锚点的退化（W1）**：模式全是 `Any`（如 `?? ??`）→ 直接拒绝（`E7102 模式无信息量`），否则 1GB 上产生 10 亿命中。只有 `HighNibble`/`LowNibble` 而无 `Exact` → 半字节锚：`(b & 0xF0) == h0` 的 SIMD 掩码扫描，吞吐约为 memchr 的 1/3~1/2。**要求 > 200MB/s；纯固定模式 ≥ 800MB/s**（对齐 00 §7 两行预算）。

**多模式**：`patterns > 8` 且全部无通配 → `aho-corasick`（`match_kind(Standard)`）；含通配的多模式 → 逐模式 memmem + k 路归并（取舍见 §10 Q9）。

### 4.2 引擎②：字符串多编码

**编码清单**：ASCII、UTF-8、UTF-16LE、UTF-16BE、UTF-32LE、UTF-32BE、Latin-1、EBCDIC(CP037)、GB18030、Shift-JIS(CP932)。`encoding_rs` 覆盖 GB18030/Shift-JIS；**UTF-32 与 EBCDIC 它不支持** → 自实现（EBCDIC 为 256 项表，以资源内置、可被内容包覆盖）；**Latin-1 不要用 `windows-1252` 顶替**（0x80~0x9F 语义不同）。

**核心决策：不把数据解码成 UTF-8 再搜。** 解码会让 100GB 文件 OOM（或 ≥4× 膨胀），且把无效字节变成替换字符，破坏区间可回链（C3）。正确做法是**把查询串编码成目标编码，再做字节搜索**。

```
大小写不敏感（SimpleFold，默认的非敏感模式）：
  a. 对查询每个码点取「简单折叠闭包」（CaseFolding.txt 的 C+S 行，1:1，长度不变）
  b. 每个位置得一个可接受码点集合 → 分别编码 → 得"每位置字节集合序列"
  c. 变体总数 ≤ variant_cap(256) → 展开成显式模式列表交给 Aho-Corasick；
     否则用"逐位置集合 + 定长 stride 匹配"（UTF-16/32 天然定长）
FullFold（opt-in）：折叠可改变长度（ß→ss、ﬁ→fi、İ→i̇），
  必须编译成小分支树（同一位置接受 1~3 码点），超 cap 则拒绝并提示改用 SimpleFold。
```

**Unicode 折叠的坑（必须写进实现说明）**：① **长度可变**——任何"折叠后逐字符定长比较"的假设都是错的，故默认只用 1:1 的 SimpleFold；② 土耳其语 I/ı 与 i/İ 的折叠依 locale，**不提供 locale 相关折叠**（用 `toCaseFold`），UI 不暴露 locale；③ **不能按 0x20 位翻转**——那只对 ASCII 成立，对 UTF-8 续字节翻转会产生非法序列；④ `K`(U+212A)→`k`、`ς/σ` 互折都已在表里，别手写特例。

**BOM**：`BomPolicy::Detect`（默认）——`scope.start==0` 且前 2~4 字节为 BOM（`EF BB BF`/`FF FE`/`FE FF`/`FF FE 00 00`/`00 00 FE FF`）时，`encoding=Auto` 据此锁编码，且**BOM 字节不作为任何命中的起始位置**（避免搜 `ÿþ` 命中 BOM）。BOM 不被跳过——它仍在 buffer 里，只被标记为"不可起始"区，偏移不漂移。

**结束符与最大长度**：`nul_terminated=Some(n)` 表示"匹配后须接 n 个 0 字节"（C 字符串 / `char16_t`，n = 编码单元宽）。这是**后置断言**，命中区间仍是字符串本身（不含结束符）——用户复制命中时应拿到干净字符串。`max_len` 仅在有结束符或多编码候选时生效，默认 `None`。**要求 ≥ 300MB/s**（ASCII/UTF-8 实测 GB/s 级，UTF-16 走 stride-2 约 1.5GB/s；300 是含候选集与折叠路径的保守下限；00 §7 无此项，建议回填）。

### 4.3 引擎③：正则

**引擎**：`regex::bytes`（meta engine：字面量预过滤 + lazy DFA + 必要时 NFA/OnePass 回退），**最坏 O(n·m) 线性时间**，与输入内容无关。

**明确不支持及原因**：反向引用 `(a)\1`——使匹配失去正则语言性质，需回溯或标记存储，最坏 O(2^n)，且无法用 DFA 表示，与流式状态续接根本冲突；lookaround `(?=) (?<=) (?!)`——需同时向前/向后看，`(?<=)` 要反向 DFA，且断言跨度可任意，破坏分块流式前提；递归/平衡组同理。UI 给人话提示："正则不支持反向引用与环视。若要匹配 'A 后跟 B'，请用 `A(?s).{0,64}?B` 这类有界跨度写法，或改用字节/数值搜索。"（附 `E7103` 供 AI 修复闭环，见 03-ai-dsl-generation）。

**跨块边界（本节真正难点）——三方案与决策**：

- **方案 A（默认）：有限最大长度 → 重叠窗口。** 用 `regex-syntax` 的 HIR，取 `hir.properties().maximum_len()`；`Some(l)` → `overlap = l-1`（封顶 `MAX_OVERLAP = 1 MiB`）。覆盖绝大多数实用正则（magic + 定长字段、`\x89PNG\r\n\x1a\n.{0,64}`）。2MiB 块 + 64B 重叠 → 吞吐损失 0.003%。
- **方案 B（无界时）：lazy DFA 跨块状态续接。** 取 `regex_automata::hybrid::dfa::DFA`（`cache_capacity ≤ dfa_cache_limit = 10MiB`），框架**手动字节循环**：逐字节 `next_state(&mut cache, cur, input)`，块边界只是"不重置状态"，在进入 special state 时记录 `(match_end, start_of_attempt)`——因为 `start_state_forward` 无法注入任意起始状态，必须自己喂字节。为防止无界模式（`a.*b` 且全文无 `b`）让"起始点集"持续膨胀，用 `min_match_start` 水位线：某次尝试已扫过 `max_match_len` 仍未命中即丢弃该尝试、从其后 `min_advance` 重起。代价：**并行度降为 1**（状态续接天然串行），吞吐按 1/P 下降。取舍明确：正确性优先。
- **方案 C（兜底）：固定重叠 + 显式警告。** `maximum_len()` 无界 **且** hybrid DFA 超 `dfa_cache_limit`（如巨型 Unicode 类）→ 退回 A 的形式，`overlap = fallback_overlap = 64KiB`，面板顶部常驻黄色提示 **"该模式匹配长度无上界，已使用 64KiB 重叠窗口；跨越窗口的长匹配可能被漏掉。"** + `Diag{Warning, E7104}`。

**匹配长度上限**：`max_match_len` 默认 1MiB；超限按 `MatchLenPolicy::ClampAndReport`（默认）：在有界点截断输出并继续，累计 10 次只报一次警告；备选 `Abort`。理由：`(?s)A.*B` 在无 `B` 的文件上会把整文件吃成一条命中，结果集与高亮全失控。

**重叠匹配**：`overlapping=false`（默认，`find_iter` 语义）；`true` 时从 `hit_start + min_advance(1)` 重起，最坏 O(n·L)，故设 `overlap_match_cap = 10_000`，达到即停并标 `truncated`。**要求 > 80MB/s**（对齐 00 §7）。

### 4.4 引擎④：数值范围

**范围 → 字节约束**：定义 MSB（最高有效字节）位置——**大端在 `offset+0`，小端在 `offset+width-1`**。所有过滤器都作用在 MSB 上（选择性最高），LSB 近似均匀分布、几乎无选择性，只做最后确认。任意闭区间可分解为至多 `2·width` 个"前缀区间 × 后缀自由"的矩形（radix 分解）。

```
相 A（粗过滤）：候选 MSB ∈ 集合 S。
    |S|==1（实际查询 >80%）→ memchr 单字节跳转，最快路径
    |S|<=8 → memchr 多次 + 块内位置归并
    |S| 很大 → 256 位 bitset 逐字节过滤，整段前缀无效则直接跳到下一块
相 B（精确）：对候选位置用 ≤2·width 个矩形做「前缀字节区间比较 + 后缀字节区间比较」，
    全不匹配才做一次完整整数解码比较。全程不分配、不做大小端交换（按字节序直接比）。
相 C（对齐）：alignment=Some(a) 时在相 A 的 memchr 循环里跳过不合法偏移（而非事后过滤）。
```

**浮点**：用 IEEE-754 totalOrder 映射 `key = if sign { !bits } else { bits | 0x8000_0000 }`（f64 同理），它单调覆盖所有位模式（含 NaN、±0）→ **浮点范围搜索整体归约为 §4.4.1 的整数路径**（过滤器作用在 key 的 MSB 上，即符号+指数位，选择性极好）；候选命中后逆变换回原始 bits 再 `from_bits` 精确判定并填 `MatchAux::Value`。

**NaN**：`allow_nan=false`（默认）时相 B 直接否决 `is_nan()`。`true` 时 NaN 的 key 分布是**两个不连续区间**，`f32` 有 `2^23-2` 个正 NaN 位模式不能枚举 → 单独一条谓词扫描（`指数全 1 ∧ 尾数 ≠ 0`），**无前缀锚点 ≈ 全量扫描**，UI 提示"含 NaN 的搜索不使用快速锚点"。**`-0.0` 与 `+0.0` 视为不同值**（totalOrder；key 分别 `0x7FFF_FFFF`/`0x8000_0000`）；否决"按 `==` 合并"，否则无法单独搜 `-0.0`。`width > 8` 编译期拒绝（`E7105`）。**要求 ≥ 400MB/s**（MSB 单值路径；本文自定，00 §7 未列）。

### 4.5 扫描框架

| 参数 | 默认 | 说明 |
|---|---|---|
| `chunk_size` | **2 MiB** | 贴合 L2/L3；256KiB~16MiB 可配 |
| 每线程缓冲 | 2MiB+overlap | thread_local 复用，16 线程 ≈ 32MiB |
| 并行度 | `min(线程数, ceil(scope/chunk/4))` | 块 < 4×线程数时不并行（省 rayon 开销） |

```
1. 切块 → 每块封装 ChunkLane（一次 read 覆盖 chunk+overlap，避免二次系统调用）
2. rayon par_iter + try_for_each：
   每 64KiB 检查 cancel → Err(Cancelled)；结果写 results[block_idx]（索引式，天然有序）
   每 100ms 节流发一次 progress
3. 合并：按块下标 concat，**无需排序**（块内升序 + 块有序）
4. 去重（仅 crosses_boundary 引擎）：丢弃 start ∉ [block_start, block_start+block_len) 的命中
   —— 规则「跨块匹配由起始块负责」，在产生点消灭重复，而非事后按区间去重（O(k log k)）
5. total_found 全量累加；区间存到 limit 为止
```

**overlap 取值**：①`len-1`（≤4095）；②`max_encoded_len-1`（≤65536）；③`Lmax-1` / 64KiB 兜底 / streaming；④`width-1`（≤7）。

**取消与进度**：检查粒度 64KiB → 单线程 100ms 可弃 ~50MB，16 线程同时响应，实测典型 < 30ms、上界 < 100ms。取消**不丢弃**已有结果（`complete=false`，UI 显示"已取消（已找到 N 处，未扫完）"）。`SizeInfo::Unknown` 的流源只报"已扫描 X 字节"、不并行、不支持反向搜索。

**内存上限与流式回调**：`Collect` 总开销 < 64MiB（结果集 + 缓冲）；`FirstOnly` **零结果集分配**，`HitSink` 为 `FnMut(Match) -> ControlFlow`，首个命中即置 cancel（并行下额外延迟 ≤ 10ms），用于"跳到下一处"；`CountOnly` 内存 O(1)，用于降级。

### 4.6 增量与后台

**决策：编辑后不自动重跑。** 理由：① 搜索可能持续数分钟，自动重跑与编辑抢 CPU，体验是"改一个字节风扇起飞"；② 结果绑定 `base_version`，过期即标 `Stale`——UI 用可关闭提示条 **"搜索结果已过期（基于版本 N，当前 M）— [重新搜索]"**，列表变灰但仍可点击（偏移有效，只是可能不再命中）。配置 `auto_rerun=true` 时：**防抖 500ms 静默**且仅当 `scope ≤ 64MiB` 生效；更大文件一律手动。**不做增量修补**（"只重算改动附近"）——对跨区间匹配与删除导致的偏移平移都会出错，收益不抵风险（§10 Q4）。

**完全后台**：Job 只在 rayon 池跑、只读 `Arc<Snapshot>`；UI 线程只做 `drain(UiEvent)` + 批量插标注（§3.5），搜索中滚动 p99 < 25ms 是验收项。

**分页返回**：`UiEvent::SearchBatch{seq, ranges}`，每 **1000 条**或每 **50ms**（先到者）投递；首批到达即插标注 → 滚到第一处命中 → 列表可交互，后续批次静默追加。

## 5 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| ①固定模式 | **≥ 800 MB/s** | 1GB，16 字节模式（对齐 00 §7） |
| ①含通配 | **≥ 200 MB/s** | `48 8B ?? 05 ?? ?? ?? ??`（对齐 00 §7） |
| ①全半字节锚 W1 | ≥ 80 MB/s | 退化，只记录不回退 |
| ②ASCII/UTF-8 | ≥ 1.5 GB/s | 固定串 |
| ②UTF-16LE | ≥ 800 MB/s | stride-2 |
| ②8 编码全开 | **≥ 300 MB/s** | 已回填 00 §7 |
| ③正则（有限 Lmax） | **≥ 80 MB/s** | `\x89PNG\r\n\x1a\n.{0,64}`（对齐 00 §7） |
| ③正则（streaming 单线程） | ≥ 60 MB/s | 无界模式 |
| ④数值 MSB 单值 | **≥ 400 MB/s** | u32 范围 |
| ④+allow_nan | ≥ 50 MB/s | NaN 慢路径 |
| 首批 1000 条到达 | < 200 ms | 1GB，命中密度 1/1MB |
| 10 万标注插入 | < 50 ms | 分批后总耗时 |
| 内存（搜索中） | < 64 MiB | 与文件大小无关 |
| 取消延迟 | < 100 ms p99 | 任意时刻 |
| 搜索中滚动帧 | p99 < 25 ms | 00 §7 |

数据：`bench/data/` 可复现生成器（1GB 混合熵：30% 零区 + 文本段 + 随机段；4GB 稀疏；100GB 虚拟 Provider 不落盘）。CI 门 `throughput >= budget * 0.9`（留抖动）。每次搜索记录引擎/块数/overlap/命中数/耗时到性能面板（00 §9 原则 5）。

## 6 失败模式与降级

| # | 失败 | 检测 | 处理（降级而非失败） |
|---|---|---|---|
| F1 | 正则编译失败 | `compile` → `Err` | 内联报错 + 人话提示（§4.3.2），不弹模态框 |
| F2 | DFA 缓存超限 | 构建时 | 降级方案 C + 警告 |
| F3 | 匹配超 `max_match_len` | 扫描中 | Clamp + 一次性警告 |
| F4 | 全 `??`（无信息量） | 编译期 | 拒绝："请至少指定一个固定字节" |
| F5 | 半字节通配 + ignore_case | 编译期 | 拒绝（语义无定义） |
| F6 | 编码不可用（EBCDIC 表未打包） | 编译期 | 该编码置灰，其它编码继续 |
| F7 | 输入非 UTF-8 文本 | 编译期 | 按 Latin-1 解释并提示 |
| F8 | `width>8` / `min>max` | 编译期 | 前者拒绝；后者自动交换并提示 |
| F9 | 分配失败（结果集超大） | `try_reserve` | 自动切 `CountOnly` 重跑，提示"内存不足，仅显示数量" |
| F10 | 扫描中 buffer 被编辑 | 回主线程校验 version | 整批丢弃 + 过期提示（00 §6 规则 3） |
| F11 | `volatile` 源内容变化 | 每次 read 前 `poll_change()` | 中止，报"数据源在搜索期间发生变化" |
| F12 | `Display` 映射失败 | 命中回映射 | 标 `NonContiguous`，灰显不可跳转 |
| F13 | 跨块长匹配可能漏 | 编译期判定 | 常驻警告条 + `E7104` |
| F14 | 用户取消 | `CancelToken` | 保留已有结果，`complete=false`，不报错 |
| F15 | 超上限 | `total_found > limit` | "结果已截断"；导出仍含全部已存区间 |
| F16 | scope 为空 | 编译期 | 立即返回空，不提交 Job |

总原则（00 §9 原则 3）：搜索的任何失败都不得让编辑器不可用，且必须留下"为什么没结果"的可读解释。

## 7 与其它子系统的契约

**与 Buffer/Provider**：只读 `read(ByteRange, &mut [u8])`（04 §3.7/§4.1 的读路径，大块读会命中 04 §4.1.1 的 `preadv` 合批与页缓存）；`volatile` 源每次读前 `poll_change()`；记录 `base_version` 并在回主线程时校验。**否决用 `read_slices` 零拷贝**：切片会切碎块边界与 overlap 语义，而 memcpy 到每线程缓冲（>8GB/s）相对 800MB/s 的扫描只占 ~8%。`Unknown` 尺寸源：不并行、无百分比、不支持从末尾反向搜索。

**与 AnnotationIndex**：只写 `Layer::Search`（写前 `clear_layer`）；**不写** `Selection`——"当前命中"由 geode-ui 用选区表达（05 是 `Selection`/`Cursor` 的唯一写入者），避免与选区语义耦合。跨层 z 序由 05-render-and-hexview §4.7 **固定为 `Layer` 枚举声明序**（`Value < Search < Structure < Selection < Bookmark < Diff < Yara < Ai < Cursor`），本子系统**无权改 z 序**；层内 `Ann.priority` 统一取 **0**（同一批命中彼此不覆盖），稠密行（单行命中 > 32）的截断与 `+N` 角标由 05 处理，本子系统只保证 priority 稳定。

**与 geode-ui（结果列表）**：

```rust
pub struct SearchPanelState { pub results: Option<Arc<SearchResults>>,
    pub current: Option<u32>,  // 当前命中下标，UI 持有，不进标注索引
    pub filter: Option<HitFilter> }
```

- **虚拟化**：固定行高，只渲染 viewport；第 i 行 → `ranges.start(i)` O(1)；滚动条用 `f64`（禁用 `f32`，见 01 §4 第 5 条）。
- **上下跳转**：`next_from(off) = partition_point(|s| s <= off)`，`prev_from` 对称；`wrap_around` 默认 true。
- **命中计数**：`共 N 处` / `显示 M 处`（截断）/ `n_unmappable` / `n_filtered`。
- **导出**：CSV（`offset_hex, offset_dec, len, hex, ascii, engine, enc`）/ JSON（`[{offset,len,hex,engine,value}]`）；**流式写临时文件 + 原子 rename**（与 04 保存事务同规矩）；> `1_000_000` 命中需二次确认。

**与结构树/BinSpec（远期）**：`NumericQuery` 预留 `within_node: Option<NodePath>`，即"只在该节点覆盖区间内搜索"，区间取 `node.offset + node.size`（02 §7.3）；本期只定形状。"按字段名/字段值搜索"属结构树视图过滤，不在本子系统。

**与 geode-yara**：复用 `ScanDriver`（块调度/取消/进度/去重），不复用 `Query`。

## 8 配置项与默认值

落盘位置与 `schema_version` 见 17-project-and-persistence §3.2；本节只定义键、默认值与语义，键名冲突以本节为准。

| 键 | 默认 | 范围 | 说明 |
|---|---|---|---|
| `search.chunk_size` | 2 MiB | 256KiB~16MiB | 分块 |
| `search.threads` | 0（rayon 默认） | 0~64 | 0=自动 |
| `search.limit` | 100_000 | 1~10_000_000 | 命中上限 |
| `search.stop_at_limit` | false | | 达上限即停（牺牲 total_found） |
| `search.space_default` | Source | Source/Display | §3.4 |
| `search.auto_rerun` | false | | §4.6 |
| `search.auto_rerun_debounce_ms` | 500 | 100~5000 | |
| `search.auto_rerun_max_scope` | 64 MiB | | 超出禁自动重跑 |
| `search.bytes.ignore_case` | false | | ① |
| `search.text.encodings` | [UTF-8, UTF-16LE] | | ② 多选 |
| `search.text.case` | Sensitive | Sensitive/SimpleFold/FullFold | |
| `search.text.variant_cap` | 256 | 16~4096 | FullFold 变体上限 |
| `search.text.nul_terminated` | false | | |
| `search.text.bom_policy` | Detect | | |
| `search.regex.overlapping` | false | | ③ |
| `search.regex.max_match_len` | 1 MiB | 4KiB~1GiB | |
| `search.regex.overlap_cap` | 1 MiB | 64KiB~64MiB | 方案 A 上限 |
| `search.regex.fallback_overlap` | 64 KiB | | 方案 C |
| `search.regex.dfa_cache_limit` | 10 MiB | 1~256MiB | |
| `search.regex.overlap_match_cap` | 10_000 | | |
| `search.numeric.alignment` | 无（任意偏移） | 1/2/4/8/width | ④（§10 Q6） |
| `search.numeric.allow_nan` | false | | |
| `search.ui.batch_size` | 1000 | | 每批条数 |
| `search.ui.batch_interval_ms` | 50 | | 每批时间 |
| `search.ui.annots_per_frame` | 4096 | 256~16384 | §3.5 |
| `search.export.confirm_threshold` | 1_000_000 | | |

## 9 测试策略

- **单元**：模式词法/半字节/非法组合（F5）、锚点平局裁决；数值范围分解用**性质测试**——"分解出的矩形并集 == 原区间"，对 `width ∈ 1..=8` 与 10 万随机 `(min,max)` 断言集合相等；编码往返（UTF-16 代理对、GB18030 四字节、EBCDIC 256 项无重复）；折叠预期（SimpleFold 不匹配 `ß↔ss`，FullFold 匹配；`İ`、`K`(U+212A)、`ς/σ`）。
- **跨块边界（必做，最易出 bug）**：`cross_boundary_matrix()` 覆盖 `chunk ∈ {4KiB 测试小块, 2MiB}` × `模式长 1..80` × `命中起始相对块边界 offset ∈ [-overlap, +overlap]` 全组合，断言"分块结果 == 单块全量结果"。**四个引擎都要跑，CI 必过。**
- **黄金对拍**：①③④ 与 Python 逐字节参考实现对拍（语料：`bench/data/` + 30 个真实格式片段 PNG/ZIP/PE 头）；②与 `iconv` 对拍编码表；③与 `ripgrep --byte-offset -a` 对拍偏移列表。
- **属性（proptest）**：`count(Query) == count(naive_scan)`；`Collect(limit=∞) == FirstOnly 反复调用的并集`；随机 `chunk_size`/`threads` 下并行结果 == 单线程结果；取消后 `complete=false` 且已收集结果是取消时刻的前缀（单调性）。
- **fuzz**：`BytePattern` 语法（不 panic，必返 `Err`）；病态正则（`(a*)*b`、`(?s).*`、超大重复计数）+ 随机数据，断言无 panic、单块 < 100ms、`max_match_len` 生效；随机 Unicode 串 × 10 编码 × 3 折叠模式。
- **基准**：§5 全表，`cargo bench --bench search`，回归 > 10% 红灯（00 §7）。
- **集成**：100GB 虚拟 Provider 端到端（提交 → 首批 1000 条 → 取消，断言延迟 < 100ms 且心跳线程测得 UI 无阻塞）；搜索中编辑（断言标 `Stale` 且旧结果不覆盖新状态）；`Display` 在非保长处理器上被正确拒绝。

## 10 开放问题

| # | 问题 | 倾向 |
|---|---|---|
| Q1 | 方案 B（正则跨块 streaming）维护成本高，v1 就做吗？ | 倾向 v1 只做 A+C，B 放 v1.1；代价是 `(?s)foo.*bar` 受 64KiB overlap 限制。需实测此类模式占比 |
| Q2 | 多结果集并存（搜索历史）如何画高亮？ | 索引只支持一个 `Layer::Search`；倾向"1 个活跃 + N 个存档（不画高亮）" |
| Q3 | 超大文件的反向搜索（上一个命中）？ | ① ② 在 `FirstOnly` 下支持（从末尾切块、overlap 语义翻转）；③④ 走"正向前缀扫描取最后一个" |
| Q4 | 增量修补是否值得？ | 本期否决（§4.6）。若"改一字节等 30 秒"被狂喷，再做"改动 ± overlap 重算 + 标不可信区" |
| Q5 | EBCDIC 只给 CP037 还是可选 codepage？ | 倾向只 CP037 + 可下载 codepage 包（15-content-distribution），不把 256 张表塞进核心 |
| Q6 | 数值 `alignment` 默认是否应为 `width`？ | 现默认"任意偏移"（漏检比误报更糟）；但逆向场景对齐搜索常用，可能需做成显眼的三态控件 |
| Q7 | 上限 100k 是否太小？ | 暂定。若"搜 `00 00`"频繁撞上限可提到 500k（内存 60MB） |
| Q8 | 是否提供搜索表达式（四引擎 AND/OR 组合）？ | 等于给用户一个小 DSL，与 BinSpec 有重叠风险；倾向不引入，用正则 + `within_node` 覆盖 80% |
| Q9 | 多通配模式用 AC 还是逐模式 memmem？ | 待基准裁决：模式数 > 8 且平均锚长 ≥ 4 时 AC 可能反超，需实测后定阈值 |
