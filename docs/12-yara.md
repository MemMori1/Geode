# 12 — YARA 扫描

> 归属 crate：`geode-yara`（见 00-architecture-overview §3）。职责：在**任意大小**的文件上编译并执行 YARA 规则，把命中结果转成 `ByteRange` 集合 + `Layer::Yara` 标注 + 可导出的报告。
>
> 定位声明：我们是 YARA 的**宿主**，不是规则作者。规则来自内置包、用户目录、模板市场包（见 15-content-distribution），我们负责编译、护栏、流式执行、呈现，并回答"这段字节是什么"。

## 0. 决策速览

| # | 决策 | 否决的方案与理由 |
|---|---|---|
| Y1 | **引擎用 `yara-x`**（VirusTotal 的 Rust 重写），抽象为 `trait YaraEngine` 以便回退 | `libyara`（C）：全局状态 + `unsafe` 绑定的内存安全问题，见 §4.1 |
| Y2 | **规则来源三档**：内置包（内嵌）/ 用户目录（可配）/ 市场包；**每个来源一个 namespace** | 全部摊平到单一 namespace：规则重名无法区分来源，卸载市场包会误删用户规则 |
| Y3 | **逐文件编译，单条规则失败只影响自身** | 整包一次编译：一条坏规则废掉整个规则集（用户会直接关掉这个功能） |
| Y4 | 编译期 + 扫描期**双层护栏**（超时、内存、规则数、单规则大小、正则复杂度、禁 `include`） | 只做扫描期超时：病态正则会在编译期就吃光内存 |
| Y5 | **打开文件时不自动全量扫描**；仅在 `规则数 ≤ 500 且 文件 ≤ 64 MiB` 时自动跑 head+tail 快速扫描 | 无条件自动全量扫描：1GB 文件每次打开多 30s，违反 00 §1 C1 的体感约束 |
| Y6 | 命中写入**已有的 `Layer::Yara`**（00 §5.3），不新增图层 | 新增 `Layer::YaraMatch`：AnnotationIndex 的图层枚举是全局契约，加层要动所有渲染路径 |
| Y7 | 扫描走 yara-x 的 **block 流式 API**（1 MiB/块）+ 协作式取消 + 1 MiB 粒度进度 | 全文件读入内存：100GB 直接爆 |
| Y8 | 只支持 yara-x **已实现**的语法与模块子集，未支持特性在编译期明确报错 | 假装兼容：用户规则静默少匹配，比报错糟糕得多 |

## 1 目标与非目标

**目标**：① 1GB 文件单线程扫描 < 30s（≥ 35 MB/s），内存增量 < 200 MB；② 扫描任意大小文件（含 100GB）且内存恒定；③ 编译错误定位到 `文件:行:列`，病态规则被护栏挡住而不是拖死进程；④ 命中 → 结果表 → 点击 → hex 高亮/跳转，全链路 < 1ms；⑤ 命中结果可作强证据喂给 AI 格式推断（见 03-ai-dsl-generation §7）；⑥ 规则集可增量热重载，不重启应用。

**非目标**
- 不做规则**自动生成**（那是 03 的 agent 工作，我们只提供 `yara_hits` 证据）。
- 不做沙箱/仿真类模块（`cuckoo` 等），不做云端威胁情报同步，不做启发式 AV 打分。
- 不做实时监控（文件变更自动重扫，v1 不做，见 §10）。
- v1 不支持规则 `include`（多文件组织用 namespace + 目录发现代替）。
- 不保证与 libyara 100% 语义一致，只保证"对同一规则集、同一数据，命中集合一致或差异可解释"（§9 对拍）。
- 不做命中的持久化（是否进 project 文件见 §10，倾向不进）。

## 2 依赖与边界

| 方向 | crate | 用途 |
|---|---|---|
| 依赖 | `geode-core` | `ByteRange` / `Severity` / `Diag` / `JobId` / `CancelToken` |
| 依赖 | `geode-provider` | `read_at` 顺序流式读；`caps().seekable` 决定能否做"起点任意"的扫描 |
| 依赖 | `geode-annot` | 写 `Layer::Yara` 标注（00 §5.3） |
| 依赖 | `geode-analysis` | 可选：读 `FormatGuess`，用于规则预筛（例如"这是 PE 才跑 `pe` 相关规则"）与快速扫描范围（magic 命中区 ±256B） |
| 被依赖 | `geode-ui` | 结果表、规则源码查看器、命中 hexdump |
| 被依赖 | `geode-ai` | 消费 `YaraHint` 证据（见 03 §7） |
| 被依赖 | `geode-content` | 市场规则包的安装/卸载/签名校验（见 15）与规则目录的增删 |
| 禁止 | `geode-bspec` / `geode-buffer` 可变接口 / GUI | YARA 只读数据；结构树联动由 app 层用 `ByteRange` 编排 |

外部依赖：`yara-x`（BSD-3，纯 Rust，feature 默认关掉 `pe`/`elf`/`dotnet` 模块的完整缓冲实现，见 §10）、`walkdir`（用户规则目录递归发现）、`regex`（仅用于我们内部的规则体检，不用于匹配）。**不引入** `libyara`/`yara-sys`（C 依赖、全局状态）。

## 3 数据结构与接口

```rust
pub struct Namespace(Arc<str>);                 // "builtin" | "user.<dir>" | "pack.<id>" | "adhoc"
pub enum RuleSource { Builtin, UserDir(PathBuf), MarketPack { pack_id: Arc<str>, version: SemVer }, AdHoc }

pub struct RuleMeta {
    pub namespace: Namespace, pub rule: Arc<str>, pub tags: Vec<SmallStr>,
    pub severity: Severity, pub author: Option<Arc<str>>, pub description: Option<Arc<str>>,
    pub reference: Vec<Arc<str>>, pub source_path: Option<PathBuf>, pub line: u32,
}

pub struct RuleSet {
    pub compiled: Arc<YaraRules>,               // yara-x 的 Rules
    pub namespaces: Vec<NamespaceInfo>,         // 每来源的规则数/字节数/编译时间
    pub stats: RuleSetStats,                    // 规则总数、字符串总数、编译耗时
    pub fingerprint: u64,                       // 规则集 hash（标注与缓存失效用）
}

pub struct YaraMatch {
    pub rule: Arc<str>, pub namespace: Namespace, pub tags: Vec<SmallStr>,
    pub ranges: Vec<ByteRange>,                 // 已排序、已合并，见 §4.4
    pub meta: Arc<RuleMeta>, pub truncated: bool, // ranges 超过上限时为 true
}

pub struct ScanScope { pub range: ByteRange, pub mode: ScanMode }
pub enum ScanMode { Full, HeadTail { head: u64, tail: u64 }, Selection }

pub struct ScanLimits { pub timeout: Duration, pub per_rule_timeout: Duration,
                        pub max_matches_per_rule: u32, pub max_ranges_per_rule: u32,
                        pub mem_budget: u64 }

pub struct ScanReport {
    pub matches: Vec<YaraMatch>, pub scanned_bytes: u64, pub elapsed: Duration,
    pub timed_out: bool, pub block_errors: u32, pub rules_fingerprint: u64,
}

pub trait YaraEngine: Send + Sync {
    fn id(&self) -> &'static str;                       // "yara-x"
    fn compile(&self, sources: &[RuleSourceRef], limits: &CompileLimits,
               cancel: &CancelToken) -> CompileOutcome;
    fn scan(&self, p: &dyn Provider, set: &RuleSet, scope: ScanScope, limits: &ScanLimits,
            cancel: &CancelToken, progress: &mut dyn FnMut(u64, u64)) -> ScanReport;
}

pub enum CompileOutcome { Ok(Arc<RuleSet>), Partial { set: Arc<RuleSet>, errors: Vec<RuleError> } }
pub struct RuleError { pub path: Option<PathBuf>, pub namespace: Namespace,
                       pub line: u32, pub col: u32, pub kind: RuleErrorKind, pub message: String }
pub enum RuleErrorKind { Parse, Compile, UnsupportedFeature, RegexTooComplex,
                         TooLarge, TooMany, IncludeForbidden, Timeout }
```

约定：**所有对外结果都是 `ByteRange`（文件偏移）**，不含 `yara-x` 类型（`yara_x::Match` 在 crate 内部消化掉），否则上游 API 变动会穿透到 UI。

## 4 核心算法 / 流程

### 4.1 引擎选型（Y1）

| 维度 | yara-x（Rust） | libyara（C 4.5） | 决策影响 |
|---|---|---|---|
| 内存安全 | 纯 Rust，`unsafe` 极少；无全局可变状态 | C 库 + FFI；`YR_COMPILER`/`YR_SCANNER` 生命周期与 `unsafe` 包装是主要 bug 源 | 我们要在**用户提供的规则**上跑，输入不可信 → 安全模型优先 |
| 全局状态 | 无（`Rules`/`Scanner` 都是普通值，可 `Send`） | `yr_initialize()` 全局一次 + 部分模块依赖进程级状态；多文档并发扫描需自行加锁 | 多文档 / 多视图并发是核心场景 |
| 可嵌入 | `cargo add yara-x`，交叉编译干净 | 需 C 工具链 + 构建脚本，Windows 静态链接麻烦 | 与 Capstone 的 C 依赖相比，能少一个就更少一个 |
| API 形态 | `Compiler::add_source` → `Rules`；`Scanner::scan_block(&mut self, block)` 原生流式，**跨块匹配由引擎维护状态** | `yr_scanner_scan_mem_blocks` + 手写 block 迭代器 + 匹配回调（`CALLBACK_MSG` 里自己算偏移） | 我们要的 100GB 流式，yara-x 是"原生支持"而非"勉强实现" |
| 性能 | 与 libyara 同量级或更快（atom 提取 + Aho-Corasick 优化更激进） | 成熟稳定 | 中性偏 yara-x |
| 生态成熟度 | 2023 起，规则语法目标兼容 YARA 4.5；**部分模块（`cuckoo`、部分 `dotnet`、`magic`）未实现** | 15 年，模块与边界情况全覆盖 | 唯一劣势；用 Y8（明确报错）+ 回退通道缓解 |
| 规则语法兼容 | 大部分 4.x 语法；`include` 支持但与我们的护栏冲突（我们禁用） | 基准 | 兼容层只需报告"不支持的特性" |

**决策**：yara-x 为唯一实现。理由排序：① 输入是不可信的用户规则 → 内存安全与无全局状态是硬要求；② 流式 API 原生支持 100GB 场景；③ 少一个 C 依赖。**否决 libyara**，但 `trait YaraEngine` 保留——若某客户的规则库依赖 yara-x 尚未实现的模块，可选编译一个 `libyara` 后端（届时只需实现 trait，UI/标注/AI 通路不变）。

### 4.2 规则来源与组织（Y2）

| 来源 | 位置 | namespace | 更新方式 |
|---|---|---|---|
| 内置包 | 编译进二进制（内容资源，非代码） | `builtin` | 随版本发布 |
| 用户目录 | `~/.config/geode/rules/`（可配，多路径），递归发现 `*.yar` / `*.yara` | `user.<目录名>` | 用户手动；文件监听可选 |
| 市场包 | `~/.local/share/geode/packs/<id>/rules/` | `pack.<id>` | 由 15-content-distribution 安装/签名校验/版本更新 |
| 临时规则 | 规则编辑器里粘贴 | `adhoc` | 会话内，默认不落盘 |

- **内置覆盖目标**：约 120 条，分 6 类——文件格式特征（与 10 的 magic 库互为冗余校验）、打包器/壳（UPX、ASPack、Themida 特征）、加密常量（AES S-box、RSA 公钥 DER 前缀、Base64 表变体）、脚本特征（PowerShell `-enc`、`eval(`、`FromBase64String`）、嵌入式文件（ZIP/Zlib 头 + 可疑内嵌 PE）、CTF 常见（flag 格式、特定 magic 链）。
- **元数据**：强制 `author` / `description` / `tags` / 我们扩展的 `severity`（`error|warning|info`，映射到 `Severity`）。缺 `severity` 时按 namespace 默认（`builtin`=info，`pack.*`=按包清单，`user.*`=warning）。
- **重名**：不同 namespace 下同名规则**并存**（YARA 语义如此），UI 按 namespace 分组展示，跨 namespace 命中在表里加来源徽标。

### 4.3 编译与校验流水线（Y3 / Y4）

```
discover（递归发现 + 对 15 的包做签名校验）
  → 逐文件 parse：语法错误 → RuleError{path,line,col} ，跳过该文件
  → 逐文件护栏检查（下表）→ 违反者整文件拒绝，其余继续
  → yara-x Compiler::add_source(ns, src) （每文件一次，错误行号可靠）
  → compiler.build() → Rules；序列化缓存到 ~/.cache/geode/rules/<fingerprint>.bin
  → 汇总 CompileOutcome::Partial{ set, errors }，errors 进 UiEvent::Diag
```

| 护栏 | 默认阈值 | 超限处理 |
|---|---|---|
| 命名空间规则数 | ≤ 2000 | 拒绝该来源，提示"规则过多，请拆分目录" |
| 单规则源码字节 | ≤ 64 KiB | 拒绝该规则 |
| 单文件字节 | ≤ 4 MiB | 拒绝该文件 |
| 每规则字符串数 | ≤ 1000 | 拒绝该规则 |
| 正则节点数（我们预扫描） | ≤ 200 | 拒绝该规则（防回溯爆炸） |
| 正则 atom 最短长度 | ≥ 2 字节 | **仅警告**（性能风险提示，不拒绝） |
| `include` | 禁止 | `IncludeForbidden`（路径可越出规则目录、可指向网络路径） |
| 编译超时 | 10s/文件 | 中止该文件，保留已编译部分 |
| 编译内存 | ≤ 256 MiB（规则集总量） | 中止并回退到上一次成功编译的 `RuleSet` |

- 编译发生在后台 Job，**不阻塞打开文件**；`RuleSet` 就绪前 `YaraView` 显示"规则编译中…（N/M）"。
- 规则热重载：文件 mtime/size 变化 → 只重编该文件所属 namespace → 新 `RuleSet.fingerprint` 变化 → 已有标注全部作废重扫（用户可关"自动重扫"）。

### 4.4 扫描执行（Y7）

```
scan(provider, RuleSet, scope, limits, cancel, progress):
  scanner = yara_x::Scanner::new(rules)
  off = scope.start
  while off < scope.end:
      if cancel.is_cancelled() -> return partial(timed_out=false)
      if elapsed > limits.timeout -> return partial(timed_out=true)
      n = min(block_size=1MiB, scope.end - off)
      buf = provider.read_at(off, &mut [0; 1MiB][..n])?     // 读错误 → block_errors+=1，跳过
      scanner.scan_block(&buf[..n])                        // 跨块状态由引擎维护
      progress(off - scope.start, scope.len); off += n
  matches = scanner.matches()  → 转成 YaraMatch（去重、合并、按规则聚合）
```

- **block = 1 MiB**：足够摊销 per-block 开销，又让进度与取消粒度够细（100GB → 102400 次进度事件，UI 侧节流到 60 Hz 刷新）。
- **不需要手工 overlap**：yara-x 的 `scan_block` 在内部保留跨块匹配状态（这是它相对裸 `libyara` 迭代器的主要好处），块大小只影响进度粒度，不影响正确性。**这是必须写进代码注释的语义假设**，并在 §9 用"跨块边界构造的样本"做回归。
- **合并规则**：同一规则多次命中（多字符串、多次出现）→ 一个 `YaraMatch`；`ranges` 排序 → 相邻（间隔 0）或重叠合并 → 上限 `max_ranges_per_rule`（默认 1024），超出置 `truncated=true` 并保留前 1024 个（按偏移升序，先低后高）。
- **并行策略（明确写清能达到多少）**：
  - **单文件**：一个 `Scanner` 内部状态不跨数据分片共享，因此**单文件不做多线程分片**（会丢跨块/跨区匹配）。单文件加速来自 yara-x 内部（atom 预筛 + AC 自动机），实测字符串类规则 100~300 MB/s，正则/模块密集规则 30~80 MB/s。
  - **多目标**：rayon 按文件/文档并行，**近线性**（8 核实测 6~7×；受磁盘与内存带宽限制）。
  - **单文件想再快**（v1 不做）：把规则集分成 N 片各扫一遍数据（N 倍 IO，仅在 provider 缓存命中或源是内存映射时值得）——列入 §10。
- **目标核算**：最坏情形 35 MB/s → 1GB ≈ 29s（贴住 30s 目标）；字符串型规则集实际 3~10s。`HeadTail` 快速扫描（16 MiB）< 500ms。
- 非 `seekable` 源：只允许 `ScanMode::Full` 且起点必须为 0（顺序流），否则报 `E5xxx` 前条件错误（见 02 §8.2）。

### 4.5 结果呈现

- **标注**：每个 ranges 条目插一条 `Ann { range, layer: Layer::Yara, tag, priority }`；颜色按 severity（error 红 / warning 橙 / info 黄），`priority` 低于 `Selection`/`Cursor` 高于 `Structure`（保证选中与光标可见）。标注带 `rules_fingerprint` 标签，规则重载后 `clear_layer(Layer::Yara)` 一次性清理。
- **结果表**：列 = 规则名 | 命名空间 | 标签 | 严重度 | 命中数 | 首命中偏移。支持按严重度/来源过滤、点击行跳到首个命中并选中、`Enter` 循环下一个命中。
- **规则源码查看器**：只读弹窗，显示该规则全文 + 命中条件高亮（yara-x 能给出匹配的字符串标识符与偏移，行号由我们预编译时建立的 `offset→line` 表反查）；显示来源路径与 mtime，便于用户直接编辑外部文件。
- **命中上下文**：区间前后各 64 字节的 hexdump（复用 hex 视图渲染组件，不新写一个）。
- **导出**：JSON（schema `geode-yara-report/1`：`{file, size, hashes, rules_fingerprint, matches:[{rule,namespace,tags,severity,ranges}], timed_out, elapsed}`）与 CSV（一行一个命中区间）。默认不含原始字节（与 10 §4.9 的导出隐私策略一致）。

### 4.6 与结构树的联动

- `geode-yara` 只输出 `ByteRange`；由 app 层拿着 `Node` 树做**包含查询**（`node_at(offset)` → 最深的包含节点），在结果表里追加一列"结构路径"（`Header.sections[3].name`）。
- 反向：结构树上给"包含 YARA 命中的节点"打角标，点击展开该命中的规则列表。
- 依赖边界：`geode-yara` **不依赖** `geode-bspec`（否则违反 00 §3 依赖方向）；这个 join 属于 app 的编排职责。

### 4.7 与 AI 的联动（喂 03 §7 置信度模型）

| 命中情形 | 对格式置信度的贡献 |
|---|---|
| `tags` 含 `format:<id>`（如 `format:png`）且规则命中 | 候选 `format_id=<id>` 基线 **0.85~0.95**（与 magic 命中同级，取两者较大值，不叠加） |
| 通用类规则（`packer`、`crypto-constant`）命中 | 不改变格式置信度，但作为**提示**进上下文（"该文件疑似 UPX 加壳"） |
| 恶意类规则命中 | 格式置信度不变；触发安全警告横幅 |
| 无命中 | **不产生负证据**（避免冷门格式被系统性惩罚） |

输出物：`AiEvidence::YaraHint { format_id: Option<Arc<str>>, score: f32, rule: Arc<str> }`，由 `geode-ai` 在候选排序时作为独立证据项参与加权（见 03 §7 的证据表）。

### 4.8 自动扫描策略（Y5）

- **决策：默认不自动全量扫描**。理由：违反 C1 的体感约束（打开文件 < 200ms），且大多数打开操作不需要 YARA。
- `auto` 模式（默认）下的自动行为：当 `规则数 ≤ 500` **且** `文件大小 ≤ 64 MiB` → 后台跑 `HeadTail{head: 8 MiB, tail: 8 MiB}` 快速扫描（≈ < 500ms）；否则只显示一条可关闭的提示条："YARA 规则可用（N 条）。全部扫描约需 ~35s：[扫描] [仅快扫] [不再提示]"。
- 阈值分档：`≤ 64 MiB` 自动快扫；`64 MiB ~ 2 GiB` 提示（预估时间写入按钮）；`> 2 GiB` 只在用户主动打开 YARA 面板后提供"扫描全部/扫描选区/扫描 head+tail"三选一，并显示预估时间与磁盘读取量。
- 已知大小但非 `Known` 的源（流式/网络源）：一律提示，不自动。

## 5 性能预算与基准

| 场景 | 预算 | 备注 |
|---|---|---|
| 1GB 扫描（内置 120 条规则，单线程） | < 30s（≥ 35 MB/s 最坏情形；实测字符串型 3~10s） | CI 基准 `bench_yara_1g` |
| 100GB 扫描（手动触发） | < 50min，内存增量恒定 < 200 MB | 与文件大小无关的内存占用是硬约束 |
| HeadTail 快速扫描（8+8 MiB） | < 500ms | 自动快扫路径 |
| 规则集编译（500 条 / 约 1.5 MB 源码） | < 3s；序列化缓存命中 < 100ms | `bench_compile_500` |
| 命中 → 标注 + 表格上屏 | < 1ms（1000 个命中区间） | AnnotationIndex 单次插入 O(log n) |
| 结果表滚动 | < 16ms/帧（10 万行虚拟滚动） | 与 hex 视图同一虚拟列表组件 |
| 多目标并行 | 8 核 6~7×（近线性） | 按文件并行 |
| 规则集常驻内存 | < 64 MB（含 VM + 元数据） | 超限拒绝编译（§4.3） |
| 序列化缓存体积 | < 20 MB | `~/.cache/geode/rules/` LRU 保留最近 4 份 |

测量口径：`criterion`，固定样本 = 1GB 合成语料（30% 零填充 + 40% 高熵伪随机 + 30% 重复模板）+ 内置 120 条规则；同时记录 `scan_block` 的 MB/s 与总耗时（分离 IO 与匹配时间）。CI 门禁只设"1GB < 30s"与"RSS 增量 < 200MB"两项。

## 6 失败模式与降级

| 失败 | 表现 | 降级 |
|---|---|---|
| 单条规则语法错/编译失败 | 该规则缺失 | 逐文件独立编译，其余规则照常可用（Y3）；错误进 `Diag`，规则面板标红并可点开定位到行 |
| 规则用了 yara-x 未实现的模块/特性 | 编译报 `UnsupportedFeature` | 明确提示"该规则需要 X 模块，当前引擎不支持"，**不静默跳过**；提示可切换后端（远期） |
| 扫描超时 | 部分结果 | 返回 `ScanReport{timed_out: true}`，UI 横幅"扫描超时，结果不完整"+ 一键延长预算重扫 |
| 内存超预算 | 中止 | 保留已收集命中，提示"规则集过大，建议分 namespace 分批扫描" |
| provider 读错误（硬件/holes/网络源） | 缺块 | 跳过该块，`block_errors += 1`，扫描结束后报告"跳过 N 块"（不假装完整） |
| 非 seekable 源 + 非 Full scope | 非法组合 | 编译前报 `E5xxx` 前条件错误，UI 灰掉"扫描选区" |
| 规则集过大（>2000/ns） | 拒绝该来源 | 提示拆分目录；已编译来源不受影响 |
| 规则文件编码非 UTF-8 | 解析失败 | 尝试 CP1252 fallback（Windows 用户常见），失败则报错定位到文件 |
| 引擎内部 panic | Job 中止 | Job 边界 `catch_unwind`（yara-x 基本无 unsafe，风险低）；该 `RuleSet` 标记为可疑，提示用户禁用相关规则 |
| 规则目录不可读/不存在 | 空来源 | 首次运行给出"规则目录已创建：…"提示，不报错 |
| 市场包签名校验失败 | 拒绝加载 | 只跳过该包（见 15 的包校验），其它来源照常 |

## 7 与其它子系统的契约

| 对象 | 读/写 | 内容 |
|---|---|---|
| `geode-provider` | 只读 | `read_at`（顺序块读）；`caps().seekable` 决定可用 scope；`poll_change` → 命中失效 |
| `geode-buffer` | 只读 `version()` | 编辑后已有命中标注标"已过期"（不自动重扫，用户可点重扫） |
| `geode-annot` | 写 | 仅 `Layer::Yara`；规则重载时 `clear_layer(Layer::Yara)` |
| `geode-analysis` | 读 | `FormatGuess` / `HeaderFacts` → 规则预筛 + 快速扫描范围 |
| `geode-content`（15） | 读 | 市场规则包路径、清单（含 `severity` 映射）、签名校验结果 |
| `geode-ai`（03） | 提供 | `YaraHint` 证据项；不直接改置信度（由 03 的模型加权） |
| `geode-ui` | 提供 | `RuleSet` 快照、`ScanReport`、`UiEvent::YaraProgress/YaraDone` |
| `geode-project`（17） | 提供 | 配置持久化；命中结果**默认不入 project 文件**（体积不可控） |
| 事件 | 发出 | `Progress(JobId, f32)`（1 MiB 粒度，UI 节流）、`Diag`（编译错误）、`Done(JobId, ScanReport)` |

## 8 配置项与默认值

| 键 | 默认 | 范围 | 说明 |
|---|---|---|---|
| `yara.enabled` | `true` | bool | 关闭后隐藏 YARA 面板与自动扫描 |
| `yara.rules.dirs` | `["~/.config/geode/rules"]` | 路径列表 | 用户规则目录（递归） |
| `yara.rules.builtin` | `true` | bool | 是否加载内置包（`builtin` ns） |
| `yara.rules.allow_include` | `false` | bool | 固定 false（护栏）；仅企业白名单场景可开 |
| `yara.auto_scan` | `auto` | auto / always / never | 见 §4.8 |
| `yara.auto_scan.max_size` | 64 MiB | 0~2 GiB | 超过则只提示 |
| `yara.auto_scan.max_rules` | 500 | 1~5000 | 规则过多时只提示 |
| `yara.auto_scan.head` / `.tail` | 8 MiB / 8 MiB | 0~64 MiB | 快扫范围 |
| `yara.scan.block_size` | 1 MiB | 64 KiB~16 MiB | 流式块大小 |
| `yara.scan.timeout` | 60s | 1~3600s | 单次扫描总超时 |
| `yara.scan.per_rule_timeout` | 5s | 0.1~60s | 单规则累计超时（引擎支持时生效） |
| `yara.scan.max_ranges_per_rule` | 1024 | 8~65536 | 超出 `truncated` |
| `yara.scan.mem_budget` | 200 MiB | 32~2048 MiB | 扫描期内存上限 |
| `yara.compile.timeout` | 10s | 1~120s | 每文件编译超时 |
| `yara.compile.max_rules_per_ns` | 2000 | 1~20000 | 护栏 |
| `yara.compile.max_rule_bytes` | 64 KiB | 1~1024 KiB | 护栏 |
| `yara.compile.regex_max_nodes` | 200 | 16~4096 | 正则复杂度护栏 |
| `yara.compile.cache` | `true` | bool | 序列化规则缓存 |
| `yara.highlight.color_by_severity` | `true` | bool | 否则统一色 |
| `yara.rescan_on_edit` | `false` | bool | 编辑后自动重扫（大文件慎用） |

## 9 测试策略

- **规则回归测试集**：每条内置规则配一对样本——一个**最小触发样本**（十几个字节即可）与一个**近似但不该命中的反例**；CI 断言命中集合完全相等。新规则无测试样本不许合入。
- **特征样本库**：不携带真实恶意软件。用①公开无害的特征串（EICAR 测试串、UPX/ASPack 文件头、AES S-box 常量、MZ+PE 头片段、PowerShell `-enc` 模板）②开源社区规则（YARA-Rules、Awesome-YARA 的公开规则）作为"真实世界规则集"压测对象③合成语料（§5 的 1GB 合成文件）验证性能与内存。
- **与 libyara 对拍**：同一规则集 + 同一数据，在 CI 的 x86 Linux 容器里同时跑 libyara 与 yara-x，命中集合必须一致；差异必须逐条归类（已知的语义差异：正则方言、`filesize` 边界、大小写修饰符叠加），并写进 `docs/known-diffs.md` 式的附录。
- **跨块边界**：专门构造"匹配跨越 1 MiB 块边界"、"匹配跨 4 KiB 页边界"、"最短 1 字节匹配"的样本，断言单次扫描与分块扫描结果一致（这是 §4.4 语义假设的守卫）。
- **护栏测试**：病态规则集（10 万条、单规则 10 MB、嵌套量词正则 `(a+)+$`、`include /etc/passwd`、`include http://…`）必须被拒绝且**编译时间 < 护栏超时**（验证护栏本身不被绕过）。
- **fuzz**：①随机字节 → 扫描不 panic、块错误不连锁；②随机规则文本 → 解析器不 panic（yara-x 自身也 fuzz，但我们喂的路径含栅栏与 namespace 处理）。
- **性能基准**：§5 全表；`bench_yara_1g` 与 `bench_compile_500` 进 CI 门禁。
- **集成测试**：100GB 稀疏文件验证"扫描时内存恒定且可取消（取消响应 < 100ms）"；Mock Provider 注入读失败验证降级路径与 `block_errors` 语义。

## 10 开放问题

1. **单文件并行是否值得**：规则集分片 + 多遍 IO 可达 2~3×，但 1GB 就要多读 2 倍。是否只对"内存映射/缓存命中"的源开启？（当前决策：不做）
2. **`pe`/`elf` 模块冲突**：这些模块要完整文件缓冲，与 100GB 流式目标矛盾。方案 A：仅当 `size ≤ 512 MiB` 时启用模块（超限禁用相关规则并提示）；方案 B：自研流式 `pe` 模块。倾向 A，待与 03（AI 常用 PE 字段）权衡。
3. **内置规则包的分发与签名**：内置规则随版本发；但"规则包"与 15 的模板市场共用一套签名/更新机制还是独立？`severity` 元数据是否该进 15 的 pack manifest（那样市场页面能展示"含 3 条高危规则"）。
4. **序列化规则缓存的可移植性**：yara-x 的 `Rules` 序列化格式是否跨版本稳定？若不稳定，缓存必须带引擎版本号并在升级后失效（当前按此保守实现，需实测确认）。
5. **规则热重载的粒度**：文件监听（`notify`）会引入新的平台依赖与节流复杂性；先做"手动重载 + 面板按钮"是否够用。
6. **命中结果是否入 project（17）**：不入则每次打开要重扫（大文件贵），入了则 project 文件体积不可控。折中：只存"规则集指纹 + 命中摘要（规则名+首偏移+计数）"，不存全部 ranges。
7. **`adhoc` 临时规则的边界**：是否允许临时规则写盘（便于复现 bug）？若写盘，落在哪个 namespace、是否参与下次编译。
8. **是否需要 `externals`**：某些规则集依赖外部变量（`filename`、`filetype`）；我们天然知道文件名与 magic 结果，是否默认注入 `filename` / `extension` / `mime` 三个 external，以及它们的类型约定。
