# 06 — 数据源（Provider）子系统

> 上游契约：见 00-architecture-overview §5.1（`Provider`/`Caps`/`SizeInfo`）、§6（线程模型）、§7（性能预算）。
> 本文只回答"字节从哪来、怎么安全地取"。**不**定义编辑语义（见 04-buffer-and-edit-model）、**不**定义渲染（见 05-render-and-hexview）、**不**定义解密解压（见 07-data-processor-graph）。

---

## 1 目标与非目标

| # | 目标 | 验收 |
|---|---|---|
| G1 | 单一 `Provider` trait 覆盖本地文件、块设备、进程内存、GDB、SFTP、HEX/SREC、流式源；上层只认 trait | 上层 crate 里 `grep` 不到具体 Provider 类型名 |
| G2 | 100GB 级数据可打开/跳转/滚动，内存与源大小无关 | 4GB 文件内存 < 500MB；100GB 打开 < 200ms（00 §7）|
| G3 | `SizeInfo::Unknown` 的流式源**功能可用**：能滚动、能搜、能导出 | `nc -l` 灌 5GB 未知长度数据，全流程不卡 |
| G4 | `volatile` / `holes` 是一等语义，不是"读失败凑合" | 空洞有专用纹理与专用错误码；volatile 有常驻徽章 |
| G5 | 危险源默认只读；提权可审计 | 打开 `/dev/disk0`、附加进程后 `caps.writable == false` |

**非目标（明确的否决）**：
- **否决 Provider 做解压**。zlib/zstd/zip 归 07；07 用一个 raw 字节 Provider 包住解压输出。理由：解压失败常发生在"已解出 30MB"之后，Provider 的 `read_at` 语义表达不了半成品，而 07 的块索引天然能表达。
- **否决 Provider 提供写事务/撤销**。`write_at` 是裸的、无历史的；崩溃恢复与 patch 归 04。
- **否决与外部写入者的强一致**。mmap 被外部改写只做"检测 + 降级"，不锁、不同步。
- **否决非 seekable 源的真随机访问**。跳出去报 `Evicted`，不假装能读。
- **否决每个 Provider 各写一套缓存**：统一分块缓存（§4.10），否则 8 种源 = 8 套 bug。

---

## 2 依赖与边界

`geode-provider` 依赖 `geode-core`（`Offset`/`ByteRange`/`Diag`/`JobId`/`CancelToken`），不依赖 `geode-buffer`、任何 DSL crate、任何 GUI。它是叶子 crate。

| 外部 crate | 用途 | 备注 |
|---|---|---|
| `memmap2` | 本地文件 mmap 快路径 | 只用 `MmapRaw`，不持有 `&[u8]` |
| `libc` / `windows-sys` | `pread`、`mach_vm_read_overwrite`、`ReadProcessMemory`、`ioctl` | unsafe 全收敛在 `providers/*/sys.rs`，顶部 `#![deny(unsafe_op_in_unsafe_fn)]` |
| `russh` + `russh-sftp` | SSH/SFTP | 纯 Rust |
| `crossbeam-channel` | I/O 线程与 job 通信 | 不用 tokio（见 00 §2） |

| 方向 | 契约 |
|---|---|
| 上游 `geode-buffer` | 只用 `read_range`/`write_at`/`size`/`caps`；placeholder 直接存 `(ProviderId, offset, len)` |
| 上游 搜索/Diff/哈希 | 只要 1MB 块的顺序吞吐达标（§5）|
| 上游 `geode-proc` | 只把 Provider 当 `SourceRef::Root` 的 raw 只读源 |
| 上游 UI | 只读 `ProviderInfo`+`Caps`+`ChangeKind`+`Diag(E6xxx)`，不 match 类型 |
| 下游 | 无 |

---

## 3 数据结构与接口

### 3.1 对 00 §5.1 的增量（**已回填 00 §5.1**）

```rust
pub enum ChangeKind { Unchanged, Resized(u64), Unmapped, Replaced }  // Replaced = 大小不变内容变
pub struct ProviderId(pub u32);                                      // 缓存键 + Arc 身份

pub trait Provider: Send + Sync + 'static {
    // 00 §5.1 的六个方法不变，新增：
    fn id(&self) -> ProviderId;
    /// 区间读：默认实现按可读段切分、逐段调 read_at。空洞不填不报错，只记录。
    fn read_range(&self, r: ByteRange, out: &mut [u8]) -> Result<ReadReport, ProviderError> { .. }
    /// 该区间内的可读子段（holes 源才有意义），默认 push(r)
    fn readable_ranges(&self, r: ByteRange, out: &mut Vec<ByteRange>) { out.push(r) }
}

pub struct ReadReport { pub filled: u64, pub holes: Vec<ByteRange>, pub eof_hit: bool }
```

**`read_at` 三条铁律**（补充 00 §5.1）：① **短读合法**，`Ok(n)` 且 `n<len` 是常态（EOF、空洞边界、设备到扇区尾），上层必须循环；② `off >= size` 返回 `Err(OutOfBounds)` 而非 panic，`off < size` 但越界则短读 + `eof_hit`；③ 空洞是显式结果——`read_at` 要么整段可读要么整段 `Err(Hole)`，跨洞一律走 `read_range`。

### 3.2 错误与诊断

```rust
pub enum ProviderError {
    OutOfBounds { size: SizeInfo },  Hole { start: u64, len: u64 },
    Evicted { since: u64 },                                     // 环形缓冲已丢弃
    PermissionDenied { need: Privilege },  Io { code: i32, msg: String },
    Timeout { op: &'static str, after_ms: u32 },  Disconnected,
    Auth { reason: String },  Corrupt { source: &'static str, at: u64 },
    Unsupported,  Cancelled,
}
pub enum Privilege { Root, Admin, FullDiskAccess, Debugger, RawDevices }
pub struct Secret(String);   // Debug 恒为 "REDACTED"，不实现 Serialize
```

错误码占 **E6xxx 段**（E1~E5 已归 BinSpec，见 02-dsl-spec-draft §8.2）：E6001 权限不足 / E6002 目标消失 / E6003 断开重连中 / E6004 协议超时 / E6005 格式校验失败 / E6006 缓冲已丢弃。每个错误必须带 `ByteRange` 才能进 `Diag`。

### 3.3 打开描述

```rust
pub enum ProviderSpec {
    LocalFile { path: PathBuf, readonly: bool },
    BlockDevice { path: PathBuf, offset: u64, len: Option<u64>, writable: bool },
    ProcessMemory { pid: u32 },  GdbRemote { endpoint: String },
    Sftp { host: String, port: u16, path: String, auth: SshAuth },
    Ihex { path: PathBuf, format: TextFormat },   // TextFormat::{Ihex, Srec}
    Base64File { path: PathBuf },  Stdin,  Udp { bind: String },
}
pub enum SshAuth { KeyFile{path: PathBuf, passphrase: Option<Secret>}, Agent, Password(Secret) }
```

### 3.4 统一分块缓存

```rust
pub struct BlockCache { cfg: CacheCfg, shards: [Mutex<Shard>;16], hits: AtomicU64, misses: AtomicU64 }
struct Block { data: Box<[u8]>, valid: Box<[u64]>, filled: u32, last_used: u64 }
struct Shard { map: FastMap<(ProviderId,u64), Arc<Block>>, ring: Vec<Arc<Block>>, hand: usize }
```

`valid` 是 64B 粒度的位图，支持短读与块内部分空洞。**淘汰用 CLOCK（二次机会）而非严格 LRU**：命中路径只需读一个 `AtomicU64` 并推进 hand，免写锁；1024 页规模下命中率与 LRU 差 2% 以内，换来的是"滚动时零锁竞争"。

---

## 4 核心算法/流程

### 4.1 `Caps` 驱动上层行为

| Cap | 为假/为真时上层必须做什么 |
|---|---|
| `writable=false` | 编辑入口置灰；`Ctrl+S` 变"导出副本…"；`write_at` 返回 `Unsupported`；patch 面板隐藏 |
| `seekable=false` | 跳转框禁用并提示"流式源，只能顺序前进"；偏移列只在 `[window_start, window_end)` 有效；滚动条长度 = 已收字节数；搜索仅允许从旧向新；书签不可反向定位 |
| `volatile=true` | 视口常驻黄色徽章；块缓存 TTL 归零；搜索/哈希/Diff 结果标注"源在变化，可能陈旧"；Diff 前强制快照读 |
| `holes=true` | 空洞渲染交叉斜线纹理（在 `Layer::Structure` 之下、`Layer::Value` 之上）；搜索跳过并单独计数；哈希把空洞记 `0x00` 并产 warning；导出要求选"填充 0 / 跳过 / 取消" |
| `SizeInfo::Unknown` | 滚动条、搜索、导出三处全走 §4.2 的探测扩展 |

**这是硬约束**：上层若不对 `caps()` 作判断就假设能力，视为缺陷。

### 4.2 `SizeInfo::Unknown` 的探测扩展

**(a) 可 seek 但长度未知**（部分 `/proc` 伪文件、某些 SFTP 路径）——**边界探测**：

```
est = 4096; steps = 0; good = 0
loop {
    if cancelled() { break }                              // 取消粒度 = 一次探测，< 100ms 响应
    match read_at(est-1, &mut b1) {
        Ok(0) | Err(OutOfBounds) => break,                 // 找到边界，est 即长度下界
        Ok(_) => { good = est; est = next(est) }
    }
    steps += 1
    if est > probe_ceiling { emit(E3010); break }          // 上限保护
    sleep(200ms)                                           // 限速，别把远端/内核打爆
}
next(x) = if steps < 8 { x*2 } else { x + x/2 }            // 2× 八次后转 1.5×
```

参数：`probe_ceiling` 默认 **64GiB**（触发即停，标"长度未知，已探测 64GiB"）、单步上限 512MB、总预算 30s 后转后台慢速继续。探测结果是**下界**，之后随时可能 `Resized`。

**(b) 不可 seek 的流**（stdin/UDP/串口）——**探测即读取**：读到的字节必须立刻进暂存，否则数据丢失。暂存超 `spool_threshold`（64MB）自动落盘为临时文件（`O_TMPFILE`+`unlink`），落盘后 `seekable` 翻转 `true`、`volatile` 保持。此类源 `size()` 恒 `Unknown`，直到观测到 EOF。

**三处上层机制**：
1. **滚动条**：虚拟总长 = `max(len, len×1.25)`。探测推进使总长变大时**保持当前顶部偏移不变**、只让 thumb 变短——否则用户视觉跳变。
2. **搜索**：转流式搜索，维护 `searched_upto` 水位；进度 = `searched_upto/virtual_len`；结果上限 100k；观测到 EOF 即定格水位。
3. **导出**：只能顺序读+边读边写，UI 必须提示"源长度未知，导出大小以实际读取为准"。

### 4.3 本地文件（mmap 快路径 + pread 回退）

选择规则（明确决策）：`不可写 → pread`；`size < 1MB → pread`（映射建立成本 > 收益）；`可移动/网络卷 → pread`；`其余只读大文件 → mmap`。

- **mmap 路径**：`MmapRaw::map_raw` 整文件只读；`read_at` = clamp 到 `known_size` 后直接拷贝。
- **pread 路径**：`FileExt::read_at` + 64KB 页缓存（§3.4）。**为什么可写时必须 pread**：mmap 写回时机不可控，且 Windows 上映射中的文件无法 truncate（见 01-competitor-feature-research §3.1）。所有写一律 `pwrite` + 记 dirty 区间，交给 04 的保存事务。
- **外部修改**：持 `fd`，每 1s `fstat` 比较 `(size, mtime_ns, inode, dev)`；任一变化 → **立即 drop 映射、切 pread、发 `Resized`/`Replaced`**，UI 提示"文件被外部修改，已切换安全读取模式"。
- **SIGBUS 窗口**：1s 轮询窗口内被外部截断仍可能崩。P0 **接受该风险**（只在外部截断时发生），P1 用进程级 `SIGBUS` handler + 每线程 `sigsetjmp` 加固，后果计入开放问题 1。
- **铁律**：所有 mmap 读必须 clamp 到 `known_size`。映射按页对齐，文件尾不足一页的填充字节**绝不能**被当成数据返回。
- **超过文件末尾**：`off >= size` → `Ok(0)`；跨界 → 短读 + `eof_hit`。不补零、不报错。
- **性能**：mmap 热读 > 3GB/s；pread > 800MB/s（NVMe）；4KB 随机命中 < 2µs。

### 4.4 原始磁盘 / 分区

| 平台 | 路径与要点 |
|---|---|
| macOS | `/dev/rdisk*`（裸设备，分区 `/dev/rdisk*s*`）。**必须用 rdisk**：`disk` 走页缓存，实测慢 5~10×。块大小 `DKIOCGETBLOCKSIZE`，枚举 `diskutil list -plist` |
| Linux | `/dev/sd*`、`/dev/nvme0n1[pN]`。块大小 `/sys/block/<dev>/queue/logical_block_size`，分区起点 `/sys/block/<dev>/<part>/start`（单位 512 扇区）|
| Windows | `\\.\PhysicalDriveN`；`CreateFileW(FILE_FLAG_NO_BUFFERING \| FILE_SHARE_READ\|WRITE)`；几何 `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX` |

- **扇区对齐（强制）**：底层 `(offset, len, buf_addr)` 必须按 512/4096 对齐。上层任意读由 Provider 内做"对齐窗口读 + memcpy 裁剪"（`aligned = off & !(sec-1)`，读 `ceil((off%sec + len)/sec)*sec`）。**这是唯一允许的未对齐入口**，绝不把对齐要求泄漏给上层。
- **权限与 UI 提示**：macOS 需 root 或"完全磁盘访问权限"（提示路径：系统设置 → 隐私与安全性 → 完全磁盘访问权限 → 勾选 Hex）；Linux 需 root 或 `cap_dac_read_search`（提示 `sudo setcap cap_dac_read_search+ep <hex>` 免常驻 root）；Windows 需管理员（按钮"以管理员身份重启"，不做自动 UAC 重提示，以免触发杀软）。macOS 还需注意 SIP 保护的设备在未授权时直接 `EACCES`。
- **默认只读**：`caps.writable == false`。开启写入要过两级：设置 → 危险操作 → 允许写入块设备，再加打开时确认。**否决"记住上次选择"**，每次会话首次写都要重新确认。
- **破坏性写入的二次确认**：① 对话框显示设备型号+序列号+容量、待写扇区范围、该范围前 64 字节预览；② 要求键盘输入设备标识（`PhysicalDrive0` / `disk2s1`），不接受点确定；③ 确认按钮冻结 500ms；④ 自动生成受影响范围 1MB 快照到临时目录；⑤ 目标分区**已挂载**时默认禁止写入（OS 页缓存会覆盖你的写，或直接损坏文件系统）；⑥ 系统盘分区 0 / EFI 分区需三次确认 + 红色警示。
- **失败语义**：按扇区原子提交，任一写失败**立即停止**，报告"已写入 N 扇区 `[start, start+N*sec)`，失败于偏移 X，errno Y"。**不做尽力重试**——半截写比失败更危险。
- **性能**：SSD 顺序 300~2000MB/s（必须 1MB 对齐读）；512B 扇区随机 4KB 约 0.3ms。故磁盘源缓存页设 **1MB**，否则随机读被 syscall 淹没。
- **安全**：写入前重校验设备序列号（防 TOCTOU 替换）；用 `stat.st_mode & S_IFBLK` 判定块设备，**不靠 `/dev/` 路径前缀**。

### 4.5 进程内存

| 平台 | 读 | 区段枚举 | 权限 |
|---|---|---|---|
| macOS | `mach_vm_read_overwrite(task, addr, size, dst, &out)` | `mach_vm_region_recurse`（含 submaps）| `task_for_pid` 需 root，或已签名 + `com.apple.security.cs.debugger` entitlement 且目标允许 |
| Linux | `pread64(/proc/<pid>/mem, buf, len, addr)` | 解析 `/proc/<pid>/maps` 的 `perms` 前两位 | root，或同 uid 且 `ptrace_scope <= 1` |
| Windows | `ReadProcessMemory` | `VirtualQueryEx` 遍历，保留 `MEM_COMMIT && !PAGE_NOACCESS && !PAGE_GUARD` | `PROCESS_VM_READ\|PROCESS_QUERY_INFORMATION`，通常需管理员 |

- **坐标 = 虚拟地址 VA**，不是 0-based。理由：反汇编、符号、崩溃栈全用 VA；换成 0-based 会让"复制地址去别处查"失效。
- **holes**：`size()` = `max(region.end)`（远超实际可读量）；`readable_ranges` 返回合并后的可读段，**gap ≤ 4KB 不合并**（避免伪造）；首次附加必须全量枚举（4GB 进程 300~2000 区，5~20ms，后台 job）。
- **不可读区渲染**：`holes` 纹理 + 悬浮显示"未映射 / 保留（PAGE_NOACCESS）"。
- **ASLR**：模块基址每次附加都不同。`ProviderInfo.extra` 记 `module → (base, slide)`；恢复会话时按 slide 重定位，模块集合或 slide 不匹配 → 提示"目标进程布局已变化，此前地址可能失效"并把旧地址标 stale。**否决自动猜基址**——猜错会把用户引到错误数据上。
- **崩溃/退出**：读出 `ESRCH` / `ERROR_INVALID_HANDLE` → `caps` 转只读、`poll_change` 返 `Unmapped`、视图转灰、红条"目标进程已退出（PID 1234）"；已缓存的 16MB 可继续浏览与导出快照。
- **性能**：`mach_vm_read_overwrite` ≈ 5µs/4KB，`ReadProcessMemory` ≈ 3µs/4KB，`/proc/pid/mem` ≈ 4µs/4KB。**这是 syscall-bound 源**：顺序读上限约 800MB/s，缓存与相邻 8 页预取比任何算法优化都重要。
- **绝不默认允许写入**：`caps.writable` 恒 false。即使打开实验开关，也必须同时满足：目标区已枚举且为 `PAGE_READWRITE`、写前重读比对一致、单次 ≤ 4KB、每次写产生一条审计日志。默认行为就是拒绝。

### 4.6 GDB 远程串行协议（RSP）

- **包**：`$<payload>#<cksum>`，cksum = payload 字节和 mod 256（两位小写 hex）；应答 `+`/`-`/`$...#..`。payload 内 `#`、`$`、`}` 必须 `}` 转义（后跟 `char^0x20`）；**我们不发送 RLE，但必须能解析对端发来的 RLE**。
- **命令最小集**：`qSupported`（协商 `PacketSize`、`qXfer:memory-map:read`）、`qXfer:memory-map:read::<off>,<len>`（一次性取完 XML 段表并缓存）、`m<addr>,<len>`（返回 hex，2 字符/字节）、`M<addr>,<len>:<hex>`（写，仅会话解锁后发）、`?`（重连后判断是否同一个 inferior）。
- 无 memory-map 时退化到 LLDB 的 `qMemoryRegionInfo`；两者皆无 → 假定 `[0, 2^64)` 可读，用读失败探测边界，并提示"目标未提供内存映射，空洞探测将变慢"。**否决凭空猜布局**。
- **单次读长度** `chunk = min(4096, PacketSize/2 - 40)`（hex 编码 2× 膨胀）。
- **超时与重连**：单包 `2000ms`；连续 3 次超时或收到 `E` 响应 → `Disconnected` → 指数退避重连（1/2/4/8/16s，最多 5 次）。**重连成功后必须重拉 memory-map 并丢弃全部缓存**（inferior 可能已换进程）。
- **吞吐差 → 大块预取 + 缓存**：每次读一个 RTT。LAN RTT 0.2~1ms，4KB/块 → 硬上限 4~20MB/s；串口 115200 baud = 11520 B/s，hex 编码再打对折 → **有效约 5.5KB/s**，50MB 要 2.5 小时。故预取块 32KB（LAN）/ 自动降到 `PacketSize/2` 以下（串口）；命中缓存后随机读退化为纯内存（< 1µs）；滚动按方向预取"下一屏 ± 2 屏"；RSP 是单通道，**所有请求必须串行化**，并发只会互相踩包。串口场景 UI 常驻提示"当前链路约 5.5KB/s，建议改用 TCP stub 或先 dump 成文件"。
- **安全**：RSP 无认证无加密。只允许 `localhost/127.0.0.1/::1`，远程需显式开启并提示明文风险；`M` 默认不发送。

### 4.7 SSH / SFTP

- **实现**：`russh` 连接 + `russh-sftp` 子通道 `read(off,len)`/`write`/`stat`。
- **认证**（按优先级，任一成功即停）：`ssh-agent`（`SSH_AUTH_SOCK`，首选，私钥不出 agent）→ 密钥文件（ed25519 优先，支持 passphrase，passphrase 存 `Secret`）→ 密码（仅当服务器禁用 publickey 时提示）。
- **host key 必须校验**：`known_hosts` + 应用私有副本，首次 TOFU 显示 SHA256 指纹，指纹变化 → **拒绝连接**要求用户手动处理。**否决 accept-all**：SFTP 读的是用户分析数据，中间人可任意篡改。
- **随机读延迟 → 窗口预取 + 本地缓存**：单次 `read(32KB)` ≈ 1 RTT。RTT 200ms 时单流仅 160KB/s，滚动必卡。故：缓存块 **256KB**，miss 时按 `align_down(off)` 发起 1MB 窗口预取（切成 4 个 256KB 请求），**最多 8 in-flight**（SFTP 支持乱序应答，按请求 id 匹配）→ 8×256KB/0.2s ≈ **10MB/s**。滚动方向感知：向上滚时窗口向低地址扩展，避免"每次从顶部重拉"。
- **断线重连与改动检测**：`poll_change` 每 1s `stat`（复用连接，O(1) 不额外握手）。断开 → 退避重连 → 重连后**立即 stat**：`size`/`inode` 变 → `Resized`/`Replaced` 且**清空该 Provider 全部缓存块**；`mtime` 变而 size 不变 → `Replaced`（`rsync --inplace` 会这样改文件）。
- **安全**：密码/passphrase 一律 `Secret`，不进 `Debug`/日志/项目文件。`ProviderInfo.uri_redacted` 形如 `sftp://user@host:22/path`，构造时过滤凭据模式。

### 4.8 Intel HEX / Motorola SREC

- **IHEX 记录** `:llaaaatt[dd..]cc`：类型 `00` 数据、`01` EOF、`02` 扩展段（×16）、`03` 起始段、`04` 扩展线性（×0x10000）、`05` 起始线性。校验 = 全记录字节和 `mod 256 == 0`（**补码和**）。
- **SREC 记录** `S<type><count><addr><data><cksum>`：`S1/S2/S3` 数据（地址 2/3/4 字节），`S0` 头、`S5/S6` 计数、`S7/S8/S9` 结束。校验 = `count+addr+data+cksum == 0xFF`（**反码和**）。
- **稀疏映射**：产物是段表 `Vec<(addr: u64, data: Arc<[u8]>)>`，排序后合并相邻/重叠段；**重叠时后者覆盖前者**并产 warning `E6005`（真实工具链确实会输出重叠记录，硬失败会让整个文件打不开）。
- **gap 不填充**（`coalesce_gap = 0`）：填充等于伪造数据，会让用户在空洞里"看到"不存在的 `0x00`。
- `size()` = `max_addr + 1`：一份只在 `0x0800_0000` 有 4KB 数据的 HEX，视图长度是 128MB。`caps = { writable:false, seekable:true, volatile:false, holes:true }`；跳到空洞 → 纹理 + "该地址不在文件记录中"。
- **成本**：文本格式必须全量扫描，100MB HEX ≈ 1s（后台 job + 进度条）；解码后内存 ≈ 文本 0.4×；`max_decoded = 2GiB`，超限降级为"只建索引不载数据"（段表保留，数据按需重扫，慢但可用）。

### 4.9 流式与一次性源（Base64 / stdin / UDP）

| 源 | `SizeInfo` | seekable | volatile | 关键设计 |
|---|---|---|---|---|
| Base64 文件 | `Known(len*3/4)` 估算，读到实数据后修正 | ✅（一次解码到内存，上限 1GiB）| ❌ | 超 1GiB 只允许"边解码边导出"，不给视图 |
| stdin/管道 | `Unknown` | ❌ → spool 后 ✅ | ✅ | 边收边追加；超 64MB 落盘临时文件后变 seekable |
| UDP | `Unknown` | ❌ | ✅ | 环形缓冲 256MB，保留 datagram 边界元数据 |

UDP 偏移从"连接建立后第一个字节"起算，有效窗口 `[tail, head)`；`read_at(off < tail)` → `Err(Evicted { since: tail })`，该区渲染"已丢弃"纹理，不报错不中断其它功能。UI 提供"暂停 / 清空 / 另存当前窗口"。流源 `poll_change` 常态是 `Unchanged`（只增长），增长时 `Resized(head)`；缓存 TTL 归零。

### 4.10 统一分块缓存（跨 Provider）

| Provider 类 | 块大小 | 容量默认 | TTL | 预取 |
|---|---|---|---|---|
| 本地文件（mmap）| 不缓存 | — | — | 由 OS 页缓存负责，mmap 路径**不叠应用层缓存**（避免双份内存）|
| 本地文件（pread）| 64KB | 64MB（1024 页）| 无 | 连续 3 次同向 miss → 异步预取 4 页 |
| 块设备 | 1MB | 64MB（64 页）| 无 | 预取 2 页 |
| 进程内存 | 4KB | 16MB | **0（不缓存）** | 无 |
| GDB RSP | 32KB | 32MB | 无 | 滚动方向 ±2 屏 |
| SFTP | 256KB | 256MB | 无 | 1MB 窗口，8 in-flight |
| HEX/SREC、流式 | 不缓存 | — | — | — |

规则：① **`volatile` 源不缓存**（TTL=0）——用户看进程内存就是要看"此刻的值"，250ms 陈旧会导致误判，**否决短 TTL 缓存 volatile**。② **空洞负缓存**：探测失败记入 per-provider hole bitmap，TTL 2s（探洞每次一个 syscall，负缓存把连续跳转压到近零）。③ **写穿透**：`write_at` 直接落盘并 `cache.invalidate(provider, range)`，缓存永只缓存读。④ **失效**：`Resized` 失效 `[old_size,∞)` 之外全清、`Replaced` 全清、`Unmapped` 冻结（不再读但保留内容）。⑤ **全局预算 512MB**，按表配额，超额优先收缩 TTL=0 且命中率最低的源。

### 4.11 生命周期与并发

- **打开**：`open_provider(spec) -> Result<Arc<dyn Provider>, ProviderError>`，在后台 job 里做（SSH 握手、设备探测、HEX 解析都是慢操作），UI 显示"正在打开…"且可取消。
- **共享**：一个 `Doc` 持一个 `Arc<dyn Provider>`，多个 `View`（hex / 结构树 / Diff / 反汇编）共用。所有可变状态在 Provider 内部的 `Mutex` 分片里，trait 全是 `&self`。
- **后台借用**：job 持 `Arc` 克隆而非 `Weak`。理由：`Weak` 会在用户关文档的瞬间让飞行中的读失败，产出一堆无意义错误；正确做法是 `Arc` + 关闭时协作取消。
- **关闭**：`cancel 所有借用 job → join(100ms)（00 §6 要求 100ms 内响应取消）→ drop 最后 Arc → Drop 中关 fd/断连/unmap`。`Drop` 必须幂等且**不超过 50ms**（SSH 直接 `disconnect`，不等对端 ACK）；未 join 成功的 job 下次访问时看到 `Closed` 立即返回。
- **change 轮询与退避**：常驻一个轻量 I/O 线程（不是线程池任务），每 tick 对所有打开的 Provider 调 `poll_change`（都是 O(1) 的 `fstat`/`stat`）。`连续 10 次 Unchanged → interval×2（上限 8s）`；任何 UI 交互立即重置到 1s；`volatile` 源固定 250ms 不退避；后台超过 60s 的非活跃标签降到 30s。
- **并发正确性**：块内 `valid` 位图用 `AtomicU64` CAS 填充，两块并发读同一偏移不互斥（数据相同，后写覆盖）；只有填满的块才进 `map`，所以"读到半块"不可能发生。

### 4.12 权限、路径与日志安全

- **提权最小化**：**否决整个应用以 root 运行**（hex 编辑器打开不可信二进制，root 等于把攻击面放大到系统级）。用户从 `sudo hex` 启动时横幅常驻红色"当前为提权模式"。macOS/Linux 只在需要时提示"以管理员身份重启"；Windows 不做自动 UAC 重提示（触发杀软误报），只给按钮。P2 选项：独立特权 helper 进程，只接受 `(device, offset, len)` 读请求、**无写能力**。
- **路径校验**：`canonicalize` 后显示真实路径（符号链接不拒绝，但必须显示"→ 实际目标"，指向设备时额外警告）；设备文件必须 `S_IFBLK`/`S_IFCHR` 才走磁盘 Provider；打开目录 → 提示"是否以归档形式打开"。
- **不写入不可信目标**：目标 inode/dev 与打开时不一致（被替换）→ 拒绝写；块设备重校验序列号。
- **日志脱敏**：`Secret` 手写 `fmt` 输出 `REDACTED`、不 `Serialize`；`ProviderError::Io.msg` 构造时过滤 `password=`/`PRIVATE KEY`/`token=`；`ProviderInfo` 禁止 `derive(Debug)`，改手写白名单字段；崩溃报告不含 URI 凭据，绝不含 RSP/SFTP 的字节内容。

---

## 5 性能预算与基准

| 场景 | 预算 | 条件 | 对齐 00 §7 |
|---|---|---|---|
| 打开 4GB 本地文件（只读 mmap）| < 50ms | 不整读 | 00 要求 < 200ms，留 4× 余量 |
| 打开 1MB 文件 | < 5ms | | 00 要求 < 50ms |
| 打开 100GB 稀疏镜像 | < 200ms | 只探测头尾各 1MB | 与 00 一致 |
| 4KB 随机读（缓存命中）| < 2µs | | 支撑"跳转 < 5ms" |
| 4KB 随机读（miss，NVMe pread）| < 100µs | p99 | |
| 1MB 顺序读（pread / mmap 热）| > 800MB/s / > 3GB/s | 连续 1GB | 支撑搜索 800MB/s |
| 块设备顺序读 | > 300MB/s | 1MB 对齐，SSD | |
| 块设备 4KB 随机读 | < 500µs | 512B 扇区 | 物理限制，靠缓存兜 |
| 进程内存 4KB 读 | < 20µs | | 顺序上限约 800MB/s |
| 进程区段枚举（4GB 进程）| < 50ms | 后台 | 不阻塞 UI |
| GDB RSP（LAN）| > 15MB/s；串口 ≈ 5.5KB/s | 32KB 块，RTT 1ms | 串口须 UI 明示估算 |
| SFTP 顺序读（RTT 200ms）| > 8MB/s | 8 in-flight × 256KB | |
| SFTP 首次远跳 | < 1.5s | 4 并发请求 | 有进度指示 |
| HEX 解析 | > 100MB/s 文本 | 单线程 | 100MB < 1s |
| 缓存命中率（滚动）| > 95% | 任意 Provider | < 90% 视为预取缺陷 |
| 后台内存占用 | < 500MB | 4GB 文件仅查看 | 00 §7 |

基准放 `bench/providers/`：本地文件用生成稀疏文件；GDB 用内置 RSP stub 模拟器；SFTP 用回环 sshd（§9）；磁盘与进程内存为 `#[ignore]` 手工触发（CI 无权限）。

---

## 6 失败模式与降级

| 场景 | 检测 | 降级行为 | UI |
|---|---|---|---|
| 权限拒绝 | open 或首次 read 的 `EACCES/EPERM` | 文档以空只读态打开 | 提示条 E6001 + "如何提权"，可关闭 |
| 握手/连接超时 | `packet_timeout_ms`、TCP connect 超时 | 进"未连接"态，可重试 | 提示条 + 重试按钮 |
| 认证失败 | 服务器拒绝 | **不无限重试**，3 次后禁用该 spec | 区分"密码错"与"host key 变了" |
| 设备消失 | `ENODEV`/`EIO` | 冻结不可读，保留已缓存内容 | 红条 E6002 + "导出已读部分" |
| 进程退出 | `ESRCH` | 转只读快照 | 红条 + PID |
| 读到 EOF 之外 | `off >= size` | `Err(OutOfBounds)` | 不提示（上层 clamp）|
| 短读 | `n < len` | 上层循环/裁剪 | 不提示 |
| 环形缓冲丢弃 | `off < tail` | `Err(Evicted)` | "已丢弃"纹理 |
| HEX/SREC 校验失败 | 记录级校验 | **跳过该记录**继续解析 | 标红文本对应行 + E6005 |
| 远程文件被改 | `stat` 差异 | 清缓存 + `Resized`/`Replaced` | 只提示一次 |
| 协议包损坏 | checksum 不匹配 | 重发一次，再失败判断开 | 静默（成功即无感）|
| 磁盘写失败 | 逐扇区返回 | **立即停止**，报已写范围 | 模态框含已写扇区清单 |
| 缓存超预算 | 全局配额 | 收缩 TTL=0 源 | 无 |

总原则（对齐 00 §9.3）：任何 Provider 失败都不得让其他文档/视图/功能不可用，只降级该数据源本身。

---

## 7 与其它子系统的契约

| 子系统 | 读什么 | 写什么 | 我们保证 |
|---|---|---|---|
| `geode-buffer`（04）| `read_range`、`size`、`caps.writable` | 通过 `write_at` 落盘 | 短读合法、越界不 panic、写穿透后缓存立刻可见 |
| `geode-search`（08）| 1MB 块顺序吞吐 | 无 | 吞吐达标；holes 返回空段，搜索自动跳过 |
| `geode-diff`（09）| 两个 Provider 并行顺序读 | 无 | 多 Provider 并发读不互相阻塞（分片锁）|
| `geode-proc`（07）| Provider 作为 `SourceRef::Root`，**只读** | 无 | 不向 07 暴露任何写路径 |
| `geode-bspec`（02）| 经 07 的 reader 读字节 | 无 | 空洞 → `Value::Unresolved`，非硬错误 |
| `geode-project`（17）| `ProviderSpec`（脱敏 URI）| 无 | `Secret` 永不序列化，恢复时要求重新输入凭据 |
| UI（16）| `ProviderInfo`/`Caps`/`ChangeKind`/`Diag(E6xxx)` | `ProviderSpec` | 不做类型 match，只按能力渲染 |

唯一共享可变状态是 Provider **内部**的块缓存；无全局静态，所有实例由 `Doc` 持有。

---

## 8 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `providers.local.mmap_enabled` / `mmap_min_size` | `true` / `1048576` | 小于 1MB 走 pread |
| `providers.local.page_size` / `page_cache_bytes` | `65536` / `67108864` | 64KB 页 / 64MB |
| `providers.local.change_poll_ms` | `1000` | 基础轮询，退避到 8s |
| `providers.local.mmap_downgrade_on_change` | `true` | 检测到外部修改即切 pread |
| `providers.disk.sector_probe` / `page_size` | `[512,4096]` / `1048576` | |
| `providers.disk.write_enabled` | `false` | **危险开关**，默认关，不记忆 |
| `providers.disk.write_confirm_delay_ms` / `snapshot_on_write_mb` | `500` / `1` | |
| `providers.proc.page_size` / `cache_bytes` / `cache_ttl_ms` | `4096` / `16777216` / `0` | 缓存仅供冻结快照 |
| `providers.proc.write_enabled` | `false` | 实验性，UI 置灰 |
| `providers.gdb.packet_timeout_ms` / `prefetch_bytes` | `2000` / `32768` | 自动降到 `PacketSize/2` 以下 |
| `providers.gdb.reconnect_attempts` / `allow_remote` | `5` / `false` | 1/2/4/8/16s 退避 |
| `providers.ssh.cache_bytes` / `block_size` / `prefetch_window` / `inflight` | `268435456` / `262144` / `1048576` / `8` | |
| `providers.stream.probe_ceiling` | `68719476736` | 64GiB 上限保护 |
| `providers.stream.virtual_size_growth` | `1.25` | 未知长度滚动条因子 |
| `providers.stream.spool_threshold` | `67108864` | 超 64MB 落盘 |
| `providers.udp.ring_bytes` / `poll_ms` | `268435456` / `250` | |
| `providers.hex.max_decoded` | `2147483648` | 超限转索引模式 |
| `providers.cache.global_bytes` | `536870912` | 512MB 全局 |
| `providers.hole.negative_ttl_ms` | `2000` | 空洞负缓存 |

**决策记录**：全局 512MB 而单源 64MB，是因为"4GB 文件仅查看"内存预算 500MB（00 §7），缓存是最容易失控项；512MB 硬上限 + 按类配额保证"同时开 5 个文档"也不爆炸。

---

## 9 测试策略

**假 Provider（所有测试的地基）**

```rust
pub struct FakeProvider { data: Mutex<Vec<u8>>, script: Vec<Fault> }
pub enum Fault { Delay(Duration), Short(u64), Err(ProviderError),
                 HoleAt(ByteRange), DisconnectAfter(u64), GrowTo(u64) }
```

一个 `FakeProvider` 就能覆盖短读、空洞、断连、超时、增长、`Unknown`。**上层（Buffer/搜索/UI）的测试全部跑在 FakeProvider 上**，不碰真实文件系统。

| 层 | 用例 |
|---|---|
| 单元 | 扇区对齐包装（512/4096 × 1000 组随机 `(off,len)`，底层全对齐且上层语义正确）；`read_range` 默认实现与"逐段 read_at 拼接"结果一致；CLOCK 命中率 ≥ 严格 LRU 的 95%；mmap 尾页 clamp（70000 字节文件 `read_at(69999,2)` 必须短读 1 字节，不得读填充）；HEX/SREC 六种记录类型各一组正负样本（长度不符、校验错、地址回绕、重叠段）；RSP 编解码（转义、RLE、校验和错）；探测退避序列与取消响应 < 100ms；探测推进时顶部偏移不变 |
| 集成 | **回环 SFTP**：CI 起临时 sshd（随机端口 + 一次性 host key + 临时 authorized_keys），测三路认证、断连重连、远端文件被 `truncate`/`mv` 替换；**回环 GDB**：自写 100 行 RSP stub（`m`/`M`/`qXfer`，可注入延迟与断连），比接真 gdb 稳定；**进程内存**：fork 子进程，`mmap` 可读段 + `PROT_NONE` 段 + 8MB gap，断言区段枚举与 holes 正确，`kill` 后断言转只读快照；**块设备**：Linux `losetup` / macOS `hdiutil attach -nomount`，无权限时 `#[ignore]`；**流式**：`nc -l` 灌数据测 spool 阈值与 `Evicted` |
| fuzz | IHEX/SREC 解析器（不 panic、不 OOM、`Corrupt` 偏移正确）；RSP 包解析器（长度字段不越界）；SFTP 路径校验（`../`、`\0`、超长、UTF-8 截断）；`read_range`+随机段表恒等式 `Σfilled + Σholes.len == requested` |
| 基准 | §5 全表；CI 只跑免权限部分（本地文件 / HEX / FakeProvider / 回环 SFTP）|

---

## 10 开放问题

1. **mmap 的 SIGBUS 加固**：P0 只做 1s 轮询 + 降级，窗口内被外部截断仍会崩进程。是否值得引入进程级 `SIGBUS` handler（需 `sigsetjmp`/`siglongjmp`，unsafe 面显著扩大）？备选：默认关 mmap 用 pread + 64MB 缓存，放弃峰值带宽。**倾向**：>2GB 文件默认开 mmap 并接受该风险，可配置关闭。
2. **特权 helper 进程**是否值得（P2）：值得为"免每次重启提权"付出一个 IPC 协议 + 签名校验的成本吗？
3. **进程内存写入**是否应该存在？误写一字节就可能让目标崩溃且无法撤销。倾向 v1 完全不提供（保留配置键但置灰）。
4. **`PacketSize` 协商失败**（很多 stub 不实现 `qSupported`）：保守按 0x400 慢 16×，还是乐观按 0x4000 冒超时风险？
5. **UDP 环形缓冲丢弃策略**：丢最旧（当前，保"最近"）还是丢新（保"开头"）？真实抓包更要前者，但用户预期可能相反。
6. **HEX 重叠段**：后者覆盖（当前）还是产 error 让用户选？
7. ~~`ChangeKind::Replaced` 需回填 00 §5.1~~ **已回填**（含 `read_range`/`ReadReport`/`ProviderId` 与四条铁律）。仍开放：是否补 `Provider::snapshot()`（一致性快照读）——Diff 与哈希都需要，留给 09/10 共同评审。
8. **文件系统级稀疏空洞**（`SEEK_HOLE`/`SEEK_DATA`）是否也映射成 `caps.holes`？能让 100GB 稀疏镜像搜索快 10×，但会牵动编辑与崩溃恢复语义。
