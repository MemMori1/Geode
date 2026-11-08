# 00 — 架构总纲（所有子系统文档的契约）

**项目代号：Geode**（晶洞，/ˈdʒiː.oʊd/）。CLI 名 `geode`；所有 crate 前缀 `geode-*`；文件扩展名 `.geows` / `.geoproj` / `.geopkg` / `.geopatch` / `.geoplug` / `.gmagic`；结构描述语言名 **BinSpec**（`.bspec`）不随品牌改名。

**许可证（待用户最终确认）**：程序本体 MIT OR Apache-2.0 双许可；模板与主题资源库 CC0-1.0；文档 CC BY 4.0；插件可用任意许可，但 ABI 头文件为 MIT。该假设影响 14（插件条款）、15（生态策略）、19（威胁模型假设）、24（迁移红线），改动需同步这四份。

**架构决策记录**：所有"为什么不用 X"的决策已归档在 `docs/adr/`（19 篇文档正文中散落的否决理由，按编号索引入口在 `docs/adr/README.md`）。新决策只增不改。

> 本文是**规范**，不是建议。所有子文档必须引用这里的类型、分层与预算；冲突时以本文为准，本文错了就改本文。

---

## 1. 产品定位

> **一个能在 100GB 数据上流畅工作的十六进制编辑器，用 BinSpec 描述结构，用 AI 生成 BinSpec。**

三条不可协商的产品约束：

| # | 约束 | 含义 |
|---|---|---|
| C1 | **任何 UI 路径都不得是 O(文件大小)** | 打开、滚动、跳转、选中、高亮、书签、结构树展示，全部与文件大小无关。破坏这条的功能一律否决 |
| C2 | **正确性优先于功能** | 编辑器处理的是"用户唯一的原始数据"。任何可能损坏数据的路径（保存、撤销、patch）必须有事务语义 |
| C3 | **一切结构都必须可回链到字节区间** | 结构节点、高亮、诊断、搜索结果、AI 推断结果，全部携带 `ByteRange`。这是双向联动的唯一基础 |

---

## 2. 技术栈定案

| 层 | 决定 | 备选与否决理由 |
|---|---|---|
| 语言 | **Rust**（edition 2024，MSRV 随 CI 滚动） | C++：解析不可信二进制 + 执行用户脚本是 C++ 的死亡地带，每年都有工具被恶意样本打穿 |
| GUI | **egui**（立即模式）+ wgpu 后端 | Tauri+Web：hex 视图必须 canvas 自绘，且大文件数据经 IPC 有成本，Web 化收益不抵复杂度。Qt：包体积 + LGPL 动态链接约束 + 自绘 hex 视图并无优势 |
| hex 网格 | egui 的 `Painter` 自绘（走 wgpu paint callback），**不用 widget 树** | 一屏 ~4000 个单元格，任何 retained widget 都会崩 |
| 并发 | **自研轻量 job 系统**（crossbeam-channel + rayon 线程池） | tokio：本系统全是 CPU-bound 批处理，无大量异步 I/O，tokio 只增加复杂度 |
| 插件 | C ABI + 版本化 vtable，`.geoplug` = cdylib | 暴露 Rust 类型：无稳定 ABI。WASM：性能与数据拷贝代价过大（可作 v2 选项） |
| i18n | JSON catalog + fluent 风格插值 | gettext：CJK 与复数规则处理差 |
| 主题 | TOML 主题文件，运行时热重载 | |
| 打包 | 静态链接优先；macOS 公证、Windows 代码签名（**必做，否则杀软误报**） | |

**不可妥协的分层约束**：除 `geode-ui` 与 `app` 外，**任何 crate 不得依赖 GUI 库**。这条决定了未来能否换 UI 框架而不动内核。

---

## 3. Crate 分层

```
                     ┌────────────────────────────────────┐
                     │            app (bin)               │
                     └───────────────┬────────────────────┘
                                     │
                     ┌───────────────▼────────────────────┐
                     │            geode-ui (egui)           │
                     └───┬───────┬───────┬───────┬────────┘
                         │       │       │       │
        ┌────────────────┘       │       │       └─────────────────┐
        │                        │       │                         │
┌───────▼────────┐  ┌────────────▼──┐ ┌──▼──────────┐  ┌──────────▼────────┐
│ geode-project    │  │ geode-ai        │ │ geode-content │  │ geode-plugin-host   │
│ 项目/布局/恢复  │  │ AI 生成/校验   │ │ 分发/更新器  │  │ 插件宿主          │
└───────┬────────┘  └────┬──────────┘ └──┬──────────┘  └──────────┬────────┘
        │                │               │                        │
        └────────────────┴───────┬───────┴────────────────────────┘
                                 │
   ┌──────────┬──────────┬───────┴────┬──────────┬──────────┬──────────┐
   │          │          │            │          │          │          │
┌──▼──────┐┌──▼─────┐┌───▼─────┐┌─────▼────┐┌────▼────┐┌────▼─────┐
│geode-bspec││geode-proc││geode-search││ geode-diff ││geode-disasm││ geode-yara │
└──┬──────┘└──┬─────┘└───┬─────┘└─────┬────┘└────┬────┘└────┬─────┘
   │          │          │            │          │          │
   └──────────┴────┬─────┴────────────┴──────────┴──────────┘
                   │
        ┌──────────┴──────────┬─────────────────┐
        │                     │                 │
┌───────▼──────┐   ┌──────────▼───────┐  ┌──────▼─────────┐
│ geode-analysis │   │   geode-annot      │  │  geode-provider  │
│哈希/熵/分布   │   │ 标注区间索引      │  │  数据源         │
└───────┬──────┘   └──────────┬───────┘  └──────┬─────────┘
        │                     │                 │
        └─────────────────────┴────────┬────────┘
                                       │
                        ┌──────────────▼──────────────┐
                        │    geode-buffer (piece table) │
                        └──────────────┬──────────────┘
                                       │
                        ┌──────────────▼──────────────┐
                        │  geode-core (类型/诊断/预算)    │
                        └─────────────────────────────┘
```

**依赖方向唯一**：箭头只能向下。任何"下游要知道上游"的需求，用事件/回调/泛型参数解决，不开反向依赖。

补充一个后加的 crate：**`geode-format`**（L0，纯 CPU）：7 个自有格式（`.geows`/`.geoproj`/`.geopkg`/`.geopatch`/`.gmagic`/`.geojournal`/`.geolock`）的读写、版本探测与迁移的唯一实现。它不持有数据源、不碰 `Provider`，只认字节与 TOML，避免每个使用方各自写一份"如何判定版本"。规范见 `23-file-formats`。

补充一个后加的 crate：**`geode-viz`**（字段可视化器的解码与度量，纯 CPU，禁止任何 GUI 依赖），位于 `geode-analysis` 同层，被 `geode-ui`（绘制）与 `geode-plugin-host`（注册插件可视化器）使用。见 `13-visualizers §2`。

---

## 4. 核心类型契约（`geode-core`）

所有子系统共用。子文档中如需扩展，**加字段可以，改语义必须先改本文**。

```rust
// ── 位置与区间 ────────────────────────────────────────────
pub struct Offset(pub u64);            // 一律 u64，禁止 usize / long
pub struct ByteRange { pub start: u64, pub len: u64 }
impl ByteRange {
    pub fn end(&self) -> u64;
    pub fn contains(&self, off: u64) -> bool;
    pub fn overlaps(&self, o: &ByteRange) -> bool;
    pub fn clamp(&self, limit: u64) -> ByteRange;   // 越界自动裁剪，绝不 panic
}

// ── 诊断 ─────────────────────────────────────────────────
pub enum Severity { Error, Warning, Info }

pub struct Diag {
    pub code: DiagCode,          // E1xxx~E5xxx 分段，见 02-dsl-spec §8.2
    pub severity: Severity,
    pub range: ByteRange,        // 必须有字节区间 → UI 可精确标红
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
    pub path: Vec<Seg>,          // 结构路径（DSL 节点 / 搜索 / diff 单元）
    pub suggestion: Option<String>,
    pub recoverable: bool,
}

// ── 值 ───────────────────────────────────────────────────
pub enum Value {
    U64(u64), I64(i64), F64(f64), Bool(bool),
    Bytes(Arc<[u8]>),
    Str(String),                 // 已按编码解码
    Compound,                    // 有子节点
    Unresolved(UnresolvedReason),
}

// ── 统一标识 ─────────────────────────────────────────────
pub struct DocId(pub u32);       // 一个打开的文档
pub struct ViewId(pub u32);      // 一个视图（同一文档可多视图）
pub struct JobId(pub u64);
pub struct EditId(pub u64);
```

---

## 5. 跨子系统接口契约

### 5.1 `geode-provider` — 数据源

```rust
pub enum SizeInfo { Known(u64), Unknown }   // 流式源可能未知

pub struct Caps {
    pub writable: bool,
    pub seekable: bool,
    pub volatile: bool,     // 内容会变（进程内存、UDP 流）
    pub holes: bool,        // 存在不可读空洞（进程地址空间）
}

pub enum ChangeKind { Unchanged, Resized(u64), Unmapped, Replaced }  // Replaced = 大小不变内容变
pub struct ProviderId(pub u32);                                      // 缓存键 + Arc 身份
pub struct ReadReport { pub filled: u64, pub holes: Vec<ByteRange>, pub eof_hit: bool }

pub trait Provider: Send + Sync + 'static {
    fn kind(&self) -> &'static str;
    fn id(&self) -> ProviderId;
    fn size(&self) -> SizeInfo;
    fn read_at(&self, off: u64, buf: &mut [u8]) -> Result<usize, ProviderError>;
    fn write_at(&self, off: u64, data: &[u8]) -> Result<(), ProviderError>;  // 默认 Unsupported
    fn caps(&self) -> Caps;
    /// 区间读：默认实现按可读段切分、逐段调 read_at。空洞不填不报错，只记录。
    fn read_range(&self, r: ByteRange, out: &mut [u8]) -> Result<ReadReport, ProviderError>;
    /// 该区间内的可读子段（holes 源才有意义），默认 push(r)
    fn readable_ranges(&self, r: ByteRange, out: &mut Vec<ByteRange>);
    fn poll_change(&self) -> ChangeKind;
    fn info(&self) -> ProviderInfo;        // 显示用元数据
}
```

**铁律**（`06-data-providers §3.1` 细化，凡实现必遵守）：
1. **短读合法**：`Ok(n)` 且 `n < len` 是常态（EOF、空洞边界、设备到扇区尾），上层必须循环读。
2. **越界不 panic**：`off >= size` 返回 `Err(OutOfBounds)`；`off < size` 但越界则短读 + `eof_hit`。
3. **空洞是显式结果**：`read_at` 要么整段可读要么整段 `Err(Hole)`，跨洞一律走 `read_range`。
4. `volatile` 源必须在 UI 上明示；`holes` 源的空洞区在 hex 视图渲染为明显的"不可读"纹理。

### 5.2 `geode-buffer` — 可编辑缓冲区

```rust
pub trait ByteBuffer: Send {
    fn len(&self) -> u64;
    fn read(&self, range: ByteRange, out: &mut [u8]) -> Result<(), BufferError>;

    // 三种编辑原语，全部返回 EditId 供撤销定位
    fn overwrite(&mut self, at: u64, data: &[u8]) -> Result<EditId, BufferError>;
    fn insert(&mut self, at: u64, data: &[u8]) -> Result<EditId, BufferError>;
    fn remove(&mut self, range: ByteRange) -> Result<EditId, BufferError>;

    fn undo(&mut self) -> Result<UndoOutcome, BufferError>;
    fn redo(&mut self) -> Result<UndoOutcome, BufferError>;
    fn checkpoint(&mut self, label: &str);      // 撤销分组边界

    fn patches(&self) -> PatchSet;              // 已修改区间（相对原文件）
    fn version(&self) -> u64;                   // 单调递增，所有缓存/异步结果靠它失效
}
```

### 5.3 `geode-annot` — 标注层（整个 UI 的高亮数据源）

```rust
// 顺序即 z 序（后写的层压在上面）。共 10 个变体，主题必须逐一提供颜色（见 16 §4）。
pub enum Layer {
    Value,       // 值类型分区（u8/u16/… 的交替底色）
    Search,      // 搜索命中
    Structure,   // BinSpec 结构区间
    Analysis,    // 熵段 / 字符串 / 坏区 / 可视化器 hover（后加，见 10 §7）
    Selection,   // 选区
    Bookmark,
    Diff,        // diff 差异
    Yara,
    Ai,          // AI 推断结果与置信度
    Cursor,
}

pub struct Ann {
    pub range: ByteRange,
    pub layer: Layer,
    pub tag: AnnTag,          // 语义引用（DSL 节点 id / 搜索命中 id / 书签 id）
    pub fg: Option<Color32>,
    pub bg: Option<Color32>,
    pub underline: bool,
    pub priority: u8,         // 同层覆盖顺序
}

pub trait AnnotationIndex {
    fn insert(&mut self, ann: Ann) -> AnnId;
    fn remove(&mut self, id: AnnId);
    fn clear_layer(&mut self, layer: Layer);
    fn query(&self, range: ByteRange, out: &mut Vec<AnnId>);  // 区间查询，O(log n + k)
    fn query_point(&self, off: u64) -> AnnId;                 // 最深优先，用于点击命中
}
```

**这是全系统最热的接口**：hex 视图每帧对可见区间查询一次。必须 O(log n + k)，且插入/删除不得使 id 失效（用 slotmap + 稳定索引）。

### 5.4 后台任务

```rust
pub struct CancelToken(Arc<AtomicBool>);   // 协作式取消，粒度为"每 N 次迭代检查"

pub trait Job {
    fn name(&self) -> &str;
    fn run(&self, ctx: &JobCtx) -> JobResult;   // ctx 提供 cancel / progress / 预算
}

// UI 每帧 drain 一次，绝不在 UI 线程上做 >1ms 的工作
pub enum UiEvent { Progress(JobId, f32), Diag(Vec<Diag>), Done(JobId, JobResult), ... }
```

---

## 6. 线程模型

```
┌─────────────┐   提交 Job       ┌──────────────────────┐
│  UI 线程     │ ───────────────► │  Job 系统 (rayon pool) │
│ (egui, 16ms)│ ◄─────────────── │  可取消 / 有预算        │
└─────────────┘   UiEvent (chan) └──────────────────────┘
       │
       │ 只读查询（无锁，Arc<Snapshot>）
       ▼
┌─────────────────────────────────────────────┐
│ 文档状态：Buffer + AnnotationIndex + 结构树   │
│ 由 RwLock 保护；写只在 UI 线程，读可跨线程     │
└─────────────────────────────────────────────┘
```

规则：
1. **UI 线程不做任何 >1ms 的工作**。违反这条是性能问题的唯一来源。
2. **编辑只在 UI 线程发生**（保证撤销顺序确定）。后台 Job 只读 `Arc<Snapshot>`。
3. **后台结果回主线程时必须校验 `buffer.version()`**，不匹配即丢弃（防止陈旧结果覆盖新状态）。
4. 任何后台任务必须有 `CancelToken`，且必须在 100ms 内响应取消。

---

## 7. 全局性能预算（CI 基准，退化即红灯）

| 场景 | 预算 | 测试条件 |
|---|---|---|
| 打开文件（只读） | < 200ms | 4GB 本地文件，不整读 |
| 打开文件 | < 50ms | 1MB 文件 |
| 滚动帧时间 | < 16ms（p99 < 25ms） | 任意大小，含高亮 + 结构树在场 |
| 跳转到任意偏移 | < 5ms | 100GB 文件 |
| 选区拖拽 | < 16ms/帧 | 选中 100 万字节 |
| hex 搜索（简单模式） | > 800MB/s | 1GB 文件，16 字节模式 |
| hex 搜索（通配符） | > 200MB/s | |
| 正则搜索 | > 80MB/s | |
| BinSpec 编译 | < 20ms | 1000 行模板 |
| BinSpec 执行 | < 100ms | 10MB 文件 + 中等模板 |
| Annotation 区间查询 | < 50µs | 10 万条标注 |
| 搜索（数值范围） | > 400MB/s | 单值 MSB 路径（见 08 §5） |
| 搜索（字符串，8 编码全开） | ≥ 300MB/s | 见 08 §5 |
| Diff | < 5s | 两个 100MB 文件 |
| Diff 内存峰值 | < 800MB | 两个 100MB 文件（见 09 §5） |
| 保存（无改动重写） | > 500MB/s | 1GB |
| 撤销/重做单步 | < 1ms | 任意历史深度 |
| 内存占用 | < 500MB | 4GB 文件，仅查看 |
| 内存占用 | < 原大小 + 改动量 × 2 | 任意编辑序列 |
| 首帧时间（冷启动到可交互） | < 250ms | 含最近文件恢复，不含内容读取（见 21 §5.3 B1） |
| 打开 100GB 稀疏镜像 | < 5ms | 仅建 Buffer 与 SizeInfo，不建索引（21 B2） |
| piece table 随机编辑 | 单步 p99 < 1ms，曲线斜率 O(log N) | 1e5 次随机覆盖编辑（21 B3） |
| 覆盖编辑 → 落盘端到端 | < 2s | 单字节改动 + 保存到本地 SSD（21 B4） |
| 熵分析吞吐 | ≥ 1.5GB/s | 1MiB 分块（21 B5） |
| magic 识别 | < 5ms | 206 条规则全量匹配（21 B6） |
| 字段可视化首渲染 | 8ms 内联 / 60ms 面板 / 250ms 触发阻塞阈值 | 64×64 缩略图两阶段（21 B7） |
| 正则搜索最坏情况 | ≥ 60MB/s | 长串无匹配、无界长度模式（21 B8） |

**这些数字写进 `bench/` 并接入 CI。没有基准的功能等于没有功能。**

---

## 8. 术语表（子文档必须用同一套词）

| 术语 | 定义 |
|---|---|
| **Provider** | 数据源。文件、磁盘、进程内存、GDB、SSH、流 |
| **Buffer** | 可编辑字节序列，由 piece table 实现，叠加在原 Provider 之上 |
| **Piece** | Buffer 的一个片段：literal（实数据）或 placeholder（窗口到 Provider） |
| **Patch** | 相对原文件的一处修改记录（offset, old, new），用于保存与比对 |
| **Annotation / Ann** | 标注区间，带图层与语义引用。所有视觉高亮都走这一层 |
| **Layer** | 标注图层，见 §5.3 的 `Layer` 枚举 |
| **Node** | BinSpec 解析产出的结构节点（`01`/`02` 文档的 `Node`） |
| **Graph / Node 图** | 数据处理器节点图（与 BinSpec 的 Node 区分，写全称） |
| **Doc** | 一个打开的文档（Buffer + Annotations + 结构树 + 视图集合） |
| **View** | 一个视图（hex / 结构树 / Inspector / Diff / 反汇编…），同一 Doc 可多 View |
| **Workspace** | 一组 Doc + 布局的持久化单元 |
| **Cursor（游标）** | 编辑位置。**UI 文案统一写「游标」**；正文里"光标"与"游标"是同一概念，允许互换（`25` 的文案表以「游标」为准） |
| **Job** | 后台任务 |
| **Verifier** | BinSpec 的编译+执行+不变量自检模块，独立于 AI 可单用 |

---

## 9. 全局设计原则

1. **越界即裁剪，不 panic。** 所有 `ByteRange` 操作返回 `clamp` 后的结果。
2. **一切异步结果都带版本号。** 陈旧结果静默丢弃，不弹错误。
3. **降级优于失败。** 一个子功能挂了（模板跑不通、插件崩溃、AI 超时），其它功能必须继续可用，并在 UI 上给出可关闭的提示条。
4. **默认安全。** 沙箱预算（02 §7.2）、只读打开、保存到临时文件再原子 rename。
5. **可观测。** 每个 Job 有耗时统计；每个渲染有帧预算统计。开发构建内置性能面板。
6. **不引入隐式全局状态。** 所有上下文显式传递，便于测试与多文档并存。

---

## 10. 子文档清单与职责边界

| 文档 | 拥有 | 不得涉及 |
|---|---|---|
| `01-competitor-feature-research.md` | 竞品、功能分层、选型依据 | 实现细节 |
| `02-dsl-spec-draft.md` | BinSpec 语言本身 | 编辑器 UI、AI |
| `03-ai-dsl-generation.md` | AI 生成与验证闭环 | 语言语法定义（引用 02） |
| `04-buffer-and-edit-model.md` | piece table、COW、撤销、patch、保存事务 | 渲染 |
| `05-render-and-hexview.md` | hex 网格渲染、虚拟滚动、光标选区、高亮合成、双向联动 | 标注索引内部结构（引用 04/06） |
| `06-data-providers.md` | 各类 Provider 实现与生命周期 | Buffer 编辑语义 |
| `07-data-processor-graph.md` | 显示前变换的节点图 | 编辑、保存 |
| `08-search-subsystem.md` | 四种搜索引擎 | UI 呈现 |
| `09-diff-subsystem.md` | 二进制 diff 与对齐映射 | 结构级 diff 的 UI |
| `10-analysis-toolkit.md` | 哈希、熵、字节分布、magic、Inspector 值解码 | |
| `11-disassembler.md` | Capstone 集成 | |
| `12-yara.md` | YARA 扫描 | |
| `13-visualizers.md` | 字段可视化器体系 | 具体渲染实现细节 |
| `14-plugin-system.md` | 插件 ABI、加载、沙箱、版本 | 具体插件功能 |
| `15-content-distribution.md` | 模板库、主题库、magic 库、更新器、签名校验 | 项目文件格式（见 16） |
| `16-ui-shell-theme-i18n.md` | 主窗口布局、多标签、工作区、命令系统、快捷键、主题、i18n、字体 | 各视图内部 |
| `17-project-and-persistence.md` | 项目/工作区文件格式、崩溃恢复、自动保存 | |
| `18-engineering-plan.md` | 目录结构、CI、测试策略、里程碑、人力 | 重复子系统细节 |
| `19-security-threat-model.md` | 信任边界、攻击面登记、TH-01~TH-22 威胁清单、漏洞响应 | 具体验证手段（见 20） |
| `20-test-corpus-and-fuzzing.md` | 测试语料库、六类边界样本、fuzz target、triage | 性能门槛（见 21） |
| `21-benchmark-suite.md` | 基准数据集、bench 口径、基准机与回归判定 | CI 流水线本身（见 18） |
| `22-bspec-toolchain.md` | fmt / lint / LSP / 语法高亮 / VM 调试器 / 错误文案映射 | 语言语义（见 02） |
| `23-file-formats.md` | 七个自有格式的正式规范、schema、测试向量、弃用政策 | 各子系统的语义（见 15/17） |
| `24-migration-and-interop.md` | 从外部生态导入模板、IR、GPL 红线、反向导出 | Verifier 本身（见 03） |
| `25-ux-flows-and-copy.md` | 核心旅程、线框、文档状态机、文案规范 | 视觉与主题（见 16） |
| `adr/NNNN-*.md` | 关键决策与否决理由的不可变记录 | 设计细节（引用正文文档） |

---

## 11. 文档写作约定

- 每份子系统文档必须包含这 10 节，缺一节视为未完成：
  1. 目标与非目标
  2. 依赖与边界（上下游 crate、引用的契约）
  3. 数据结构与接口（Rust 签名草案）
  4. 核心算法 / 流程
  5. 性能预算与基准
  6. 失败模式与降级策略
  7. 与其它子系统的契约（明确读写哪些共享状态）
  8. 配置项与默认值
  9. 测试策略（单元 / 集成 / fuzz / 基准）
  10. 开放问题
- 中文写作；标识符、类型名、trait 名用英文。
- 允许伪代码与签名，不允许真实实现代码。
- 引用其它文档用 `见 02-dsl-spec §7.2` 形式。

---

## 12 错误码分配登记表（唯一权威）

诊断码是跨子系统的全局命名空间，**新增段必须先在此登记**。子文档各写各的段是既往事故来源（06/08/13/15 曾撞码），此处统一裁决。

| 段 | 归属 | 文档 | 说明 |
|---|---|---|---|
| `E0xxx` | UI 外壳 / 工程工具链 | 16, 18 | 主题 token 缺失、字体非等宽、依赖分层违规 |
| `E1xxx` | DSL：数据与类型不匹配 | 02 §8.2 | 字段值不符、magic 不符 |
| `E2xxx` | DSL：执行限制 | 02 §8.2 | 预算超限、超时、栈深 |
| `E3xxx` | DSL：边界 | 02 §8.2 | 越界读、递归过深 |
| `E4xxx` | DSL：语法 / 类型 | 02 §8.2 | 编译期错误 |
| `E5xxx` | DSL：前条件 | 02 §8.2 | 断言失败 |
| `E6xxx` | 数据源 Provider | 06 §3.2 | 权限、断连、协议、格式校验 |
| `E7xxx` | 搜索 | 08 §6 | 模式无信息量、不支持的特性、跨块风险 |
| `E8xxx` | 字段可视化器 | 13 §6 | 参数错误、未知 id、解码炸弹、超时 |
| `E9xxx` | 插件宿主 | 14 §6 | ABI 不符、签名、panic、配额 |
| `EAxxx` | 内容分发 | 15 §6 | 摘要/签名不符、zip slip、版本冲突 |
| `EBxxx` | 自有文件格式与迁移 | 23 §6, 24 §6.1 | `EB0xx` 格式解析/校验/迁移失败（版本过新、schema 不符、CRC 不符、zip slip 之外的结构错误）；`EB1xx~EB5xx` 迁移子域（源语法、语义不可表达、产物质量、许可红线、批处理） |
| `ECxxx` | 安全策略 | 19 §7.4 | 安全策略拒绝、路径越界、超上限拒绝（拒绝而非失败）、缺二次确认、凭据外泄自检、未同意联网、预算来源非法 |

规则：
1. 一个码 = 一个可被 UI 稳定翻译的语义，**不得复用**。
2. 每条 `Diag` 必须带 `ByteRange`（没有字节区间的诊断无法定位，视为缺陷）。
3. 面向 AI 的修复闭环（见 03）依赖码的稳定性：码值一旦发布不得改义，只能新增。
