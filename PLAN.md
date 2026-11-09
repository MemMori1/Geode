# Geode 开发计划与进度记录

> 这个文件是**开发期间的唯一进度事实来源**。规则只有四条：
>
> 1. **开工前**看 §2（当前迭代）与 §6（阻塞）；**收工后**往 §4 追加一行日志，改 §2 的状态。
> 2. 状态只有四种：`[ ]` 待办 · `[~]` 进行中 · `[x]` 完成 · `[!]` 阻塞（必须在 §6 写原因）。
> 3. 任务一旦开始，**ID 不改**；拆出来的子任务用 `T1.2a` 这种后缀。
> 4. 数字（测试数、耗时、体积）必须是真的——**没测就写"未测"，不写估计值**。

- 仓库：`github.com/MemMori1/Geode`（分支 `main`）
- 当前里程碑：**M0 收尾 → M1（编辑与持久化）**
- 文档基线：26 份设计文档 + 18 篇 ADR（`docs/`），本文件不重复它们的内容，只记"做到哪了"
- 最近更新：2026-11-09

---

## 1 现在在哪（一句话）

M0 的读路径与骨架已落地（4 GiB 打开 0.013 ms、31 测试、CI 绿），**M0 还差 `geode-annot` 与 `geode-ui` 两块**；M1 的写路径与计数 B-tree 尚未开始。当前迭代先把 M1 的"验收工具"补齐（`18 §10.2`），否则 M1 写了也没法验收。

| 里程碑 | 状态 | 出口条件（摘要） | 备注 |
|---|---|---|---|
| M0 骨架 | `[~]` 部分完成 | 打开 4GB <200ms ✓；滚动 p99 <25ms ✗（未测）；`xtask deps` + CI ✓；`--version/--help` ✓ | 缺 `geode-annot`、`geode-ui` |
| M1 编辑与持久化 | `[ ]` | 见 §3 的 T1.1~T1.8，出口是 `18 §10` 的 M1 五条 | 本迭代的目标 |
| M2 视图完备 | `[ ]` | — | 依赖 M1 |
| M3 BinSpec v0.1 | `[ ]` | — | 依赖 M1/M2 |

---

## 2 当前迭代：Sprint 0「把尺子造出来」（≈3.0 人日）

目标：让 M1 的每一条验收都有**可执行的测量手段**，并把四个押着核心假设的技术未知打掉。

| ID | 任务 | 人日 | 验收（怎么算做完） | 状态 |
|---|---|---|---|---|
| P1 | 错误码登记表 + `xtask docs-errors`（双向校验 + 生成 `error_codes.rs`） | 0.3 | `cargo xtask docs-errors --check` 绿；126 码全登记；生成文件进 CI | `[x]` 2026-11-09（释义 54/126，余 72 条随 27 文档补） |
| P2 | `schemas/*/v1.json` ×4（geopatch / geojournal / geows / geoproj） | 0.3 | 四个 schema 能被 `jsonschema` 校验器加载；与 23 §3 的字段表逐字段对齐 | `[x]` 4/4 + 2 份 `v1.layout.md`（二进制格式的字节表）；`xtask schema-check` 已接 CI |
| P3 | `tests/vectors/` 每格式 ≥3 个损坏向量 | 0.2 | 每个损坏向量写明"期望哪个错误码"，CI 校验该码已登记 | `[x]` 结构完成：4 格式 16 条样本（含 12 条损坏），`xtask vectors-check` 双向校验；**字节级真跑待 M1 的 `geode-format`（`--run`）** |
| P4 | bench 骨架（criterion，先 6 项：open_4gb / scroll_p99 / save_1gb / read_64k / edit_random / undo_all） | 0.3 | `cargo bench` 能跑；`xtask bench audit` 认得出 6 项与 00 §7 的对应关系 | `[~]` 2/6 实现（open_4gb 6.19 µs、read_64k 889 ns）；registry 已登记 4 项阻塞原因 |
| P5 | proptest 骨架（piece table vs `Vec<u8>` 影子对拍） | 0.2 | 随机操作序列 1 万次，双实现对拍无差异（此时 buffer 还没有编辑，先锁只读读路径） | `[x]` 只读路径完成：`ByteRange` 5 条 + `Buffer::read` 2 条属性；编辑对拍随 T1.4/T1.5 接入 |
| S1 | spike：placeholder 4 KiB 下限是否碎片化 | 0.5 | 1 万次随机编辑后 `piece ≤ O(C + 改动字节/4KiB)`、内存放大 <2× | `[ ]` |
| S2 | spike：journal 不逐条 fsync 在 `kill -9` 下的丢帧窗口 | 0.3 | 1000 次随机时刻 kill：最大丢失 ≤1 秒编辑、恢复成功率 100% | `[ ]` |
| S3 | 纯 `Painter` 5000 quad/帧 | 0.4 | 一屏 ~5000 图元整帧 <16.6 ms | `[x]` **通过**：15,360 图元 0.83 ms（余量 20×）；见 `docs/spikes/S3-painter-throughput.md`。不含 GPU 提交与命中测试 |
| S4 | spike：`epaint` 字体图集按字体局部失效 | 0.3 | 换字体不全量重建；首帧到可交互 <1.2 s | `[ ]` |

**顺序**：P1 → P2 → P3 → P5 → P4 → S1 → S2 → S3 → S4。
理由：P1 先做是因为 M1 从第一天就会产码；S3/S4 要等 `geode-ui` 有最小渲染循环才有意义，排最后。

---

## 3 队列：M1「编辑与持久化」任务拆解（40 人日）

| ID | 任务 | 人日 | 验收 | 状态 |
|---|---|---|---|---|
| T1.1 | 写路径：`FileProvider` 可写（`pwrite`）+ `Caps::READ_WRITE_STABLE` + 短写重试 + `fsync` 语义 | 4 | 在 4 GiB 文件随机位置写 1 MiB，回读逐字节相等 | `[~]` **2/4**：`open_rw`/`write_at`/`write_all_at`（短写重试，0 字节写判错）/`sync_all`/`caps` 随句柄变 RW 已完成；**原子保存** `save_provider_atomic`（同目录临时文件 + fsync + rename + 刷目录 + 权限位保留 + 失败清理）已完成并有 4 条测试；**4 GiB 随机写 1 MiB 的验收尚未跑**（缺 B-tree，见 T1.2）；journal 未做 |
| T1.2 | 计数 B-tree piece table：`Piece`(32B) / 叶 64 piece / fanout 64 / split / join / `PieceCursor` | 8 | 跨 100 piece 连续读 O(log N + 100)；与 `Vec<u8>` 对拍 | `[~]` **拆成 T1.2a 语义层（已完成）+ T1.2b 索引层（未开始）**。T1.2a：`Piece::{Source,Literal}`、`PieceTable::{from_source,from_bytes,spans,overwrite,insert,remove}`、相邻 Source 连续即合并 / 相邻 Literal ≤64 KiB 合并、越界返回 `EditError` 不 panic；9 条测试 + 200 例 proptest 与 `Vec<u8>` 对拍。**T1.2b 未做**：计数 B-tree、`PieceCursor`、O(log N) 定位。**placeholder ≥4 KiB 降级也未做**——降级要读源字节，归到 T1.3 的 hydrate 路径（`piece.rs` 头部已写明，不静默填零） |
| T1.3 | placeholder（≥4 KiB 阈值、R1 降级 / R2 禁升级 / R3 相邻合并）+ lazy file loading | 5 | 打开 4 GiB **不读一个字节**；首次滚动才填 | `[ ]` |
| T1.4 | 覆盖编辑：`Overwrite` 默认、EOF 追加、nibble 400 ms 分组、`EditOp` 日志 | 4 | 输入 `41 42` 后字节变为 `41 42`，撤销一次回到原值 | `[ ]` |
| T1.5 | 撤销：操作日志 + 大操作 COW 快照；`undo_mem_limit = min(512MB, max(64MB, 2%×len))` | 4 | 1 万次随机编辑后全部撤销，文件逐字节相等（P5 对拍） | `[ ]` |
| T1.6 | patch 收集与合并（间隙 ≤16 归并）+ `.geopatch` 导出（`geode-format`，BE 定长帧 + 每记录 CRC32） | 3 | 导出再导入，字节等价；损坏向量产出指定 `EB` 码 | `[ ]` |
| T1.7 | 保存事务：temp + `fsync(file)` + `fsync(dir)` + 原子 rename + 硬链接 `.bak` + 进度与取消 | 5 | 保存 1 GiB >500 MB/s；`kill -9` 后原文件不损坏 | `[ ]` |
| T1.8 | journal 写入与恢复：80 B 头 + `kind│seq│len│payload│crc32`，四道关卡（magic/CRC/seq/指纹） | 5 | `kill -9` 重启提示"恢复 N 条修改"并成功恢复（S2 的结论写进文案） | `[ ]` |
| T1.9 | M1 集成验收：把 `18 §10` M1 的五条跑成 CI 用例 | 2 | 五条全绿，作为 M1 的出口 | `[ ]` |

**M1 的硬顺序**：T1.1 → T1.2 → T1.3 → T1.4 → T1.5 →（T1.6 与 T1.7 可并行）→ T1.8 → T1.9。
T1.2 是全项目最危险的一块（R1：静默数据损坏），必须先有 P5 的对拍再写。

---

## 4 进度日志（只追加，不改旧行）

| 日期 | 做了什么 | 证据 |
|---|---|---|
| 2026-11-09 | 文档收口：M0 实现状态、M1 前置工件、4 项 spike 写进 `18 §10.1~10.4`；README 增「开工前清单」 | `7d0d7a2` |
| 2026-11-09 | **P1 完成**：建 `docs/errors.toml`（126 码唯一事实来源）+ `xtask docs-errors`（双向校验 + 生成 `crates/geode-core/src/error_codes.rs`）+ CI 加 `--check`；测试 31 → 33 | `d0627c2` |
| 2026-11-09 | **P5 完成（只读路径）**：7 条 proptest 属性（`ByteRange` 5 + `Buffer::read` 2）；属性测试当场抓出 `clip` 的 `Option` 永远为 `Some` 的假信号，已改为返回 `Self` | 见下一次提交 |
| 2026-11-09 | **P2 完成**：补齐 `geopatch`/`geojournal` 的 schema 与 `v1.layout.md`（头 32/80 B、帧格式、CRC 覆盖范围），并纠正 23 §3.6 示例里 `payload_len=09` 的排版错误（Overwrite 恒为 16+len） | 见下一次提交 |
| 2026-11-09 | **T1.2a 完成**：piece table 语义层 + 9 条测试（含 200 例随机编辑对拍）；抓到并修掉一个真 bug——`split_at` 在 `at` 恰好是边界时返回 `OutOfRange`，导致"插入后马上删除"必失败。**索引层 T1.2b 未开始** | 本提交 |
| 2026-11-09 | **T1.1 第一片**：`FileProvider::open_rw/write_at/write_all_at/sync_all`、`geode-buffer::save::save_provider_atomic`（临时文件→fsync→rename→刷目录，保留权限位，失败不留垃圾）；4 条 save 测试 + 3 条 provider 写测试；测试 45→48。**未做**：4 GiB 随机写验收（等 T1.2）、journal | 本提交 |
| 2026-11-09 | **S3 完成**：`crates/geode-ui` 骨架 + `grid` 几何（乘法定位）+ `benches/paint`；15,360 图元 0.83 ms、文本路径 1,088 格 0.114 ms；结论与"没证明什么"写入 `docs/spikes/S3-painter-throughput.md` | 见下一次提交 |
| 2026-11-09 | **P3 结构完成**：4 个格式 16 条向量（12 条损坏，覆盖 EB001/EB002/EB004/EB009/EB010/EB011/EB012/EB013），`xtask vectors-check`（负向测试：删一个样本 ⇒ 退出码 1） | 同上 |
| 2026-11-09 | **P2 部分**：写 `schemas/geows/v1.json`、`schemas/geoproj/v1.json` + 两份 `CHANGELOG.md`；新增 `cargo xtask schema-check` 并接入 CI（负向测试：塞一个 `format` ⇒ 退出码 1，报两处） | 见下一次提交 |
| 2026-11-09 | **P4 部分**：新建 `bench/` crate（criterion）+ `M1_BENCHES` 注册表（6 项，4 项写明阻塞任务）；`open_4gb` 6.19 µs、`read_64k` 889 ns（内存源，仅回归基线） | 同上 |

---

## 5 决策记录（开发期，与 `docs/adr/` 互补：这里记"小决定"，架构级的进 ADR）

| ID | 决定 | 理由 | 日期 |
|---|---|---|---|
| D-001 | 许可证按 **MIT OR Apache-2.0** 落地，Q8 未拍板前不改 | 代码已带两个 LICENSE；改起来只是措辞与 `deny.toml`，不阻塞开发 | 2026-11-09 |
| D-002 | `docs/errors.toml` 是诊断码唯一事实来源，**不接受**从文档正文反向抽取当权威 | 文档是散文，抽取不可靠；宁可让 CI 逼着两边对齐 | 2026-11-09 |
| D-003 | 生成的 `error_codes.rs` 允许 `#[rustfmt::skip]` | 126 个常量一行一个才好 diff；被 rustfmt 压成行会毁掉可读性 | 2026-11-09 |
| D-004 | 错误码 `severity` 暂按段默认 `error`，逐码复核随 27 文档做 | 先要"码存在且一致"，再要"分级准确"；反过来会拖住 P1 | 2026-11-09 |
| D-005 | `ByteRange::clip` 从 `Option<Self>` 改为 `Self` | 属性测试证明 `Option` **永远是 `Some`**（`min()` 不会失败），这种"看起来会失败其实不会"的签名会诱导调用方写无效分支；短读语义统一为空区间 | 2026-11-09 |

---

## 6 阻塞与风险

| # | 事项 | 影响 | 需要谁 | 状态 |
|---|---|---|---|---|
| B1 | **Q8 许可证**未拍板 | 只影响 14/15/19/24 的措辞，不阻塞代码 | 用户 | `[!]` 等一句话 |
| B2 | M0② 滚动 p99 未测 | M0 无法宣布完成；`geode-ui` 落地后才能测 | — | `[!]` 等 `geode-ui` |
| B3 | SFTP / GDB 无真实环境 | 只影响 M4 的数据源验收 | 后期再找目标机 | `[ ]` |
| B4 | `severity` 分级未经复核 | 27 文档前，日志里的严重级别可能偏重（全是 error） | — | `[ ]` |

---

## 7 度量看板（每次收工更新，数字必须真实）

| 指标 | 当前值 | 目标 | 来源 |
|---|---|---|---|
| Rust 行数（不含生成） | 2052 | — | `tokei`/`wc` |
| 测试数 | **57** | M1 出口 ≥80 | `cargo test` |
| 打开 4 GiB 稀疏文件 | 0.013 ms（app 端到端）/ 6.19 µs（bench 纯打开） | <200 ms | `app` / `bench open_4gb` |
| release 二进制 | 461 KB | <15 MB | `ls -l` |
| CI | 3 OS 绿 | 保持 | GitHub Actions |
| 诊断码登记 | 126 / 释义 54 | 126 / 126 | `xtask docs-errors` |
| `00 §7` 性能条目达标数 | 1 / 19（打开；read_64k 是内存源基线不算数） | 19/19（M7） | `cargo bench` |

---

## 8 怎么用（给未来的自己）

```bash
export PATH=$HOME/.cargo/bin:$PATH          # 本机 cargo 不在默认 PATH
cargo test --workspace                       # 33 个测试
cargo run -p xtask -- deps                   # 依赖方向门禁
cargo run -p xtask -- docs-errors --check    # 诊断码双向一致
cargo run -p app -- --help                   # CLI
git push                                     # 需要显式 key（见下）
```

**推送**：本机 ssh 以 root 运行、`~` 解析成 `/var/root`，必须显式指定 key：

```bash
GIT_SSH_COMMAND="ssh -i /Users/edwa2d4th/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push
```
