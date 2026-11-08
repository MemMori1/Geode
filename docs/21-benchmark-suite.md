# 21 — 性能基准套件

> **本文拥有**：`bench/` 这个 workspace 成员的**全部 bench 定义**、数据集生成脚本（`scripts/ds/`）、判定算法（`bench/src/judge.rs`）、`bench-data` 分支的产物格式、白名单。**不拥有**：`00 §7` 的预算数值本身（本文只能引用与请求回填）、CI 编排与 runner 供给（归 18 §5/§6）、各子系统的内部优化手段。
> 一句话：**"退化 >5% 即红灯"这条规则在 18 §5 只是一句话；本文负责让它变成一条能被执行、能被复现、且误报率可控的门禁。**

## 0 决策速览

| # | 决策 | 理由 / 代价 |
|---|---|---|
| 1 | 每个 `00 §7` 的预算行**必须且只能**有一个守护 bench（`bench/registry.rs` 里一对一映射） | 否则"预算是预算，bench 是 bench"，两边慢慢漂移；代价是 `00 §7` 每加一行都要同时提 PR 加 bench |
| 2 | 门槛由**绝对预算**与**相对退化**两条同时组成，任一失败即红 | 只看相对会让"本来就慢 3 倍"的代码永远绿；只看绝对会让 5 年前定的数字锁死优化空间 |
| 3 | 判定用 **MAD（中位绝对偏差）**而不是标准差，报 p50/p95/p99 不报均值 | 基准分布右偏且含抖动尖峰，均值与 σ 会被一个 200ms 的 GC 式尖峰带偏 |
| 4 | 判定只在**自管 bench-runner** 上做；共享 CI runner 只采集趋势不判定 | 共享 runner 的方差不支持 5% 分辨力。代价：PR 的判定要等一台专用机 |
| 5 | 帧级指标用**自建 harness**（离屏 `egui::Context::run` + 4096 帧环形缓冲），库级指标用 criterion | criterion 测不了"一帧内 8 个阶段各花多久"，也测不了 p99 帧时间 |
| 6 | 数据集**永不入库**，由 `scripts/ds/*` 确定性重建，manifest 记录 sha256 + 实测熵 | 100 GB 稀疏镜像与 1 GiB 随机数进 git 是灾难；manifest 让"数据集不对"能被立刻发现 |

## 1 目标与非目标

### 1.1 目标

| # | 目标 | 机器可验证形式 |
|---|---|---|
| G1 | `00 §7` 每一行都有唯一守护 bench，无遗漏无重复 | `cargo xtask bench audit` 对比 `00 §7` 解析结果与 `registry.rs`，差集非空即红 |
| G2 | 真退化（>5%）红灯，误报 <1% | 每月用 §9.1 的 canary bench 反向验证判定器本身 |
| G3 | 同一 commit 任意两次运行给出同一结论 | 同 commit 双跑，`Verdict` 必须一致；不一致即 `inconclusive` 并记录 |
| G4 | 红灯时给出"最可疑的 3 个 bench + 火焰图命令"，不需要读日志 | PR 评论模板固定，见 §4.6 |
| G5 | 报告 p50/p95/p99 + MAD + 分配次数 + RSS 峰值 + 环境指纹 | jsonl schema 见 §3.2，缺字段即丢弃该记录 |

### 1.2 非目标

- **不与其它机器横比**。绝对数字只对"同一 runner、同一构建配置"有意义；跨机比较归各文档 §5 的理论推导。
- **不做绝对性能宣称**。不对用户承诺"打开 100 GB 只要 X 秒"，只承诺"不比上个 commit 慢 5%"。理由见 §6 的 `NoiseFloor`。
- **不替代 profiling**。bench 说"慢了"，不说"为什么慢"；后者是 `perf record`/`samply` 的活。
- **不测正确性**。bench 只保证结果被消费（`black_box`），不对结果做语义断言——那是各文档 §9 与 20 的语料回归的活。
- **不做云上横向扩展**。一台机器，一条时间线。

### 1.3 与 18 的分工（一句话）

**18 定"在哪跑、什么时候跑、机器怎么配"（`bench.yml`、cron、runner 规格、`[profile.bench]`）；本文定"测什么、怎么测、怎么判"。** 冲突时以本文的判定算法为准，18 只保留编排（见 §7.1 的回填清单）。

## 2 依赖与边界

### 2.1 上下游

- **工具**：`criterion`（库级统计）、自建 `FrameHarness`（帧级）、`stats_alloc`（分配计数）、`fd-lock`+`mlock`（数据集驻留）、`xtask bench`（编排与判定）。
- **被测 crate**：`geode-buffer`、`geode-search`、`geode-diff`、`geode-bspec`、`geode-analysis`、`geode-viz`、`geode-project`、`geode-ui`。
- **不依赖**：网络、GPU 驱动的具体版本（帧级只看 CPU 侧编码与提交，见 §6）、任何时钟之外的系统状态。

### 2.2 `bench/` 是独立 workspace 成员

```
bench/
├── Cargo.toml            # 独立 profile：debug=1, lto=thin（18 §8）
├── src/registry.rs       # BenchSpec 表：bench ↔ 00 §7 行的映射（G1 的唯一真相）
├── src/harness.rs        # FrameHarness、run_scripted、环形缓冲
├── src/judge.rs          # MAD 判定、Verdict、白名单
├── src/report.rs         # jsonl 写出、PR 评论渲染、趋势 SVG
├── src/datasets.rs       # 数据集定位与 manifest 校验（不生成，只校验）
└── benches/*.rs          # 每个 00 §7 行一个文件（见 §5.2）
```

`bench/` 不进 release 依赖图，也不被 `geode-ui` 依赖。它与 `fuzz/` 共用 `scripts/` 但**不共用数据集**（fuzz 语料 ≤512 KiB，bench 数据集 GB 级，见 20 §7）。

### 2.3 数据集在磁盘上的位置

- 默认 `$GEODE_BENCH_DATA`（未设则 `~/.cache/geode/bench-ds`），**不在仓库内**、不在 workspace 内。
- 每个数据集一个目录 + `manifest.toml`（§4.1），含 `size_bytes`、`sha256`、`entropy_measured`、`generator`。
- 生成是幂等的：`cargo xtask ds gen <name>` 已存在且 sha256 一致则跳过；不一致则报错并要求 `--force`（防止半截文件被当基线）。

### 2.4 四条不可协商

1. bench 里**禁止 `println!` 之外的 IO**（例外：读数据集）；criterion 的 `--save-baseline` 禁止使用（18 §5 已禁，理由：会诱使拿历史基线比，而历史基线跨了 profile 变更）。
2. 每个 bench 必须 `black_box` 消费结果，否则 LLVM 会把整个被测路径当死代码删掉——这是最常见的"bench 突然快了 30 倍"。
3. 每个 bench 声明 `warmup`/`sample` 与 `iters_guard`（见 §4.2），不许用 criterion 默认值凑。
4. 数据集**不得**被 bench 修改（只读打开 + `mlock`），否则第二次运行与第一次不可比。

## 3 数据结构与接口

### 3.1 bench 描述与门槛

```rust
pub struct BenchSpec {
    pub name: &'static str,             // 全局唯一，也是 jsonl 的 key
    pub budget_row: BudgetRow,          // 对应 00 §7 的哪一行（G1 的锚）
    pub kind: BenchKind,                // Lib | Frame | EndToEnd | Memory | Alloc
    pub metric: Metric,                 // P50/P95/P99 | Throughput | Mad | Rss | Allocs
    pub gate: Gate,
    pub dataset: Option<&'static str>,  // ds 名；无则 None（合成小输入）
    pub cold: bool,                     // 是否属于冷启动组（§4.2）
}
pub struct Gate {
    pub absolute: Option<Budget>,       // 00 §7 的绝对门槛（缺则见 §5.3 回填项）
    pub max_regress_pct: f64,           // 默认 5.0
    pub direction: Dir,                 // LowerIsBetter | HigherIsBetter
    pub noise_mult: f64,                // 显著性倍数，默认 3.0（= 3·MAD）
    pub samples: u32,                   // 默认 30；帧级为帧数
}
```

`BudgetRow` 是一个枚举，与 `00 §7` 的表格行一一对应（`Open4Gb`、`ScrollFrameP99`、`Jump100Gb`、`SearchHexWildcard`、`DiffMemoryPeak`、`FirstFrame`…）。`registry.rs` 里 `BudgetRow` 必须**恰好出现一次**，重复与缺失都由 `xtask bench audit` 报错——这就是 §1.1 G1 的落地方式。

### 3.2 jsonl 记录（`bench-data` 分支的原子单元）

```jsonc
{ "schema": 1,
  "commit": "9f2c4e1", "dirty": false, "run_id": "2026-11-08T21:52:13Z-01",
  "bench": "scroll_frame_p99", "metric": "p99_ms", "value": 11.4,
  "p50": 9.8, "p95": 11.0, "p99": 11.4, "mad": 0.31, "n": 4096,
  "env": { "cpu": "i9-13900K", "mhz_min": 3200, "mhz_max": 3200, "turbo": false,
           "temp_c": 61, "isolated_cores": "4-11", "hugepages": "madvise",
           "build_id": "a1b2c3d4e5f60718", "profile": "bench", "rustc": "1.83.0" },
  "dataset": { "name": "sparse100g", "sha256": "…", "size": 107374182400 },
  "allocs": { "count": 0, "bytes": 0 }, "rss_peak_mb": 214.6,
  "verdict": { "status": "pass", "delta_pct": 1.2, "gate_pct": 5.0, "reproduced": 3 } }
```

`env` 是**判定的一部分而非装饰**：`mhz_max != mhz_min`（未锁频）或 `temp_c > 85` 的记录必须被标 `env_unstable: true`，判定时降级为 `inconclusive`。`build_id = blake3(可执行文件)` 前 16 hex，用于识别"两次运行其实是两个不同二进制"。

### 3.3 `FrameHarness`

```rust
impl FrameHarness {
    pub fn new(w: u32, h: u32, dpi: f32) -> Self;      // 离屏，不创建真实窗口
    pub fn ring(&self) -> &RingBuffer<FrameSample>;     // 容量 4096，覆盖 ~68s@60fps
    pub fn run_scripted(&mut self, script: &[Action]) -> FrameReport;
}
pub struct FrameSample { pub t_total_us: u64, pub stage: [u16; 8] }  // 每阶段 µs
pub struct FrameReport {
    pub p50_ms: f64, pub p95_ms: f64, pub p99_ms: f64, pub mad_ms: f64,
    pub stages_p99: [f64; 8],        // prepare/layout/tessellate/upload/submit/present/viz/overlay
    pub frames_dropped: u32, pub allocs_per_frame: u32,
}
```

`Action` 是一段可重放的脚本（`Scroll { dy, frames }`、`Select { from, to }`、`Jump { pct }`、`Resize { w, h }`、`OpenViz { id }`），滚动 bench 的脚本固定为"1 帧预热 + 600 帧恒速滚动"，这样 600 帧的 p99 才有统计意义。阶段划分必须与 `05 §5.1` 的 8 阶段预算一致，否则 `stages_p99` 与 `00 §7` 的帧预算对不上。

### 3.4 分配计数

```rust
#[global_allocator] static A: StatsAlloc<System> = StatsAlloc::new(System);
// 每个 bench 前后读 A.snapshot()，记录 count/bytes 差值
```

只在 `Alloc` 类 bench 里挂（帧路径的稳态 0 分配是硬指标，见 §5.2），其它 bench 挂上反而污染噪声。

### 3.5 判定结果

```rust
pub enum Status { Pass, Regress, AbsoluteFail, Inconclusive, Flaky, Whitelisted }
pub struct Verdict {
    pub status: Status,
    pub delta_pct: f64, pub gate_pct: f64,
    pub noise_pct: f64,          // 3·MAD 折算成百分比
    pub reproduced: u8,          // 连续同向次数（阈值见 §4.3）
    pub reason: Option<String>,
}
```

## 4 核心算法流程

### 4.1 数据集怎么造

全部由 `scripts/ds/<name>.sh` 生成，**不入库**，`manifest.toml` 记录可复算的指纹。命令与预期特征如下（熵为实测全文件香农熵，`entropy_measured` 由生成脚本用 1 MiB 分块算完取加权平均）：

| 数据集 | 生成命令（要点） | 预期大小 | 预期熵 | 守护什么 |
|---|---|---|---|---|
| `sparse100g` | `truncate -s 100G sparse100g.img` 后在 0/50%/99% 处各写 4 KiB 结构头；`du` 应 <32 MiB | 100 GiB 逻辑 | ≈0（稀疏区）+ 头部局部高熵 | 打开、跳转、空洞读取、Offset 算术（`open_100gb`、`jump_100gb`） |
| `fw500m` | 生成器拼"ELF 头 + 若干 PE 段表 + 压缩载荷 + 尾部签名"，载荷用 `xorshift` 逐块生成后 `zstd -3` 存（解压后 500 MiB） | 500 MiB | 整体 ≈5.8，压缩段 ≈7.9 | 搜索、magic、熵分析、BinSpec 模板解析 |
| `text10m` | 生成 10 000 000 行、每行 24~56 B 的混合语言行（含 UTF-8/UTF-16 交替段） | ≈420 MiB | ≈4.2 | 字符串搜索（8 编码）、diff、正则 |
| `rand1g` | `head -c 1G /dev/urandom`（用固定 seed 的 `pcg64` 重实现以保证可重现） | 1 GiB | 8.0 | 搜索最坏情况、压缩类可视化、保存吞吐 |
| `zeros1g` | `head -c 1G /dev/zero` | 1 GiB | 0.0 | 熵/直方图退化、通配搜索命中上限、diff 全异/全同 |
| `mixed40` | 40% 4 KiB 零页与 60% 随机页随机交错（固定 seed） | 1 GiB | ≈5.6 | 熵分块、magic 误判、稀疏比例扫描 |
| `deepnest` | 由 `02` 的 EBNF 反推生成 256 层嵌套结构模板 + 10 MiB 匹配数据 | 模板 1.2 MiB / 数据 10 MiB | 8.0 | BinSpec 编译与执行预算（`00 §7` 唯一指定 10 MB） |
| `diff100m` | 从 `fw500m` 取两段 100 MiB，`B` 上做 10% 字节位翻转（固定 seed） | 2×100 MiB | ≈7.9 | diff 两阶段耗时与内存峰值 |

生成脚本必须打印 `size`/`sha256`/`entropy_measured`，由 `xtask ds gen` 写入 `manifest.toml`；`xtask ds check` 在每次 bench 前校验，不一致即 abort 并提示重生成——**数据集漂移是基准噪声的最大来源**（例如 `text10m` 多生成 1% 行，字符串搜索会凭空"慢" 1%）。

### 4.2 测量方法

| 类 | 工具 | 采样 | 冷/热 | 注意 |
|---|---|---|---|---|
| `Lib` | criterion | `sample_size ≥ 30`，`warmup 3s`，`measurement 10s` | 热 | 每个 bench `iter_custom` 里跑够 1 MiB 以上输入，避免计时开销占比过大 |
| `Frame` | `FrameHarness` | ≥600 帧（滚动类 1200 帧），环形缓冲 4096 | 热（先跑 60 帧预热并丢弃） | 报 p50/p95/p99 与 MAD；`stages_p99` 只在 `perf-panel` feature 下开 |
| `EndToEnd` | 自建计时 + `perf_event` | 5 次取中位 | 冷（`posix_fadvise(DONTNEED)` 后） | 冷/热必须分开报，名字前缀 `cold_`/`warm_`，不能混进同一趋势线 |
| `Memory` | RSS 采样线程（1 kHz）+ 峰值 | 5 次取最大 | 冷 | 记录 `rss_peak_mb` 与 `rss_p95_mb`；`mlock` 关闭（要与真实场景一致） |
| `Alloc` | `StatsAlloc` | 1 次迭代 | 热 | 断言稳态**每帧 0 次分配**（帧路径）或"分配次数不随数据规模增长"（搜索类） |

统一规则：**报 p50/p95/p99 与 MAD，不报均值**；像素/吞吐类报 `MB/s` 并同时给出 `MB` 分母（`bytes/max(p50, 1ns)`）。任何 bench 的 `n < 30` 一律 `inconclusive`。

### 4.3 判定算法

```
fn judge(cur, base, spec, history) -> Verdict {
    // 1) 环境合法性
    if !cur.env.stable() || cur.build_id != base.build_id { return Inconclusive }

    // 2) 绝对预算（00 §7），缺失则跳过这一段
    if let Some(b) = spec.gate.absolute {
        if violates(cur.value, b, spec.gate.direction) { return AbsoluteFail }
    }

    // 3) 相对退化：MAD 判别
    let delta  = signed_pct(cur.value, base.value);            // 正 = 变慢
    let noise  = 3.0 * (cur.mad + base.mad) / (2.0 * base.value) * 100.0;
    let gate   = max(spec.gate.max_regress_pct, noise);        // 5% 是地板，不是天花板
    if delta <= gate { return Pass }

    // 4) 显著性 + 复现性：两个条件都要
    let significant = delta - noise > spec.gate.max_regress_pct * 0.5;  // 不允许"噪声把门槛撑大再通过"
    let reproduced  = history.count_same_direction(spec.name, 3);       // 连续 3 次同向
    if significant && reproduced { Regress } else { Flaky }
}
```

三条必须写进注释的理由：① **`gate` 取 max 而非 min**——机器越吵门槛越高，但 ② `significant` 又要求"超出噪声的部分本身还得显著"，避免噪声大时永远绿；③ 连续 3 次同向才判红（不是 1 次、也不是 2 次），因为单次抖动与真实退化的分布重叠区就在 1~2 次之间。这与 `18 §6` 写的"连续 2 次"冲突，须回填（§7.1）。

### 4.4 噪声控制

| 项 | 做法 | 上限/阈值 |
|---|---|---|
| 频率 | `cpupower frequency-set -g userspace -f 3.2GHz`（与 18 §5 的 runner 规格一致） | 若 `mhz_max != mhz_min` 则记 `env_unstable` |
| 核心 | `taskset -c 4-11` + `isolcpus=4-11` 内核参数；bench 与 fuzz 互斥排队 | 同一时刻只允许一个 bench 进程 |
| 温度 | 每轮记录 `temp_c`；连续两轮 >85 °C 则中止该 bench | >85 °C → `inconclusive` |
| 布局 | THP=`madvise`；`prev 幂` 不做（会改变真实行为） | 与 18 §5 一致 |
| 重复 | 每 bench 最少 **5 次**，取中位作为该 run 的代表值 | 少于 5 次 → `inconclusive` |
| 共享 runner | 允许采集趋势，**禁止**参与判定 | `GEODE_BENCH_JUDGE=0` |
| 构建 | 同 commit、同 `[profile.bench]`、同 `rustc`；`build_id` 不同则不可比 | 见 §3.2 `env` |

原则：**噪声要被测量而不是被祈求**。所以每次运行都记 MAD 与温度，这比"跑之前先拜一拜冷启动"可靠得多。

### 4.5 白名单机制

只在"外部因素确实不可控"时使用，且必须写归属人与到期日：

```toml
[[whitelist]]
bench     = "cold_open_4gb"        # 受 OS page cache 回收策略影响
reason    = "冷启动受内核 readahead 启发式影响，方差 18%"
owner     = "@perf-owner"
until     = 2027-02-01             # 到期自动失效，CI 会开口提醒
exempts   = "relative"             # 只豁免相对退化；absolute 仍判定
```

规则：① 白名单**只能豁免相对退化**，绝对预算永远判定（否则等于取消门槛）；② 全局条目 ≤5 条，超限需要 RFC；③ 到期未续自动失效并开 issue（防止"临时豁免"变永久）；④ 白名单条目的 bench 仍在跑、仍写 jsonl（趋势要连续）。

### 4.6 产物与趋势

```
bench-data/                        # 独立分支，不合并到主干
├── runs/2026/11/<commit>.jsonl    # 每次 run 一个文件（§3.2 一行一 bench）
├── trends/<bench>.svg            # 由 report 生成，按周聚合
└── whitelist-history.toml        # 白名单变更留痕
```

- **PR 自动评论**（由 `xtask bench comment` 渲染，模板固定）：一句结论 + 一张表（`bench | 当前 | 基线 | Δ% | 噪声 | 门槛 | 结论`），只列 `Regress`/`AbsoluteFail`/`Flaky` 行；全绿则一行"✅ 27 benches, 0 regressions, max Δ=+2.1%"。
- **红灯时必须给出的 3 个最可疑 bench**：按 `|Δ| / max(5%, 3·MAD%)` 排序取前 3，并附该 bench 的火焰图命令：
  `perf record -g -F 999 --call-graph dwarf -o <bench>.data -- cargo bench --bench <name> -- --exact`
  以及一句"先看 `<stage>` 阶段"的指引（阶段来自 `FrameReport.stages_p99`，取劣化最大的那一项）。
- **趋势图**只用 p50 与 p99 两条线 + MAD 阴影带，不做移动平均（移动平均会把一次真实台阶抹成斜坡）。

### 4.7 一次 run 的流程

```
xtask ds check → 构建 bench（固定 profile）→ 锁频/独占核 → 逐 bench 采 5 轮
→ judge（对同 commit 的 base 构建，非历史基线）→ 写 jsonl → 生成评论/趋势
```

`base` 的取法与 18 §5 一致：**同一提交构建两次**（`--features bench-base` 的空改动重编译不做，直接两次运行同一二进制取中位作为 base），不用历史基线——因为历史基线跨了 profile/依赖升级，Δ 里混入了构建差异。**首轮不判定**（机器刚从别的负载恢复，首轮同时充当预热与自检）。

## 5 性能预算与基准

### 5.1 门槛的合法性来源

**只有 `00 §7` 的行可以出现在 `Gate.absolute` 里。** 本文不新造绝对数字；凡 `00 §7` 缺行而 bench 又必须有的（§5.3 8 行），先落 `absolute: None`（只判相对退化），并在 PR 里同时提 `00` 的回填，回填完成后再补 `absolute`。这样"缺行"不会阻塞工作，也不会让门槛变成某人随手编的数字。

### 5.2 bench 清单（27 个，权威）

门槛列里 `00 §7` 表示直接引用其数值；`待回填` 表示 §5.3 的缺口。`↔00` 是 `BudgetRow`。

| # | bench | 场景 / 数据集 | 指标 | 门槛 | 方向 | ↔00 |
|---|---|---|---|---|---|---|
| 1 | `cold_first_frame` | 冷启动 → 首帧上屏（`fw500m`，冷 page cache） | p50 ms | **待回填**（建议 <250ms = 打开 200 + 一帧 16 + 余量） | ↓ | 首帧 |
| 2 | `open_4gb` | 只读打开 4 GiB（`fw500m` 复制四份） | p50 ms | <200ms | ↓ | `Open4Gb` |
| 3 | `open_1mb` | 只读打开 1 MiB | p50 ms | <50ms | ↓ | `Open1Mb` |
| 4 | `open_100gb` | 打开 `sparse100g`，不整读 | p50 µs | **待回填**（04 §5.2 实测 <1ms） | ↓ | 100GB 打开 |
| 5 | `scroll_frame_p99` | 恒速滚动 1200 帧，`fw500m` | 帧时间 p99 ms | p99 <25ms（目标 <16ms） | ↓ | `ScrollFrameP99` |
| 6 | `scroll_stages_p99` | 同上，取 8 阶段 `stages_p99` | 各阶段 p99 ms | 每阶段 p99 <16ms，硬上限 25ms（05 §5.1） | ↓ | `ScrollFrameP99` |
| 7 | `jump_90pct` | 跳到 90% 偏移（`sparse100g`） | p50 µs | <5ms | ↓ | `Jump100Gb` |
| 8 | `select_drag_1mb` | 拖拽选区 100 万字节，每帧重算 | 帧时间 p95 ms | <16ms | ↓ | `SelectDrag1Mb` |
| 9 | `piece_edit_1e5` | 1e5 次随机编辑（插入/删除/覆盖混合，`rand1g` 前 64 MiB） | 单步 p99 ms + 曲线斜率 | p99 <1ms 且 `⌈log₂N⌉` 拟合残差 <15%（验证 O(log N)），piece 数 ≤1.15×（04 §5.2） | ↓ | 待回填 |
| 10 | `edit_to_disk_e2e` | 覆盖 100 MiB 后保存（`fw500m`） | 端到端 p50 s | **待回填**（建议 <2s） | ↓ | 待回填 |
| 11 | `search_hex_simple` | 16 B 固定模式 × 1 GiB（`rand1g`） | MB/s | >800 MB/s | ↑ | `SearchHexSimple` |
| 12 | `search_hex_wildcard` | `??` 通配模式 | MB/s | >200 MB/s | ↑ | `SearchHexWildcard` |
| 13 | `search_string_8enc` | 8 编码全开（`text10m`） | MB/s | ≥300 MB/s | ↑ | `SearchString8Enc` |
| 14 | `search_regex` | 有限 `Lmax` 正则（`text10m`） | MB/s | >80 MB/s | ↑ | `SearchRegex` |
| 15 | `search_regex_pathological` | 1 MiB 长串**无匹配** + 无界 `.*` 前缀 | MB/s | **待回填**（08 §5 streaming ≥60 MB/s） | ↑ | 待回填 |
| 16 | `search_numeric_range` | 数值区间（MSB 单值，`rand1g`） | MB/s | >400 MB/s | ↑ | `SearchNumeric` |
| 17 | `diff_two_phase` | `diff100m`（两阶段：粗筛 + 细化） | 总耗时 s + `rss_peak_mb` | <5s（目标 3s）且 RSS <800 MiB | ↓ | `Diff100Mb` |
| 18 | `diff_identical` | 相同 100 MiB 短路 | p50 ms | <400ms（09 §5） | ↓ | `DiffIdentical` |
| 19 | `save_rewrite_1gb` | 无改动重写 1 GiB | MB/s | >500 MB/s | ↑ | `Save1Gb` |
| 20 | `undo_step` | 单步撤销/重做（大块替换后） | p99 µs | <1ms | ↓ | `UndoStep` |
| 21 | `entropy_chunk_1mib` | 1 MiB 分块熵+直方图（`mixed40`，4 线程） | MB/s | **待回填**（10 §5 ≥1.5 GB/s） | ↑ | 待回填 |
| 22 | `magic_identify` | 头 64 KiB + 尾 64 KiB × 206 规则（`fw500m`） | p50 ms | **待回填**（10 §5 <5ms） | ↓ | 待回填 |
| 23 | `viz_first_render` | 字段可视化首渲染三段：缩略图 / 全尺寸 raw / PNG 4096² | 各段 p50 ms | **待回填**（13 §5：8 / 60 / 250 ms） | ↓ | 待回填 |
| 24 | `bspec_compile` | `deepnest` 模板（≈1000 行） | p50 ms | <20ms | ↓ | `BspecCompile` |
| 25 | `bspec_run` | 10 MiB 数据 + 中等模板（`deepnest`） | p50 ms | <100ms | ↓ | `BspecRun` |
| 26 | `annot_query` | 10 万条注释上的区间查询 | p99 µs | <50µs | ↓ | `AnnotQuery` |
| 27 | `mem_rss_p95` | 4 GiB 只读浏览全程 RSS 采样（1 kHz） | `rss_p95_mb` | <500 MiB；编辑场景 <原大小+改动×2 | ↓ | `Mem4GbRo` |

附两条不属于 27 但也判定的子指标：`*_allocs`（帧路径稳态每帧 0 分配；搜索类分配次数不随输入线性增长）与 `scroll_stages_p99` 的阶段分解（#6）。**`00 §7` 的 19 行全部被覆盖**（#2/#3 共用 `Open*`、#5/#6 共用 `ScrollFrameP99`、#17/#18 共用 diff、#27 单独），映射由 `registry.rs` 保证。

### 5.3 需要回填 `00 §7` 的 8 行（附建议数值）

这 8 项本文必须测、`00 §7` 却没有行——缺行意味着"绝对门槛不存在"，只能靠相对退化守，等于放任它在原地慢下去：

| # | 建议新增行 | 建议数值 | 依据 |
|---|---|---|---|
| B1 | 首帧时间（冷启动到第一帧上屏） | <250ms p50 | = 打开 4GB 200ms + 一帧 16ms + 余量 |
| B2 | 100 GB 稀疏镜像打开（不整读） | <5ms p50 | 04 §5.2 实测 <1ms，留 5× 余量 |
| B3 | piece table 1e5 次随机编辑 | 单步 p99 <1ms，piece 数 ≤1.15×，斜率 O(log N) | 04 §5.2 `edit_1m_pieces` |
| B4 | 覆盖编辑到落盘端到端（100 MiB 改动 + 保存） | <2s p50 | 由 "保存 500MB/s" 与写放大 2~3× 反推 |
| B5 | 熵分析 1 MiB 分块吞吐（4 线程） | ≥1.5 GB/s | 10 §5 单遍计数 |
| B6 | magic 识别（头 64 KiB + 尾 64 KiB，206 规则） | <5ms p50 | 10 §5 |
| B7 | 字段可视化首渲染（缩略图 / 全尺寸 raw / PNG 4096²） | 8 / 60 / 250 ms | 13 §5 |
| B8 | 正则最坏情况（长串无匹配、无界模式）吞吐 | ≥60 MB/s（流式单线程） | 08 §5 |

另有三处**口径冲突**必须一起回填，否则门槛自相矛盾：

- `04 §9.3` 与 `05 §5.2` 写"退化 >10% 红灯"，`18 §5` 写" >5% 红灯"——本文统一为 **5%**（`Gate.max_regress_pct` 默认值），前两处须改口径。
- `09 §5` 的注脚"`00 §7` 未列 diff 内存预算"已过期：`00` 已有「Diff 内存峰值 <800MB」，删注脚。
- `18 §6` 的"连续 2 次同向超阈"与本文 §4.3 的 3 次冲突——统一为 **3 次**（理由见 §4.3 ③），`18 §6` 须改。

## 6 失败模式与降级

| 失败 | 行为 | 可见性 |
|---|---|---|
| 判定器自身失准（噪声门槛被撑大→漏判） | 靠 §9.1 的 canary bench 每月反向校准；漏判一次即调 `noise_mult` 并记录 | 月度校准报告 |
| 数据集漂移（sha256/熵不符） | bench 直接 abort，不给数字 | PR 检查项：`ds check failed` |
| 机器温度过高 / 未锁频 | 记录 `env_unstable`，判定降级 `Inconclusive`，不阻塞合并 | PR 评论标 🟡 并附实测温度与频率 |
| 同一 commit 两次结论不一致 | 标 `Flaky`，自动重跑一次；仍不一致则要求人工看趋势 | 不允许 `--force` 合并（18 §5） |
| 帧级 bench 只测到 CPU 侧编码与提交 | **GPU 完成时间不在 `FrameReport` 内**；若 GPU 侧劣化而 CPU 侧不变，本套件看不见 | 明写在 §10 Q2；暂以 `stages_p99.upload` 的间接信号兜底 |
| 无法复现外部依赖波动 | 白名单（§4.5），到期失效 | `whitelist-history.toml` |
| bench 编译失败（被测 API 改动） | CI 红，且**不允许**把 bench 注释掉（`xtask bench audit` 校验 `registry.rs` 完整性） | 与 G1 同一条闸门 |
| 100 GB 数据集所在盘空间不足 | 生成阶段就失败并提示所需空间；不静默降级为更小文件（会让历史趋势失真） | 提示 `GEODE_BENCH_DATA` 剩余空间 |
| 冷启动 bench 受 OS 缓存策略影响 | 白名单 + 冷/热分线；不得为了"稳定"而预读（会让指标失去意义） | 趋势图中冷线单独标注 |

## 7 与其它子系统的契约

| 子系统 | 本文要求它提供 | 为什么没有它本文就测不了 |
|---|---|---|
| `geode-buffer`（04） | `piece_count()`、`undo_stack_bytes()`、`stats()`（读放大/写放大计数） | `piece_edit_1e5` 要断言 piece 上界与 O(log N) 曲线，没有内部计数只能看时间 |
| `geode-ui`（05/16） | `FrameReport` 的 8 阶段计时器，编译期 `perf-panel` feature 开关 | 阶段分解是"哪一帧阶段劣化"的唯一来源 |
| `geode-search`（08） | `SearchStats { bytes_scanned, hits, blocks, elapsed }` | MB/s 必须有分母；`hits` 用于区分"扫完了"与"提前命中截断" |
| `geode-analysis`（10） | `PassPlan` 可分块注入（chunk size 可控）、`entropy_of_chunk(&[u8]) -> f64` 可单独调用 | `entropy_chunk_1mib` 与 `magic_identify` 需要绕开 UI 直接调 |
| `geode-viz`（13） | `measure() -> BytesHint` 与 `render_stage()` 的分离 | `viz_first_render` 三段分别计时，且 `bytes_hint` 与实测 RSS 的偏差是 20 §4.5 I14 的同一条不变量 |
| `geode-diff`（09） | 阶段钩子 `on_phase_end(Phase)`（粗筛 / 细化） | "两阶段耗时"必须分阶段报，否则无法定位是哪一阶段退化 |
| `geode-project`（17） | `save()` 的写放大计数、fsync 开关 | `save_rewrite_1gb` 与 `edit_to_disk_e2e` 要区分"数据量"与"实际落盘量" |
| **18** | `bench.yml` 的 runner 规格、cron、`[profile.bench]`、`GEODE_CI_STRICT` | 编排归 18（§1.3）；本文的判定算法须被 `bench.yml` 调用而非重写 |

### 7.1 需回填的其它文档

| 位置 | 现状 | 应改为 |
|---|---|---|
| `00 §7` | 缺首帧 / 100GB 打开 / piece 编辑曲线 / 覆盖落盘 / 熵吞吐 / magic / 可视化首渲染 / 正则最坏情况 | 补 §5.3 的 B1~B8 八行 |
| `18 §3.1` | `bench/src/` 只列 8 个文件、总 bench 20~30 | 按本文 §5.2 的 27 个 bench 与 `bench/benches/*.rs`；新增 `judge.rs`/`harness.rs`/`report.rs`/`datasets.rs` |
| `18 §5` | 只给"退化 >5% 红灯"与采样规则 | 补"判定算法与 MAD 判别见 21 §4.3"，采信 3 次复现 |
| `18 §6` | "连续 2 次同向" | 改为连续 **3** 次（21 §4.3） |
| `04 §9.3`、`05 §5.2` | "退化 >10% 红灯" | 统一为 5%（21 §5.3） |
| `09 §5` | 注脚"`00 §7` 未列 diff 内存预算" | 删除（`00` 已有该行） |

## 8 配置项与默认值

| 文件 | 键 | 默认 | 范围 / 说明 |
|---|---|---|---|
| `bench.toml` | `bench.max_regress_pct` | 5.0 | 0.5~50；相对退化门槛地板 |
| `bench.toml` | `bench.noise_mult` / `min_samples` | 3.0 / 30 | MAD 显著性倍数；低于 30 样本一律 `inconclusive` |
| `bench.toml` | `bench.runs_per_bench` / `runs_take` | 5 / median | 每次 run 内的重复次数与聚合方式 |
| `bench.toml` | `bench.reproduce_threshold` | 3 | 连续同向次数（§4.3） |
| `bench.toml` | `bench.base_mode` | `"same-commit"` | `same-commit`｜`previous-commit`（后者仅用于趋势，禁判定） |
| `bench.toml` | `bench.always_on` / `bench.sample_gpu` | `false` / `false` | 常驻 perf 采样与 GPU 时间戳（§10 Q1/Q2） |
| `bench.toml` | `bench.whitelist_max` | 5 | 超过需 RFC（§4.5） |
| `frame` | `frame.ring_capacity` / `frame.warmup_frames` | 4096 / 60 | 环形缓冲容量；预热帧数（丢弃） |
| `frame` | `frame.scroll_frames` | 1200 | 滚动脚本帧数（20s@60fps） |
| `frame` | `frame.stages_enabled` | `perf-panel` feature | 8 阶段计时开关（关掉可省 ~2% 帧时间） |
| `env` | `GEODE_BENCH_DATA` | `~/.cache/geode/bench-ds` | 数据集根；遵循 XDG |
| `env` | `GEODE_BENCH_JUDGE` | `1`（自管 runner）/ `0`（共享 runner） | 0 = 只采集趋势 |
| `env` | `GEODE_BENCH_CPU_SET` | `4-11` | `taskset` 掩码，与 `isolcpus` 一致 |
| `noise` | `noise.max_temp_c` / `noise.max_mad_pct` | 85 / 25 | 超限 → `env_unstable` → `inconclusive` |

## 9 测试策略

基准套件是"测量仪器"，仪器自己必须被校准。本节是它的测试。

### 9.1 canary bench：判定器的自检

判"通过"有两种原因：没退化，或者判定器坏了。三个 canary（编译在 `feature = "bench-canary"` 下）区分它们：

| canary | 注入 | 期望 |
|---|---|---|
| `canary_stable` | 空转 `sleep(1ms)` 的固定工作 | 连续 10 次 `Pass`（验证无假阳性，误报率 <1% 的实测手段） |
| `canary_regress_20pct` | 在基线路径上多跑 20% 工作量 | 3 次内必判 `Regress` |
| `canary_mad_trap` | 工作量不变但人为插入 30% 抖动的 `sleep` | 判 `Inconclusive`/`Flaky`，**不得**判 `Regress`（验证"噪声不冒充退化"） |

三个 canary 每次 `bench.yml` 都跑（总耗时 <20s），任一不符则本次所有 bench 的结论作废。

### 9.2 数据集自检

`xtask ds check` 断言：① 每个 manifest 的 `sha256`/`size` 与实际一致；② `entropy_measured` 与实测差 <0.01；③ `sparse100g` 的 `du` <32 MiB（否则不算稀疏，`jump_90pct` 会失去意义）；④ `deepnest` 模板确实 256 层（解析后断言）；⑤ 全部文件只读打开（`mode 0444`）。

### 9.3 判定器的单元测试

`judge()` 是纯函数，因此可以直接用合成序列测：给一段 `(value, mad)` 序列断言输出的 `Status` 序列。固定用例包括：单次尖峰 → `Flaky`；连续 3 次 7% → `Regress`；6% 但 MAD 折算噪声 8% → `Pass`；`AbsoluteFail` 优先于相对判定；`env_unstable` 覆盖一切。这条测试是本文最重要的单测——**门槛的逻辑错了，27 个 bench 一起骗人**。

### 9.4 每月噪声校准

每月首个工作日跑一次 `calibration`：同一二进制连续 20 次全套 bench，统计每个 bench 的实测 MAD 分布。产出两件事：① `noise_mult` 与 `min_samples` 是否需要调；② 若某 bench 的固有 MAD >8%，直接进白名单候选（因为 5% 分辨力已不可达）。校准报告进 `bench-data/calibration/`。

### 9.5 criterion 的正确用法（与反用法）

用：`iter_custom` 手动控制批量、`Throughput::Bytes` 让 MB/s 自带分母、`black_box` 包裹输入与输出、每个 bench 独立 `bench_with_input` 避免编译器跨 bench 内联。
**不用**：`--save-baseline`（会诱使跨 profile 比较，18 §5 已禁）、`criterion::BenchmarkId` 做参数扫描当门槛（那是探索，不是门禁）、把 `assert` 写进 bench 循环（断言成本混进指标）。

## 10 开放问题

| # | 问题 | 候选 | 需要什么数据 |
|---|---|---|---|
| Q1 | 是否把 `perf` 采样做成常驻（`bench.always_on`） | ① 只红灯时采 ② 每次 run 都采（+15% 时长，能得到"帧时间劣化↔函数热点迁移"的长期对照） ③ 不采 | 夜间 full 的剩余时间预算；`perf` 在 `isolcpus` 机器上的采样丢失率 |
| Q2 | 是否对 GPU 帧时间做专测（`bench.sample_gpu`） | ① 用 `wgpu` timestamp query 取 GPU 侧完成时间 ② 只测 CPU 侧（当前）③ 用外部工具（RenderDoc）离线 | timestamp query 在 Metal/Vulkan/DX12 后端的支持度差异；是否需要固定驱动版本（与"可复现"冲突） |
| Q3 | 是否给插件作者提供 bench 模板 | ① 提供 `geode-bench-template` ② 只公开 `registry.rs` 的 schema | 插件性能是否属于我们的责任范围（14 未定）；模板会让第三方数字与我们的门槛混在一起 |
| Q4 | 100 GB 数据集放在真实盘还是 ramdisk | ① 真实 NVMe（真实但有 5% 抖动）② ramdisk/tmpfs（稳定但不是真实 IO）③ 两者都跑 | `open_100gb` 与 `jump_90pct` 在两种介质上的实测差；tmpfs 下 `mlock` 行为 |
| Q5 | 趋势图是否对外公开 | ① 只内部 ② 公开只读站点（吸引贡献者，但会被当性能宣称） | §1.2 的"不做绝对宣称"是否与公开趋势冲突；需要法务/文案口径 |
| Q6 | 首帧"上屏"的语义如何界定 | ① `present()` 返回 ② 首个 vsync ③ 首帧像素被 compositor 合成 | 三者相差 1~2 帧（16~33ms），足以让门槛的判定结论反转 |
