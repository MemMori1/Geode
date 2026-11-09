# 18 — 工程计划（目录结构 / CI / 测试 / 里程碑）

> 本文**拥有**：workspace 目录结构、crate 允许依赖矩阵与其 CI 强制、feature flags、CI 流水线、测试金字塔与数量目标、里程碑 M0~M7 与人力、风险登记册、开发实践、发布分发、许可与法律。
> 本文**不得涉及**：任何子系统的内部设计（见 04~17）、性能预算的数值定义（00 §7 定义，本文只做 CI 落地映射）。
> 与 00 的关系：00 §3 的分层是本文 §3.2 依赖矩阵的直接来源，00 §7 的预算是本文 §5 与 §9 的红灯阈值。冲突以 00 为准。

| 必备主题 | 位置 |
|---|---|
| 1 目标与非目标 / 2 依赖与边界 / 3 数据结构与接口 | §1 / §2 / §3（"接口"= crate 依赖面与 feature 面） |
| 4 核心算法·流程 / 5 性能预算与基准 | §4（一次提交到一次发布的流程）/ §5 |
| 6 失败模式与降级 / 7 与其它子系统的契约 | §6 / §7 |
| 8 配置项与默认值 / 9 测试策略 | §8 / §9 |
| 里程碑 / 风险 / 实践 / 发布 / 法律 / 开放问题 | §10 / §11 / §12 / §13 / §13 的法律与许可段 / §14 |

## 1 目标与非目标

**五条工程目标**（每条都可被机器判定）

| # | 目标 | 判定方式 |
|---|---|---|
| E1 | **默认构建零 C/C++ 依赖**：任何 `cargo build` 不需要 cc/gcc/nasm | CI 对产物跑 `cargo tree -e normal \| grep -Ec '(^| )(cc\|cmake\|bindgen)$'` 必须为 0 |
| E2 | **分层不可违反**：除 `geode-ui`/`app` 外无人能链接 GUI，依赖方向不反向 | `cargo xtask deps` 在 11 分钟内失败并指出违规边（§3.3） |
| E3 | **性能退化会被拦住**：00 §7 任一条超预算，或对比 main 退化 >5%，CI 红灯 | `bench.yml` 与 `bench-baseline.json` 对比（§5） |
| E4 | **三平台同一二进制可交付**：本地 `cargo test` 通过的代码，CI 五目标全绿 | 目标矩阵 `x86_64/aarch64` × `linux/macos/windows` |
| E5 | **文档与代码不漂移**：每条设计决策在代码里有 `// DESIGN:` 回链，且文档改了对应测试没改会红 | `cargo xtask docs` 校验 DESIGN 锚点存在于 docs/ |

**非目标**：多仓库拆分（单 workspace，M6 后才考虑抽出 `geode-core` 供外部复用）；容器化开发环境；自建 CI runner 农场（除 §5 的一台基准机）；支持 32 位目标或 wasm（`usize` 与 `Offset(u64)` 的取舍见 00 §4）。

## 2 依赖与边界

| 项 | 取值 | 理由 / 影响 |
|---|---|---|
| 语言 / 版本 | Rust edition 2024，MSRV `1.85.0`，`rust-toolchain.toml` 固定 stable | edition 2024 需要 1.85+；MSRV 进 CI 单 job，破坏 MSRV 必须显式 bump 并在 CHANGELOG 说明 |
| 异步运行时 | **不引入 tokio** | 00 §6：自研 job 系统（crossbeam-channel + rayon）。谁引 tokio 谁解释 |
| 日志 / 追踪 | `tracing` + `tracing-subscriber` | 仅 app 层初始化，库 crate 只 `emit` |
| CI | GitHub Actions（公开仓库，免费额度）+ 一台自管 `bench-runner`（Linux x86_64 裸金属） | 基准必须在稳定机器上跑，共享 runner 噪声 >10% |
| 内容分发 | 静态对象存储 + 签名的 `index.json` | 见 15-content-distribution；CI 不依赖其可用性 |
| AI | 可选特性，默认仅编译客户端；CI 用 mock server | 绝不让 CI 依赖外部 API key |
| 人力前提 | **1 名全职核心（作者）+ 0.5 名兼职（UI/文档/发布）**；§10 的工时按"1 熟手"计 | 单点风险见 §11 R6 |
| 时间盒 | 每 2 周一个 sprint，每 sprint 末一次可运行 demo | 里程碑不按日期硬排，按"人日 + 依赖"排 |

**边界决策**：① 只依赖 crates.io（`vendor/` 仅用于发布复现，不用于日常开发）；② 引入任何含 C 代码的 crate 必须走 feature flag 且默认关（§3.4）；③ 不引 `libmagic`（C）——magic 用移植表 + 纯 Rust 匹配，见 10-analysis-toolkit。

## 3 数据结构与接口（仓库结构 / crate 依赖面 / feature 面）

### 3.1 目录树

```
geode/
├── Cargo.toml                 # [workspace] resolver="3"；members = crates/* + app + xtask + bench
├── Cargo.lock                 # 提交（应用产物，非库）
├── rust-toolchain.toml  deny.toml  clippy.toml  rustfmt.toml  .editorconfig
├── LICENSE-MIT  LICENSE-APACHE  README.md  CHANGELOG.md  CONTRIBUTING.md  SECURITY.md
├── .github/workflows/{ci.yml, bench.yml, fuzz.yml, nightly-full.yml, release.yml, docs.yml}
├── app/src/{main.rs, cli.rs, app.rs, wiring.rs, recovery.rs, jobs.rs, updates.rs, crash_hooks.rs}
├── crates/
│   ├── geode-core/src/{lib.rs, offset.rs, range.rs, diag.rs, value.rs, ids.rs, budget.rs,
│   │                   version.rs, error_codes.rs}   # error_codes.rs 由 xtask docs-errors 生成，勿手改（见 10.2 P1）
│   ├── geode-buffer/src/{lib.rs, piece.rs, tree.rs, undo.rs, patch.rs, save.rs, journal_sink.rs}
│   ├── geode-format/src/{lib.rs, version.rs, migrate.rs, detect.rs, geo_ws.rs, geo_proj.rs,
│   │                   geo_pkg.rs, geo_patch.rs, geo_magic.rs, geo_journal.rs, geo_lock.rs}  # 7 格式的读写与迁移，规范见 23
│   ├── geode-provider/src/{lib.rs, file.rs, mapped.rs, memory.rs, process.rs, gdb.rs, ssh.rs, net.rs, holes.rs}
│   ├── geode-annot/src/{lib.rs, layer.rs, index.rs, interval.rs, property.rs, slot.rs}
│   ├── geode-analysis/src/{lib.rs, hash.rs, entropy.rs, histogram.rs, magic.rs, strings.rs, decode.rs}
│   ├── geode-viz/src/{lib.rs, registry.rs, desc.rs, measure.rs, offsetmap.rs, render_ctx.rs, builtin/{mod.rs, image.rs, mesh.rs, timeline.rs, audio.rs, matrix.rs}}
│   ├── geode-bspec/src/{lib.rs, lexer.rs, parser.rs, ast.rs, sema.rs, codegen.rs, vm.rs, verifier.rs, diag.rs, incremental.rs}
│   ├── geode-proc/src/{lib.rs, graph.rs, node.rs, eval.rs, cache.rs, builtin/{mod.rs, xor.rs, decrypt.rs, unpack.rs}}
│   ├── geode-search/src/{lib.rs, engine.rs, hexpat.rs, text.rs, regex.rs, numeric.rs, results.rs}
│   ├── geode-diff/src/{lib.rs, anchors.rs, myers.rs, align.rs, render.rs}
│   ├── geode-disasm/src/{lib.rs, backend.rs, capstone.rs, cache.rs}          # feature "disasm"
│   ├── geode-yara/src/{lib.rs, engine.rs, ruleset.rs, hits.rs}               # feature "yara"
│   ├── geode-migrate/src/{lib.rs, ir.rs, origin.rs, from_bt.rs, from_hexpat.rs,
│   │                     from_ksy.rs, from_c.rs, to_c.rs, to_ksy.rs, emit.rs}  # 见 24
│   ├── geode-project/src/{lib.rs, config.rs, workspace.rs, project.rs, schema.rs, migrate.rs,
│   │                    journal.rs, snapshot.rs, secrets.rs, lock.rs, recent.rs, atomic.rs, export.rs}
│   ├── geode-ai/src/{lib.rs, client.rs, sampler.rs, context.rs, prompt.rs, verifier_bridge.rs,
│   │                repair.rs, confidence.rs, provenance.rs, eval.rs}     # feature "ai"
│   ├── geode-content/src/{lib.rs, index.rs, source.rs, cache.rs, update.rs}
│   ├── geode-plugin-host/src/{lib.rs, abi.rs, vtable.rs, loader.rs, sandbox.rs, host_api.rs}   # feature "plugin"
│   └── geode-ui/src/{lib.rs, shell.rs, theme.rs, i18n.rs, fonts.rs, keymap.rs,
│                   renderer/{mod.rs, grid.rs, text.rs, painter.rs},
│                   views/{hex.rs, structure.rs, inspector.rs, diff.rs, disasm.rs,
│                          search.rs, yara.rs, processor.rs, vis/},
│                   widget/{mod.rs, splitter.rs, table.rs, dialog.rs}}    # ← 唯一允许 egui/wgpu 的库
├── docs/  {00-architecture-overview.md … 26-visual-design.md, adr/, img/{mockup.html, pages/}, book/}
├── bench/src/{lib.rs, registry.rs, ...}   # 27 项 bench 的权威清单在 21 §5.2，此处不重复列举
├── fuzz/fuzz_targets/{...}                # 25 个 target 的权威清单在 20 §3.4，此处不重复列举
├── testdata/{samples/, golden/, corpus/<kind>/<name>/{meta.toml,payload}, vectors/<format>/,
│             schemas/<format>/v*.json, bad/, crash/}                         # 见 20/23
├── scripts/{bootstrap.sh, gen-golden.sh, check-size.sh, notarize.sh, build-flatpak.sh, aur-push.sh}
└── xtask/src/{main.rs, deps.rs, golden.rs, size.rs, docs.rs, release.rs}
```

**约定**：每个 crate 顶部 `//!` 必须有一行 `//! DESIGN: <文档> §<节>`；`pub` 项必须有文档注释（`#![deny(missing_docs)]` 在 12 个内核 crate 上开启，ui 不开）。

### 3.2 允许依赖矩阵（层号即约束，CI 强制）

层号只能**严格下降**：`L4 app → L3 ui → L2 {project, ai, content, plugin-host, migrate} → L1 {analysis, bspec, viz, proc, search, diff, disasm, yara} → L0 {buffer, provider, annot, format} → L-1 {core}`。同层依赖只允许在 §3.2 显式列出，未列出即违规。（`geode-migrate` 依赖 L1 的 `geode-bspec`，所以必须记 L2，不能因为"概念上属于模板旁支"就记 1；`geode-analysis` 与 `geode-bspec` 同为 L1 但互不依赖。）

| crate | 层 | 允许的内部依赖 |
|---|---|---|
| geode-core | -1 | 无（叶子；只依赖 serde/thiserror） |
| geode-buffer | 0 | core, provider（同层显式例外：buffer 的职责是"从 Provider 读并叠加编辑语义"，方向恒为 buffer → provider，反向禁止） |
| geode-provider | 0 | core |
| geode-annot | 0 | core |
| geode-format | 0 | core（+ serde/toml/zip 只读；**禁止 provider/buffer**：只认字节与文本，不持有数据源） |
| geode-analysis | 1 | core, provider |
| geode-bspec | 1 | core（读写字节只通过 `&dyn Provider`，不依赖 buffer） |
| geode-viz | 1 | core, bspec(`Node`/attrs), buffer, annot（**禁止 egui/wgpu/winit**） |
| geode-migrate | 2 | core, bspec(AST + 编译期校验)，可选 clang（feature `from-c`）（**禁止 egui/wgpu/winit、禁止 provider**：转换器只处理文本，不碰文件字节） |
| geode-proc | 1 | core, analysis |
| geode-search | 1 | core, provider |
| geode-diff | 1 | core, provider |
| geode-disasm | 1 | core, provider（+ 可选 capstone） |
| geode-yara | 1 | core, provider（+ 可选 yara-x） |
| geode-project | 2 | core, provider, buffer（仅 `EditOp`/`version()`）, bspec(`TemplateRef`), proc(`Graph` 的 serde) |
| geode-ai | 2 | core, bspec, project(`SecretRef`) |
| geode-content | 2 | core, bspec（模板装载） |
| geode-plugin-host | 2 | core, annot, provider, viz（同层显式例外：注册插件可视化器） |
| geode-ui | 3 | **全部 L2/L3 + L1/L0**，且唯一可依赖 egui/egui-wgpu/wgpu/winit/crossbeam |
| app | 4 | 全部 + geode-ui；外加接线所需的 core / provider / buffer / format（app 是唯一允许"拿具体 Provider 实现组装"的层） |

三条机器可判定的规则：**①** 除 `geode-ui`/`app` 外任何 crate 的依赖树中不得出现 `egui*`/`wgpu`/`winit`/`arboard`/`egui_kittest`；**②** `dep.layer >= pkg.layer` 即失败；**③** 目标不在上表 = 失败（防止"顺手加个依赖"）。**允许例外只有一条**：`--cfg test` 下可依赖 `egui_kittest`（仅 `geode-ui`）。任何例外必须写进本节表格，不允许内联 `#[allow]` 绕过。

### 3.3 `cargo xtask deps` 伪代码

```
fn check_layers(md: &cargo_metadata::Metadata) -> Result<()> {
    let layer = LAYERS;                        // §3.2 表，硬编码，改动需 PR 改本节
    let allowed = ALLOWED;                     // pkg -> 允许 dep 集合
    let mut viol = vec![];
    for pkg in md.workspace_packages() {
        for d in pkg.deps.filter(|d| d.is_internal()) {
            let (l, ld) = (layer[pkg], layer[d]);
            if d.name.is_gui() && !pkg.name.is_ui_or_app() { viol.push(E0101(pkg, d)); }   // ① GUI 污染
            if ld >= l                                   { viol.push(E0102(pkg, d, l, ld)); } // ② 方向
            if !allowed[pkg].contains(d)                 { viol.push(E0103(pkg, d)); }        // ③ 白名单
        }
        for d in pkg.deps_all_features() { if d.is_cc_backed() && !d.feature_gated_in(pkg) {
            viol.push(E0104(pkg, d)); } }                                                   // ④ C 依赖须有 flag
    }
    // ⑤ 反向检查：以外部 crate 视角看谁把 GUI 拽进来（防传递依赖）
    let t = run("cargo tree -e normal -i egui --workspace --exclude geode-ui --exclude app");
    if !t.stdout.trim().is_empty() { viol.push(E0105(t.stdout)); }
    if viol.is_empty() { Ok(()) } else { print_grouped(viol); bail!("{} 项依赖违规", viol.len()) }
}
```
输出格式固定为 `E01xx <pkg> -> <dep>: <一句话>  (见 18-engineering-plan §3.2)`，让报错本身就指向修复方式。

### 3.4 Feature flags

| flag | 默认 | 解锁内容 | 传递依赖 | 二进制增量 | 干净构建增量 | C/C++ |
|---|---|---|---|---|---|---|
| `cli` | **on** | `app --headless`：`hex search/edit/dump`、脚本化处理 | clap | +0.6MB | +8s | 否 |
| `ai` | **on** | 03-ai-dsl-generation 全链路 | reqwest(rustls)+serde | +4.2MB | +70s | 否（rustls，不用 openssl） |
| `disasm` | off | 11-disassembler 的 x86/arm64 后端 | capstone-sys | +3.4MB | +45s（含 C 编译） | **是** |
| `yara` | off | 12-yara 的规则引擎 | yara-x | +6.1MB | +95s | 否（纯 Rust） |
| `plugin` | off | 14-plugin-system 的 `.geoplug` 装载 | libloading | +0.2MB | +3s | 否 |
| `full` | — | `cli+ai+disasm+yara+plugin` | — | ≈31MB | ≈7min | **是** |

**决策：CI 主矩阵只构建默认特性，`full` 只跑 2 个平台 + 每日一次。** 理由：默认构建 0 C 依赖（E1）意味着 `cargo test` 在任何机器上 5 分钟能跑完；把 C 工具链的失败面限制在一个 job 里，三平台矩阵才不会天天红。**`full` 必须至少在一个 job 里通过编译 + 跑 `geode-disasm`/`geode-yara` 的单元测试**，否则这两个特性会在发布前才第一次被编译。缺特性必须编译失败而非静默降级（`compile_error!` 在误开时给明确提示）。

## 4 核心算法 / 流程（一次提交、一次发布）

**提交路径（目标 < 12 分钟拿到绿灯）**：`pre-commit` 跑 fmt + `cargo xtask deps`（本地 2s）→ push → PR 触发 `ci.yml`（fmt→deps→clippy(3)→test(5)→doc，并行）→ 若改动了 `crates/**` 且 PR 标签 `bench` 或触及 `geode-buffer|geode-bspec|geode-search|geode-ui` 则跑 `bench.yml` → 双人 review（数据修改路径必须核心作者审）→ squash merge。**`main` 永远可发布**：任何一次合并都产出一个通过全部绿灯的候选版本。

`ci.yml` 的 job 结构（示意）：

```yaml
on: [push, pull_request]
jobs:
  fmt:    { runs-on: ubuntu-24.04, steps: [checkout, cache, "cargo fmt --all --check"] }
  deps:   { runs-on: ubuntu-24.04, steps: [checkout, "cargo xtask deps"] }
  clippy: { strategy: { matrix: { os: [ubuntu-24.04, macos-14, windows-2025] } },
            runs-on: ${{ matrix.os }}, steps: [checkout, cache,
              "cargo clippy --workspace --all-targets -- -D warnings"] }
  test:   { strategy: { matrix: { os: [ubuntu-24.04, ubuntu-24.04-arm, macos-13,
                                      macos-14, windows-2025] } },
            runs-on: ${{ matrix.os }}, steps: [checkout, cache, "cargo test --workspace --locked"] }
  full:   { runs-on: ubuntu-24.04, steps: [checkout, cache,
              "cargo test --workspace --features full"] }        # 唯一带 C 依赖的 job
  doc:    { runs-on: ubuntu-24.04, steps: [checkout, cache,
              "RUSTDOCFLAGS=-D warnings cargo doc --no-deps --workspace",
              "cargo xtask docs", "mdbook build docs/book"] }
  deny:   { runs-on: ubuntu-24.04, steps: [checkout, "cargo deny check", "cargo audit"] }
  size:   { runs-on: ubuntu-24.04, steps: [checkout, "cargo xtask size --budget +3%"] }
```
`bench.yml` 只在 `pull_request` 且 `paths` 命中性能敏感 crate、或 workflow_dispatch 时跑；`fuzz.yml` 用 `schedule: cron "0 2 * * *"`；`nightly-full.yml` 用 `cron "0 4 * * *"` 跑全特性 + 完整基准归档。

**发布路径**：打 `v0.x.y` 标签 → `release.yml` 五平台构建（`full`）→ 生成 SBOM 与校验和 → macOS 公证 + Windows 签名 → 上传草稿 release → 冒烟测试（下载 → 运行 `--version` → 打开 `testdata/samples/zip_small.zip` → 截图）→ 人工点发布 → 更新官网与 mdBook。任一步失败则 release 保持草稿，绝不留半成品给用户。

**文档同步路径**：设计变更必须先改 `docs/`（PR 里 docs 与代码同一提交），`cargo xtask docs` 校验：① 每个 crate 的 `DESIGN:` 锚点在 docs 里真实存在（正则匹配 `§\d+(\.\d+)?`）；② 每个里程碑表格里的文档编号存在；③ mdBook 相对链接可解析。**只改代码不改文档的 PR 会被这个 job 拦下。**

## 5 性能预算与基准（00 §7 落成门槛）

| 基准（`bench/`） | 对应 00 §7 条目 | 硬预算 | 对比 main 的回退阈值 |
|---|---|---|---|
| `open_4gb` / `open_1mb` | 打开只读 4GB<200ms、1MB<50ms | 200ms / 50ms | +5% 红灯 |
| `scroll_p99` | 滚动帧 <16ms、p99 <25ms | p99 25ms | +3% 红灯（帧时间是用户体感最敏感的） |
| `jump_100gb` | 跳转 100GB <5ms | 5ms | +5% |
| `select_1mb` | 100 万字节选区 <16ms/帧 | 16ms | +5% |
| `search_{simple,wildcard,regex}` | 800 / 200 / 80 MB/s | 见 00 §7 | −5% 吞吐红灯 |
| `bspec_{compile,run}` | 编译<20ms、执行<100ms | 见 00 §7 | +5% |
| `annot_query` | <50µs @10 万条 | 50µs | +5% |
| `save_1gb` / `undo_step` | >500MB/s、<1ms | 见 00 §7 | −5% / +5% |
| `mem_4gb_readonly` | <500MB | 500MB | +3%（RSS 峰值） |

**判定规则**：在自管 `bench-runner` 上跑，`criterion` 采样 3 轮取中位数；相对 main 的**同一提交构建**比较（不用历史基线，避免机器漂移）；噪声抑制——首轮不判定、样本 `n≥30`、同时报告 p50 与 p99；**基准红灯不允许 `--force` 合并**，只能改代码或（在 CHANGELOG 里写明理由的）改预算。每日一次 `nightly-full` 跑完整基准并归档，用于长期趋势图，不判定。

## 6 失败模式与降级

| 失败 | 行为 | 用户/维护者可见 |
|---|---|---|
| 基准 runner 抖动 | 连续 3 次同向超阈才判定失败；否则标 `inconclusive` 并自动重跑一次（判据见 21 §4.3） | PR 里贴 p50/p99 与变异系数（CV>5% 直接判 inconclusive） |
| 测试 flaky | 自动重跑 ≤2 次；**任何被重跑救活的测试必须当天开 issue 并标 `flaky`**，两周内不修则移出必跑集 | `#flaky` 标签 + 周报计数，超过 5 个暂停合并 |
| 夜间 fuzz 崩溃 | 自动留存 crash 输入到 `testdata/bad/`、开 P0 issue、阻塞下一次发布 | issue 自动带最小复现命令 |
| 公证/签名失败 | 发布中止（草稿保留）；Apple/Windows 凭据失效自动告警；**绝不允许发布未签名产物** | release 页面不更新，用户无感 |
| 依赖安全公告 | `deny` job 每日跑 `cargo-audit`；CRITICAL 当日修，HIGH 一周内修，MEDIUM 进 backlog | issue + 周报 |
| 上游 crate yank/破坏 | 锁定精确版本 + `Cargo.lock` 提交；`deny.toml` 的 `[bans]` 禁止同一 crate 多版本 | CI 红灯 + 升级 PR |
| CI 总时长膨胀 >15min | 拆 job、缓存 `target/`、目标平台降为 `check` 而非 `build`（test 只在 3 个主平台跑） | 周报里的 CI 时长曲线 |
| 内容源/CDN 不可用 | 应用侧降级为本地缓存（见 15），CI 不依赖 | 无 |

## 7 与其它子系统的契约

| 文档 | 拥有 | crate | 验收测试锚点 |
|---|---|---|---|
| 04-buffer-and-edit-model | piece table、撤销、patch、保存事务 | geode-buffer | `proptest` 对拍 + 崩溃测试 |
| 05-render-and-hexview | 网格渲染与列配置 | geode-ui/renderer | `scroll_p99` 基准 + UI 快照 |
| 06-data-providers | Provider 实现与容量探测 | geode-provider | `fuzz/provider_protocol` |
| 07-data-processor-graph | 节点图求值 | geode-proc | 黄金文件 + 缓存一致性 |
| 08-search-subsystem | 四引擎 | geode-search | 三档吞吐基准 + 黄金匹配集 |
| 09-diff-subsystem | 锚点 + 精细 diff | geode-diff | `proptest` 对拍朴素 LCS |
| 10/11/12/13 | 分析工具 / 反汇编 / YARA / 可视化器 | geode-analysis, disasm, yara, ui | 各 crate 单测 + `full` job |
| 14-plugin-system | ABI vtable 与装载 | geode-plugin-host | ABI 版本协商测试 |
| 15-content-distribution | 索引与更新 | geode-content | 索引签名校验 |
| 16-ui-shell-theme-i18n | 布局、主题、i18n | geode-ui | `egui_kittest` 快照 |
| 17-project-and-persistence | 配置/工作区/项目/journal | geode-project | 迁移矩阵 + 崩溃模拟 |

**跨子系统契约的落地方式**：任何跨 crate 的 trait 变更（如 `Provider`/`ByteBuffer`/`EditOp`）必须在本文 §3.2 与 00 §5 同时更新，并新增一个 `testdata/golden/abi/*.json` 冻结快照；快照变化 = 破坏性变更，走 §12 的版本规则。

## 8 配置项与默认值

| 配置文件 | 关键项 | 默认 | 说明 |
|---|---|---|---|
| `rust-toolchain.toml` | `channel` / `components` | `stable` / `rustfmt,clippy` | MSRV job 单独用 `1.85.0` 校验 |
| `[profile.release]` | `lto` / `codegen-units` / `strip` / `panic` | `"fat"` / `1` / `"debuginfo"` / `"abort"` | 体积与速度优先；`panic=abort` 会禁用 `catch_unwind`，插件宿主因此用 `unwind`（见 14） |
| `[profile.bench]` | `debug` / `lto` | `1` / `"thin"` | 保留行号供火焰图 |
| `.cargo/config.toml` | `rustflags`（linux） | `-C target-cpu=x86-64-v2` | **不上 v3**，避免老机器非法指令 |
| `deny.toml` | `licenses.allow` | `MIT, Apache-2.0, BSD-3-Clause, ISC, Unicode-3.0, Zlib` | 其余一律人工审（§13.4） |
| CI 环境 | `RUSTFLAGS` / `CARGO_INCREMENTAL` | `-D warnings`（clippy job） / `0` | 增量缓存交给 `Swatinem/rust-cache` |
| 构建开关 | `GEODE_CI_STRICT` / `GEODE_NO_NETWORK` | `1` / `1` | 测试中禁止访问网络（AI 用 mock） |
| 版本号 | `workspace.package.version` | `0.1.0` | 全 crate 同版本，见 §12 |

## 9 测试策略（金字塔 + 数量目标）

| 层 | 数量目标（M3 时 / M7 时） | 工具 | 运行时机 |
|---|---|---|---|
| 单元测试 | 350 / 600+ | `#[test]` | 每次 PR，5 平台 |
| 黄金文件对拍 | 45 / 120 | 快照 + `insta` 风格目录比对 | 每次 PR |
| property test | 12 / 25 | `proptest`（默认 256 case，夜间 4096） | 每次 PR（小规模） |
| fuzz | 6 targets | `cargo-fuzz` + libFuzzer | 每日 02:00 UTC，每 target 5 分钟 |
| 集成测试 | 15 / 40 | 端到端 CLI + 库级 | 每次 PR（3 主平台） |
| UI 快照 | 10 / 30 | `egui_kittest` + 固定字体 | 每次 PR（linux + macos） |
| 基准 | 20 / 30 | `criterion` | PR 命中时 + 每日 |

- **单元**：每个 crate 至少覆盖公开 API 的边界值（`Offset` 溢出、`ByteRange::clamp` 越界、`Diag` 序列化）。目标行覆盖：`geode-core`/`geode-buffer`/`geode-bspec` ≥ 85%，其余 ≥ 70%；**覆盖率只做趋势，不做门槛**（防凑数的无意义断言）。
- **黄金文件**：`testdata/samples/` 放 ≤ 2MB 的合成与真实样本（ZIP、PNG、PE、ELF、SQLite、FAT16 镜像各一），`testdata/golden/<sample>/<模板版本>.json` 存期望结构树；**字节级稳定**——任何排序/浮点/时间格式化都必须确定，否则黄金文件会天天变。
- **property**：piece table 与 `Vec<u8>` 对拍（操作序列含 insert/remove/overwrite 混合，1 万次随机操作）；diff 与朴素 LCS 对拍；`byte_offset ↔ display_cell` 映射表往返；journal 回放（见 17 §9.2）。
- **fuzz**：六个入口全部要求 `#![no_main]` + 内存上限 + 5 秒超时；**任何 panic/abort 都算 bug**（含 `unwrap`）；BinSpec 的 fuzzer 必须能发现"预算耗尽被正确报告"而非挂死。
- **集成**：`open → 解析 → 搜索 → 编辑 → 崩溃 → 恢复 → 保存 → 重开 → 字节比对` 全链路，用 `testdata/crash/` 的钩子（17 §9.3）。
- **UI 快照**：固定 DPI、固定字体（打包 `testdata/fonts/`）、固定主题，只比较 hex 网格区域与关键面板；**允许 0.1% 像素差**，超过则更新基线必须说明原因。
- **禁止**：任何测试依赖真实网络、真实时钟（用注入的 `Clock`）、或用户 home 目录；测试并行度必须让 5 平台都在 5 分钟内完成。

## 10 里程碑（M0~M7）

| 里程碑 | 目标 | 涉及 crate / 文档 | 可演示的验收标准 | 人日 | 依赖 |
|---|---|---|---|---|---|
| **M0 骨架** | 最小可验证闭环：打开文件 + 只读 hex 视图 + 60fps | core, buffer(空壳), provider, annot, ui, app；00 05 06 18 | ① 打开 4GB 文件 <200ms 且内存 <500MB；② 4GB 文件滚动 p99 <25ms 持续 30s 无掉帧；③ `cargo xtask deps` 与 5 平台 CI 全绿；④ `--version`/`--help` 可用 | 30 | — |
| **M1 编辑与持久化** | Buffer 编辑 + 撤销 + patch + 保存事务 + 崩溃恢复 | buffer, **format**, project, app；04 17 23 | ① 在 4GB 文件 0x8000_0000 处插入 16 字节，1 秒内可见；② 1 万次随机编辑后撤销全部，字节与原文件逐字节相等；③ `kill -9` 后重启，提示"恢复 N 条修改"并成功恢复；④ 保存 1GB >500MB/s 且断电模拟后原文件不损坏；⑤ `.geopatch`/`.geojournal`/`.geows`/`.geoproj` 的 `schemas/*/v1.json` 随 M1 打 tag，`tests/vectors/` 每格式 ≥3 个损坏向量在 CI 中产出指定的 `EB` 码 | 40 | M0 |
| **M2 视图完备** | 可配行列 + 选区 + 高亮层 + Inspector + 四引擎搜索 + 哈希/熵/分布 | ui, annot, search, analysis；05 08 10 16 | ① 每行 8/16/32/64、分组 1/2/4/8 全部可切换且列对齐不裂；② 100 万字节矩形选区拖拽 <16ms/帧；③ 四引擎在 1GB 文件上达到 00 §7 吞吐；④ Inspector 全类型解码可反写 | 35 | M1 |
| **M3 BinSpec v0.1** | 编译器 + VM + Verifier + 结构树 + 双向联动 + magic 自动加载 | bspec, ui, project；02 03(A0/A1) | ① 用 ZIP 模板解析真实 zip，结构树与黄金文件一致；② 点结构树节点跳到对应 hex，点 hex 反查最深节点 <10ms；③ 编译 1000 行 <20ms、执行 10MB <100ms；④ 死循环模板 5s 内被预算掐断并给 E2001；⑤ 手工模板可独立跑 Verifier 并给结构化诊断 | 80 | M1, M2 |
| **M4 分析增强** | 数据处理器图 + Diff + Capstone + YARA | proc, diff, disasm, yara, ui；07 09 11 12 | ① 三级处理器链（xor→解压→显示）不改源数据且可随时旁路；② 两个 100MB 文件 diff <5s 且块级结果与 `bsdiff` 语义一致；③ 反汇编窗口与 hex 视图双向定位；④ YARA 命中以 `Layer::Yara` 呈现且不阻塞 UI | 70 | M2 |
| **M5 AI** | Verifier 强化 → 增量补全 → 字段命名 → 完整生成 + eval | ai, bspec, ui；03(A2~A7) | ① 在 20 个已知格式样本上 best-of-N 一次通过率 ≥60%（eval 集固化）；② 增量补全 p50 延迟 <1.5s 且 10 秒内可取消；③ 所有 AI 输出必须过 Verifier 才允许插入项目；④ 隐私模式下无任何出网请求（抓包/单测断言） | 50 | M3 |
| **M6 平台化** | 插件 ABI + 内容分发 + 主题 + i18n + 公证签名 | plugin-host, content, ui；14 15 16 | ① 一个示例 `.geoplug`（纯 C ABI）在三平台装载并注册一个可视化器；② ABI 版本不匹配时给出明确拒绝而非崩溃；③ 内容库更新可断点续传并签名校验；④ 三平台签名产物可在干净系统双击运行无警告 | 55 | M4 |
| **M7 打磨** | 可视化器全家桶 + 性能攻坚 + 无障碍 + 文档站 | ui, analysis, 全 crate；13 + 全部文档 | ① 00 §7 全部条目在 CI 基准中达标且连续 7 天无回归；② 键盘可达所有功能、屏幕阅读器可读结构树；③ mdBook 站点可离线构建、无死链；④ 首次启动到可交互 <1.2s | 40 | M3~M6 |

**总量与关键路径**：合计 **400 人日 ≈ 20 人月**（与 01 §6 的 15~20 人月估值一致，差额是 M6/M7 的工程化尾巴）。**关键路径 = M0(30) → M1(40) → M3(80) → M5(50) → M7(40) = 240 人日（12 人月，占总量 60%）**，这条链的任何一天延期都直接推迟发布；M2/M4/M6 挂在其侧可并行。单人全职 ≈ 20 个月；核心 + 0.5 兼职 ≈ 14 个月；两人全职（核心 + 一名 UI/工程化）≈ **11~12 个月**。每个里程碑预留 15% 缓冲（已含在 400 人日内），里程碑内部不允许"顺手加需求"——新需求进 backlog 排到 M+1。

**M0 的任务级拆解**（30 人日，最小闭环不能被裁剪——它决定了后面所有性能假设是否成立）：

| # | 任务 | 产出物 | 人日 |
|---|---|---|---|
| 1 | workspace 骨架：`Cargo.toml`、`rust-toolchain`、`xtask deps`、CI 最小集（fmt/clippy/test/doc） | 五平台绿灯的空仓库 | 4 |
| 2 | `geode-core`：`Offset`/`ByteRange`/`Diag`/`Value`/id 类型 + 越界裁剪 | 后续所有 crate 的地基 | 3 |
| 3 | `geode-provider`：`file` Provider（`pread` 路径）+ `SizeInfo` + `Caps` | 打开 4GB 不整读 | 4 |
| 4 | `geode-buffer` 只读骨架 + `version()` 单调递增 | 视口渲染所需的最小接口 | 5 |
| 5 | `geode-annot`：区间索引（有序区间 + 二分）+ slotmap 稳定 id | `query` O(log n+k) | 3 |
| 6 | `geode-ui`：Painter 自绘网格 + 视口虚拟化 + 等宽字体度量 + 滚动条（整数、非 f32） | 60fps 只读视图 | 8 |
| 7 | `app`：命令行参数、拖放、打开/关闭、错误弹窗 | 可双击运行的二进制 | 3 |

**工时分布**（含 15% 缓冲，与上表同口径；`geode-ui` 与 `geode-bspec` 是两大头，合计 30%）：

| crate | 人日 | crate | 人日 | crate | 人日 |
|---|---|---|---|---|---|
| geode-ui | 62 | geode-bspec | 60 | geode-buffer | 40 |
| geode-project | 28 | geode-ai | 26 | geode-search | 24 |
| geode-provider | 22 | geode-proc | 20 | geode-analysis | 18 |
| geode-diff | 16 | geode-annot | 14 | geode-plugin-host | 14 |
| app | 12 | geode-content | 10 | 工程化（CI/xtask/发布/文档） | 10 |
| geode-disasm | 8 | geode-yara | 8 | geode-core | 8 |

合计 400 人日。**估算纪律**：任何人日超过 20 的任务必须先拆；拆不动的说明设计还没想清，回文档补 §4。

### 10.1 实现状态（滚动更新）

| 里程碑 | 状态 | 已完成 | 未完成 | 证据 |
|---|---|---|---|---|
| **M0** | **部分完成**（2026-11-09） | workspace 骨架（resolver 3 / edition 2024 / 统一 lints）、`xtask deps` 四条机器规则（E0101~E0104）、三平台 CI、`geode-core`（Offset/ByteRange/Diag/Value/Ids/Budget/Version）、`geode-provider`（file `pread` + memory，Caps/SizeInfo/ReadReport）、`geode-buffer` 只读骨架（`read`/`version`）、`app` CLI（`--version`/`--help` + hexdump） | `geode-annot` 区间索引、`geode-ui` Painter 自绘网格与比例尺滚动条、M0② 滚动 p99 验收 | 2052 行 Rust、31 测试全绿、`clippy -D warnings` 干净；4 GiB 稀疏文件打开 **0.013 ms**（验收①要求 <200ms）；release 二进制 461 KB；CI 一次 success。提交 `ad5e8f6` → `be7f327` → `09ff4ef` → `8a9ad4b` |

**M0①（打开 4GB <200ms）已达成；M0②（滚动 p99 <25ms）未测**——它需要 `geode-ui` 的网格落地后接 21 §5.2 的 `scroll_p99`，不在读路径上。

### 10.2 M1 前置工件（≈1.4 人日，不占里程碑，但缺了 M1 无法验收）

这五项不是"顺手做"，是 M1 三条验收的**测量手段本身**。顺序建议：错误码表 → schemas → vectors → bench → proptest。

> **执行状态**看仓库根 [`PLAN.md`](../PLAN.md) §2：本表是规格，PLAN 是进度。P1（错误码表）已于 2026-11-09 完成。

| # | 工件 | 对应验收 | 现状 | 产出物 | 人日 |
|---|---|---|---|---|---|
| P1 | **错误码常量表** | 全部（M1 起开始产 `EB` 码） | 代码里只有 `Code::segment()` 段位逻辑，126 个具体码一个没落 | `xtask docs-errors` 扫描 26 份文档的码定义 → 生成 `crates/geode-core/src/error_codes.rs`；CI 双向校验（文档有码代码无码 / 反之）均报错 | 0.3 |
| P2 | **`schemas/*/v1.json`**（geopatch / geojournal / geows / geoproj） | M1⑤ | **0 个文件** | 由 23 §3 的字段表机械生成，JSON Schema draft 2020-12；随 M1 打 tag | 0.3 |
| P3 | **`tests/vectors/`** | M1⑤ | 目录不存在 | 每格式：最小样本 + 完整样本 + **≥3 个故意损坏样本**，各配 `.expect.toml` 写明期望的 `EB` 码 | 0.3 |
| P4 | **bench 骨架** | M1④（保存 1GB >500 MB/s） | 无任何 bench | `benches/` + criterion；先建 6 项（`open_4gb` / `scroll_p99` / `save_1gb` / `read_64k` / `edit_random` / `undo_all`），与 21 §5.2 的 27 项同注册表、`xtask bench audit` 强制一对一 | 0.3 |
| P5 | **proptest 骨架** | M1②（1 万次随机编辑后撤销，字节相等） | 无 | `crates/geode-buffer/tests/prop_*.rs`：piece table 与 `Vec<u8>` 影子实现对拍（R1 的缓解措施） | 0.2 |

### 10.3 需要 spike 的四个技术未知

在写对应模块**之前**打原型，成本 0.3~0.5 人日/项，避免"写完才发现前提不成立"。

| # | 未知 | 卡住什么 | 判定标准 | 上限 |
|---|---|---|---|---|
| S1 | placeholder 4 KiB 下限在真实文件上是否碎片化 | 04 §4.2 的核心假设（"1 字节 literal 与 placeholder 交替"的病态结构） | 1 万个随机编辑序列后统计 piece 数，须满足 `piece ≤ O(C + 改动字节 / 4KiB)` 且内存放大 <2× | 0.5 |
| S2 | journal 不逐条 fsync 在 `kill -9` 下的实际丢失窗口 | M1③ 的"恢复 N 条修改"文案与 17 §9.3 的诚实度 | 1000 次随机时刻 kill：最大丢失 ≤1 秒编辑、恢复成功率 100%、`Partial` 率 <1% | 0.3 |
| S3 | 纯 `egui::Painter` 一帧 ~5000 quad 能否稳 60fps | 05 Q8；决定是否需要 wgpu 实例化 callback | 4 GB 文件滚动 30 s，p99 <25 ms，无掉帧 | 0.5 |
| S4 | `epaint` 字体图集能否按字体局部失效 | CJK 字体加载策略与首启时间 | 切换字体不全量重建图集；首帧到可交互 <1.2 s（M7④ 同口径） | 0.3 |

### 10.4 文档与工件缺口（开工后按需补，不阻塞 M1）

| 缺口 | 何时必须 | 归属 | 估量 |
|---|---|---|---|
| **诊断码集中字典**（126 个码、每码一句人话 + 触发条件） | M1 开始产码时 | 新文档 `27-error-code-dictionary.md`（P1 的生成器的输入侧说明） | 200 行，可自动生成 |
| **CLI 参考手册**（`--patch` / `--dump` / `--json` / 退出码约定） | M1（保存与补丁要能被脚本调） | 新文档 `28-cli-reference.md` | 150 行 |
| `theme.toml` 与 i18n `<lang>/*.json` 的格式规范 | M3 前 | `23 §3.10` / `§3.11` | 120 行 |
| 插件 ABI 完整可编译 C 头文件 | M4 前 | `14` 附录 + `include/geode/plugin.h` | 1 文件 |
| 首批内置内容清单（30 模板 / 5 主题 / 2 语言，具体是哪 30 个） | M5 前（决定冷启动观感与工作量） | `15` 附录或新文档 | 150 行 |
| `docs/book/` 教程 4 篇（30 分钟上手 / 写第一个 BinSpec / 写插件 / 发模板） | 发布前 | 新建 | 400 行 |

## 11 风险登记册

| # | 风险 | 影响 | 概率 | 缓解措施 | 触发信号 |
|---|---|---|---|---|---|
| R1 | **piece table 实现出错导致静默数据损坏** | 致命（用户丢数据，声誉不可恢复） | 中 | 与 `Vec<u8>` proptest 对拍（1 万次随机操作）；保存前整文件回读校验（`full_blake3` 可选开关）；M1 前冻结"块存储与 COW"RFC；**数据修改路径必须双人 review** | proptest 出现反例、保存后哈希不符、任何 `unsafe` 新增 |
| R2 | **BinSpec 语言设计反复**（三个月后想推倒重来） | 高（M3 返工，AI 全部依赖它） | 中高 | A0 阶段先写 30 个真实模板（ZIP/PNG/PE/ELF/私有格式）验证语法表达力；语法进 02 后变更走 ADR；**M3 后语言冻结到 v1.0，只加不删** | 同一语法点在 3 个模板里都要 hack、模板作者抱怨率上升、Verifier 规则反复重写 |
| R3 | **AI 生成质量不达标**（私有格式全部失败） | 中（差异化卖点落空） | 中高 | 03 §14 的诚实定位：对已知格式求高覆盖、对未知求骨架；A7 eval 集先行且进 CI；**AI 永远不能是唯一路径**（模板手写体验必须自洽） | eval 通过率连续 2 个 sprint 不涨、用户把生成结果全删 |
| R4 | **大文件性能不达标**（4GB 打开 >200ms 或滚动掉帧） | 高（产品前提崩塌） | 中 | M0 就把性能基准与 CI 门槛跑通（不等 M7）；Provider 层从第一天支持 `pread`+LRU 与 mmap 双路径；每 sprint 看趋势图 | `open_4gb`/`scroll_p99` 连续三次超阈（判据见 21 §4.3）、内存 RSS 超预算 |
| R5 | **插件 ABI 破坏性变更** | 中（生态信任受损） | 中 | ABI 从 v1 起带版本化 vtable + `abi_version` 协商；**v1 前不对外宣传插件生态**；只暴露 `#[repr(C)]` POD，绝不暴露 Rust 类型 | 任何需要在 vtable 中间插入字段的需求 |
| R6 | **单点人力不足**（核心作者不可用） | 高（项目停摆） | 中 | 文档先行（本库即为此存在）；每模块至少 1 个可交接的 RFC；CI 与发布流程脚本化到"看 README 能做"；关键算法配 golden 测试使接手者可验证 | 连续 2 周无提交、review 队列 >5 |
| R7 | **杀软误报**（Rust 二进制 + 自解压/签名缺失） | 中（用户装不上） | 中高 | Windows 从一开始就买 EV 代码签名；不打包壳/不加自解压；发布页给 SHA256；对 Defender/Sophos 预提交样本 | 发布后 48h 内出现 3 例以上误报 |
| R8 | **许可证污染**（GPL 代码或模板被搬进来） | 高（必须下架重写） | 中 | 见 §13「法律与许可」：`deny.toml` 白名单 + 每 PR 的 license job；**明令禁止参考 ImHex 源码与 Pattern 模板实现**，只允许读其文档与行为；模板库独立许可审查 | `deny` 报 GPL/AGPL、PR 里出现可疑"参考实现"注释 |
| R9 | 崩溃恢复本身引入新 bug（恢复出错误字节） | 高 | 中 | journal 回放对拍 + 随机 kill 测试（17 §9.3）作为 M1 出口条件；恢复结果必须可一键丢弃 | 恢复后哈希与预期不符、恢复对话框出现 `Partial` 频率异常 |

## 12 开发实践

**分支**：`main` 保护（禁直推、禁 force push、必须 1 approval + 绿灯）；功能分支 `feat/<milestone>-<slug>`，修复 `fix/<issue>-<slug>`；发布分支只在需要 cherry-pick 补丁时开 `release/0.x`。**Squash merge**，PR 标题即提交标题。

**提交规范**：Conventional Commits，scope 用 crate 名：`feat(geode-buffer): 支持跨 piece 的 overwrite`。破坏性变更必须 `!` + `BREAKING CHANGE:` 段。**每个涉及数据修改路径的提交必须在正文写"为什么安全"**（一句话即可），否则 review 直接打回。

**代码审查（重点：数据修改路径）**：`geode-buffer` 的 `overwrite/insert/remove/save/undo/redo` 与 `geode-project` 的 journal/迁移代码，审查清单固定四条——① 越界是否被裁剪而非 panic；② 失败路径是否留下半写状态（必须事务/可回滚）；③ 是否有 `unwrap`/`expect` 在可达路径上；④ 是否新增 `unsafe`（新增即需要独立 reviewer 与 SAFETY 注释）。UI 侧审查重点不同：① 是否在 UI 线程做了 >1ms 的工作；② 后台结果是否校验 `buffer.version()`。

**文档同步**：每条设计决策的实现在代码里必须有 `// DESIGN: 04-buffer-and-edit-model §6.2` 形式的锚点（`cargo xtask docs` 校验锚点存在）；反之，文档里"本文拥有"的每个决策在代码里应能找到对应锚点，孤儿锚点作为软警告进周报。ADR 放 `docs/adr/NNNN-title.md`，用于记录"曾经考虑过但否掉"的方案与其否决理由（防止半年后重开同一个辩论）。

**CHANGELOG 与版本**：Keep a Changelog 格式，每个里程碑一节。版本号 `0.x.y`：`y` = 修复，`x` = 有新功能或（在 1.0 前）有破坏性变更。**0.x 期间的冻结规则**：① 每个里程碑发布的 `0.x.0` 之后只做 bugfix，新功能进下一个 `x`；② 破坏性变更必须成组发布（同一 `x` 内不连续破坏），并在 CHANGELOG 顶部列"迁移说明"；③ **`*.geoproj` 与 BinSpec 模板格式的破坏性变更必须带迁移器**（17 §4.4），否则不允许发版；④ 1.0 的门槛是"三平台发布 + 六个月内无数据损坏级别 bug + 文件格式稳定一个里程碑"。

## 13 发布与分发

| 平台 | 产物 | 关键动作 |
|---|---|---|
| macOS | `.dmg`（通用二进制 `arm64+x86_64`）+ `.tar.gz` | `lipo` 合并 → `codesign --deep --options runtime` → `notarytool submit --wait` → `stapler staple`；至少支持最近 2 个大版本 macOS |
| Windows | `.msi`（WiX）+ 便携 `.zip` | EV 证书签名 exe 与安装器（SignTool + 时间戳）→ 提交 SmartScreen；不做自解压壳 |
| Linux | `.AppImage`、`.flatpak`（Flathub）、AUR `geode-bin`/`geode` | AppImage 用 `linuxdeploy`；flatpak 走 runtime 24.08；AUR 由 `scripts/aur-push.sh` 更新 PKGBUILD |

**仓库与站点**：GitHub 主仓（公开）+ 只读镜像；下载走 GitHub Releases（附 SHA256 与 SBOM）；官网用 mdBook（`docs/book/`）+ 一个单页落地页，随 `docs.yml` 自动部署到 Pages；文档站包含 00~18 全文与 `--help` 生成的手册页。

**法律与许可**：

- **双许可 MIT OR Apache-2.0**（Rust 生态惯例），`LICENSE-MIT`/`LICENSE-APACHE` 双文件 + 每个 crate 的 `Cargo.toml` 声明 `license.workspace = true`。
- **第三方审计**：`cargo-deny` 的 `licenses` 白名单（§8）+ `bans`（禁多版本、禁未维护）+ `advisories`；新增依赖必须在 PR 里贴 `cargo deny check` 输出。发布附带 SBOM（`cargo cyclonedx`）。
- **GPL 注意（红线）**：**不得链接任何 GPL/AGPL 代码**（ImHex 本身是 GPL-2.0，直接参考其源码即污染）；**不得搬运 ImHex 的 Pattern 模板与 `.hexpat` 文件**（模板是受版权保护的表达，不是"想法"）；允许阅读其**文档、行为、截图**并独立实现，禁止复制代码结构；任何"看过的实现"要在 ADR 里声明以确保可追溯。yara-x 与 capstone 的许可必须逐个核对（capstone 是 BSD，yara-x 是 BSD-3），且它们只在 feature flag 后面。
- **专利与出口**：不实现、不分发任何受专利保护的专有算法（如某些压缩/加密实现需评估）；本产品含**加密功能**（解密数据、TLS），部分国家/地区对加密软件出口有登记或限制要求，发布页附带标准出口声明，法律审查在 1.0 前完成一次。

## 14 开放问题

| # | 问题 | 现状 / 倾向 | 时限 |
|---|---|---|---|
| Q1 | 是否把 `geode-core` 单独发布到 crates.io？ | 倾向 M6 后再评估（先保证内部 API 稳定，避免过早背上 semver 承诺） | M6 |
| Q2 | 双人团队时 M4（Capstone/YARA）是否外包？ | 现按自研估算；若外包需先冻结 ABI 与测试 | M3 末 |
| Q3 | UI 快照测试要不要进 5 平台？ | 现只 linux+macos（Windows 字体栈不同，基线难稳定） | M2 |
| Q4 | `--headless` CLI 是否与 GUI 共享全部能力（含 AI）？ | 倾向共享库层，CLI 不含 UI 布局相关功能；AI 在 CI 用 mock | M5 |
| Q5 | 基准机要不要公开数据以自证性能？ | 倾向公开 nightly 趋势图（营销价值），但需先排除噪声 | M2 |
| Q6 | flatpak 的沙箱如何访问 `/proc/*/mem` 与网络设备（多数据源）？ | 需 `--device=all` 或 portal，可能被迫降级这些 Provider | M6 |
| Q7 | MSRV 政策：跟 stable 还是固定 6 个月？ | 倾向固定 1.85 且每 2 个里程碑 bump 一次，减少 CI 维度 | M1 |
| Q8 | 是否引入 `cargo-mutants` 对 `geode-buffer` 做变异测试？ | 夜间跑，成本高但对数据安全值得 | M1 后 |
| Q9 | 发布节奏：里程碑发版还是月度发版？ | 倾向里程碑 `0.x.0` + 每月 `0.x.y` | M1 |
| Q10 | 是否提供 Homebrew / winget / Scoop 清单？ | 社区维护优先，官方只保证 Releases + AUR | M7 |
