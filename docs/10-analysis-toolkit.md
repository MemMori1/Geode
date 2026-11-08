# 10 — 分析工具集（哈希 / 熵 / 字节分布 / magic / Inspector）

> 归属 crate：`geode-analysis`（见 00-architecture-overview §3）。本文定义**单遍多分析器**扫描骨架、哈希、熵、字节分布、magic 识别、Inspector 值解码、字符串提取、加密/压缩启发式与报告导出。
>
> 铁律（00 §1 C1、§5.4、§6）：**任何分析都不得整读文件**；任何分析都必须 `流式 + 后台 Job + CancelToken + 预算`。结果只有两种形态：**Ann（走 `Layer`）** 与 **报告对象（不进 AnnotationIndex）**。

## 0. 决策速览

| # | 决策 | 否决的方案与理由 |
|---|---|---|
| D1 | **单遍多分析器**：一次 I/O pass 喂 N 个 `StreamAnalyzer` | 每个分析各扫一遍：1GB 上 6 个分析 = 6 倍 I/O，且各结果时间点不一致 |
| D2 | 打开时**默认只算 CRC32 + BLAKE3** | 无条件算 SHA-256：无并行性，1GB 需 2.5~4s，破 §5 预算 |
| D3 | 熵摘要用**自适应分桶（≤1M 桶，每桶 min/mean/max）+ 档位 RLE** | "每窗口一个 f32"：100GB/256B = 4 亿点 = 1.6GB |
| D4 | magic 用**自建 `.gmagic` 规则 + 定点字节码**（可嵌入、可分发、可被模板市场扩展） | libmagic：不可嵌入、规则不可分发、更新与许可证不可控 |
| D5 | Inspector 是**纯函数** `decode(bytes, ty, endian) -> Decoded` | 有状态解码器：无法在 UI 每帧对上百类型求解、无法单测 |
| D6 | magic → 模板自动加载的**编排在 `app`/`geode-project`** | `geode-analysis` 依赖 `geode-bspec`：违反 00 §3 依赖方向唯一 |

## 1 目标与非目标

**目标**：① 打开后 5ms 内给出"这是什么"（magic）；② 200ms 内出哈希与熵摘要（4GB）；③ 一次 pass 覆盖直方图/2-gram/熵/字符串/magic；④ Inspector 覆盖 02 §3 的全部标量类型，解码与反写都是纯函数；⑤ 启发式结论必须**可解释**（带理由与证据区间），不给孤立分数；⑥ 编辑 1KB 后不重算全文件（哈希树 + 分布差异补偿）；⑦ 报告与 UI 同源。

**非目标**
- 不做密码学验证（签名/证书链/HMAC），只算摘要。
- 不实现 `file(1)` 完整语义（不做解压后递归识别、不做 PE/CDF 深度探测）。
- 不做递归结构解析（`geode-bspec` 的职责），本 crate 只输出 `FormatGuess`。
- 不做数据变换（07）、搜索（08）、diff（09）、反汇编（11）、YARA（12）。
- 不做插件式自定义分析器（走 14 的 ABI，本 crate 只留注册点）。
- 不承诺 100GB 的"精确"字节分布：大文件按 §4.4 抽样协议降级并显式标注。

## 2 依赖与边界

| 方向 | crate | 用途 |
|---|---|---|
| 依赖 | `geode-core` | `Offset` / `ByteRange` / `Diag` / `Value` / `JobId`（00 §4） |
| 依赖 | `geode-provider` | 只读 `read_at` / `size` / `caps` / `poll_change`（00 §5.1） |
| 依赖 | `geode-annot` | 写 `Ann`（`Layer`），见 00 §5.3 |
| 被依赖 | `geode-bspec` | 消费 `FormatGuess`（经 `app` 传递），决定加载哪个模板 |
| 被依赖 | `geode-disasm` | 复用 `headers::probe()` 的 ELF/PE/Mach-O 头事实（架构/位宽/入口点） |
| 被依赖 | `geode-ai` | 熵图、直方图、`FormatGuess` 作为 agent 工具（03 §3.2） |
| 被依赖 | `geode-ui` / `geode-project` | 分析面板、熵图、报告视图、配置持久化 |
| 禁止 | `geode-ui`/`egui`/`wgpu`；`geode-bspec`/`geode-ai`/`geode-proc` | 分层与依赖方向（00 §2、§3）。需要"变换后视图"时由上层实现 `ScanSource` 注入 |

第三方依赖（全部宽松许可）：`crc` + `crc32fast`（CRC16/32/32C/64）、`md-5`/`sha1`/`sha2`、`blake3`（开 `rayon`+`mmap`）、`xxhash-rust`、`murmur3`、`adler2`、`half`（f16 显示）、`memchr`。FNV-1a 十行自研以免依赖。

## 3 数据结构与接口

```rust
pub trait ScanSource: Send + Sync {                 // 上层注入：整个文件/选区/结构节点/geode-proc 视图
    fn len(&self) -> u64;
    fn read(&self, off: u64, out: &mut [u8]) -> Result<usize, ScanError>;
    fn fingerprint(&self) -> SourceFingerprint;     // 含 buffer.version()，见 §7
    fn label(&self) -> &str;                        // "整个文件" / "选区 0x100..0x2000" / "节点 IHDR"
}

pub struct PassPlan { pub chunk: u32 /*1 MiB*/, pub analyzers: Vec<AnalyzerId> }
pub trait StreamAnalyzer: Send {
    fn needs(&self) -> Needs;                        // 逐字节? 窗口? 2-gram?
    fn on_chunk(&mut self, off: u64, data: &[u8], ctx: &ScanCtx) -> Result<(), ScanError>;
    fn finish(self: Box<Self>, ctx: &ScanCtx) -> AnalysisOutput;
}
pub struct ScanCtx<'a> { pub cancel: &'a CancelToken, pub progress: &'a dyn Fn(f32),
                         pub budget: &'a Budget }    // 复用 02 §7.2 的预算语义

pub enum HashAlgo { Crc16Ccitt, Crc32Ieee, Crc32C, Crc64Ecma, Crc64Xz, Md5, Sha1, Sha256, Sha512,
                    XxHash32, XxHash64, Murmur3X86_32, Murmur3X128, Blake3, Fnv1a32, Fnv1a64, Adler32 }
pub struct Digest { pub algo: HashAlgo, pub bytes: SmallVec<[u8; 32]>, pub scope: ByteRange,
                    pub chunked: bool /*分块合成摘要，≠ 标准全量摘要*/ }
pub struct DigestSet { pub digests: Vec<Digest>, pub computed_at_version: u64 }
pub struct HashTree { pub algo: HashAlgo /*仅 Blake3*/, pub leaf: u32 /*1 MiB*/,
                      pub leaves: Vec<[u8; 32]>, pub root: [u8; 32] }

pub struct EntropyBucket { pub min: f32, pub mean: f32, pub max: f32 }
pub struct EntropyMap { pub window: u32, pub stride: u32, pub bucket_size: u64,
                        pub buckets: Vec<EntropyBucket> /*≤1_048_576*/,
                        pub global: f32, pub segments: Vec<EntropySegment> /*≤4096*/ }
pub struct EntropySegment { pub range: ByteRange, pub class: EntropyClass }
pub enum EntropyClass { Struct, Mixed, Code, Compressed }

pub struct ByteStats { pub hist: Box<[u64; 256]>, pub bigram: BigramTable /*65536 bins 或稀疏*/,
                       pub quadgram: SpaceSaving /*top-4096 + 总数*/,
                       pub chi2_blocks: Chi2Summary, pub total_bytes: u64 }

pub struct FormatGuess { pub format_id: &'static str, pub mime: &'static str, pub score: f32,
                         pub evidence: Vec<ByteRange> /*P3：必须可回链*/, pub header_probe: Option<HeaderFacts>,
                         pub template_hint: Option<String> /*与 15 的模板索引同键*/ }
pub struct HeaderFacts { pub arch: Arch, pub bits: u8, pub endian: Endian,
                         pub entry: Option<u64>, pub image_base: Option<u64> }

pub struct Decoded { pub ty: ValueType, pub raw: ByteRange, pub endian: Endian, pub text: String,
                     pub alt: Vec<(String, String)>, pub warn: Option<InsWarning>,
                     pub editable: Option<EditableValue> }

pub struct AnalysisReport { pub meta: FileMeta, pub digests: DigestSet, pub entropy: EntropySummary,
                            pub stats: ByteStatsSummary, pub structures: Vec<StructureOutline>,
                            pub verdicts: Vec<HeuristicVerdict>, pub generated_at: String }
```

`AnalysisOutput` 是上述结果的 tagged union。本 crate **不持有文档状态**：结果由 `app`/`geode-project` 存进 Doc，按 §7 的失效规则丢弃。

## 4 核心算法 / 流程

### 4.1 单遍骨架（D1）

```
job(PassPlan) ─► 逐 1 MiB chunk（provider.read_at）
                   ├─ HistogramAnalyzer  (256 × u64)
                   ├─ BigramAnalyzer    (65536 × u32, saturating)
                   ├─ EntropyAnalyzer   (滑窗增量计数)
                   ├─ StringAnalyzer    (memchr 预筛 + 可打印率)
                   ├─ MagicAnalyzer     (仅头 64KB + 尾 64KB)
                   └─ HashAnalyzer      (默认 CRC32 + BLAKE3，可扩展)
                 每块末：cancel 检查 + progress 上报
```

- **块 1 MiB**：更小则 syscall/锁开销占比升高，更大则取消延迟变大（1 MiB 逐字节约 0.3ms，远低于 00 §6 的 100ms 取消要求）。
- **holes 源**（进程内存）按可读区间切分 pass，空洞记 `unreachable` 而不是错误（00 §5.1 铁律）。
- **非 seekable 源**（UDP/串口）：只做"从头追加"的增量 pass，每 1 MiB 输出一次部分结果。
- **收益**：I/O 只发生一次，成本受限于逐字节计数（实测 ≥ 1.5 GB/s）。

### 4.2 哈希

| 类 | 算法 | 用途 | 单线程吞吐（x86-64） |
|---|---|---|---|
| 校验 | CRC32 / CRC32C / CRC64(ECMA,XZ) / CRC16(CCITT) | 完整性、与格式内 crc 字段对拍 | CRC32/32C 6~20 GB/s；CRC64 2 GB/s |
| 校验 | adler-32 | zlib 流、快速校验 | 4~8 GB/s |
| 校验 | MD5 / SHA-1 / SHA-256 / SHA-512 | 与外部工具、情报库、样本记录对齐 | 0.6 / 1.2 / 0.35（SHA-NI 1.8）/ 0.6 GB/s |
| 查找 | xxHash32/64、MurmurHash3、FNV-1a 32/64 | 分块去重、diff 锚点、缓存键、模板索引 | 8~15 / 3~6 / 0.8 GB/s |
| 查找+加密 | BLAKE3 | 文件指纹、**增量哈希**（树形）、去重 | 3~7 GB/s 单线程，多线程近线性 |

- **作用域**：整个文件 / 选区 / 结构节点（`Node.offset+size`，02 §7.3）/ 数据处理器视图（07 的节点实现 `ScanSource`）。四种作用域共用一个 `Digest` 结构，UI 以 `scope` 标注，杜绝"这 3 个 hash 算的是哪段"。
- **默认组（D2）**：≥1MB 文件自动算 `CRC32 + BLAKE3`（1GB ≈ 0.35s，后台 Job 不阻塞首帧）。SHA-256/MD5/SHA-1 仅在用户显式点击、或文件 < 256MB、或报告导出勾选时计算，按钮上显示预计耗时。

**增量哈希树**：叶子 1 MiB；内部节点用域分离组合 `node = BLAKE3("geode:node:v1" ‖ child_0 ‖ …)`；只有根摘要配称"文件 hash"。
- 语义边界写清楚：`chunked=true` 的摘要**不等于** `blake3sum file`。UI 分别标 `BLAKE3 (chunked)`/`(raw)`；对外发布只用 `raw`，增量只服务"编辑后刷新显示"。
- overwrite：只重算被触及叶子（≤2 个）+ O(log n) 内部节点；1 MiB 叶子重算 ≈0.2ms → "编辑 1KB 后刷新 < 5ms"可达。
- insert/remove：后续叶子偏移全变 → **显式退回全量**（UI 从"增量"变为"重算中"），不做叶子重编号。
- 非树形算法（MD5/SHA-*）不提供增量：编辑后标 `stale` 要求用户重算，不做假增量。

### 4.3 熵

`H = -Σ p_i·log2 p_i`，`p_i = count_i/n`，`H ∈ [0,8]`，256 bin。

- **一次扫描出全局 + 滑窗**：维护 256 个计数与 n，滑窗"减出、加进"，每字节 O(1) → 总成本 O(N) 单遍，≥1.5 GB/s（与直方图同一循环）。
- **窗口/步长**：窗口默认 1KB（可 256B/4KB），步长 `window/4`（4× 重叠，避免把低熵小结构劈断）。256B 档服务 03 §3.1 的 L3 熵谷采样。
- **渲染形态（D3）**：`bucket_size = max(window, ceil(len/1_048_576))`，每桶聚合其覆盖窗口的 `(min, mean, max)`，3×f32 = 12B/桶 → 内存上界 **12 MiB，与文件大小无关**。100GB 时 `bucket_size = 96 KiB`。
- **多分辨率**：全局缩放时对桶做二次聚合（金字塔下层按需临时计算、不缓存）；放大到某区段时对**该区段重扫**得 256B 精度序列——点需才细扫，绝不预存 4 亿点。
- **分段常量数组**：`mean` 量化成 4 档，相邻同类桶合并为 `EntropySegment`（<256B 并入邻居；>4096 段时合并最短段）。渲染器只画这些段，100GB 与 1MB 成本相同。
- 熵相关高亮**不入 AnnotationIndex**（一屏数万段会打爆 00 §5.3 的查询预算）；用户点"高亮高熵段"时才按段（≤32 段）插 `Layer::Analysis`。

| H 区间 | 判定 | 说明 |
|---|---|---|
| > 7.5 | `Compressed` | 加密/压缩/已编码媒体；7.99 附近几乎必然加密 |
| 6.0~7.5 | `Code` | x86 代码常 6.2~6.9；f32 数组 6.5~7.0 |
| 4.0~6.0 | `Mixed` | 索引表、日志、UTF-16 文本、二进制/文本混排 |
| < 4.0 | `Struct` | 头部、目录、长度表、padding（UTF-16 文本因高字节恒 0 落在此） |

### 4.4 字节分布

- **1-gram**：256×u64，成本可忽略。**2-gram**：65536×u32（256KB，`saturating_add`）；若非零 bin < 4096 落地到 `HashMap<u16,u32>` 稀疏表示。
- **4-gram 采样**：默认 1/64（每 64 字节取一个 4 字节起点），**Space-Saving** 维护 top-4096（单 pass、O(1) 内存、误差界 n/K）。100GB 精确 4-gram 需 4G bin，不可能，因此只承诺 top-K + 采样。
- **渲染**：16×16 热图（行=高 4 位，列=低 4 位），色阶按 `log2(count+1)` 分 5 档；旁挂 top-32 表（字节、计数、占比、相对均匀期望的倍数）。
- **偏差检测**：`χ² = Σ(o-e)²/e`，`df=255`，均匀随机期望 255（σ≈22.6）。对**每 4KB 块**计算，统计 256 个采样块中落在 `[210,300]` 的比例 `p_rand`：
  - `p_rand > 0.9` 且 2-gram 最大频率 < 6× 期望 → **疑似加密/压缩，置信度 0.80~0.90**；
  - `p_rand < 0.3` → 数据有强结构，反向贡献给"非加密"；
  - `n < 4KB` 只给 1 个块并标 `low confidence`。
  - 不用全局 χ²：1GB 时任何微小偏差都会显著，毫无区分度；"多块通过率"才是稳健统计量。

### 4.5 magic / MIME 识别与模板自动加载

**为什么自建（D4）**：libmagic 依赖外部数据库（不可嵌入单二进制）、规则无版本化、Windows 上易触杀软签名兼容问题，且需要**模板市场**能下发新规则（15）。代价是覆盖度自己攒：内置 206 条 + 市场增量。

`.gmagic` 规则子集（文本，**`schema_version` 必填**——本规范向 `23 §3.8` 补的字段，早稿缺失）：

```toml
[magic.png]
mime = "image/png"; ext = ["png"]; kind = "image"; weight = 10; read = 64   # read 硬上限 4KB
[[magic.png.test]] off = 0;  len = 8; mask = "ffffffffffffffff"; value = "89504E470D0A1A0A"
[[magic.png.test]] off = 12; len = 4; value = "49484452"; required = false  # 可选证据 +weight
[[magic.png.test]] jump = 8; len = 4; op = "eq"; value = "49484452"        # 跳转指令
```

可用指令（全部有界）：`LOAD{off,len}`、`MASK`、`EQ/NE/LT/GT`、`IN_RANGE`、`CONTAINS`、`JUMP{from_read}`、`REQUIRE/OPTIONAL`、`RET{score}`。编译成线性字节码，**每规则 ≤64 条指令、≤8KB 读取**，杜绝病态规则。

**内置覆盖 206 种**：图像 24、音视频 26、归档/压缩 30、可执行/对象/库 28、文档 22、数据库/序列化 18、字体 6、磁盘镜像/固件 14、抓包/协议 8、游戏 ROM/存档 10、其他 20。

**P1 核心链路（D6）**

```
open Doc ─► geode-analysis: probe(head 64KB + tail 64KB)          ≤5ms
             ▼ FormatGuess{format_id, mime, score, template_hint}
          UiEvent::FormatGuessed ──► app/geode-project（编排者）
             ▼ 模板库查表（键 = format_id，见 15 模板索引）
          geode-bspec: compile(source)   ≤20ms（00 §7）
             ▼ run(source, entry=0)    ≤100ms/10MB
          Node 树 ──► Structure 层 Ann + 结构树 + 置信度着色（03 §7）
```

**逐级降级**：

| 级 | 触发 | 行为 |
|---|---|---|
| L0 | score ≥ 0.90 且模板命中 | 编译+执行（用户可关自动加载，§8） |
| L1 | magic 命中、模板库无对应项 | 只显示"识别为 X（MIME）"+ "市场有 1 个相关模板 / Ctrl+Shift+G 让 AI 生成" |
| L2 | 未命中但有格式族特征（`50 4B 03 04` 出现 ≥3 次等） | 给 ≤3 个候选 + 证据区间，用户点选 |
| L3 | 完全未知 | 提示条：①手动选模板 ②AI 猜结构（03）③只看熵/分布 |
| L4 | 模板编译失败 | 退到"无结构"状态 + 一条 `Diag`（附 magic 结果作上下文） |
| L5 | 模板执行失败 | 保留部分结果（02 §8.3）+ 错误清单，不弹模态框（00 §9.3） |

自动加载只在**只读打开后**触发一次；`buffer.version()` 变化不重触发（避免编辑风暴），仅用户显式"重新识别"时重跑。

### 4.6 Inspector 值解码

类型清单与 02 §3 严格对齐（02 是唯一真理）：

| 组 | 类型 | 字节 | 要点 |
|---|---|---|---|
| 无符号 | u8/u16/u24/u32/u48/u64/u128 | 1/2/3/4/6/8/16 | u24/u48 掩码拼接；u128 显示 hex + 十进制 + 位分组 |
| 有符号 | s8/s16/s24/s32/s48/s64/s128 | 同上 | 符号扩展后格式化，并提供"当无符号看"的 `alt` |
| 浮点 | f16/f32/f64 | 2/4/8 | **必须显示非规格化数**、`±inf`、`NaN(payload)`；f32 用最短往返表示（不显示 `0.100000001`） |
| 变长 | LEB128 无符号 / 有符号 | 1~10 | 显示"值 + 消耗字节数"；越界/非规范 → `InsWarning::NonCanonical` 但仍解出可用部分 |
| 标识 | GUID / UUID | 16 | **Windows 坑**：`Data1:u32 LE, Data2:u16 LE, Data3:u16 LE, Data4:[u8;8] BE`；同时并列显示 RFC 4122 原始序，两者并存 |
| 时间 | unix 秒(u32/u64)、unix 毫秒、FILETIME(1601,100ns)、DOS date+time、Mac HFS(1904) | 4/8/4 | 按本地时区显示、可切 UTC；越界（FILETIME=0、年份 >9999）显示 `invalid` 不 panic |
| 颜色 | RGBA8 / RGB565 / ARGB32 / BGRA | 4/2/4/4 | 色块 + hex；RGB565 位扩展 `v<<3 | v>>2` |
| 网络 | IPv4 / IPv6 / MAC | 4/16/6 | IPv4 同时给点分与 `0x` 整数 |
| 位标志 | UNIX 权限位 / DOS 属性位 | 2/1 | 符号串（`-rwxr-xr-x`）+ 逐位表 |
| 四字码 | FourCC | 4 | ASCII 与 `'abcd'` 双形态，非可打印显示 `\xNN` |
| 指针 | off/rva/rel/va（02 §3.3） | 4/8 | 显示 raw、解析后绝对偏移、所在段/模块（有 `HeaderFacts` 时） |

- **大小端一键切换**：`Endian` 是入参而非状态 → 切换只是重调纯函数；16B 输入 × ~60 个类型全解一遍 < 1ms。
- **一键复制**（对**当前作用域字节**，非解码值）：C 数组、Rust 数组、Python `bytes`、Base64、hex 字符串（可调分隔符/大小写/每行字节数）。
- 反写只对定宽标量开放（`EditableValue`），走 `geode-buffer::overwrite`（00 §5.2），不做改长度的事。

### 4.7 字符串提取

参数：最小长度 4（2~64）、编码 ASCII/UTF-8/UTF-16LE/BE 可多选、可打印率阈值 0.90、条目上限 500k 或 64MB（超出置 `truncated`）。
算法：① SIMD 可打印位图预筛（memchr 类）→ ② 候选段验证可打印率与长度 → ③ UTF-16 对偶/奇双通道重复①②。输出 `StringTable { entries: Arc<Vec<StrEntry{range, enc, width}>> }`：只存区间不复制字符串，UI 现读现解。可排序/跳转/一键高亮（此时才插标注，≤2000 条）。

### 4.8 加密 / 压缩启发式

| 信号 | 权重 |
|---|---|
| 全局熵 > 7.9 且高熵段占比 > 60% | +0.35 |
| χ² 块通过率 > 0.9 | +0.20 |
| 已知 magic 缺席（score < 0.5） | +0.15 |
| 2-gram 最大频率 < 6× 期望 | +0.10 |
| 无 ≥8 字节可打印串 | +0.10 |
| 命中已知压缩头（gzip/zstd/xz/7z/bz2/lz4/lzo magic） | −0.40，直接判 `Compressed(known)` |
| 有 ELF/PE/Mach-O 头 | −0.50，直接判 `Code` |

可选微信号：对低熵段抽样 4KB，统计 x86 常见前导字节（`55 8B 48 E8`）比例。阈值：`≥0.70` 输出"疑似加密/压缩（置信度 p）"；`0.45~0.70` 输出"可能已压缩/编码"；`<0.45` **不给结论**（03 §7"宁可承认不知道"）。加密 vs 压缩用"熵段方差 + 是否存在 4~16B 低熵前缀"区分，结论始终是三态 `(加密|压缩|未知)`。

### 4.9 报告导出

Markdown（人读）与 JSON（机器读）同源，都来自一个 `AnalysisReport`：

```json
{ "schema": "geode-report/1",
  "meta": { "name": "...", "size": 0, "provider": "local-file", "opened_at": "..." },
  "digests": [ { "algo": "crc32", "value": "1a2b3c4d", "scope": "0..0x40000000" } ],
  "entropy": { "global": 7.62, "segments": [ { "start": 0, "len": 4096, "class": "struct" } ] },
  "byte_stats": { "top_values": [], "top_bigrams": [], "chi2_pass_rate": 0.94 },
  "structures": [ { "name": "ZipArchive", "coverage": 0.87, "nodes": 43 } ],
  "verdicts": [ { "kind": "crypto_or_compressed", "confidence": 0.85, "reasons": [] } ] }
```

默认**不含原始字节**（防泄漏，03 §10）；勾选"包含采样"时最多 64KB。生成成本 = O(已有摘要)，1GB < 100ms，不重扫。

## 5 性能预算与基准

| 场景 | 预算 | 条件 | `bench/` 名 |
|---|---|---|---|
| magic 识别 | < 5ms | 头 64KB + 尾 64KB，206 规则 | `bench_magic_probe` |
| 单遍计数（hist+2gram+entropy1KB） | ≥ 1.5 GB/s | 1GB，4 线程 | `bench_pass_1g` |
| 哈希默认组 | 1GB < 2s（实测 0.35~0.6s） | 8 核，BLAKE3 多线程 | `bench_hash_default` |
| BLAKE3 单文件 | ≥ 3 GB/s | 8 核 | `bench_blake3_mt` |
| SHA-256 单文件 | 0.35 GB/s（SHA-NI 1.8） | 单线程，不可并行 | `bench_sha256` |
| 增量哈希刷新 | 编辑 1KB 后 < 5ms | 4GB，BLAKE3 树 | `bench_hash_incremental` |
| 熵摘要内存 | ≤ 12 MiB，与大小无关 | 桶上限 1M | `bench_entropy_mem` |
| 熵图渲染 | < 2ms/帧 | 段数 ≤ 4096 | `bench_entropy_render` |
| 字符串提取 | ≥ 400 MB/s | 1GB，min_len=4 | `bench_strings` |
| Inspector 全类型解码 | < 1ms | 16B × ~60 类型 | `bench_inspector_all` |
| 报告生成 | < 100ms | 1GB，复用摘要 | `bench_report` |
| 全部分析摘要内存 | ≤ 64 MiB | 100GB 文件 | `bench_analysis_mem` |

100GB 现实预期（写清以免被当成卡死）：单遍计数约 65~70s（后台、可取消、有进度）；熵图在扫描 5% 后即按已有桶**流式渲染**。

## 6 失败模式与降级

| 失败 | 表现 | 降级 |
|---|---|---|
| Provider 读失败 / `holes` | 该区间无数据 | 记 `unreachable`，分析继续；报告标注覆盖率，绝不伪造数据 |
| 非 seekable 源 | 无法随机读 | 只做流式增量分析；magic 仅首 64KB；UI 标"结果不完整" |
| `volatile` 源变化 | 内容抖动 | 每次 poll 校验 `fingerprint`，不一致即清空结果并重启 pass |
| 用户取消 | 部分结果 | 已完成块保留并标 `partial`，UI 以 60% 透明呈现 |
| 预算耗尽（时间/内存） | 未完成 | 部分结果 + `Diag{E2001}`，可提预算续算 |
| 2-gram 溢出 | u32 饱和 | 该 bin 停加并置 `approx`（>400GB 才可能） |
| 病态 magic 规则 | 单规则卡死 | 64 指令 + 8KB 读窗上限，超限 `Ret{0}` |
| magic 误报 | 认错格式 | 所有 test 有 `required` 位；score < 0.9 只列为候选，不自动加载模板 |
| 模板自动加载失败 | 无结构树 | §4.5 的 L4/L5 降级梯 |
| 时区缺失/时钟异常 | 时间戳错 | 退回 UTC 并标 `(UTC)` |
| f16 异常值 | NaN/denormal | 显示原字节 + 标签，不 panic |

## 7 与其它子系统的契约

| 对象 | 读/写 | 内容 |
|---|---|---|
| `geode-provider` | 只读 | `read_at`/`size`/`caps`/`poll_change`；**绝不写**数据源 |
| `geode-annot` | 写 | `Ann{layer: Layer::Analysis}`。<br>`Layer::Analysis` **已登记于 00 §5.3**（10 个变体之一，位于 `Structure` 与 `Selection` 之间） |
| `geode-buffer` | 只读 `version()` | 结果携带 `computed_at_version`，不匹配即丢弃（00 §6 规则 3） |
| `geode-bspec` | 提供 | `FormatGuess`（经 `app`）、`HeaderFacts` 预填 `#pragma image_base` |
| `geode-ai` | 提供 | `entropy_map`/`histogram`/`format_guess` 三个工具（03 §3.2）；`FormatGuess.evidence` 是"不得发明 magic"的硬约束来源 |
| `geode-ui` / `geode-project` | 提供 | 分析面板、熵图、热图、Inspector、报告视图、配置与结果缓存生命周期 |
| 事件 | 发出 | `Progress(JobId,f32)`、`FormatGuessed(FormatGuess)`、`Done(JobId, AnalysisOutput)` |

## 8 配置项与默认值

| 键 | 默认 | 范围 | 说明 |
|---|---|---|---|
| `analysis.chunk_size` | 1 MiB | 64 KiB~8 MiB | 单遍骨架块 |
| `analysis.threads` | min(物理核, 8) | 1~32 | 并行/预读 |
| `hash.default_set` | `["crc32","blake3"]` | 任意子集 | 打开时自动算 |
| `hash.auto_large_threshold` | 256 MiB | 0~∞ | 超过则不自动算 SHA 家族 |
| `hash.chunked_leaf` | 1 MiB | 4 KiB~16 MiB | 增量树叶子 |
| `entropy.window` | 1024 | 256/1024/4096 | 滑窗 |
| `entropy.max_buckets` | 1_048_576 | 4K~4M | 摘要内存 = ×12B |
| `entropy.segments_max` | 4096 | 256~65536 | RLE 段上限 |
| `dist.bigram` | on | on/off | 大文件可关 |
| `dist.quadgram_sample` | 64 | 1~4096 | 采样率 1/N |
| `dist.quadgram_topk` | 4096 | 64~65536 | Space-Saving K |
| `dist.chi2_blocks` | 256 | 16~4096 | χ² 抽样块数（4KB/块） |
| `magic.autoload_template` | on | on/off | P1 体验链 |
| `magic.autoload_score` | 0.90 | 0.5~1.0 | 自动加载阈值 |
| `magic.library_paths` | 内置 + `~/.config/geode/magic/` | 路径表 | 追加 `.gmagic` 目录（15） |
| `strings.min_len` / `printable_ratio` / `max_entries` | 4 / 0.90 / 500_000 | 2~64 / 0.5~1.0 / 1K~5M | |
| `report.format` / `include_samples` | markdown / false | — | |

## 9 测试策略

- **哈希对拍**：公开向量（`"123456789"` 的 CRC32=`0xCBF43926`、CRC32C=`0xE3069283`、MD5/SHA-1/SHA-256 的 NIST 向量）+ 与 `coreutils`/`openssl`/`zlib` CLI 逐位对拍；边界长度 1B/1KiB/1MiB/4GiB+1（稀疏文件）。
- **增量哈希一致性（fuzz）**：随机 overwrite/insert/remove 序列后，`chunked` 根与"从零重建"完全一致；insert/remove 后 `raw` 摘要与全量重算一致。
- **熵**：均匀 1MiB → `[7.996, 8.0]`；全零 → 0；256 种字节各一次 → 8.0；桶聚合 vs 实际窗口重扫偏差 ≤ 0.02。
- **magic**：206 条规则各配 1 正样本 + 1 负样本；与 `file(1)` 对拍，允许 file 更细但**格式族必须一致**，不一致红灯。
- **magic fuzz**：随机字节 + 随机合法规则 → 不 panic、单次 < 1ms；规则语法用 `cargo-fuzz`。
- **大文件**：稀疏文件构造 100GB，验证"打开到首帧 < 200ms"、"熵摘要 ≤ 12MiB"、"无 O(size) 路径"。
- **Inspector**：每类型 × 两端序 × 边界字节（0x00/0xFF/0x80/denormal）快照测试；GUID 用已知 Windows GUID 双向验证；时间戳在固定 `TZ` 的 CI 上跑。
- **黄金文件**：`tests/golden/` 存一份 4KB 合成文件的全量分析 JSON，任何判定阈值变化都会被 diff 抓到。
- 全部性能项进 CI，退化 > 15% 红灯（00 §7）。

## 10 开放问题

1. ~~`Layer::Analysis` 是否进 00 §5.3~~ **已裁决**：加入，z 序在 `Structure` 与 `Selection` 之间（00 §5.3）。
2. **SHA-256 的增量化**：分块合成摘要语义偏离标准（§4.2）。是否接受"编辑后 SHA-256 显示 `stale`、不自动重算"作为长期行为？
3. **熵桶分辨率**：100GB 时 96KiB 桶够不够？"点需才细扫"在大跳转场景会不会太慢（是否要预扫用户指定区段）？
4. **大文件分布是否全扫**：100GB 全扫 ≈ 70s I/O。是否默认改成"首 1GB + 尾 1GB + 均匀 64 个 4MB 点"（< 2s），把精确全扫降级为选项？
5. **`.gmagic` 是否吃 libmagic 语法子集**：吃 → 迁移 800+ 现成规则，但被语法怪癖绑住；当前选择"不吃"，只保留值/掩码/偏移/跳转四件套，覆盖度自己攒。
6. **GUID 主显示端序**：Windows 语义还是 RFC 4122？当前并列，终归要定一个主显示。
7. **小文件是否提供精确 4-gram 表**（< 256MB，只对出现过的 key 建表，最坏 256MB 内存）？
8. **magic 库签名与信任**（与 15 联动）：市场下发的 `.gmagic` 能影响"自动加载哪个模板"，等于能影响用户看到的解析结果，审核策略需与 15 一起定。
