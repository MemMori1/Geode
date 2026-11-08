# 11 — 反汇编视图（Capstone 集成）

> 归属 crate：`geode-disasm`（见 00-architecture-overview §3）。职责：把一段字节**线性地**翻译成指令列表，并与 hex 视图双向联动。本文不含任何实现代码，只有接口草案与决策。
>
> 定位声明：**我们做的是"字节级的指令浏览器"，不是反汇编器（IDA/Ghidra）**。不做代码/数据区分、不做控制流重建、不做反编译。这条定位决定了下面几乎所有的取舍。

## 0. 决策速览

| # | 决策 | 否决的方案与理由 |
|---|---|---|
| E1 | **Capstone 5.x 为主引擎**（`capstone-rs`），覆盖 x86/ARM/ARM64/MIPS/RISC-V/PPC/WASM/S390x/M68K | 只支持 iced-x86：架构覆盖不足（磁盘镜像/固件/CTF 里 ARM/MIPS 占大头）；自研解码器：单架构就要数月且必然有解码 bug |
| E2 | **x86/x64 允许切到 iced-x86** 做更精确的格式化，两者抽象成 `DisasmEngine` trait | 只用 iced-x86：单架构；只用 Capstone：x86 的 Intel/AT&T/MASM 语法表达力与内存操作数细节弱于 iced |
| E3 | **只做线性扫描（linear sweep）**，不做递归下降、不做代码/数据区分 | 递归下降需要加载器语义（重定位、间接跳转、ARM/Thumb 模式切换），成本高、误判多，且用户明确知道自己在看哪段 |
| E4 | 虚拟滚动必须**从锚点顺序解码到窗口**，锚点间距 8 KiB，**最多回溯 16 KiB**（可配到 256 KiB） | "从任意偏移开始解码"：变长指令集在非边界处解码必然产出垃圾 |
| E5 | 无效指令降级为 `.byte 0xNN`，连续 ≥8 个折叠为一行；无效率 > 50% 触发"架构可能选错"提示 | 静默跳过（用户会以为数据是代码）；整段显示为 `.byte`（丢失可读性） |
| E6 | 解码缓存的键 = `(provider 指纹, 架构+模式, 基址, 起始偏移)`，LRU 上限 256 个 chunk | 只按起始偏移缓存：换架构/换文件/编辑后读到陈旧指令 |

## 1 目标与非目标

**目标**：① 10 万条指令 < 200ms 出全量结果，滚动时首屏 < 16ms（缓存命中路径 < 1ms）；② 多架构（9 个架构族，见 §4.2）；③ 反汇编结果**每一行都携带 `ByteRange`**（00 §1 C3），与 hex 视图双向可点；④ 从 ELF/PE/Mach-O 头自动推断架构/位宽/字节序/入口点，推不出来就让用户手选（不猜）；⑤ 无效字节有明确、可读、可折叠的降级形态；⑥ 指令级 API 与引擎实现解耦，未来可替换/并存引擎。

**非目标**
- 不做控制流图、不做递归下降、不做间接跳转解析、不做基本块划分。
- 不做代码/数据自动区分（不"猜"哪些字节是代码）。
- 不做反编译、不做类型恢复、不做栈帧分析。
- 不做 xrefs（作为远期能力明确列入 §4.5，v1 不做）。
- v1 不支持 PDB（仅 ELF symtab / 动态符号 / PE 导出表），见 §4.6。
- 不支持"边解析边执行"的仿真（那是调试器/6 的进程源的事）。

## 2 依赖与边界

| 方向 | crate | 用途 |
|---|---|---|
| 依赖 | `geode-core` | `ByteRange` / `Offset` / `Diag` / `JobId` / `CancelToken`（00 §4、§5.4） |
| 依赖 | `geode-provider` | 只读 `read_at`（**要求 `seekable`**，见 §6） |
| 依赖 | `geode-annot` | 写 `Layer::Cursor` / `Layer::Selection` 标注（00 §5.3） |
| 依赖 | `geode-analysis` | `headers::probe()` 的 `HeaderFacts`（架构/位宽/字节序/入口点/镜像基址），避免重复实现头部解析 |
| 被依赖 | `geode-ui` | 反汇编视图渲染、跳转交互、符号显示 |
| 被依赖 | `geode-bspec`（间接） | 未来"按结构字段标注代码/数据"只通过 `ByteRange` 交换，不走类型依赖 |
| 禁止 | `geode-ai` / `geode-bspec` / `geode-yara` / `geode-ui` 内部类型 | 依赖方向唯一（00 §3） |

外部依赖：

| 用途 | crate | 许可 | 说明 |
|---|---|---|---|
| 多架构解码 | `capstone`（绑定 Capstone 5.0.x C 库） | BSD-3（Capstone）/ MIT（绑定） | 需 C 工具链；静态链接；交叉编译需为 9 个目标预编 |
| x86 精确格式化 | `iced-x86`（纯 Rust，feature 可关） | MIT | 默认开启，仅在 x86/x64 且用户选择 iced 语法时使用 |
| 符号名解析 | 自研（ELF/PE 头解析，< 600 行） | — | 不引入 `object`/`goblin`：我们只需要符号名 + 地址，不需要完整链接器语义（评审时若发现收益明显再换） |

## 3 数据结构与接口

```rust
pub enum Arch { X86, Arm, Arm64, Mips, RiscV, PowerPc, Wasm, S390x, M68K }
pub struct DisasmMode { pub arch: Arch, pub bits: u8 /*16|32|64*/, pub endian: Endian,
                        pub arm_thumb: bool, pub mips_mips16: bool }
pub struct AddrSpace { pub base: u64, pub file_len: u64, pub sections: Vec<SectionMap> }
pub struct SectionMap { pub va: u64, pub file: u64, pub len: u64, pub exec: bool }

pub struct Insn {
    pub addr: u64,                 // 虚拟地址（= AddrSpace.file_to_va(bytes.start)）
    pub bytes: ByteRange,          // 绝对文件偏移区间（联动唯一基础，00 §1 C3）
    pub mnemonic: SmallStr,        // "mov"
    pub operands: SmallStr,        // "rax, qword ptr [rbx+0x10]"
    pub groups: InsnGroups,        // call|jump|ret|int|privileged|...
    pub target: Option<u64>,       // 直接跳转/调用的立即数目标（虚拟地址）
    pub sym: Option<SymRef>,       // 符号化结果（§4.6）
    pub invalid: bool,             // true → mnemonic=".byte"
}

pub enum DecodeOutcome { Ok(usize) /*消费的字节数*/, Invalid(u8) /*无效字节数*/, Truncated }

pub trait DisasmEngine: Send + Sync {
    fn id(&self) -> EngineId;                        // Capstone | Iced
    fn mode(&self) -> &DisasmMode;
    fn max_insn_len(&self) -> u8;                    // x86=15, ARM=4, WASM 变长…
    fn decode_block(&self, va: u64, code: &[u8], out: &mut Vec<Insn>, max: usize) -> DecodeOutcome;
    fn format(&self, insn: &Insn, opts: &SyntaxOpts, out: &mut String);
}

pub struct Chunk { pub start: u64, pub len: u32 /*64 KiB*/, pub insns: Vec<Insn>,
                   pub ends_cleanly: bool }
pub struct AnchorTable { pub spacing: u32 /*8 KiB*/, pub anchors: BTreeMap<u64, u64> }
                        // anchor: 已验证的指令边界 → 该边界所属 chunk 的可用起点
pub struct DisasmView {
    pub mode: DisasmMode, pub space: AddrSpace, pub engine: EngineId,
    pub anchors: AnchorTable, pub cache: DecodeCache, pub symbols: Option<SymbolTable>,
}
```

`Insn` 结构是**唯一**跨层交换物：UI 只要 `addr / bytes / mnemonic / operands`，联动只要 `bytes`，跳转只要 `target`。任何新需求都加字段，不改语义（00 §4 的扩展规则）。

配套类型（同样只是签名草案）：

```rust
pub struct InsnGroups(u16);                  // CALL|JUMP|RET|INT|PRIV|BRANCH_REL|NOP|INVALID
pub struct SymRef { pub name: SmallStr, pub off: u32, pub kind: SymKind } // Func|Object|Section|None
pub struct SymbolTable { pub by_addr: BTreeMap<u64, SymRef>, pub count: usize }
pub struct SyntaxOpts { pub syntax: Syntax, pub show_bytes: bool, pub use_hex_imm: bool }
pub struct DecodeCache { /* LRU: 键 → Arc<Chunk>，容量上限见 §5 */ }
impl DecodeCache {
    pub fn get(&self, key: &ChunkKey) -> Option<Arc<Chunk>>;
    pub fn insert(&mut self, key: ChunkKey, chunk: Arc<Chunk>);   // 触发 LRU 淘汰
    pub fn invalidate_range(&mut self, r: ByteRange);             // 编辑后精确失效
    pub fn clear(&mut self);
    pub fn mem_used(&self) -> usize;
}
pub struct ChunkKey { pub fingerprint: u64, pub version: u64, pub engine: EngineId,
                      pub mode: DisasmMode, pub start: u64 }
impl AddrSpace {
    pub fn file_to_va(&self, off: u64) -> u64;
    pub fn va_to_file(&self, va: u64) -> Option<u64>;   // 不在映射内返回 None，绝不 panic
}
impl DisasmView {
    pub fn insn_at(&self, file_off: u64) -> Option<(Arc<Chunk>, usize)>;  // 二分，O(log n)
    pub fn request_range(&self, vis: ByteRange) -> RequestOutcome;        // Hit | Pending(JobId)
}
```

`DisasmView` 的所有查询方法都是只读且**幂等**的，UI 每帧可随意调用（00 §6：UI 线程 > 1ms 的工作一律外移）。

## 4 核心算法 / 流程

### 4.1 引擎选型（E1/E2）

| 维度 | capstone-rs（Capstone 5） | iced-x86 | 自研 |
|---|---|---|---|
| 架构覆盖 | **9 族，20+ 模式**（x86 16/32/64、ARM/Thumb/Thumb2、ARM64、MIPS32/64 LE/BE、RISC-V 32/64、PPC、WASM、S390x、M68K） | 仅 x86/x64 | 需逐架构写 |
| 格式化质量 | 中（语法选项有限，内存操作数细节少） | **最好**（Intel/AT&T/MASM/NASM/GAS，可定制） | 取决于投入 |
| 依赖形态 | C 库 + `bindgen`，需工具链，二进制 +~2MB | 纯 Rust，无 C 依赖 | 无 |
| 性能（单线程 decode） | 3~6 M insn/s | **10~20 M insn/s** | — |
| 风险 | C 库崩溃不可 `catch_unwind`（见 §6） | 无常驻状态，最安全 | 解码 bug 自负 |
| 维护活跃度 | 高（5.x 仍在发版） | 高（作者极活跃） | — |

**决策**：Capstone 是唯一能满足"多架构"的选项，作为主引擎；iced-x86 是 feature（默认编译进），用户在 x86/x64 上可切"语法引擎 = capstone | iced"。两者都实现 `DisasmEngine`，`DisasmView.engine` 决定用哪个，**切换引擎即清空缓存**（缓存键含 engine id）。

> 抽象要点：`decode_block` 只吐 `Insn`（含 `bytes`），格式化是独立方法。Capstone 的"解码 + 格式化一体"与 iced 的"Instruction → Formatter"因此被拉平；`Insn` 里额外存 `ops_ref`（引擎私有索引）以便二次格式化，切换语法时不重新解码。

### 4.2 架构 / 模式矩阵与推断

| 架构 | Capstone 模式 | 从文件推断的启发式（`geode-analysis::headers`） | 推不出来时 |
|---|---|---|---|
| x86 | 16/32/64 | ELF `e_machine` 0x03/0x3E + `EI_CLASS`；PE `Machine` 0x14C/0x8664；Mach-O cputype 7/0x01000007 | 默认 **x86-64**（桌面二进制最常见） |
| ARM | ARM / Thumb / Thumb2 | ELF `e_machine` 0x28 + `e_flags`；PE `Machine` 0x1C0/0x1C2；Mach-O cputype 12；**入口点 LSB=1 → Thumb** | 默认 ARM32 LE，Thumb 由用户切 |
| ARM64 | arm64 / arm64be | ELF 0xB7；PE 0xAA64；Mach-O 0x0100000C | 默认 AArch64 LE |
| MIPS | mips32/64, LE/BE | ELF `e_machine` 0x08 + `EI_DATA`（BE 很常见） | 默认 MIPS32 LE |
| RISC-V | riscv32/64 | ELF 0xF3 + `EI_CLASS` | 默认 RV64 |
| PowerPC | ppc32/64, BE/LE | ELF 0x14（BE 为主）；Mach-O 18 | 默认 PPC32 BE |
| WASM | wasm | magic `\0asm` + version u32 LE | 默认 i32 段起点 8 |
| S390x | s390x | ELF 0x16（BE） | 默认 s390x BE |
| M68K | m68k | Amiga HUNK 头 / ELF 0x04 | 手选（极少见，不做启发式） |

- **推断流程**：读头 64 字节 → `MagicDb` 判定容器 → 解 `HeaderFacts`（架构 / 位宽 / 字节序 / 入口点 / section 映射）→ 填 `AddrSpace`。
- 头解析失败（裸二进制/固件/dump）：**不猜**，弹"选择架构"下拉（带上次选择记忆）；hex 视图仍可正常用。
- `AddrSpace.sections` 来自 ELF program/section headers 或 PE section table；无头时退化为 `base`（用户设，默认 0）+ 恒等映射。

### 4.3 扫描策略：只做线性扫描（E3）

**为什么不做递归下降**：① 需要加载器语义（重定位、`.plt` 间接跳转、跳转表、ARM/Thumb 交错的 `BLX`）；② 需要代码/数据区分，而 hex 工具的用户经常**就是要看数据里的指令**（shellcode、ROP gadget、被混淆的段）；③ 误判代价高——递归下降漏一段，用户会以为是"没有代码"；线性扫描漏一条，用户看得到 `.byte`。④ 成本：准确实现相当于写半个 IDA。

**线性扫描规则**：

1. 从 `start` 起逐条解码，无条件按引擎返回的长度前进。
2. 解码失败（无效指令 / 跨越 chunk 末尾）：输出 `Insn{invalid: true, mnemonic: ".byte"}`，宽度 = **1 字节**，前进 1 字节重新同步。
3. **连续无效 ≥ 8 条**折叠为一行 `db 0xNN *N`（UI 层渲染折叠，数据层仍逐条存在，便于"展开"与"跳到第 k 条"）。
4. 状态栏显示 `有效/无效` 比例与"最长的无效连续段"；无效率 > 50% → 顶部提示条"架构/模式可能选错" + 一键切换（架构、Thumb、位宽、字节序、基址）。
5. 起始偏移交互：右键 hex → "从这里反汇编"（把 `start` 设为该偏移，插入锚点并重建）；视图顶部/底部提供 `< 0x1000`、`0x1000 >` 的"向前/向后一屏解码"按钮。
6. Thumb/ARM 混合：v1 **不自动跟随** `BLX` 切换；用户可对某区段手选模式（区段级覆盖存放在 `DisasmView` 外层的 `mode_overrides: Vec<(ByteRange, DisasmMode)>`）。

### 4.4 数据结构与渲染：虚拟滚动 + 锚点（E4）

**难点陈述**：x86 指令 1~15 字节变长，ARM 固定 4（Thumb 2/4），WASM 变长。滚动到偏移 X 时，X 通常**不是**指令边界；从 X 开始解码会产出从第一条就错的垃圾（且错误会沿滚动条位置"抖动"，用户会认为工具坏了）。

**算法**：

```
需要渲染 [vis_start, vis_end]（可见窗口 ± 1 屏）：
1. 在 AnchorTable 找 ≤ vis_start 的最大锚点 a；若无锚点则 a = 0（文件起点）。
2. 若 vis_start - a > max_rewind（默认 16 KiB）→ 直接以 a 为起点（可能整屏都在窗口之前，
   表现为"顶部留白"而不是"错位指令"）。← 宁可留白，绝不错位。
3. 从 a 顺序解码到 vis_end + 1 屏（预取），边解码边每 8 KiB 落一个锚点。
4. 命中 DecodeCache 的 chunk 直接复用（缓存命中路径零解码）。
5. 全部在后台 Job 完成；UI 线程只查缓存并绘制，未命中的行绘制为"……"占位并在下一帧补齐。
```

- **锚点为什么是 8 KiB**：x86 平均 3~4 字节/条，8 KiB ≈ 2000~2700 条，解码耗时 0.4~0.8ms；即使每次滚动都从锚点重解，也在帧预算内。ARM 固定 4 字节更进一步（任何 4 对齐处都是合法边界，几乎不会错）。
- **16 KiB 回溯上限的理由**：x86 最长 15 字节，理论上 1 个 chunk（64 KiB）内必然多次出现边界；16 KiB 是"坏数据 + Thumb 交错"的余量。超过则拒绝回溯（出现顶部留白 + 提示"从此处起解码可能不准确，点击『设为起点』重建"）。
- **每行映射**：`Chunk` 内指令有序且不重叠 → 二分 `bytes.start` 定位指令（`insn_at(off)`），并判断 `off` 是否落在指令中间（显示 `+3` 偏移，高亮整条）。
- **渲染四列**：地址（`AddrSpace` 有效时显示 VA，否则显示文件偏移，表头可切）| 字节（hex，随指令长度变宽，超 8 字节截断加 `…`）| 助记符 | 操作数。行高固定 18px，可见 ≈ 60 行，**与文件大小无关**（00 §1 C1）。
- **内存**：`Insn` 约 64 字节 → 10 万条 ≈ 6.4 MB；`DecodeCache` 上限 256 个 chunk（64 KiB 源码 + 指令表）≈ 24~40 MB，LRU 淘汰。

### 4.5 与 hex 视图的联动

| 交互 | 实现 |
|---|---|
| 选中指令 | 对 `insn.bytes` 插 `Layer::Selection` 标注；hex 视图滚动到该区间并高亮（复用 05 已有的选区机制，不新增通道） |
| 光标在 hex | 反汇编视图 `insn_at(cursor)` → 滚动到该行并整条高亮（`Layer::Cursor`），中间偏移显示 `+n` |
| 点跳转目标 | `insn.target` 经 `AddrSpace.va_to_file` 映射 → 若命中映射区，`start = target` 重建视图；否则提示"目标 0x… 不在文件映射内"（不静默失败） |
| 点击符号 | `sub_401000` 跳到符号地址（映射失败同上） |
| 结构树联动 | 结构节点区间与 `Chunk` 相交时，在指令行前缀显示所属结构名（只读 `Node` 的 `ByteRange`，不依赖 `geode-bspec` 类型） |
| **xrefs** | **v1 不做**。远期方案：对已解码的 `Chunk` 建"立即数 → 引用者"反向索引（只覆盖已解码区域，不做全文件扫描），UI 标注"（已解码区域内 3 处引用）"，明确声明不完整 |
| 分割视图 | 支持"上 hex / 下反汇编"与"左右分栏"两种布局，两者共享同一 `cursor` 与 `selection`，双向滚动由"光标跟随"开关控制（默认开，用户滚动反汇编时临时关闭） |

### 4.6 符号化 / 重定位

- **来源**：ELF `.symtab` + `.dynsym`（含 `STT_FUNC` 过滤）、PE 导出表（Export Directory）与 debug directory 里的 COFF 符号（可选）；PDB **v1 不做**（需要专用 crate 与 MSF 解析，列入 §10）。
- **加载策略**：只在文件头识别为 ELF/PE/Mach-O 时尝试；符号数 > 200k 时只加载 `FUNC` 类型并给出提示；解析在后台 Job，失败不影响反汇编（符号是增强项）。
- **显示**：优先"符号 + 偏移"，例如 `sub_401000 + 0x20`；无符号时 `sub_401000`（对 `target` 指向的未命名函数生成稳定伪名 `sub_<addr>`，与 IDA 习惯一致）；在段内且段有名字时显示 `text + 0x20`。
- **重定位**：v1 不算重定位后的值（显示文件中的原始字节），`AddrSpace` 只做 VA↔文件偏移的线性映射；PIE 的 `image_base=0` 情形按"用户设定基址"处理。
- **调试信息（DWARF / PDB）**：v1 **一律不解析**。DWARF 需要独立解析器 crate，`.debug_*` 段常比文件本体还大（100GB 目标下不可接受）；PDB 需 MSF 容器解析。但必须**诚实告知**：识别到 `.debug_info` 或 PDB 路径时提示"该文件含调试信息，v1 未利用"，而不是静默当作没有。源行号与变量名对反汇编体验是分水岭，列为 v1.1 第一顺位候选（见 §10）；若实现，只在 ELF/PE 命中且调试段 ≤ 256MiB 时后台加载，仍作为增强项，失败不影响反汇编。

### 4.7 线程、缓存生命周期与事件流

反汇编视图自己不拥有线程，全部借 00 §5.4 的 Job 系统：

```
UI: 滚动改变 vis_start
  → view.request_range(vis_start, vis_end)        // UI 线程，O(log n) 查缓存
  → 缓存未覆盖 → 提交 Job "disasm.range"，携带 CancelToken + 起始锚点 a
后台（rayon 池）:
  loop { 检查 ctx.cancel() → 读 8 KiB 源码 → decode_block → 落锚点
         → 发 UiEvent::ChunkReady{chunk} ；每批 8 KiB }
UI: 收到 ChunkReady → 校验 (provider_fingerprint, buffer.version(), mode, start) 匹配
  → 匹配则插入 DecodeCache 并标脏该屏；不匹配则静默丢弃（00 §9：陈旧结果不报错）
```

**失效规则**（缓存条目带四元组标签，任一不匹配即不可用）：

| 事件 | 处理 |
|---|---|
| `buffer.version()` 变化且改动区间与本 chunk 相交 | 逐 chunk 精确失效（不相交的 chunk 保留） |
| 架构 / 模式 / 起始偏移 / 引擎 / 语法改变 | 全清（语法只清格式化缓存，不清解码结果） |
| `provider.poll_change()` 返回 `Resized` / `Unmapped` | 全清 + 重建 `AddrSpace` |
| 内存压力（缓存超 48 MB） | LRU 淘汰，最老 chunk 先丢；当前可见 chunk 永不淘汰 |

- **预取策略**：滚动期间只解可见窗口 ± 1 屏；滚动**停止 50ms** 后再预取 ± 1 屏（避免拖动滚动条时把 CPU 打满）。
- **撤销/重做的特殊处理**：编辑在 UI 线程完成后立刻发一次 `buffer.version()` 变化通知，反汇编视图对受影响 chunk 显示"≈ 已过期"并重新提交 Job，不做同步解码（不阻塞编辑）。
- **预算**：单次 `request_range` 若需从锚点重解 > 32 KiB 源码，则先上屏已缓存部分、把剩余部分留给后台（保证帧时间 < 16ms）。

### 4.8 交互细节

- 右键菜单：`从这里反汇编`、`设为起点`、`设为代码/数据（仅标注，不影响解码）`、`复制字节 (hex / base64)`、`复制指令 (text / bytes)`、`在此处设置基址`、`选择架构…`。
- 键盘：`↑/↓` 逐条、`PgUp/PgDn` 逐屏、`Enter` 跟随跳转目标、`Esc` 回到光标所在指令、`Ctrl+E` 切引擎（x86 才有）、`Ctrl+B` 切位宽。
- 复制格式：`0x401000: mov rax, [rbx+0x10]`（默认）/ 纯助记符行 / `Insn` JSON（供脚本与插件用，见 14）。

## 5 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 10 万条指令全量解码 + 格式化 | < 200ms（Capstone 实测 40~90ms；iced 15~30ms） | x86-64，单线程，不含 I/O |
| 滚动首屏（缓存命中） | < 1ms | 任意文件 |
| 滚动首屏（缓存未命中，从锚点重解 16 KiB） | < 6ms | x86；ARM 更快 |
| 解码吞吐 | ≥ 2 M insn/s（单线程） | 含格式化 |
| 缓存内存 | ≤ 48 MB | LRU 256 chunk |
| 后台分批 | 每批 ≤ 8 KiB 源码，批间检查 cancel | 保证 100ms 内响应取消（00 §6 规则 4） |

上屏方式：**流式**——每解完一个 chunk 就发一次 `UiEvent::ChunkReady`，UI 增量替换占位行，不做"等全量再上屏"。

**测量口径**：用 `criterion`，样本为固定 400 KiB x86-64 代码段（≈10 万条）与 200 KiB ARM64 段；`decode_block` 与 `format` **分开计时**（格式化通常占 40~60%，是优化首选目标）；滚动帧时间用离屏 `egui` headless 渲染测量，取 p50/p99。CI 只对"吞吐 ≥ 2 M insn/s"与"滚动 p99 < 25ms"两条设门禁，其余作为趋势记录（机器噪声会让绝对 ms 抖动）。

## 6 失败模式与降级

| 失败 | 表现 | 降级 |
|---|---|---|
| Capstone 返回解码失败 | 无效指令 | `.byte 0xNN` + 1 字节前进；连续 ≥8 折叠（§4.3） |
| 架构选错 | 无效率 > 50%、助记符离谱 | 顶部提示条 + 一键切换架构/位宽/Thumb/字节序（不自动改，用户确认） |
| 超长指令区（100GB 全反汇编） | 无限后台任务 | 视图**只解码可见窗口 ± 1 屏**，永不"全文件反汇编"；用户可选"导出全量"（按预算/进度/可取消的长任务） |
| Capstone C 层崩溃 | 进程直接死（无法 `catch_unwind`） | ① 输入长度硬上限（每次 `decode_block` ≤ 64 KiB）；② 只用上游 fuzz 过的稳定版本；③ 远期把反汇编放子进程（§10）；④ 崩溃后 `geode-project` 的 journal 恢复（17） |
| 非 seekable 源（UDP 流） | 无法随机回溯 | 禁用反汇编视图并说明原因（需要 `caps().seekable`） |
| `volatile` 源内容变化 | 指令表陈旧 | 逐 Job 校验 `provider 指纹 + buffer.version()`，不匹配即清缓存 |
| 无符号表 / 符号解析失败 | 只显示 `sub_xxxx` | 静默降级 + 一次非阻塞提示 |
| 跳转目标不在映射内 | 点了没反应 | 明确提示（"目标不在文件内"），不静默 |
| `AddrSpace` 缺失（裸文件） | 地址列显示文件偏移 | 表头标 `file offset`，允许用户填基址 |

## 7 与其它子系统的契约

| 对象 | 读/写 | 内容 |
|---|---|---|
| `geode-provider` | 只读 | `read_at`（需 `caps().seekable == true`）；`poll_change` 用于失效 |
| `geode-buffer` | 只读 `version()` | 缓存键一部分；编辑后指令表失效（`Chunk` 所在区间被改动即丢） |
| `geode-annot` | 写 | `Layer::Selection`（选中指令）、`Layer::Cursor`（光标所在指令）；不新增图层 |
| `geode-analysis` | 读 | `HeaderFacts`（架构/位宽/入口点/`SectionMap` 来源） |
| `geode-ui` | 提供 | `DisasmView` 快照（只读 `Arc`）、`UiEvent::ChunkReady`、跳转请求 |
| `geode-project` | 提供 | 配置持久化、上次选择的架构/基址记忆 |
| 事件 | 发出 | `Progress(JobId,f32)`、`ChunkReady{chunk_start, insns}`、`Diag`（架构推断失败等） |

## 8 配置项与默认值

| 键 | 默认 | 范围 | 说明 |
|---|---|---|---|
| `disasm.engine` | `capstone` | capstone / iced | iced 仅 x86/x64 可用 |
| `disasm.syntax` | `intel` | intel / att / masm / nasm | iced 支持更全 |
| `disasm.default_arch` | `x86-64` | 见 §4.2 | 推断失败时使用 |
| `disasm.chunk_size` | 64 KiB | 4~256 KiB | 解码/缓存单位 |
| `disasm.anchor_spacing` | 8 KiB | 1~64 KiB | 锚点间距 |
| `disasm.max_rewind` | 16 KiB | 0~256 KiB | 回溯上限 |
| `disasm.cache_chunks` | 256 | 16~4096 | LRU 容量 |
| `disasm.fold_invalid_run` | 8 | 1~64 | 折叠阈值 |
| `disasm.invalid_warn_ratio` | 0.5 | 0.1~1.0 | 触发"架构可能错" |
| `disasm.show_bytes` | on | on/off | 字节列 |
| `disasm.symbols.enabled` | on | on/off | 符号加载 |
| `disasm.base_address` | 0 | u64 | 无头文件的虚拟基址 |

## 9 测试策略

- **objdump 对拍**：固定样本集（`busybox`、`libc.so.6`、`ld-linux`、自搓 ARM/MIPS/RISC-V 目标文件），逐条比对 `(地址, 字节, 助记符)`；允许语法差异（白名单规范化：操作数顺序、`ptr` 写法、逗号空格），**差异必须逐条解释，不能"自动忽略"**。
- **已知序列单测**：手工构造的字节序列 → 期望指令表（含边界：`0x00 0x00`、`0xCC`、`0x0F 0x0B`、ARM `0xE1A00000`、Thumb 2 字节/4 字节交错、x86 最长 15 字节前缀、RISC-V 压缩指令）。
- **fuzz**：随机字节 × 随机架构/模式 → 不 panic、单次 decode < 50ms、无效字节必被 1 字节前进（不出现死循环）。
- **锚点/回溯正确性（属性测试）**：对任意 `start`，`decode_from_anchor(start)` 的结果必须是 `decode_from(0)` 结果的**后缀子序列**；否则锚点逻辑有 bug。
- **性能基准**：§5 全部条目进 CI；10 万条指令基准用固定 400KB x86-64 代码段。
- **集成测试**：Mock Provider（可注入读失败/短路）验证降级路径；100GB 稀疏文件验证"打开反汇编视图不触发全量解码"。

## 10 开放问题

1. **Capstone 的 C 层崩溃无法捕获**：是否值得把反汇编放进子进程（IPC 成本 vs 崩溃隔离）？还是接受"依赖上游 fuzz + 输入长度限制"？
2. **Capstone 5 交叉编译矩阵**：9 个架构 × 5 个目标平台静态链接的 CI 时间与二进制体积（估计 +2~4MB/平台），是否改用"每个平台只编本平台常见架构"？
3. **iced-x86 是否默认编译**：默认开 → 二进制 +~1.2MB 但 x86 体验更好；改为可选 feature 会让"切换语法引擎"变成付费功能，体验割裂。
4. **Thumb/ARM 交错的自动跟随**：是否值得为 `BLX` + `bx` 做一次简单的边界探测（可能引入不确定性，与 E3 的"不猜"哲学冲突）？
5. **符号解析要不要换 `goblin`/`object`**：自研 600 行能覆盖 symtab/导出表，但 Mach-O 与 COFF 的边角（stripped、`LC_SYMTAB` 压缩、COFF 长名）会不断踩坑。
6. **x86 16 位的段寄存器寻址**（`CS:IP` → 物理地址映射）v1 未处理，是否需要"段基址"配置？
7. **反汇编视图是否该有"跟随光标"模式**（光标移动即自动重定位到所属指令）——可能和用户正在滚动冲突，交互上待验证。
