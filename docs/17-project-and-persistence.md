# 17 — 项目 / 工作区 / 持久化 / 崩溃恢复

> 本文**拥有**：三层配置的划分与字段、`*.geows` / `*.geoproj` 格式、schema 迁移、自动保存与 journal、崩溃恢复、并发锁、最近文件。
> 本文**不得涉及**：Buffer 编辑语义（见 04-buffer-and-edit-model）、布局控件树（见 16-ui-shell-theme-i18n）、模板条目格式（见 15-content-distribution）。
> 上游硬契约：00-architecture-overview §3（分层）、§5.2（`ByteBuffer`）、§7（预算）、§9（默认安全）。冲突以 00 为准。

## 1 目标与非目标

**目标**（每条同时是验收标准）

| # | 目标 | 判定 |
|---|---|---|
| G1 | **永不丢数据**：崩溃、断电、`kill -9`、磁盘满都不能让改了半天的东西消失 | 随机 kill 1000 轮，已 fsync 的编辑恢复率 100%（§9.3） |
| G2 | **分析方案可进 git**：纯文本、无绝对路径、无密钥、可 diff | 两台机器 clone 后打开，书签/模板/断言一致 |
| G3 | **密钥永不落盘**：SSH 密码、解密密钥、AI key 走系统钥匙串 | 导出项目后字节级 grep 明文，命中 0 |
| G4 | **恢复可解释**（先预览再恢复）且**一切持久化带版本号**，旧版不得静默损坏新版 | 对话框列出文档/编辑条数/时间范围；迁移矩阵见 §9.1 |

**非目标**：云同步/协作后端（N1）；在项目里存 buffer 内容或样本副本（N2）；超过三层的嵌套配置（N3）；持久化完整撤销栈（N4，见 §4.8）；自定义加密容器（N5）。

**三条不可协商约束**：① 同目录临时文件 → `fsync` → 原子 `rename`（00 §9.4），禁止就地覆写；② journal 是"未保存编辑"的唯一真相来源，**只记 `EditOp` 序列**、与 piece table 实现解耦；③ **恢复失败绝不自动删除 journal**，只保留 + 报路径 + 提供导出。

## 2 依赖与边界

`geode-project` 位于 00 §3 第二层，与 `geode-ai`/`geode-content`/`geode-plugin-host` 平级，只依赖内核层，**不得依赖 `geode-ui`**。

| 方向 | 对象 | 用途 |
|---|---|---|
| 上游 | `geode-core` / `geode-buffer` / `geode-format` | 类型与 id；只取 `EditOp` 枚举（序列化）与 `version()`，**不碰 piece 内部结构**；文件格式的版本探测/迁移一律调 `geode-format`（`23`），本模块不自写第二套 |
| 上游 | `geode-provider` | `ProviderInfo`、`Caps::writable`、指纹来源 |
| 下游 | `app` / `geode-ui` | app 做装配与恢复编排；ui 读写布局与 `ViewState`（serde 在 ui 侧，本模块用 `toml::Value` 中转） |
| 平级 | `geode-bspec`/`geode-proc`/`geode-search`/`geode-yara` | 只引用其 id/引用类型（`TemplateRef` 等），不调用其逻辑 |

第三方：`serde`+`toml`（全部格式；TOML 而非 JSON：可手改、可 diff、可注释）、`blake3`（指纹）、`crc32fast`（journal 完整性）、`keyring`（钥匙串）、`fs4`（跨平台 `flock`/`LockFileEx`）、`tempfile`、`ulid`（`doc_uuid`，单调可排序）、`directories`（XDG）。

**决策：不引入 sqlite。** 单文件 TOML + 追加式 journal 已够；嵌入式数据库会让 G2（进 git）直接失败。

## 3 数据结构与接口

### 3.1 三层划分的判据

规则只有一条：**这个信息属于谁？换项目该不该变？该不该给同事？**

| 信息 | 归属 | 判据 |
|---|---|---|
| 主题、语言、快捷键、字号、AI 端点/模型名 | 全局 `config.toml` | 换项目不变 |
| API key、SSH 密码、解密密钥 | **系统钥匙串**（不属于任何层） | 敏感 |
| 打开了哪些文档、布局、每视图光标滚动 | 工作区 `*.geows` | 只对本次会话有意义 |
| 该文件用哪个模板、书签、断言、处理器图、YARA 规则集 | 项目 `*.geoproj` | 是分析结论，要对团队可复现 |
| 最近文件、收藏、窗口位置 | 全局 `state/` | 机器本地状态，不是偏好 |

```
~/.config/geode/        config.toml · keymap.toml · themes/           # 用户可手改
~/.local/state/geode/   recent.toml · session.snapshot · instances/ · journals/ · scratch/
~/.cache/geode/         magic 缓存 · 字体子集 · 模板索引 · AI 采样缓存（可丢弃）
```

**决策：`state` 与 `config` 分离。** 最近文件写得频繁且可能很大，不能与用户手改的 `config.toml` 混写——否则会覆盖掉用户的注释。

### 3.2 全局配置 `~/.config/geode/config.toml`

| 键 | 默认 | 说明 |
|---|---|---|
| `schema_version` | `2` | 见 §4.4 |
| `ui.theme` / `ui.language` / `ui.scale` | `"dark-default"` / `"auto"` / `1.0` | 主题名或 `file:./x.toml`；语言 BCP-47 |
| `ui.fonts.hex` | `"JetBrains Mono"` | hex 列**必须**纯等宽无 CJK（见 05） |
| `editor.default_bytes_per_row` / `.default_group` / `.offset_radix` / `.undo_memory_limit_mb` | `16` / `1` / `"hex"` / `256` | 撤销栈内存上限见 04-buffer-and-edit-model §8 |
| `files.open_mode` / `files.fingerprint` / `files.mmap_threshold_mb` | `"read-write"` / `"head64k"` / `512` | 超过阈值改用 `pread`+LRU（见 06） |
| `ai.enabled` / `.provider` / `.model` / `.privacy_mode` | `true` / `"anthropic"` / `"claude-sonnet-4"` / `"sampled"` | 隐私模式见 03 §10 |
| `ai.api_key_ref` | `""` | **`keyring:geode/ai/anthropic` 形式，永不存明文** |
| `content.sources` / `content.auto_update` | `["official"]` / `"notify"` | 见 15 |
| `recovery.prompt` | `"ask"` | `ask`/`auto`/`never` |

```toml
schema_version = 2
ui = { theme = "dark-default", language = "zh-CN", hex_font = "JetBrains Mono" }
editor = { default_bytes_per_row = 16, group = 1, offset_radix = "hex", undo_memory_limit_mb = 256 }
ai = { provider = "anthropic", model = "claude-sonnet-4", privacy_mode = "sampled",
       api_key_ref = "keyring:geode/ai/anthropic" }   # ← 只有引用，没有值
content = { sources = ["official"], auto_update = "notify" }
files = { open_mode = "read-write", fingerprint = "head64k", mmap_threshold_mb = 512 }
recovery = { prompt = "ask" }
```

### 3.3 工作区 `*.geows`

工作区 = 一组打开的文档 + 布局 + 各视图状态，是**会话的持久化**；默认建议进 `.gitignore`（允许用户刻意提交以复原分析现场）。

| 字段 | 说明 |
|---|---|
| `schema_version` / `kind` | `kind` 固定 `"geows"`，防止误把 `.geoproj` 塞进来 |
| `workspace_id` / `project` | ULID（锁与跨实例识别）/ 可选 `.geoproj` 相对路径；工作区可独立存在 |
| `docs[]` | `doc_uuid` / `source{kind:"file"\|"untitled"\|"provider"}` / `dirty_at_exit` / `project_doc` |
| `layout` / `window` | `Split{axis,ratio,children}` / `Tab{views:[view_id]}`；window 为机器本地，可安全丢弃 |
| `views[]` | `ViewState`（§3.4.2） |

```toml
schema_version = 1
kind = "geows"
workspace_id = "01J9Z8QK7R3M2X0V5B4N6T1C8D"
project = "acme-router.geoproj"
window = { w = 1680, h = 1020, dpi_scale = 2.0 }        # 机器本地，可安全丢弃
layout = { type = "split", axis = "horizontal", ratio = 0.62,
           children = [{ type = "tab", views = [1, 2] }] }
docs = [
  { doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1", project_doc = "fw", dirty_at_exit = false,
    source = { kind = "file", path = "/home/ana/samples/router_v1.2.bin" } },
  { doc_uuid = "01J9Z9B2S4P6N8K0H2G4F6D8E1", dirty_at_exit = true,    # → 启动走恢复流程
    source = { kind = "untitled" } },
]
views = [
  { view_id = 1, doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1", kind = "hex", cursor = 0x1040A,
    selection = { ranges = [{ start = 0x10400, len = 32 }], rect = false },
    scroll_first_row = 4096,                  # 行号，不是像素
    columns = { bytes_per_row = 16, group = 4, radix = "hex", width_adaptive = true },
    text_encoding = "windows-1252" },
]
```

### 3.4 项目 `*.geoproj` — 可提交的"分析方案"

**项目不含**：文件内容、绝对路径、密钥、布局、光标。

| 组 | 字段 |
|---|---|
| 元信息 | `schema_version`(1) / `kind="geoproj"` / `project_id`(ULID) / `name` / `created_at` / `modified_at` / `app_version` |
| 文档引用 | `documents[]`：`doc_uuid` / `id`(短名) / `path`(相对) / `optional` / `fingerprint` / `note` |
| 结构解析状态 | `documents[].templates[]`：`template` / `version` / `source`(`library`\|`project_file`\|`generated`) / `sha256` / `entry_point` |
| 处理器图 | `[processors]`：`nodes[]`(id/kind/params) / `edges[]` / `bypass` |
| 书签 / YARA / 断言 / 搜索史 | `bookmarks[]`(id/doc/range/label/color/tags) / `[yara]`(ruleset/rule_dirs/namespaces/min_hits) / `assertions[]`(id/doc/expr/severity) / `[search_history]`（≤20 条查询） |
| AI provenance | `[[ai.provenance]]`（§3.4.3，只记元数据） |

```toml
schema_version = 1
kind = "geoproj"
project_id = "01J9Z8P2M4K6H8G0F2D4S6A8B1"
name = "ACME 路由器固件逆向"
created_at = "2026-11-08T12:00:00Z"
bookmarks = [{ id = "bm1", doc = "fw", range = { start = 0x12, len = 4 },
               label = "magic", tags = ["hdr", "checked"] }]
processors = { nodes = [{ id = 3, kind = "xor",
    params = { key = { secret_ref = "keyring:geode/proc/acme-xor" }, length = 16 } }],  # ← 引用不是密钥
  edges = [{ from = 1, to = 3 }] }
yara = { ruleset = "rules/acme.yar", rule_dirs = ["rules/"], min_hits = 1 }
assertions = [{ id = "a1", doc = "fw", expr = "$size % 16 == 0", severity = "error" }]
ai = { provenance = [{ at = "2026-11-08T13:10:44Z", provider = "anthropic",
    model = "claude-sonnet-4", task = "generate",     # generate|complete|name|repair
    input_digest = "blake3:5c1e…",        # 采样上下文的摘要，不含原始字节
    output_sha256 = "a71f…", accepted = true,
    human_edits = 34 }] }                 # 接受后改了多少行 → eval 回灌（03 §11）
[[documents]]
doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1"
id = "fw"                                  # 项目内短名，被书签/断言/处理器引用
path = "samples/router_v1.2.bin"           # 相对 geoproj 所在目录
optional = false
note = "从 SPI flash 0x0 读出的完整镜像"
fingerprint = { size = 8388608, head_len = 65536, head_blake3 = "9f2a1c7b5e07…",
                full_blake3 = "1b7d0f3a92c4…" }
[[documents.templates]]
template = "megapack/mips-firmware"
version = "^1.2"                           # 内容库负责解析为具体版本
source = "library"                         # library|project_file|generated
sha256 = "3c9f…"
entry_point = "firmware.bin"
```

**安全策略字段不得进项目文件**：`allow_unsigned`、`ai.privacy_mode`、写入类开关、路径白名单根等一律只存在于 `config.toml`；项目文件里出现即解析失败（`EC001`）。理由：项目文件会被同事发来发去，让它能降低你的安全姿态等于开了一个后门（见 19 §8）。

#### 3.4.1 文档引用与指纹

```rust
// full_blake3 仅当 size <= 512MB 或 config.files.fingerprint="full" 时计算
pub struct Fingerprint { size: u64, head_len: u32, head_blake3: [u8;32], full_blake3: Option<[u8;32]> }
```

**为什么不只存路径**：项目被 clone 到另一台机器、样本被重新下载、样本被同名替换——三者必须被区分。`doc_uuid` 标识"文档实例"（与 journal 交叉引用）、`id` 标识"项目内角色"、`path`+`fingerprint` 定位现实文件，三者不可合并。

#### 3.4.2 `ViewState`

```rust
pub enum ViewKind { Hex, Structure, Inspector, Diff, Disasm, Search, Yara, Processor, Vis(VisKind) }
pub struct ViewState { view_id: ViewId, doc: DocId, kind: ViewKind, cursor: Offset,
    selection: SelState,              // ranges: Vec<ByteRange> + rect
    scroll_first_row: u64,            // 行号，DPI 无关
    columns: ColumnConfig, text_encoding: EncodingId,
    extras: toml::Table }             // 视图私有：结构树展开路径、diff 锚点…
```
**决策：滚动位置存行号不存像素。** DPI 变化或字体切换后像素值全部失效，行号永远有效。

#### 3.4.3 密钥

```rust
pub struct SecretRef { id: String }   // "keyring:geode/ssh/10.0.0.5"
pub trait SecretStore: Send + Sync {
    fn get(&self, id: &str) -> Result<Zeroizing<Vec<u8>>, SecretError>;
    fn set(&self, id: &str, secret: &[u8]) -> Result<(), SecretError>;
    fn delete(&self, id: &str) -> Result<(), SecretError>;
    fn backend(&self) -> SecretBackend;  // Keychain|CredentialManager|SecretService|FileFallback|Memory
}
```
`keyring` crate 覆盖三平台后端（macOS Keychain / Windows Credential Manager / Linux Secret Service）。项目文件里**只允许出现 `SecretRef`**，任何"值"形式的密钥字段均为 bug（§9.4 用自动化测试守住）。

### 3.5 最近文件、收藏、无标题文档

| 对象 | 存储 | 生命周期 |
|---|---|---|
| 最近文件 / 收藏 | `state/recent.toml`（30 条）/ 同文件 `[[favorites]]` | 路径不可达时**不删除**，标灰显示路径；仅用户显式清除才移除 |
| 无标题文档 | `state/scratch/<doc_uuid>/` | **创建瞬间即分配 `doc_uuid` 并具备完整 journal 能力**；关闭时 dirty 则问"保存/丢弃/保留暂存"，干净关闭才清理 |

## 4 核心算法 / 流程

### 4.1 启动顺序（严格有序，任一步失败降级不中断）

```
1 路径 → 2 config.toml+迁移 → 3 SecretStore → 4 recent.toml
5 扫 journals/ 找孤儿 journal（无人持 flock）= 恢复候选 → 6 session.snapshot
7 校 snapshot 内路径可达+指纹 → 8 后台 Job 打开文档（不阻塞 UI）
9 有候选则弹非模态恢复框（prompt="auto" 直接恢复）→ 10 应用布局（缺失 view_id 丢弃该叶子）
```
任一步失败：默认值继续 + 顶部提示条 + 坏文件备份为 `<name>.bad-<ts>`。

### 4.2 路径解析（项目内一律相对）

```
① 相对 geoproj 目录  ← 唯一"可提交"的写法    ② 项目根下 .geode/samples/（团队"样本不入 git"覆盖）
③ config.content.sample_dir  ← 用户全局样本目录 ④ recent.toml 中同指纹的已知副本
```
解析成功但指纹不符 → **不自动绑定**，进入 §4.3。

### 4.3 指纹校验决策表

| 情况 | 行为 |
|---|---|
| 可达 + 指纹全等 | 静默绑定 |
| 可达 + `size` 等但 `head_blake3` 不等 | 拒绝绑定，三选一：重新绑定 / 作为新文档打开 / 忽略（只读） |
| 可达 + `size` 不等 | 同上，额外提示"相差 N 字节，可能是打了 patch 的样本" |
| 不可达 + `optional=true` | 标灰，项目其余部分正常 |
| 不可达 + `optional=false` | 打开项目，依赖它的视图置为不可用 + "定位文件…"入口 |
| `full_blake3` 存在且不等 | 最高置信度失败，禁止任何自动绑定 |

### 4.4 `schema_version` 与迁移

八个版本号**各自独立**（落盘 `schema_version` **只表主版本**，次版本与补丁只记工件 `rev`，见 `23 §3.1`）：

| 文件 | 键名 | 当前值 | 说明 |
|---|---|---|---|
| `config.toml` | `schema_version` | 2 | 用户级配置 |
| `*.geows` | `schema_version` | 1 | 工作区（本地状态，**不承诺跨主版本互读**） |
| `*.geoproj` | `schema_version` | 1 | 项目（团队资产，**承诺前向迁移**） |
| `*.geopkg` | `schema` | 1 | 内容包 manifest；与 `content_api` **不得合并**（`23 §3.4`） |
| `*.geopatch` | `fmt_ver` | 1 | 补丁，二进制大端帧 |
| `*.gmagic` | `schema_version` | 1 | magic 规则库 |
| `*.geojournal` | `fmt_ver` | 1 | 崩溃恢复日志 |
| `*.geolock` | —（无版本） | — | 锁文件靠 `instance_uuid`+`pid`+`host` 三元组识别，不参与迁移 |
| `snapshot` | `schema_version` | 1 | 会话快照（内部文件） |

```rust
pub trait Migration { fn from(&self)->u32; fn to(&self)->u32;          // 纯函数，无 I/O
    fn apply(&self, doc: &mut toml::Value) -> Result<(), MigrateError>; }
pub enum OpenVerdict { Ok, Migrated{from:u32}, TooNew{found:u32,supported:u32}, Corrupt(Diag) }
```
1. **链式应用** v1→v2→v3，每步一个 `Migration` 注册于静态表按 `from` 排序；缺环 = 编译期测试失败（§9.1）。
2. **迁移前备份一次** `<file>.v<old>.bak`，仅留最近一份，14 天后清理。
3. **打开更新版本 → 拒绝**（`TooNew`）：不加载、不写，弹"文件 v4 / 本应用支持 v3"，提供"以原始文本查看"（只读 TOML 视图）。**绝不忽略未知字段后写回**——那是最常见的静默毁数据路径。
4. **未知字段保留**（仅 `config.toml`）：`#[serde(flatten)] extra: toml::Table`，写回原样输出，保证"新版加字段 → 用户回退旧版 → 再升级"不丢设置。
5. **兼容承诺**：同一里程碑内只加字段；删字段/改语义必须 bump 并写迁移。`geows` 不保证跨里程碑互读（本地状态），但 **`geoproj` 保证可跨里程碑前向迁移**（团队资产）。

| 示例迁移（config） | 变更 |
|---|---|
| v1→v2 | `editor.bytes_per_row` → `view.default.bytes_per_row`；`general.theme` → `ui.theme` |
| v2→v3 | 删除明文 `ai.api_key`，转入钥匙串并写 `ai.api_key_ref`，同时发 Warning（**唯一允许副作用的迁移，必须幂等**） |

### 4.5 自动保存节奏（三机制职责不重叠）

| 机制 | 写什么 | 触发 | 目的 |
|---|---|---|---|
| **journal** | `EditOp` 序列 | 空闲 800ms / 累计 64 次编辑 / `Ctrl+S` 前强制 flush | 崩溃不丢改动 |
| **snapshot** | 会话结构（开什么、在哪、布局） | 每 30s / 每 2000 次编辑 / 窗口失焦 | 崩溃复原现场 |
| **项目自动保存** | `*.geoproj` | 书签/断言/图变更后 2s 防抖 + 失焦 | 分析结论不丢 |

**决策：journal 高频、snapshot 低频。** journal 单条 <1KB；snapshot 是整表重写，过频导致写放大。

### 4.6 journal 格式与回放校验（核心）

**只记 `EditOp` 序列，绝不记文件副本**：(a) 100GB 文件无法复制；(b) 与 Buffer 实现解耦，piece table 换 B-tree 不影响；(c) 回放天然可验证。

```
Header(80B): magic "GEOJRN01"(8) | fmt_ver u16 | flags u16 | doc_uuid[16] | … | reserved[7]（字段累加 73B，补齐到 80B，**头无 CRC 是刻意的**，见 `23 §3.7`）
             | base_kind u8 | base_size u64 | head_len u32 | base_head_blake3[32]
Record: kind u8 | seq u64 | payload_len u32 | payload | crc32 u32
  0x01 Overwrite  at u64 | len u64 | data[len]      0x02 Insert  at u64 | len u64 | data[len]
  0x03 Remove     at u64 | len u64                  0x04 Checkpoint  label_len u16 | label
  0x05 SaveMark   new_base_size u64 | new_head_blake3[32]   0x06 FinalLen  len u64
```
**回放四道关卡，每道失败都必须说清失败在哪条记录**：
1. **格式**：magic + `fmt_ver` 匹配，否则 `JournalUnsupported`。
2. **CRC**：逐条校验；遇首条坏记录 → **停止回放、标 partial，已回放前缀仍用于恢复**，绝不整份丢弃。
3. **序号**：`seq` 严格递增；乱序/重复 = 文件被撕裂，同样按"首个异常前"截断。
4. **语义**：`base_*` 必须与磁盘文件指纹一致，否则 journal 属于别的文件版本 → 进入**强制恢复**分支（以 base 重建 Buffer，把当前磁盘内容整体作为一个 `Overwrite` 追加再回放，用户可选丢弃该条）；回放后 len 必须等于末条 `FinalLen`。

`SaveMark` 的意义：保存成功后 journal **不删除而是截断重写**——Header 的 `base_*` 更新为刚写出的指纹，旧记录全部丢弃。这样"崩溃→恢复→保存→再崩溃"不会累积误重放。

`ReplayOutcome = Clean{ops,final_len} | Partial{good_ops,bad_at_seq,reason} | Mismatch{expected,actual}` —— 三分支全都要在恢复对话框里如实呈现，不得把 `Partial` 显示成 `Clean`。

### 4.7 崩溃恢复流程

**异常退出判定**：`instances/<uuid>.json` 残留且其 `pid` 已失效（同 `boot_id`）或 `boot_id` 变化；且对应 journal 存在、无人持 `flock`。

```
找到孤儿 journal → 回放 §4.6 → 非模态对话框（不阻塞启动）：
  "上次异常退出，检测到 3 个未保存的修改"
  每行：文档名 | 编辑条数 | 时间范围 | 大小变化 | Clean/Partial
  可展开预览：首尾各 20 条 EditOp 的人类可读摘要（"0x1040A 覆盖 16 字节 → 41 42 43 …"）
  [全部恢复] [选择恢复] [查看详情…] [丢弃…]
恢复 = 打开 + 重放 EditOp + 标 dirty。失败 → 保留 journal 并弹：
  "无法自动恢复 1 个文档，journal 已保留：…/journals/<uuid>.journal
   [打开目录] [导出为 patch] [仍然尝试强制恢复]"
```
**铁律**：① 绝不自动删 journal，用户点"丢弃"才删且先移入 `journals/.trash/`（留 7 天）；② 强制恢复是独立路径、需二次确认，成功后标签页永久标黄；③ 恢复期间禁止自动保存，直到用户确认过一次。

### 4.8 撤销历史是否持久化

**决策：默认不持久化；崩溃后由 journal 重建"到崩溃点的状态"，但不重建完整撤销栈。**

| 维度 | 持久化撤销栈 | 仅 journal 重建（采纳） |
|---|---|---|
| 磁盘 | 含 piece tree 快照，100 万次编辑 1~4GB | 只存 EditOp，同场景 ~200MB，保存后清零 |
| 内存 | 恢复时全部载入，大文件场景直接 OOM | 回放后只留一份增量 |
| 语义 | "能撤销到崩溃前 50 步"预期混乱 | 撤销栈从恢复点开始，行为可预测 |
| 实现 | 需 piece tree 稳定序列化格式，与 04 强耦合 | 与 Buffer 实现解耦 |

**补充：恢复动作本身可撤销**——回放完成后于撤销栈底压一个 `RecoveredFromJournal(count)` 标记，撤销它即回到未恢复状态。既省磁盘又给后悔药。`editor.persist_undo` 配置项保留但 **M1 不实现**。

### 4.9 并发与锁

三类 advisory 锁（`flock`/`LockFileEx`）：被编辑的数据文件 → 同目录 `.<name>.geolock`；工作区 → `.<name>.geows.lock`（后到者只能"以副本打开"）；journal → `journals/<uuid>.journal`（拿不到 = 他人正在恢复，跳过）。`geolock` 内容：`{instance_uuid, pid, host, boot_id, started_at, app_version}`。

| 场景 | 行为 |
|---|---|
| `flock` 失败（真冲突） | 三选一：**以只读打开**（默认，`caps.writable=false`）/ 强制独占（警告可能覆盖）/ 取消 |
| `flock` 成功但 lock 存在（崩溃残留） | 静默接管并覆盖写 |
| 无锁但外部已改（`mtime`/`size` + 指纹变化） | 只读降级 + "文件已被其它程序修改" + [重新加载]/[保留我的修改] |
| 文件系统不支持 `flock`（网络盘） | 退化为 5s 指纹轮询，状态栏显示"无锁保护" |

**决策：只读降级优先于拒绝打开。** 用户 90% 的场景只是想看一眼。

### 4.10 原子写与导出

```
写 <target>.tmp（同目录，保证同文件系统）→ fsync(file) → rename → fsync(parent_dir)
```
漏掉最后一步在大规模断电下会丢目录项；Windows 上 rename 到已存在文件需 `ReplaceFileW`/`MoveFileEx`，由 `atomic_write` 封装三平台差异。

**导出项目（zip）**：① 遍历 TOML 树，把所有 `secret_ref.id` 替换为 `"REDACTED"`；② 可选剔除 `ai.provenance`；③ 可选内联 `source="project_file"` 的模板；④ **默认不含样本数据**（勾选需逐个确认大小）；⑤ 导出后自检：对产物 grep `api_key|password|BEGIN OPENSSH PRIVATE KEY|sk-`，命中即中止——自动化，不靠人。

## 5 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 全局配置加载 | < 5ms | 冷启动，5KB TOML |
| 工作区恢复（布局+视图态） | < 80ms | 10 文档 / 20 视图 |
| 项目加载 + 校验 | < 15ms | 500 行 TOML、8 个文档引用 |
| 指纹（head64k / full 4GB） | < 2ms / < 6s | full 走后台 Job、可取消、有进度 |
| journal 追加一条 / 批量 fsync | < 0.5ms (p99) / 每 1s 一次、单次 < 10ms | payload ≤ 4KB；内存缓冲 ≤ 4MB 即立即 flush |
| snapshot 写 | < 10ms | 20 视图（不含 buffer 内容） |
| 崩溃恢复回放 / 对话框可交互 | < 800ms / < 1s | 10 万条 EditOp 含 CRC；从进程启动算起 |
| 项目原子写 / 密钥读取 | < 20ms / < 50ms | 密钥异步；Keychain 首次可能弹系统授权 |
| state 目录占用 | < 200MB | 30 天常规使用，超出触发清理 |

**决策：journal 不逐条 `fsync`。** 逐条在机械盘上单条 5~10ms 会卡编辑；批量 1s 的语义是"最多丢 1 秒编辑"，这条必须写进恢复对话框文案（显示"最后保存于 xx:xx:xx"）。

## 6 失败模式与降级

| 失败 | 检测 | 行为 | 用户可见 |
|---|---|---|---|
| **磁盘满致 journal 写失败** | `ENOSPC`/quota | **立即停止自动保存**（不重试、不退避循环），内存缓冲保留，全文档标"未保护" | 持久告警条 + "释放空间/另存到其它盘"；恢复后需手动重启自动保存 |
| snapshot 损坏 | TOML/CRC 失败 | 丢弃该份，尝试 `.snapshot.1`；全坏则只做 journal 级恢复 | 无感（粒度降一级） |
| snapshot 内路径全不可达 | 探测失败 | 打开空工作区，**不覆盖** snapshot | "上次的 3 个文件都找不到，是否重新定位？" |
| journal CRC 失败 | 关卡 2 | 前缀恢复，标 `Partial` | "第 1841 条记录损坏，之前 1840 条可用" |
| journal base 不匹配 | 关卡 4 | 强制恢复分支，需二次确认 | "文件已变化" + 字节级差异摘要 |
| 项目被外部改动 | `mtime`+哈希 | 拒绝自动保存 | 重载 / 覆盖 / 另存副本 |
| 钥匙串不可用 | `backend==Unavailable` | 退化 `FileFallback`（`state/secrets.toml`，0600）或内存 | **首次必须显式确认**并说明"将以明文存于本地" |
| 迁移出错 / 打开更新 schema | `MigrateError` / `TooNew` | 不写、留 `.bak`、只读打开 / 不加载不写 | "从 v1 升级失败：字段 X 类型不符，已备份到 …" / 拒绝 + 升级提示 + 只读原文 |
| 时钟倒退（NTP） | `created_ms > now` | 一律用 `seq` 排序，时间仅显示 | 无感 |
| scratch 不可写 / 目录只读致无锁 | 创建/打开失败 | 禁止新建无标题文档 / 无锁 + 只读 | "暂存目录不可写" / 状态栏"无锁保护（只读）" |

**统一原则**：走 00 §9.3——子功能挂了其它必须继续工作；提示条可关闭但**不可静默**（`ENOSPC` 这类必须常驻）。

## 7 与其它子系统的契约

| 子系统 | 读 | 写 | 接口 |
|---|---|---|---|
| `geode-buffer` | `EditOp` 流、`version()` | 无 | 每次编辑把 `EditOp` 推给 `JournalSink` trait，本模块实现它 |
| `geode-provider` | `ProviderInfo`/`Caps::writable`、指纹 | 设置只读标志 | 打开时由本模块决定 `open_mode` |
| `geode-ui` | `ViewState`、布局树 | 同左 | serde 在 ui 侧，本模块用 `toml::Value` 中转 → 不依赖 ui |
| `geode-bspec` / `geode-content` | `TemplateRef`(id+版本+哈希)/内容源配置 | `templates[]` | 只存引用，`version="^1.2"` 由内容库解析；`source="project_file"` 才存源码 |
| `geode-proc` | 节点图序列化形态 | `[processors]` | 图结构由 `geode-proc` 定义 `Serialize`；密钥替换为 `SecretRef` |
| `geode-ai` | `AiProvenance` | `[[ai.provenance]]` | **只记元数据，不记 prompt 与原始字节** |
| `geode-search` / `app` | 最近查询 / 全部 | `search_history` / 全部 | 本模块不自起线程；flush 由 app 定时器驱动 |

**关键契约：`geode-project` 永不持有 `ByteBuffer` 的可变引用**，只消费 `EditOp` 流。这保证"换 Buffer 实现不动持久化层"，也是 journal 能独立测试的前提。

## 8 配置项与默认值

| 键 | 默认 | 范围 / 影响 |
|---|---|---|
| `project.autosave` / `.autosave_debounce_ms` | `true` / `2000` | 200~30000，书签变更的合并窗口 |
| `autosave.journal_idle_ms` / `.journal_max_edits` | `800` / `64` | 100~10000 / 1~10000 |
| `autosave.journal_fsync_ms` | `1000` | `0`=逐条 fsync（慢，给偏执用户） |
| `autosave.snapshot_secs` / `.snapshot_edits` | `30` / `2000` | 5~600 / 100~1e6 |
| `recovery.prompt` / `.keep_days` / `.trash_days` | `"ask"` / `14` / `7` | `auto` 直接恢复但仍发可撤销通知 |
| `files.fingerprint` / `.head_len` | `"head64k"` / `65536` | `full` 更准但 100GB 要跑 6s；4096~1048576 |
| `files.reopen_last_workspace` / `.recent_max` / `.verify_on_open` | `true` / `30` / `"prompt"` | 0~200；`strict`/`prompt`/`none` |
| `files.lock_mode` | `"exclusive"` | `exclusive`/`readonly-first`/`off` |
| `files.atomic_write_fsync_dir` | `true` | 关掉省时间但断电会丢目录项 |
| `state.cleanup_on_start` | `true` | 清理过期 journal / cache |
| `keyring.backend` / `.confirm_file_fallback` | `"auto"` / `true` | `auto`/`file`/`memory` |
| `export.redact_secrets` / `.include_templates` / `.include_ai_provenance` | `true` ×3 | 第一项**不建议关** |
| `telemetry.enabled` | `false` | **默认零遥测**。关闭时程序不得发起任何非用户触发的网络请求 |
| `telemetry.crash_reports` | `false` | 崩溃转储默认只落本地 `state/crash/`，不上传；上传必须逐次询问并先展示将发送的字段清单（绝不含文件内容） |
| `update.check` / `.interval_h` | `"manual"` / `24` | `manual`/`startup`/`off`。开启时只发当前版本 + 平台标识，不含路径与文件内容 |

**关于遥测的立场（一句话写死）**：这是一个会被用来翻看别人隐私数据的工具，所以**默认零遥测、零自动上传**。崩溃转储只写本地磁盘并进入"恢复对话框"流程（§4.7），是否上报由用户在对话框里逐次选择；任何网络行为都必须在 UI 上有可见指示。

环境变量：`GEODE_CONFIG_DIR`/`GEODE_STATE_DIR`/`GEODE_CACHE_DIR`（便携版、CI）、`GEODE_NO_KEYRING=1`（CI 免弹授权）、`GEODE_DISABLE_AUTOSAVE=1`（fuzz/基准）、`GEODE_JOURNAL_DIR`（崩溃测试）、`GEODE_SCHEMA_STRICT=1`（迁移 Warning 即失败）。

## 9 测试策略

| 层 | 数量目标 | 工具 | 覆盖 |
|---|---|---|---|
| 单元 | ≥ 180 | `#[test]` | 路径解析、指纹、`ViewState` 往返、锁状态机、配置默认值 |
| schema 迁移 | ≥ 40（每跳 ≥8） | 快照对比 | 每条迁移链 + 损坏/缺字段/类型错 |
| journal 回放 | ≥ 60 + 3 property | `proptest` | §9.2 |
| 崩溃模拟 | 12 场景 × 50 轮 | 子进程 + `SIGKILL` | §9.3 |
| 集成 | ≥ 25 | e2e | 打开→编辑→崩溃→恢复→保存→重开→字节比对 |
| 安全 / 基准 | ≥ 15 / ≥ 12 | grep + 断言 / `criterion` | §9.4 / §5 全表 |

### 9.1 schema 迁移测试

对每种 `kind` 的 `testdata/schemas/<kind>/v*.toml`：① 用最新版反序列化 → 断言成功且值与黄金文件一致；② 序列化回来再反序列化 → 幂等；③ 对高于当前版本的样本断言 `TooNew` 且**文件字节未被修改**；④ 对损坏样本断言 `Corrupt` + 有可读诊断 + 除 `.bak` 外无写入。CI 开 `GEODE_SCHEMA_STRICT=1`，Warning 即红灯——否则 Warning 会无声积累成事故。

### 9.2 journal 回放 property test（"数据不丢"的唯一护栏，不允许绕过）

- **roundtrip**：随机初始字节流（0~1MB）+ 随机 `EditOp` 序列（1~5000 条），分别应用到朴素 `Vec<u8>` 参照实现与"编码→解码→回放"路径，断言字节相等。
- **truncation**：在任意位置截断（含记录中间）→ 必须返回 `Clean` 或 `Partial`，**绝不 panic**，且已回放前缀与参照在同前缀下一致。
- **prefix_recovery**：随机翻转 1 字节 → 返回 `Partial` 且 `good_ops < total`，`good_ops` 之前的字节仍与参照一致。

### 9.3 崩溃模拟

时机矩阵：打开后立即 / 编辑 1 条 / 编辑 1 万条 / flush 前 / flush 中 / `fsync` 中 / snapshot 写中 / 保存中 / rename 前 / 恢复中；方式：`SIGKILL` / `abort()` / 模拟 `ENOSPC` / 模拟 `EIO` / tmpfs 截断。判定：① 退出后 state 结构可解析；② 重启给出明确结论（Clean/Partial/Mismatch）且不崩不挂；③ **已通过 fsync 的编辑 100% 可恢复**；④ 源文件字节与崩溃前一致（未保存改动不得污染原文件）。实现：`tests/crash.rs` 启子进程，`GEODE_CRASH_AT=<probe>` 钩子仅在 `feature="crash-test"` 下编译，父进程随机延迟 `SIGKILL`。

### 9.4 安全测试

含 `secret_ref` 的项目执行导出 → 产物字节级 grep（`api_key`/`password`/`sk-`/密钥 id 明文）必须 0 命中；`GEODE_NO_KEYRING=1` 下 `state/secrets.toml` 权限必须 `0600`；日志中 `Zeroizing<Vec<u8>>` 的自定义 `Debug` 实现不得输出明文（有断言测试）。

## 10 开放问题

| # | 问题 | 现状 / 倾向 | 决策时限 |
|---|---|---|---|
| Q1 | `geows` 是否允许绝对路径？ | 现允许（本地状态），但因此无法"把工作区发给同事" | M1 末 |
| Q2 | 是否支持"多文档联合解析"（一模板跨多份样本）？ | 现为每文档独立，影响 `documents[]` 语义 | M3 |
| Q3 | journal 是否 zstd 压缩？ | 密集编辑可达 200MB；省盘但增加 CPU 与"部分损坏"复杂度 | M1 后按实测 |
| Q4 | 大 patch（>1MB）如何进项目？ | 现决定外置引用 + `.gitignore` 建议 | M4 |
| Q5 | 要不要项目级模板 lockfile？ | `version="^1.2"` 在团队内容库不同步时会跑出不同结果，破坏"可复现" | M3 |
| Q6 | `secret_ref` 是否机器限定命名空间？ | 同一 id 在两台机器会读到不同/不存在的密钥 | M6 |
| Q7 | Linux 无 Secret Service（headless）默认行为？ | 现退化文件 + 二次确认；或改为"禁用 AI 功能"更安全 | M5 |
| Q8 | 恢复预览是否渲染成 hex diff？ | 现只给文本摘要；hex diff 需 diff 引擎 | M4 后回填 |
