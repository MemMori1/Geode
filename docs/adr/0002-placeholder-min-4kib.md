# 0002 — placeholder 块最小 4KiB

## 状态

已接受（2026-11-08）

## 背景

Buffer 用 placeholder（窗口到 Provider）表示未改动区，`Piece` 描述子 = Source(16B) + 2×u64 = **32B**。若允许 1 字节 placeholder，1GB 被切碎成 10⁸ 片 ⇒ 3.2GB 描述子 + 10⁸ 次 O(log N) split，最坏放大 **32 倍**；且每个低于阈值的 placeholder 边界等价于一条"有语义的原文件边界"，而用户只改 8 字节时这些边界毫无语义。4KB 取自主流页大小（x86-64/aarch64；Apple Silicon 16KB 也 ≥ 4KB，4KB 是安全下界），同时是 `pread` + 页缓存的最小有用单位——一次 `read_at` 固定成本 1~2µs，读 1 字节与读 4KB 一样贵。见 04 §3.3。

## 决策

`min_placeholder_bytes = 4096`，并用 R1/R2/R3 三条规则保证 piece 数与编辑次数解耦。

- **R1 降级**：任何操作后某 placeholder `len < 4096` ⇒ 整段读入并替换为 literal（≤4KB 内存/次）。
- **R2 禁止升级**：literal 永不"变回" placeholder（provider 上仍是旧值，升级会静默出错）。
- **R3 相邻 literal 合并**：同叶相邻、`Arc::strong_count == 1` 且 `src_off` 相邻 ⇒ 原地拼接，单次 ≤ `coalesce_max_bytes` = 1MB。

## 后果

**好处**
- 病态结构被证伪：设改动簇为 C，R3 使簇内反复编辑不增加片段，每簇最多切碎原文件 2 处且碎片 < 4KB 立即被 R1 literal 化，故 **piece 数 ≤ O(C + 改动字节/4KB)**，与编辑次数无关。
- 反例对照：`min = 1` 且无 R1 时，"0x1000 改 1 字节、0x1001 改 1 字节…"在 1MB 上产生 **50 万片 / 16MB 描述子**（即 01 §3.1 坑 4 的 Tatham 警告）；加上 R1+R3 后同一场景 = **1 个 1MB literal、0 个 placeholder**。
- 内存模型可写死：`O(改动字节 + 4KB×placeholder 数 + 32B×piece 数)`，支撑"4GB 只读 < 500MB"（00 §7）。

**代价**
- 每次小编辑可能触发一次 4KB 读入（R1），对小窗口的随机编辑有额外 I/O；`min_placeholder_bytes` 调大到 64KB 会放大单次降级代价（配置区间 1024~65536）。
- 低于 4096 的配置是"可配但会爆炸"的陷阱，文档与 UI 必须明示（低于 4096 引发 piece 爆炸）。
- `remove` 产生的"缝"在 source 上不连续、无法 re-join，是**信息损失而非缺陷**，只能靠 defrag 局部缓解（见 04 §4.2）。

**何时应重新考虑**
- 若 Provider 侧出现远超 4KB 的原子读粒度（如固定 64KB 块的网络源），下界应跟随上调。
- 若真实负载的编辑簇极密集且 I/O 成为瓶颈，应重估 R1 与"延迟降级 + defrag"的组合。

## 备选方案与否决理由

| 方案 | 否决理由 |
|---|---|
| 1 字节粒度窗口 | 1GB → 10⁸ 片 → 3.2GB 描述子，最坏 32 倍放大；边界失去语义 |
| 整文件预读 | 打开 100GB 需读满全文件，违反 C1 与"打开 < 200ms" |
| 每片 `Vec<u8>` | 不可共享 ⇒ 快照必须深拷贝，O(1) 快照失效（见 04 §3.4） |
| 分块 `ChunkedVec<Arc<[u8;4096]>>` | 只有单 literal > 64MB 才需要，而那时走临时文件 + `TempFileProvider` |
