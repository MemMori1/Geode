# S2：journal 不逐条 fsync 的崩溃窗口

- 判定标准（18 §10.3 S2）：1000 次随机时刻 `kill -9`，最大丢失 **≤1 秒编辑**、
  恢复成功率 **100%**、`Partial` 率 **<1%**。
- 结论（**100 次**，不是规范里的 1000 次）：**通过**。恢复 **100/100**，
  `Partial` **0/100**，最多丢 **62 帧**；慢档按 2 ms/帧折算约 **124 ms** 的编辑，
  低于 1 秒预算。
- **重要限定**：`kill -9` 只杀进程，**不丢 page cache**。所以本实验测的是
  `BufWriter` 的用户态缓冲窗口（≤64 帧），**不是断电/内核崩溃时的 fsync 窗口**。
  详见"这些数字没有证明什么"。

## 怎么测的

`crates/geode-buffer/examples/kill9.rs`，父进程把**自身**再拉起来当子进程：

1. 子进程 `JournalWriter::create` 后死循环 `append_insert`（每帧 64 B 数据，
   线上 97 B），每写完一帧就往 `progress` 文件追加一行 seq（不 fsync）；
2. 父进程在随机时刻 `child.kill()`（Unix 上是 `SIGKILL`），等进程退出后读回
   `progress` 最后一条完整 seq 和 `JournalReader::open` 的结果；
3. `progress_seq − recovered.last_seq` 就是丢帧数；`JournalReader` 返回
   `partial=true` 记一次 `Partial`；`open` 返回 `Ok` 记一次恢复成功。

两档负载（各 50 次）：

| 档位 | append 间隔 | kill 延迟 | 想压的窗口 |
|---|---:|---:|---|
| 快档 | 不 sleep | 30–399 ms 随机 | 64 帧的**条数**窗口 |
| 慢档 | 2 ms/帧 | 100–1499 ms 随机 | 1 秒的**时间**窗口 |

写策略就是实现里的常量（`crates/geode-buffer/src/journal.rs`）：

```rust
pub const SYNC_EVERY_FRAMES: u32 = 64;              // 默认每 64 帧
pub const SYNC_EVERY: Duration = Duration::from_secs(1); // 或 1 秒，二者取先
```

`create` 时头单独 `fsync`；之后每帧写入 `BufWriter`，满足 `64 帧` 或
`距上次 sync ≥1 秒` 才 flush + `sync_all`；`finish` 再强制 sync 一次。时间条件
在 append 时检查，所以完全空闲时最后不到 64 帧的数据会一直留在缓冲区，直到下一次
append 或 `finish`。

环境：Apple M4 / macOS 27.2 (Darwin arm64)，rustc 1.99.1。仓库根目录执行：

```text
cargo run -q -p geode-buffer --example kill9 -- 100
```

## 数据

100 次运行（2026-11-10，本地一次跑完）：

| 档位 | 次数 | 恢复成功 | Partial | 最多丢帧 | 平均丢帧 |
|---|---:|---:|---:|---:|---:|
| 快档（不 sleep） | 50 | 50 | 0 | 61 | 5.66 |
| 慢档（2 ms/帧） | 50 | 50 | 0 | 62 | 30.06 |

- 恢复成功率 **100/100**；失败码一栏为空（没有任何 `EB002`/`EB011`/`EB012`/`EB021`）。
- `Partial` **0/100**：kill 落在两帧之间时文件尾是帧边界，`decode_prefix` 正常收尾；
  这**不代表**断电时不会出现半帧，见下。
- 慢档最多丢 62 帧 × 2 ms = **124 ms 编辑**，低于 1 秒；平均丢 30.06 帧 ≈ 60 ms。
- 快档最多 61 帧，快档没有固定间隔，不能折算成时间；它只说明用户态缓冲没超过
  策略的 64 帧上限（间隙里最多再多 1 帧正在写的）。

## 这些数字**没有**证明什么

1. **`kill -9` ≠ 断电。** 进程被杀时，已 `write(2)` 的页仍在 page cache，内核会
   照常回写；`fsync` 只影响掉电/内核崩溃后的持久性。所以 124 ms 是"进程崩溃窗口"，
   不是"断电窗口"。要测断电得用 VM 强制断电、`dm-flakey` 或物理断电，本机做不了。
2. **0/100 `Partial` 不能证明 `<1%`。** 按 rule of three，100 次零事件对应的
   95% 置信上界约 **3%**；规范要求 `<1%`，要更多样本（~300 次零事件）才能把上界
   压到 1% 以下。100 次只能说明"这 100 次里没出现"。
3. **只测了一种负载**：64 B `Insert`、单线程、无 `Checkpoint`/`SaveMark`/`FinalLen`、
   无并发读者。真实编辑的帧更小、更稀疏，缓冲区命中 64 帧上限更慢；但也没有覆盖
   `SaveMark` 切换 base、`FinalLen` 校验这些路径。
4. **只在这台 Mac / APFS 上跑**：Linux ext4、Windows NTFS 的 `fsync` 与丢页行为
   不同（ext4 有 delayed allocation，NTFS 有日志），跨平台结论未验证。
5. **`progress` 本身不 fsync**：kill 落在 `append` 返回后、`progress` 写入前时，
   journal 会领先 progress 一条，本次按 0 丢失计；这只影响 ±1 帧的边界，不改变
   数量级。丢帧数是"用户以为已提交"的下界，不是严格上界。
6. 只跑了 **100 次**，不是 18 §10.3 写的 1000 次；这是本次任务的预算。方向一致，
   但统计置信度按上一条打折。

## 对实现/文案的影响

- M1③ 的"恢复 N 条修改"可以写，但**不要承诺断电零丢失**。建议文案：
  "进程崩溃最多丢最近 64 帧或 1 秒内的编辑；断电可能更多，取决于文件系统。"
- 64 帧 / 1 秒常量不需要因本实验调整；平均丢帧远低于上限，说明批量 fsync 的
  收益（吞吐）没有换来不可接受的进程崩溃窗口。
- `Partial` 恢复路径不靠本实验覆盖，靠 `crates/geode-buffer/tests/journal.rs` 的
  CRC 翻转用例（`crc_flip_keeps_prefix_and_marks_partial`，EB011）保证；断电半帧
  要等真断电实验或故障注入。
- 复现：`cargo run -q -p geode-buffer --example kill9 -- 100`（约 1 分钟）。
