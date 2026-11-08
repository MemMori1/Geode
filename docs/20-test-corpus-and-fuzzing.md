# 20 — 测试语料库与模糊测试计划

> **本文拥有**：`tests/corpus/`（可回归语料）、`fuzz/fuzz_targets/`（模糊目标）、崩溃 triage 流程、`cargo-mutants` 适用范围、语料生成器。**不拥有**：各 crate 的模块级单元测试（归各文档 §9）、性能判定（归 21）、威胁建模本身（归 19，本文只消费其 TH 编号）。
> 一句话：**本文回答"每个不可信输入入口用什么字节流去打、期望什么反应、崩溃之后怎么归档成永久回归"。**

## 0 决策速览

| # | 决策 | 理由 / 代价 |
|---|---|---|
| 1 | 语料按 `tests/corpus/<kind>/<name>/` 组织，**每样本强制 `meta.toml`**，`expect` 是唯一真相 | 没有期望的二进制无法回归，只能占空间。代价：新样本多写 12 行 TOML |
| 2 | **合成优先**，入库字节必须能被 `scripts/gen/` 逐字节重现（CI 用 sha256 断言） | 让"真样本许可失效"不再是删库风险。代价：要写 18 个生成器 |
| 3 | **25 个 fuzz target**，全部 `cargo-fuzz`+libFuzzer；OSS-Fuzz 只承接 19 个纯 CPU、无 C 依赖的 | 有 C 依赖的（`geode-disasm`/`geode-yara`）在 OSS-Fuzz 上无法自定义 sanitizer |
| 4 | 每 target 有硬预算（wall + RSS），**超预算算 P0「挂死」而不是超时** | `E2001 执行预算耗尽`（02 §7.2）只有能返回时才是降级；不返回就是死锁 |
| 5 | triage 用**栈哈希 + 首 8 帧**去重；最小化后**必须**转成回归语料才允许关 issue | 关了 issue 不留样本 = 半年后同一个 bug 再来一遍，且第二次没人认得 |
| 6 | PR 只跑 ≤2 分钟 smoke，深度靠夜间 30 分钟；覆盖率只做趋势不做门槛 | 覆盖率天花板由语料决定，不由 CI 时长决定（18 §9 已定原则，本文给实现） |

## 1 目标与非目标

### 1.1 目标

| # | 目标 | 机器可验证形式 |
|---|---|---|
| G1 | 每个"处理不可信字节"的入口都有可回归语料，且**只增不减** | `xtask corpus audit`：每 target ≥3 `accept` + ≥3 `reject`；`xtask corpus stat` 计数下降即红 |
| G2 | 任意 crash 能在 5 分钟内从 issue 走到本地复现 | 归档目录必含 `repro.sh`，`cargo xtask repro <dir>` 一条命令跑通 |
| G3 | 25 target 在 30 分钟夜间预算内全跑到，且**挂死可检出** | 用 target 内部 deadline 判定，不依赖 libFuzzer `-timeout` |
| G4 | 语料可再分发：仓库内不含无法溯源的字节 | `origin` + sha256 + 生成器重放三方一致 |
| G5 | 跨模块语义不变量用 proptest 常驻（§4.5 的 14 条） | 每条一个具名 `#[test]`，PR 必跑 |

### 1.2 非目标

- **不追求覆盖率数字**。行/分支覆盖写 jsonl 做趋势（§9.5），不做门槛——门槛会诱导无意义断言（18 §9 同判）。
- **不做 GUI 自动化**。点击、拖拽、像素比对归 `egui_kittest` 快照（见 16 §9）。
- **不做性能判定**。"退化 >5% 红灯"归 21；本文只保证 fuzz 的 time budget 不掩盖性能病；也**不追求最优最小化**（`tmin` 收敛到 ≤4 KiB 即可定位）。
- **不做渗透测试、不 fuzz 正确性**。威胁建模归 19（本文只审"每条 TH 至少一个样本"）；黄金文件与单元测试负责"对不对"，fuzz 负责"崩不崩、挂不挂、越不越界"。

### 1.3 与既有测试层的分工

一句话：各文档 §9 拥有**模块内**断言（`ByteRange::clamp` 不 panic、piece table 对拍 `Vec<u8>`、`apply(ops,left)==right`），本文拥有**跨模块语料与 fuzz**，是 18 §9 那张汇总表的权威来源（18 只写"6 targets"，本文写 25 个）；同一份语料被单元测试、fuzz 种子、黄金文件输入三处复用，语义必须一致——详见 §4.8。

## 2 依赖与边界

### 2.1 上下游

- **被 fuzz 的 crate**：`geode-bspec`、`geode-buffer`、`geode-provider`、`geode-analysis`、`geode-viz`、`geode-search`、`geode-diff`、`geode-content`、`geode-project`、`geode-annot`、`geode-yara`、`geode-ui`（仅 `theme::parse`）。
- **工具链**：`cargo-fuzz`、`proptest`、`cargo-mutants`、`cargo-llvm-cov`、`xtask` 的 `corpus`/`repro` 子命令。
- **不依赖**：网络、真实时钟、用户 home 目录（18 §9 禁止项）。样本生成必须是 `seed → bytes` 的纯函数。

### 2.2 一处必须做的架构让步

`theme_load` 是唯一"靠 GUI crate"的 target。为让它能在无显示环境跑，**`geode-ui` 必须把主题解析拆成 `theme::parse::<T>(&str) -> Result<Theme, Vec<Diag>>` 纯函数模块，不引入任何 egui/wgpu 类型**（对齐 16 §4 的 token 表）。这不是为 fuzz 让步：`00 §2` 说"除 `geode-ui`/`app` 外不得依赖 GUI"，反过来说 GUI crate 内部也该有可独立测试的纯逻辑核。

### 2.3 目录边界（谁在 git 里）

| 路径 | 内容 | 入 git | 说明 |
|---|---|---|---|
| `tests/corpus/<kind>/<name>/{sample.*, meta.toml}` | 已判定样本 | 是 | 单样本 >512 KiB 走 §10 Q3 |
| `tests/corpus/<kind>/_gen/{README.md, snapshot-<seed>.sha256}` | 生成说明与种子快照 | 是 | 证明"可重现" |
| `scripts/gen/<kind>.{py,rs}` | 生成器 | 是 | 纯函数、禁时间/随机源 |
| `testdata/bad/<target>/<yyyymmdd>-<hash>/` | 未处理 crash 落地 | 否 | 与 18 §6「崩溃输入留存 `testdata/bad/`」一致 |
| `fuzz/corpus/<target>/`、`fuzz/seeds/<target>/` | libFuzzer 自增长语料 / 由 `xtask corpus fuse-seeds` 从语料硬链接出的种子 | 否 / 生成 | 自增长语料只作种子来源，不回灌仓库 |

### 2.4 四条不可协商

1. 样本**不得**是用户文件；`origin = "redistributable"` 必须同时给 `source_url` + SPDX 许可 + sha256。
2. target **不得**用 `catch_unwind` 吞 panic（`panic=abort` 下吞不掉，18 §8 profile），也**不得**读写外部状态（`geopkg_unpack`/`journal_recover` 用 `tempfile` 在进程内完成）。
3. 样本必须有可判定 `expect`；`unknown` 只允许存活 30 天，超期 `audit` 报错阻塞 PR。
4. 语料不得含真实凭据、用户路径、主机名（`sftp_path_norm` 出现 `user@host` 视为缺陷，见 06 §4.12）。

## 3 数据结构与接口

### 3.1 目录布局

```
tests/corpus/<kind>/<name>/{sample.bin, meta.toml}    kinds（= fuzz target 前缀）：
ihex  srec  bspec  gmagic  geopkg  geopatch  journal  mesh3d  theme  diff  regex
sftp-path  paste  provider  yara  image/{bmp,png,jpeg,gif,qoi,tga,webp}
```

每类的代表样本（完整清单由 `xtask corpus list` 生成，不手写死）：

| kind | 代表样本名 |
|---|---|
| `ihex` | `empty-file`、`one-byte`、`header-only`、`bad-checksum-mid`、`len-vs-data-mismatch`、`addr-overflow-type04`、`missing-eof`、`data-after-eof` |
| `srec` | `count-mismatch`、`addr-width-mixed`、`non-hex-char`、`crlf-mixed` |
| `image` | `png-idat-huge`、`png-ihdr-zero-width`、`bmp-negative-height`、`gif-1000-frames`、`qoi-size-mismatch`、`tga-negative-stride`、`webp-truncated`、`jpeg-sof-nonfinite` |
| `geopkg` | `slip-dotdot`、`slip-absolute`、`slip-drive`、`slip-symlink`、`slip-nul`、`bomb-1g-zeros`、`bomb-10k-entries`、`bomb-200to1` |
| `geopatch` / `journal` | `ok-small`、`offset-u64-max`、`hole-mid`、`split-0xffff`、`cbor-len-mismatch` / `crc-bad-mid`、`final-len-mismatch`、`trunc-in-record` |
| `bspec` / `gmagic` / `theme` | `deep-nest-256`、`infinite-loop`、`bad-utf8` / `loop-jump`、`over-64-instr`、`mask-len-mismatch` / `missing-token`、`recursive-include`、`keys-1e6` |
| 其余 | `diff/{empty-empty,one-byte,identical-64k,self}`、`regex/{nested-quantifier,literal-1mb,bad-syntax}`、`sftp-path/{dotdot,drive,nul,deep-1024}`、`paste/{hex-with-spaces,base64,utf16-bom,binary}`、`provider/{rsp-malformed,sftp-unknown-status}`、`mesh3d/{nan-vertex,face-index-oob}`、`yara/{nested-quantifier}` |

**命名规则**：`<kind>/<kebab-case-问题描述>`，描述必须能看出**坏在哪**。`bad-checksum-mid` 合格，`test3` 不合格（`audit` 对纯数字后缀报警告）。

### 3.2 `meta.toml` schema（v1）

```toml
schema    = 1
name      = "ihex/bad-checksum-mid"
kind      = "ihex"                    # 必须等于父目录名
size      = 1284
sha256    = "9f2c…"
origin    = "synthetic"               # synthetic | redistributable（另需 source_url）| public-domain
generator = "scripts/gen/ihex.py --seed 7 --at 3 --variant bad-checksum"
license   = "CC0-1.0"                 # SPDX；synthetic 也必填（生成器脚本的许可）
features  = ["ihex:checksum", "edge:truncated"]
threats   = ["TH-06-03"]              # 见 19 §4
targets   = ["ihex_parse"]            # 名字必须在 §3.4 里
keep      = "regress"                 # regress（PR 必跑）| corpus-only（只喂 fuzzer）
expect    = { kind = "reject", code = "E6005", range = [1024, 1284] }
notes     = ["offset 0x200 的第 4 条记录 XOR 校验被翻转一个 bit"]
```

`expect.kind` 五种语义（穷举，扩展须改本文）：

| kind | 含义 | 必附字段 | 违规即 |
|---|---|---|---|
| `accept` | 成功返回 | `structure_hash`（可选） | 返回 `Err` = 回归 |
| `reject` | 必须返回错误 | `code`（须在 00 §12 登记）+ `range` | 成功返回 = 回归 |
| `partial` | 部分成功 + `Diag` | `code` + `min_units` | 全失败 = 降级过度 |
| `budget` | 必须返回 `E2001`，**不得挂死** | `max_ms` | 挂死 = P0 |
| `unknown` | 待定 | — | 30 天后 audit 报错 |

`xtask corpus check` 逐条断言：sha256 & size 匹配；`code` 落在 00 §12 的 E0xxx~EAxxx 段内；`targets` ⊆ §3.4；`threats` ⊆ 19；`origin=redistributable` 时三字段非空；`generator` 重放后字节相等。

### 3.3 生成器接口

```rust
// xtask/src/corpus.rs
pub trait SampleGen: Send + Sync {
    fn kind(&self) -> &'static str;
    fn gen(&self, seed: u64, variant: &str) -> Vec<u8>;   // 纯函数，逐字节确定
    fn variants(&self) -> &'static [&'static str];
    fn parse_cmdline(&self, cmd: &str) -> Result<(u64, String)>;
}
pub fn materialize(kind: &str, out: &Path) -> Result<usize, GenError>;  // 重放并写盘
pub fn audit(root: &Path) -> AuditReport;   // target → 样本数 / 过期 unknown / TH 覆盖
```

`AuditReport` 五个列表直接被 CI 消费：`missing_targets`、`thin_targets`、`stale_unknown`、`uncovered_threats`、`nonreproducible`，任一非空即红。

### 3.4 fuzz target 清单（25 个，权威）

`wall` 是单次迭代的**判定**预算（target 内部 deadline，超时算挂死），`RSS` 是增量上限。`本地+OSS` = 同时进本地夜间与 OSS-Fuzz；`本地` = 因 C 依赖或特殊构建只在自管 runner 跑。

| # | target | 输入类型 | crate | wall / RSS | 位置 | 崩溃期望 |
|---|---|---|---|---|---|---|
| 1 | `bspec_compile` | `.bspec` UTF-8 源码（≤64 KiB） | `geode-bspec` | 500ms / 256MiB | 本地+OSS | 编译期 panic = bug；拒绝必带 `E4xxx` |
| 2 | `bspec_vm_run` | 编译产物 + ≤4 KiB 数据段 | `geode-bspec` | 150ms / 512MiB | 本地+OSS | **必须返回**（结果或 `E2001`/`E3xxx`）；未返回 = P0 挂死 |
| 3 | `ihex_parse` | Intel HEX 文本（≤1 MiB） | `geode-provider` | 200ms / 64MiB | 本地+OSS | panic = bug；坏记录 → `E6005`（06 §3.2） |
| 4 | `srec_parse` | Motorola SREC 文本 | `geode-provider` | 200ms / 64MiB | 本地+OSS | 同上 |
| 5 | `paste_sniff` | 任意字节（模拟粘贴框） | `geode-analysis` | 100ms / 64MiB | 本地+OSS | 绝不 panic；无法识别 → `None`；禁 O(n²) |
| 6 | `gmagic_parse` | `.gmagic` TOML | `geode-analysis` | 200ms / 64MiB | 本地 | 语法错 → `Diag`；超 64 指令/规则拒绝（10 §4.5） |
| 7 | `gmagic_engine` | 已编译规则 + 512 B 输入 | `geode-analysis` | 100ms / 64MiB | 本地+OSS | 每规则 ≤64 指令；循环 jump 编译期拒绝 |
| 8 | `geopkg_unpack` | `.geopkg`（zip 字节） | `geode-content` | 500ms / 256MiB | 本地 | 穿越 → `EA004`、炸弹 → `EA005`，判定 <200ms（15 §9） |
| 9 | `viz_image_bmp` | BMP（≤64 MiB） | `geode-viz` | 200ms / 增量 ≤8MiB | 本地+OSS | 声明尺寸超限 → `E8004`，**不得先分配**（13 §4.4） |
| 10 | `viz_image_png` | PNG | `geode-viz` | 300ms / ≤8MiB | 本地+OSS | 同上 |
| 11 | `viz_image_jpeg` | JPEG | `geode-viz` | 300ms / ≤8MiB | 本地+OSS | 同上 |
| 12 | `viz_image_gif` | GIF（多帧） | `geode-viz` | 300ms / ≤8MiB | 本地+OSS | 帧数 >256 只解首帧，不得解全帧 |
| 13 | `viz_image_qoi` | QOI | `geode-viz` | 200ms / ≤8MiB | 本地+OSS | 声明尺寸与数据不符 → `E8004` |
| 14 | `viz_image_tga` | TGA | `geode-viz` | 200ms / ≤8MiB | 本地+OSS | 负行跨度必须被处理而非 panic |
| 15 | `viz_image_webp` | WebP（含动画） | `geode-viz` | 300ms / ≤8MiB | 本地+OSS | 截断 → 部分图像 + 横幅（13 §4） |
| 16 | `viz_mesh3d` | OBJ/PLY 文本 + 二进制 | `geode-viz` | 300ms / 128MiB | 本地 | 顶点/面数超限 → `E8002`；NaN 坐标不得进产物 |
| 17 | `geopatch_apply` | CBOR `.geopatch` + 基础 buffer | `geode-buffer` | 200ms / 64MiB | 本地+OSS | 越界 patch → `Err`；**原 buffer 不得被部分修改**（04 §4.5） |
| 18 | `journal_recover` | `.geojournal` 追加流 | `geode-project` | 300ms / 128MiB | 本地+OSS | 坏记录 → 停止并标 `partial`（17 §4.6）；**绝不删文件** |
| 19 | `diff_pair` | 长度前缀的两个 ≤64 KiB 片段 | `geode-diff` | 500ms / 128MiB | 本地+OSS | 必须满足 `apply(ops,left)==right`（09 §9.2） |
| 20 | `regex_wrap` | 正则源码 + 32 KiB haystack | `geode-search` | 300ms / 64MiB | 本地+OSS | 编译失败 → `E7103`；无界模式不得超预算 |
| 21 | `sftp_path_norm` | 任意字节（当作远端路径） | `geode-provider` | 50ms / 8MiB | 本地 | **绝不能**产出逃出 root 的 `PathBuf`（`..`/绝对/盘符/`\0`） |
| 22 | `theme_load` | TOML 主题 | `geode-ui`（`theme::parse`） | 100ms / 32MiB | 本地 | token 缺失 → `E0001`；未知键不得崩（16 §4） |
| 23 | `provider_protocol` | RSP/SFTP 响应字节流 | `geode-provider` | 200ms / 64MiB | 本地 | 畸形响应 → `E6004`；不得无限等待（06 §4.6/§4.7） |
| 24 | `annot_index` | 操作序列（insert/remove/query 编码） | `geode-annot` | 200ms / 64MiB | 本地 | `query` 结果 ⊆ 已插入集合；不得退化为 O(n)（00 §5.3） |
| 25 | `yara_rule` | `.yar` 文本 | `geode-yara` | 500ms / 256MiB | 本地（C 依赖） | 病态正则 → 编译护栏拒绝，编译 <10s（12 §4.3） |

### 3.5 崩溃记录结构

```rust
pub struct CrashRec {
    pub target: &'static str,
    pub stack_hash: u64,          // §4.6 定义
    pub top_hash: u64,            // 首帧符号哈希，用于同函数不同路径聚类
    pub first_seen: Timestamp,    // 用于 30 天 TTL 与周报
    pub input: PathBuf,           // testdata/bad/... 原始输入；minimized 为其同目录产物
    pub class: CrashClass,        // Panic | Abort | Oom | Hang | Sanitizer | Assert
    pub build_id: String,         // blake3(二进制) 前 16 hex，见 21 §4.2
    pub budget: Budget,           // 产生该 crash 时的实际预算（可复现性）
}
```

## 4 核心算法流程

### 4.1 语料来源三原则

**P1 合成优先。** 能给生成器的绝不放二进制。三条硬账：① 二进制在 git 里不可 review、不可 diff；② 许可失效时无法重建；③ 生成器本身就是该格式的一份可执行规格，写它的时候必然发现解析器的错误假设。

**P2 真样本必须可再分发。** 必须同时具备 SPDX 许可、`source_url`、sha256，缺一拒收。合格来源：公共领域语料、开源项目自带的测试夹具、我们自己用开源工具生成再回灌的产物。

**P3 禁止把用户文件提进仓库。** 包括"脱敏过的"——脱敏脚本漏一个字段就是隐私事故。用户现场问题一律走"生成器复刻"：拿到现象描述 + 字节区间 + 字段值，造一个等价坏样本。造不出来的，说明我们对格式的理解还不完整，那本身就是设计缺陷，应先补生成器。

### 4.2 各格式的合成策略

| kind | 生成方式 | 必须覆盖的变体 |
|---|---|---|
| `ihex` | 按记录结构拼 `:`+`len`+`addr`+`type`+`data`+`checksum` | 校验和错（首/中/末条）、`len` 与 `data` 长度不符、type 04 后地址回绕、跨 64 KiB 段、缺 EOF、EOF 后有数据、单字节、0 字节 |
| `srec` | S0/S1/S2/S3/S5/S7/S8/S9 各一条 + 计数错 | 计数字节错、地址宽度混用（S1 与 S3 混排）、非十六进制字符、CRLF/CR/LF 混排 |
| `image` | 手写最小合法码流（每格式 <200 B 的"最小可解"），再改字段 | 接管 13 §9 的解码炸弹库：PNG 声明 60000×60000、GIF 1000 帧、BMP 负高负宽、PNG 超大 IDAT、QOI 尺寸不符、TGA 负行跨度、WebP 截断、JPEG SOF 非有限值 |
| `gmagic` | 从 10 §4.5 模板出发做参数变异 | 循环 jump（a→b→a）、>64 指令、`mask` 长度与 `value` 不符、`jump` 越界、`required=false` 全缺 |
| `geopkg` | 直接构造 zip 中央目录与本地头 | 15 §9 的五种 zip slip + 三种炸弹（1 GB 零字节、10 000 条目、200:1）+ 8 KB 文件名 + `\0` 注入 |
| `geopatch` | 从 04 的 `PatchSet` 导出产物出发变异 | 16 MB 边界、`0xFFFF` 拆分边界、offset=`u64::MAX`、空洞区内 patch、CBOR 明确长度与实际不符 |
| `journal` | 从 17 §4.6 的 `EditOp` 生成器造，再按变体破坏 | CRC 坏（首条/中间）、`FinalLen` 不符、`base_*` 指纹不符、记录中间截断、`SaveMark` 后仍有旧记录 |
| `bspec` | 语法驱动生成（从 02 的 EBNF 反推）+ 种子变异 | 256 层嵌套（= `depth` 上限，02 §7.2）、超大常量、除零、无退出循环、越界读、超长标识符、非法 UTF-8 |
| `theme` | TOML 模板 | 缺 token、类型错（`#FFF` 写成 true）、递归 `include`、10^6 键、未知键、超长值 |
| `diff`/`regex`/`sftp-path` | 直写 + 变异 | 空 vs 空、1 B vs 0、自身短路 / `(a+)+$`、`(?s)a.*b` 无界、1 MB 字面量 / `../`、`C:\`、`\0`、1024 段、`U+2215` 归一化陷阱 |
| `paste`/`provider`/`yara` | 直写 + 构造协议流 | 带空格 hex、ASCII art、base64 padding 错、UTF-16 BOM / RSP 分包在 `$` 中间、`$qXfer` 超长、SFTP 未知状态码 / 病态正则 |

### 4.3 六类边界样本矩阵（本文核心表）

每类都要对**所有相关子系统**测一遍，并在 `meta.toml` 的 `features` 里打对应 tag（`edge:zero`/`one`/`truncated`/`constant`/`huge`/`contradictory`），`audit` 断言每格非空。每类至少 1 个样本 `keep = "regress"`。

| 类别 | 应测子系统 | 明确期望行为（可断言） |
|---|---|---|
| **0 字节** | 打开（06）、hex 视图（05）、直方图/熵（10）、magic（10）、搜索（08）、diff（09）、BinSpec（02）、可视化（13）、保存（04） | 必须能打开，`len()==0`，UI 空态 + "文件为空"提示，**不 panic**；熵 `==0.0` 且 `is_finite()`（不是 NaN：`-0·log0` 按 0 定义）；直方图 256 桶全 0 且 `Σ==0`，百分比走 `if total==0 { 0.0 }` 而非除法；magic 返回 `Unknown` 而非"最像的那个"；搜索立即 0 命中（<1ms）；`diff(空,空)` 短路 <400ms（09 §5）；BinSpec 空输入 → `E3001` 或正常结束，不得 `unwrap` 空切片；保存成功且生成 0 字节文件 |
| **1 字节** | hex 视图（05）、Inspector（10）、搜索（08）、数值搜索（08 §5）、图片解码（13）、diff（09） | hex 显示 1 个 cell + 其余占位（不得越界画第 2 个）；Inspector 在 `off=0` 读 8 B → **短读** + `E3001`，不是 panic；`width>1` 的数值搜索长度不足走 `E7105`（08 §5 两条路径都覆盖）；搜索 1 B 模式命中 ≤1；图片 → `E8001`（输入不足）或 `E8004`；`diff(1B,0B)` → 1 个 Insert |
| **截断** | 所有解析器、zip、图片、journal、BinSpec、geopatch | 统一规则：**尽力返回已解析前缀 + `Diag`（`range` 指向截断点）**，绝不返回"成功"。图片 → 部分图像 + 横幅（13 §4.4）；journal → `partial` 恢复、已回放前缀可用（17 §4.6 第 2 关）；zip → `EA006`/`EA007`；geopatch → `Err` 且 buffer 未被部分修改；BinSpec → `E3001` 且 `recoverable=true` |
| **全 0 / 全 FF** | 熵/直方图（10）、magic（10）、搜索通配（08 §4.2）、diff（09）、可视化（13） | 熵 = 0（全 0）或 ≈0（全 FF 单桶）；直方图单桶 = 总量；全 0 **不得**被判为 PE/ELF（内置规则必须要求非零证据位）；`?? ??` 全通配 → `E7102 模式无信息量`，不得产生 10 亿命中；1 GB 全零上搜 `00 00 00 00` 必须被命中上限截断而不是 OOM；可视化器对全 0 图像不得除零 |
| **超大与稀疏** | 打开（06、04）、空洞（06）、可视化预算（13）、Offset 算术（00 §4） | 100 GB 稀疏镜像打开 <200ms（00 §7）且不整读；空洞返回 `ReadReport.holes`（**不是 Err**，06 §3.1 铁律 3）；`ByteRange::clamp` 越界自动裁剪；声明长度 > 实际长度 → 裁剪 + `Diag`（`recoverable=false`）；可视化 `bytes_hint` 超限 → `E8004` **在分配前**；`Offset` 加法全走 `checked_add`，溢出 → `E3001`；`u64::MAX` 长度不得进任何 `Vec::with_capacity` |
| **结构损坏自相矛盾** | PE/ELF 结构（BinSpec 模板侧）、PNG IHDR、zip 中央目录、journal、BinSpec 循环引用 | "语法合法但语义不可能"必须产生 `Diag` 并保留可解析部分；**绝不静默修正**（静默修正让用户以为文件没问题，这是 C2 的对立面）；证据充分 → `Error`+`recoverable=false`，证据不足 → `Warning`+`recoverable=true`；BinSpec 结构互引必须在编译期以 `E4007` 拒绝，不得运行期栈溢出 |

**为什么这六类值钱**：它们是"边界值 + 不可能态"的完备划分——长度（0/1/截断）、内容（常量/极值）、规模（超大）、语义（矛盾）。任何新解析器过完这六类，就消灭了历史上 80% 的 panic 类 bug。

### 4.4 预算语义："必须返回"如何落地

```
fn run_one(data: &[u8]) {
    let budget   = Budget::for_fuzz();   // steps=2_000_000, wall=100ms, depth=256
    let deadline = Instant::now() + budget.wall_time + GRACE;        // GRACE = 200ms
    let ctx      = JobCtx::new(CancelToken::new(), budget);
    // 未在 deadline 内返回 → abort 让 libFuzzer 记为 crash；
    // 归档时归类为 Hang（产品缺陷），不是 timeout（fuzzer 参数问题）
    if run_with_deadline(&ctx, || compile_and_run(data)).is_none() { std::process::abort(); }
}
```

三条推论：① libFuzzer 的 `-timeout=25` 只是最后防线，真正的判定是我们自己的 `deadline`——`-timeout` 会把"慢但线性"和"死循环"混为一谈，这是两个不同级别的 bug。② fuzz 用退化预算：`02 §7.2` 生产默认 `steps=50_000_000`/`wall=5s`，fuzz 降到 2M/100ms，否则 30 分钟只跑几十个 exec。③ fuzz 同时断言 `E2001` 的 `Diag.path` 非空且带 `ByteRange`（00 §12 规则 2），否则"预算耗尽"退化成无法定位的字符串。

### 4.5 proptest 不变量清单（14 条，跨模块）

各文档 §9 已有模块内的 P1~P8（04）、`apply(ops,left)==right`（09）、`cross_boundary_matrix`（08）。**本节只列跨两个以上 crate 或泄漏到产品路径的**，每条在 CI 里有一个具名 `#[test]`。

| # | 不变量 | 生成器 | 归属 |
|---|---|---|---|
| I1 | `apply_undo(apply_edit(x)) == x`（内容、`len()`、`patch_set` 三者同时相等） | 随机 `EditOp` + undo/redo 交织 | 04 §9.1 |
| I2 | 任意切分读 == 整体读（切点含 0、`len`、`len-1`、4 KiB 边界两侧） | `partition_gen` | 04 |
| I3 | 保存后重开字节相等（含 0 字节与 100 GB 稀疏的抽样区间） | `op_gen` + 真实临时目录 | 04 §9.2 P8 |
| I4 | `map(unmap(x)) == x`，仅 `MapQuality ∈ {Identity, Exact}` 且全链 `reversible` 时成立；`unmap_ambiguous` 为真必须拒绝写回 | 随机节点图（07 §3 白名单节点） | 07 §3 |
| I5 | piece 数上界 `pieces ≤ C + Σ⌈改动字节/4096⌉`（`C` 为初始片数） | 随机编辑序列逐次断言 | 04 §3.3 |
| I6 | 撤销栈内存 ≤ `undo_mem_limit`；超限**降级**（合并/丢最旧）而非 OOM | 大块替换序列 | 04 §8 |
| I7 | `apply(ops, left) == right` | 09 §9.2 的随机变异器 | 09 |
| I8 | journal 回放**幂等**：回放 k 次 == 回放 1 次（依赖 `SaveMark` 截断） | 17 §4.6 生成器 + 随机截断点 | 17 §4.6 |
| I9 | 分块搜索结果 == 单块全量（4 引擎 × 模式长 1..80 × 命中跨块 ±overlap） | 08 §9 矩阵的属性化推广 | 08 §9 |
| I10 | `.geopatch` round-trip：`apply(parse(export(B)), A) == B` | 随机 patch 集（含空洞与 16 MB 边界） | 04 §4.5 |
| I11 | 熵 ∈ `[0.0, 8.0]` 且 `is_finite()`；直方图 `Σ == len`；任何输入不得出 NaN/Inf | 全 0/全 FF/随机/单字节 | 10 |
| I12 | `version()` 严格单调；带旧 `version` 的异步结果必须静默丢弃（不产生 `Diag`） | 编辑与"陈旧结果注入"交织 | 00 §6 规则 3 |
| I13 | 任意 `ByteRange` 操作不 panic：`clamp` 幂等、`contains`/`overlaps` 对称、`end()` 不溢出 | 边界值偏向（0、`u64::MAX`、`u64::MAX-1`） | 00 §4/§9 |
| I14 | 可视化 `measure()` 的 `bytes_hint` 与实测 RSS 偏差 ≤2×（13 §5 约束 3） | 每内置可视化器 × 随机参数 | 13 §5 |

CI 每条 256 case（18 §9 默认），夜间 4096。

### 4.6 triage 流程

```
crash 落盘 → 去重(栈哈希) → 分类 → 最小化 → 归档 → 转回归 → issue
```

**① 采集**：crash 输入 + stderr 全量 + 栈 + `build_id`（`blake3(二进制)` 前 16 hex）+ 当时的 `Budget`（不同预算下的挂死不可比）。**② 去重（栈哈希）**：
```
stack_hash = blake3(symbols[0..8].join("|"))      // 前 8 帧
top_hash   = blake3(symbols[0])                    // 首帧相同者二次聚类
symbols[i] = "crate::module::fn"                   // 去 fn hash 后缀、去行号、去地址
```
地址与行号不进哈希：同一 bug 在不同 commit 上行号必变，带了就永远去重不掉。同一 `stack_hash` 只开一个 issue，新输入追加到该 issue 附件列表。

**③ 分类**：`Panic | Abort | Oom | Hang | Sanitizer | Assert`。`Hang` 专指"§4.4 的 deadline 到了还没返回"，对应 `E2001` 路径被绕过。

**④ 最小化**：`cargo fuzz tmin <target> <crash> -max_len=4096`；若仍 >4 KiB 则手工结构感知裁剪（IHEX/TOML 类基本能到 <200 B）。**最小化后必须重跑确认仍崩**——`tmin` 偶发产出不崩的产物，它只保证覆盖同一路径。**⑤ 归档**：`testdata/bad/<target>/<yyyymmdd>-<hash>/{crash,minimized,repro.sh,env.json,stack.txt}`；`repro.sh` 自包含（含 `RUST_BACKTRACE`、`ASAN_OPTIONS`），内容为 `cargo +nightly fuzz run <target> minimized -- -runs=1`。

**⑥ 转回归（强制）**：最小化样本 → `tests/corpus/<kind>/<name>/`，`expect` 从 `unknown` 改为实际期望（崩溃类必须是 `reject` + 具体 `code`；解析器未修好则先 `keep="corpus-only"` 并挂 `#[ignore]` 单测指向同一 issue）。**没有回归样本的 issue 不允许关闭**，由模板 checklist 强制。

**⑦ issue 模板要素（缺一不可）**：
```
标题：<target>: <class> @ <stack_hash[0..8]>
 □ 复现命令（一行可粘贴） □ 栈（前 20 帧）与 build_id □ 输入附件（最小化后）
 □ 影响面：该入口在生产路径上是否可达？（打开/粘贴/装包/加载规则/…）
 □ TH 编号（19） □ 期望语义（应给哪个错误码；没有合适的码则先在 00 §12 登记）
 □ 回归样本路径 tests/corpus/... —— **只在补上后勾选**
```

**⑧ SLA**：`Hang`（预算未返回）/`Sanitizer`（UB）/越界写（`geopatch_apply`、保存路径的数据损坏）→ **24h P0，阻塞发布**（18 §6）；`Panic`/`Abort`/`Oom` → 1 周 P1；`Assert`（不变量被破坏）→ 2 周 P1；仅"比预期慢但未挂死" → backlog P2，转 21 的趋势观察。

### 4.7 CI 分层

| 层 | 触发 | 硬预算 | 内容 |
|---|---|---|---|
| **smoke** | 每 PR | **≤2 min** | ① `corpus_regress`（全部 `keep="regress"` 按 `expect` 断言，≤60s）② 6 个高危 target 各 8s（`bspec_vm_run`、`geopkg_unpack`、`viz_image_png`、`geopatch_apply`、`sftp_path_norm`、`journal_recover`）= 48s ③ `corpus check` + `audit` ≤5s。合计 ≈110s，留 10s 余量 |
| **deep** | 夜间 02:00（18 §4 的 `fuzz.yml`） | **≤30 min** | 25 target × 60s（8 核并行 → 墙钟 ≈5 min）+ proptest 4096 case + 全量 `corpus_regress` |
| **refeed** | 每周一 | ≤60 min | 拉 OSS-Fuzz 上周 crash 集与 corpus 增量 → 本地去重 → 最小化 → 并入 `tests/corpus`；新 crash 各自开 issue |
| **mutants** | 夜间（PR 用 `--in-diff`） | PR ≤3 min / 夜间 ≤60 min | 见 §9.4 |

**PR 上不跑新 fuzz，只跑语料回归**：PR 的 fuzz 命中率极低（新代码带进新 bug 的概率 < 新增样本能抓到的概率），而 2 分钟是硬约束。

### 4.8 与各文档 §9 的关系

分工只有一句：**各文档 §9 拥有模块内的"对不对"（单元、边界值、模块内 proptest、黄金文件），本文拥有跨模块的"崩不崩"与语料本身**。具体地——① 各文档 §9 的边界值断言被 `corpus_regress` 用真实字节再喂一遍（同一期望语义，不同输入载体）；② 模块内 proptest（04 的 P1~P8、09 的 `apply(ops,left)==right`、08 的跨块矩阵）不重写，本文只补 §4.5 那 14 条跨 crate 的；③ fuzz target 与 `tests/corpus/` 的唯一权威在本文（18 §9 的"6 targets"是汇总，须按 §7.1 回填）；④ mutation testing 的适用范围与豁免结论由 §9.4 给出，各文档不再各自提名。

## 5 性能预算与基准

本文不管产品路径性能，但与 `00 §7` 有**一处硬耦合**：`bspec_vm_run` 的 fuzz 预算必须 ≤ `00 §7` 的「BinSpec 执行 <100ms（10MB + 中等模板）」。若沿用 `02 §7.2` 的生产默认 5s，fuzz 只会在严重病态时才报 `E2001`，真实的"轻微超预算"路径永远测不到。

### 5.1 与 00 §7 的对应

| 00 §7 行 | 与本文的关系 |
|---|---|
| BinSpec 编译 <20ms | `bspec_compile` 的 wall 500ms 是它的 25 倍——允许慢，因为 fuzz 关心"会不会崩/指数爆炸"，不关心绝对快 |
| BinSpec 执行 <100ms | `bspec_vm_run` 的 wall 150ms = 100ms 产品预算 + 50ms 环境余量。**唯一必须逐条对齐的一行** |
| 其余所有行（打开/滚动/搜索/保存/内存） | 一律归 21，本文不测、不判定、不设门槛 |

`00 §7` **没有**语料/CI 资源相关的行，也不需要——那是工程资源预算，属本文 §5.2。

### 5.2 CI 资源预算（本文自定，不进 00 §7）

| 项 | 预算 | 超限动作 |
|---|---|---|
| `tests/corpus/` 总体积 | ≤8 MiB（>512 KiB 单样本见 §10 Q3） | 手工评估，优先改"生成时构建" |
| 单样本体积 | ≤512 KiB | 拒收，要求用生成器 |
| `corpus_regress`（PR） | ≤60s / ~200 样本 | 拆分：每类只留 3 个代表样本 `regress` |
| PR smoke 总时长 | ≤120s | 削减 target 数（最多减到 4）而非削减语料 |
| 夜间 deep 总时长 | ≤30 min | 按 exec/s 排序砍最低 5 个 target 的**时长**，不砍 target 数 |
| `fuzz/corpus/` 磁盘 / 单个 crash 归档 | ≤2 GiB / ≤4 MiB（最小化后） | 前者按 exec/s 最低的 target 裁剪；后者只留 `repro.sh` + 生成器参数，不存字节 |

### 5.3 只做趋势、不做门禁的计数

`fuzz/corpus` 新增路径数、每 target exec/s、crash 数按类别、`cargo llvm-cov` 每 crate 行覆盖。四项写 jsonl（与 21 的 `bench-data` 分支共用，字段加 `"kind":"fuzz"`），**不设阈值**——理由见 §1.2。

## 6 失败模式与降级

| 失败 | 行为 | 用户 / 维护者可见 |
|---|---|---|
| target 编译失败（依赖升级破坏） | 标 `build-failed` 并 **CI 红**，不静默跳过 | PR 检查项直接失败；夜间汇总单列一行 |
| 语料 sha256 不符（有人手改样本） | `corpus check` 红，并**拒绝运行**依赖它的 target | 防止"用错样本得出错误判定"；报实际与期望哈希 |
| 生成器不可重现（跨机器字节不同） | 断言失败，定位到生成器用了时间/无种子 `rand`/`HashMap` 迭代序 | 修法固定：禁 `Instant`/`SystemTime`/无种子 `rand`/`HashMap` |
| 样本 `expect` 超期未判定 / OSS-Fuzz 中断或配额用尽 | 前者 `audit` 报错阻塞 PR；后者本地夜间继续并标 `oss-fuzz-down`，14 天未恢复升 P1 | 路径与剩余天数 / 周报一行状态 |
| 夜间 deep 超 30 min | 自动按 exec/s 降时长，并开 issue 记录被削的 target | 周报附清单，防"悄悄不测了" |
| 发现可利用漏洞（UB、越界写） | 转 19 的响应流程；本文提供最小复现 + 回归样本；**公开 issue 不贴可利用细节** | 私有安全公告 |
| triage 积压 >50 条未处理 | 暂停"新增 target"，人力转向清账 | 周报计数，>100 条暂停合并（同 18 §6 的 flaky 纪律） |
| 某 target 长期零发现且零新覆盖 | 标 `dormant`，降到每周一次；**不删** | 零发现往往意味着语料不够，而不是代码够好 |

## 7 与其它子系统的契约

| 子系统 | 本文读 | 本文写 | 对其提出的要求（否则本文无法工作） |
|---|---|---|---|
| `geode-bspec` | `compile`、`run`、`Budget` | 2 target | 提供 `Budget::for_fuzz()`；`E2001` 的 `Diag` 必带 `path` 与 `ByteRange` |
| `geode-provider` | IHEX/SREC 解析器、`MockProvider` | 5 target | 解析器必须是**纯函数**不触 IO；协议 target 需可注入字节流 trait |
| `geode-buffer` | `geopatch` 应用、`PatchSet` | 1 target | patch 应用必须事务化：失败后 buffer 与失败前逐字节相同（04 §4.5） |
| `geode-viz` | `decode`、`measure` | 8 target | 尺寸预检 `check(est)`（13 §4.4）必须可单独调用（当前草案为内部函数，需 `pub`） |
| `geode-content` / `geode-project` | `unpack` 到 staging、`journal::replay` | 2 target | 解包不经网络、staging 根由参数注入；`replay` 能对 `&mut dyn Read` 工作（不绑路径） |
| `geode-search` / `geode-annot` | 正则包装层、`AnnotationIndex` | 2 target | 编译与扫描分离（只 fuzz 编译阶段）；操作序列需可反序列化 |
| `geode-ui` | `theme::parse` | 1 target | 主题解析必须与 egui 解耦（§2.2） |
| `geode-yara` | 编译护栏 | 1 target | 护栏阈值（12 §4.3）可从 `RuleLimits` 注入，不硬编码 |
| **18 / 19 / 21** | CI 结构、TH 编号、`scripts/gen/` 约定 | `fuzz.yml` 矩阵、`threats` 字段 | 每条 TH 至少 1 个样本（`audit` 断言）；数据集与语料不混放 |

### 7.1 需回填的其它文档

| 位置 | 现状 | 应改为 |
|---|---|---|
| `18 §9` 测试金字塔 | "fuzz：6 targets" | "fuzz：25 targets（清单见 20 §3.4）" |
| `18 §3.1` 目录树 | `fuzz/fuzz_targets/` 列 6 个文件；无 `tests/corpus/` | 按 20 §3.4 的 25 个文件；新增 `tests/corpus/` 与 `scripts/gen/` |
| `18 §6` | "崩溃输入自动留存到 `testdata/bad/`" | 补"triage 后最小化并入 `tests/corpus/`，见 20 §4.6" |
| `04 §9.3` | "退化 >10% 红灯" | 统一为 5%（见 21 §5.1） |
| `13 §9` | 解码炸弹样本库 | 实现归本文（`tests/corpus/image/`），13 保留"必须被拒绝"的期望 |

## 8 配置项与默认值

### 8.1 `fuzz.toml` 与 `corpus.toml`

| 文件 | 键 | 默认 | 范围 / 说明 |
|---|---|---|---|
| `fuzz.toml` | `fuzz.max_len` | 1 MiB | 1 KiB~64 MiB；libFuzzer `-max_len`，单 target 可覆盖 |
| `fuzz.toml` | `fuzz.libfuzzer_timeout_s` / `deadline_grace_ms` | 25 / 200 | 前者只是最后防线，不作判定依据；后者是内部 deadline 后的 abort 宽限（§4.4） |
| `fuzz.toml` | `fuzz.rss_limit_mb` | 256 | 32~2048；`viz_*` 与 `bspec_vm_run` 各自覆盖为 512 |
| `fuzz.toml` | `fuzz.nightly_seconds_per_target` / `smoke_seconds_per_target` / `smoke_targets` | 60 / 8 / 6 | 夜间 deep 与 PR smoke 时长、高危列表（§4.7） |
| `fuzz.toml` | `fuzz.parallel_jobs` | 8 | 1~32；与 `bench.yml` 互斥，避免抢核 |
| `fuzz.toml` | `fuzz.oss_fuzz_project` / `seed_from_corpus` | `"geode"` / `true` | 空串 = 不上 OSS-Fuzz；后者从 `tests/corpus` 硬链接种子 |
| `fuzz.toml` | `fuzz.budget.{steps,wall_ms}` | `2_000_000` / `100` | fuzz 专用，远低于 02 §7.2 的生产值 |
| `corpus.toml` | `corpus.min_accept_per_target` / `min_reject_per_target` | 3 / 3 | 1~50；`audit` 断言 |
| `corpus.toml` | `corpus.unknown_expect_ttl_days` | 30 | 7~180；超期阻塞 PR |
| `corpus.toml` | `corpus.max_sample_bytes` / `total_budget_mib` | 512 KiB / 8 | 单样本与总体积上限（§10 Q3） |
| `corpus.toml` | `corpus.regress_budget_s` / `tmin_max_bytes` | 60 / 4096 | PR 语料回归预算；最小化上限 |
| `corpus.toml` | `triage.dedup_frames` / `stale_backlog_threshold` / `sla_hours` | 8 / 50 / P0=24h,P1=1w | 栈哈希帧数、积压阈值与 SLA（§4.6 ⑧） |
| `corpus.toml` | `gen.forbid_nondeterministic` | `true` | 生成器出现 `Instant`/`SystemTime`/无种子 `rand` 即拒绝（静态检查） |

## 9 测试策略

整份文档讲测试，最容易漏的是**测试基础设施自己有没有被测**。本节是它的测试。

### 9.1 生成器测试

每个 `scripts/gen/*` 有固定 seed 的产出快照 `tests/corpus/<kind>/_gen/snapshot-<seed>.sha256`，CI 断言"重放 → 哈希一致"。这既验证生成器确定性，也验证"语料确实是生成器的产物"（防止有人手改样本却没改生成器）。静态检查（`gen.forbid_nondeterministic`）用 AST/正则扫描禁止 `Instant::now`、`SystemTime::now`、无种子 `rand`、`HashMap` 迭代。

### 9.2 canary：harness 自检（最重要的一条）

**fuzz 变绿有两种原因**：代码没问题，或者 fuzzer 根本没在跑。区分二者的唯一方法是注入已知 bug。`fuzz/canary/` 在 `feature = "fuzz-canary"` 下提供三个假 target：

| canary | 注入的缺陷 | 期望 |
|---|---|---|
| `canary_panic` | 输入含 `0xde 0xad` 时 panic | 60s 内必被发现，`class == Panic` |
| `canary_hang` | 输入含 `0xfa 0xce` 时死循环 | 60s 内必被发现，`class == Hang`——**验证 §4.4 的 deadline 逻辑，这是最容易写错的** |
| `canary_clean` | 无缺陷 | 60s 内零发现（验证无假阳性） |

三个 canary 全通过才认为本次 fuzz 结果有效。

### 9.3 语料审计的六条断言（`xtask corpus audit`）

1. 每 target 的 accept / reject 数达标（`corpus.min_*`）；2. 每条 TH（19）至少 1 个样本；3. 无超期 `unknown`；4. 所有 sha256 与 size 匹配；5. 所有 `expect.code` 落在 00 §12 段内；6. 六类边界矩阵每格非空（用 `features` 的 `edge:*` tag 检查）。

### 9.4 mutation testing（`cargo-mutants`）

**适用范围与成本**（只对"改坏一个运算符就会丢数据或漏判"的模块；下表按 PR 前实测 LOC 估）：

| 模块（关键文件） | 有效 LOC | ~mutant | 全量耗时（8 核） |
|---|---|---|---|
| `geode-buffer`（save/patch/undo） | 2.4k | 900 | ~10 min |
| `geode-bspec`（sema/vm） | 4.0k | 1500 | ~13 min |
| `geode-diff` / `geode-search` / `geode-provider` / `geode-project` | 4.5k | 1650 | ~10 min |
| **合计** | ~10.9k | **~4050** | **~40 min（单核 ~4.5 h）** |

**豁免（每条写理由，不许"因为慢"）**：`geode-ui` 全部渲染（差异只在像素上，靠 `egui_kittest` 快照覆盖，mutation 无法自动判定）；`geode-viz` 的 GPU 路径与 `image` crate 边界（前者无 GPU 跑不了，后者属第三方）；平台 shim（大量等价 mutant，`n<0` 与 `n<=0` 在 errno 语义下等价）；常量/查表模块（CRC 表、Unicode 宽度表、内置 magic 规则——数据而非逻辑，由语料覆盖）；`unsafe` FFI 声明体（无逻辑）；`Diag` 文案与 i18n catalog（属文案）。

**门槛与时机**：`geode-buffer` 的保存事务与 patch 路径 **mutation score ≥85%**（漏一个 mutant 就是一条丢数据路径没被覆盖，这是 C2 的直接度量），其余 **≥60%，只做趋势**；PR 上 `cargo mutants --in-diff`（≤3 min），夜间全量（≤60 min，与 deep fuzz 并行）。等价 mutant 用 `mutants.toml` 的 `exclude_re` 显式豁免**并注释为什么等价**，禁止无注释批量豁免。

### 9.5 覆盖率与回归语料的三重身份

`cargo llvm-cov` 夜间跑，写 jsonl（与 21 的 `bench-data` 共用），字段含 `crate`/`lines_covered`/`lines_total`/`fuzz_hit`，**不设门槛**（18 §9）；软报警：某 crate 的 fuzz 路径覆盖连续 4 周下降 → 查是不是 target 挂了。同一份语料被三处使用、语义必须一致，因此**不允许"只给单测用的样本"**：① `corpus_regress`（PR，按 `expect` 断言）；② `fuzz/seeds`（夜间，作种子）；③ 黄金文件的输入侧（可选）。

## 10 开放问题

| # | 问题 | 候选 | 需要什么数据 |
|---|---|---|---|
| Q1 | 是否自建 fuzz 农场 | ① 现有 8 核 `bench-runner` 兼职 ② 专用 4×16 核 ③ 纯依赖 OSS-Fuzz | 夜间 30 min 的实际 exec/s；OSS-Fuzz 拒收哪些 target（`yara_rule` 的 C 依赖、`theme_load` 的 `geode-ui` 依赖）；`bench.yml` 与 `fuzz.yml` 抢机器的冲突频率；OSS-Fuzz 是否肯收（25 target 里 19 个纯 CPU，`zip`/`image` 均纯 Rust，理论上可上） |
| Q2 | 是否对 Capstone / YARA 的 C 层做 UBSan/ASan 构建 | ① 单独 `-Zsanitizer=address` job ② 只在 OSS-Fuzz 上游做 ③ 不做 | 18 §8 的 `full` job 是唯一带 C 依赖的 job，加 sanitizer 会让时长翻倍；Rust 侧 `-Zsanitizer` 需 nightly，与 `rust-toolchain.toml` 的 `stable` 冲突，要单独 toolchain |
| Q3 | 语料库是否随仓库走 Git LFS | ① 直接入 git（当前 8 MiB 够）② LFS ③ 全部"生成时构建" | 目前最大的样本是图片炸弹，若都 ≤64 KiB 则 ① 足够；一旦超过 512 KiB（如真实固件片段）就必须 ② 或 ③。倾向 ③——连"LFS 也要拉全量历史"都省了 |
| Q5 | 插件 ABI 是否 fuzz（14） | ① fuzz `geode-plugin-host` 的 vtable 调用 ② 只测 ABI 版本协商 ③ 不做 | 需 stub 插件二进制（跨平台 cdylib）；`panic=unwind` 的边界（18 §8）让"插件 panic 被捕获"无法在 `panic=abort` 的 fuzz 构建里测 |
| Q6 | 是否对 `geode-ai` 的模型响应做 fuzz | ① 是（模型输出确实是不可信输入）② 否 | 用 mock client（18 §8 `GEODE_NO_NETWORK=1`）时响应解析是纯函数，可 fuzz；但需先冻结响应 schema（03 尚未冻结） |
| Q7 | 语料是否需要跨平台变体 | ① 单份 ② 按平台分 | `sftp-path` 与 `theme` 有明显平台差异（盘符、`\`）；`ihex`/`srec` 无差异。倾向只对前两类分 |
