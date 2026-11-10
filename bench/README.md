# bench

M1 基准（criterion，`harness = false`）。`edit_random` / `undo_all` 共用固定 xorshift 种子的
10,000 步随机编辑计划（64 MiB 内存源，覆盖 1~64B / 插入 / 删除混合）。

## 怎么跑

```sh
export PATH=/Users/edwa2d4th/.cargo/bin:$PATH
cargo bench -p geode-bench --bench edit -- --warm-up-time 0.2 --measurement-time 0.5
```

## 当前数字（2026-11-10，Apple M4 / macOS 27.2，**未锁频**，仅供趋势参考）

- `edit_random/64MiB`：10,000 步总耗时 **454.762 ms**，单步 **p99 105.9 µs**；criterion mean 455.34 ms（95% CI 453.33~457.45，CV 0.77%）。
- `undo_all/10000`：`undo_all()` 总耗时 **717.101 ms**（71.7 µs/步）；逐步撤销 p99 95.8 µs；撤销后**字节与原文逐字节相等**；criterion mean 718.10 ms（95% CI 713.85~722.98，CV 1.09%）。
- 判定：单步 p99 105.9 µs < 1 ms，达标；CV 均 <5%，测量不属 inconclusive。未测 1e5 步规模、O(log N) 斜率与 piece 数上界。
- 复跑波动：宿主有 Infuse/浏览器/并发编译负载时，p99 102.8~118.0 µs，criterion mean 可达 574 ms（edit）/810 ms（undo）；以上为一轮低负载数字，**不要跨机器比较**。
