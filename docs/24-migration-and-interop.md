# 24 — 迁移与互操作（模板导入 / 转换器 / GPL 边界 / 反向导出）

> 本文**拥有**：外部模板格式的导入可行性、`geode-migrate` crate（IR / 流程 / CLI）、转换质量分级与评测、GPL 红线操作指南、`origin` 溯源规范、BinSpec 反向导出。
> 本文**不涉及**：BinSpec 语法（见 02 §3.4）、verifier 与置信度模型（见 03 §5）、`.geopkg` 格式与签名（见 15 §3.2）、层号裁决（见 00 §3 / 18 §3.2）。
> 状态：Draft。转换器未实现；本文是它的验收标准。

---

## 0 决策速览

| # | 决策 | 一句话理由 |
|---|---|---|
| D1 | **模板库冷启动只能靠导入，不自建** | 竞品生态是数百上千个模板 × 多个版本样本（见 01 §3.5(f)），自建等于把 M3 之后的人力全投进内容运营 |
| D2 | **Kaitai `.ksy` 是第一优先源** | Apache-2.0、类型语义离 BinSpec 最近、官方格式库可直接作评测集 |
| D3 | **C header 第二优先**（无许可风险，覆盖自研/固件），**010 `.bt` 第三**（仅本地、不可分发） | 按许可风险从低到高排；`.bt` 生态最大但商业许可（见 15 §4.8） |
| D4 | **ImHex `.hexpat` 默认禁用** | GPL-2.0，转换即改编即传染；仅用户显式声明权利后以 `local-only` 启用，永不入官方索引（§7.4） |
| D5 | **新 crate `geode-migrate`，纯 CPU、无 GUI、不联网** | 转换是离线批处理，进 UI 进程会违反"UI 线程不做 >1ms 工作"（00 §6） |
| D6 | **先建统一 IR，绝不直接拼字符串** | 直接文本替换是这类工具的经典死法（§3.1） |
| D7 | **产物必须过 03 的 verifier 三层 + I1~I10** | 转换器不能成为绕过质量门槛的后门 |
| D8 | **A/B/C 分级；只有 A/B 可入索引，C 只落本地** | 半成品模板进库会摧毁唯一剩下的信任资产 |
| D9 | **`origin` 强制写入、不可关闭** | 溯源是法律义务，不是体验选项；产物头注释 + manifest 双写 |
| D10 | **反向导出三出口：JSON（最保真）/ C（B+）/ Kaitai（C）** | 出口存在本身就是"没上锁"的证明 |

---

## 1 目标与非目标

### 1.1 目标

1. 把用户已有的结构资产搬进 BinSpec——**让新用户的第一个模板不从空白文件开始**。
2. 转换**可解释**：每个字段为何这样映射、哪里必须人工改，全部落进 `--report`。
3. 转换**法律可辩护**：只做语法层机械转换，不复制表达，产物带溯源，分发前必须过许可检查。
4. 提供出口（`-> json/c/ksy`），避免"进来就出不去"。

### 1.2 非目标

- 不承诺"任意模板一键 100% 可用"。**能自动的只有布局，语义永远需要人**（同 03 §14）。
- 不执行源格式的运行时：不内嵌 Python 跑 Construct、不内嵌 Lua 跑 dissector、不内嵌 C 运行时跑 `.bt` 的 `ReadBytes`。
- 不做在线转换服务；不做 BinSpec → `.bt` 反向导出。
- **不替用户判断"他有没有权利转换某个模板"**：工具只能记录与拦阻。

### 1.3 源格式可行性矩阵

接近度 1~5（5 = 几乎同构）。可自动比例 = 不经人工即达 A/B 的字段占比，下界按"源无额外元数据"，上界按"源自带 magic/assert 且有样本"。

| 源格式 | 许可 | 接近度 | 可自动比例 | 依据 | 必须人工的部分 | 主要风险 |
|---|---|---|---|---|---|---|
| **Kaitai `.ksy`** | Apache-2.0 / 部分 CC0 | 3 | **60~75%** | 其类型语义与 BinSpec 最接近：`meta/endian`、`size`/`size-eos`、`terminator`+`encoding`、`bit-endian`、`repeat-until` 近似一对一；无指针概念，不触发最难的指针降级 | `instances` 惰性求值与依赖图、`switch-on` 多态、`process:`（zlib/xor/rol）、`contents:` 数组 magic | 表达力边界（无预算、无软失败）造成语义丢失；继承需单态化致产物膨胀 |
| **C header `struct`** | 随宿主（多为自有） | 4 | **30~45%** | 字段序列与标量 100% 可映射，但 C 只声明**类型**不声明**解析**：谁是长度、谁是偏移、字符串终止符、可有 magic 全不可推断 | 长度/偏移关联、变长数组、字符串编码、条件字段、真实 align 策略 | 隐式 padding 与 P2 直接冲突（§4.3 第 5 条）；union 大小歧义；宏与模板参数 |
| **010 Editor `.bt`** | 商业许可（禁再分发） | **5** | **40~55%** | 语法最像 C（P1 的源头）：`typedef`/`struct`/位域/`if`/`while` 直对应，`FSeek`/`FSkip` 对应 `@`/`padding` | 过程式定位与 `local` 变量需重建成声明式；`ReadBytes`/`Sum`/`CRC` 换 `std::`（部分无对应）；`<template>` 需单态化 | 产物仅限本地（§7.4）；"读"与"算"混写，静态可分析性差 |
| **ImHex `.hexpat`** | **GPL-2.0** | 4 | **技术 55~70%，法律 0%** | 同为 C-like + `@` + `#[...]`，距离近；但产物属改编作品，受 GPL-2.0 约束 | 同 `.bt` | 法律风险 > 技术收益；默认禁用（§7.4） |
| **Wireshark dissector（Lua/C）** | GPL-2.0 | 1 | **10~20%** | 命令式 `tree:add(field, buf(off,len))`，布局藏在控制流里，恢复需数据流分析 | 几乎全部：偏移/长度恢复、分支展开 | 需执行 Lua 才拿到 offset 常量 → 违背"不内嵌运行时"；GPL 叠加 |
| **Construct（Python）** | MIT | 2 | **15~30%** | `Struct`/`Array`/`Switch`/`Pointer`/`Const` 结构清晰，但宿主语言任意性使静态分析不可靠 | `Rebuild`/`Computed`/`this` 表达式、自定义 `Adapter`/`Validator`、任意 Python | 表达式可能带副作用；只能覆盖纯声明式子集 |

**结论**：v1 只承诺 Kaitai 与 C header（M3），`.bt` 到 M5 做到骨架级（C 级），`.hexpat`/Wireshark/Construct 不承诺任何自动比例，只提供"骨架 + 逐字段 TODO"的降级路径。

---

## 2 依赖与边界

### 2.1 crate 位置与允许依赖

`geode-migrate` 与 DSL 工具链同带（00 §3 图中 bspec 旁支的 L1 带）。按 18 §3.2 的数值层号记为 **2**，内部依赖只有 `geode-core`（`Diag`/`ByteRange`）与 `geode-bspec`（AST + 编译期类型表）——因为 `bspec` 在 18 编号里是 L1，严格下降要求 migrate 不得低于 2。外部的 `serde`/`toml` 之外不加依赖；`cargo xtask deps` 需新增这一行，否则"目标不在表中 = 失败"（18 §3.3）。

**边界铁律**：① 绝不依赖 `geode-ui`/`egui`/`wgpu`/`winit`/`geode-buffer`/`geode-provider`；② 绝不联网（连 `std::net` 都不引入），许可白名单是本地静态表；③ 不读文件，只吃调用方给的字节（便于 fuzz）；④ 只写 `.bspec` 文本、`--report` JSON、`.geopkg` 目录草稿，不写真实二进制。

### 2.2 上下游

| 方向 | 对象 | 契约 |
|---|---|---|
| 上游 | `geode-bspec` | 复用 AST、类型表、L1 编译；**不复制其类型系统**，不支持泛型就单态化 |
| 下游 | verifier（`geode-ai`） | 产物送 L1/L2/L3 与 I1~I10（见 03 §5）；migrate 不强依赖 AI 模块 |
| 下游 | `geode-content` | `--pack` 目录草稿交给打包器；`origin` 必须与 15 §3.2 兼容 |
| 下游 | `app`/CLI | `geode migrate ...` 子命令；GUI 仅"导入向导"（M6） |
| 平行 | UI | 只读 `ConvertReport` 渲染"需人工复核清单" |

---

## 3 数据结构与接口

### 3.1 为什么必须先有 IR

直接"`.ksy` 文本 → `.bspec` 文本"必死三条：① **组合爆炸**——6 源 × 3 出口 = 18 个转换器，有 IR 后是 6 个 frontend + 3 个 backend；② **不可校验**——字符串中间态没有节点身份，无法回答"这个字段有没有字节来源"，而 IR 让"每个 `MigType` 必须有确定字节数或显式 `Opaque`"成为类型级约束；③ **报告无处安放**——`--report` 要逐字段给依据/人工项/置信度，`#[ai:conf]` 要写进产物（见 03 §7），都要求字段是有身份的实体。反向导出复用同一 IR 是第二个红利。

### 3.2 统一类型模型

```rust
pub enum MigType {
    Scalar  { kind: ScalarKind, endian: Option<Endian> },
    Bits    { width: u32, order: BitOrder, fields: Vec<MigBitField> },
    Str     { enc: Encoding, term: Option<Terminator>, size: Option<LenSrc>, max: Option<u64> },
    Array   { elem: TypeRef, count: CountSrc },
    Compound{ kind: CompoundKind, layout: LayoutRef },   // struct | union | enum-backed
    Ptr     { space: PtrSpace, pointee: TypeRef },       // Off/Rva/Rel/Va × 32/64
    Padding { len: LenSrc },                             // 显式占位，永不隐式
    Opaque  { fixed_len: Option<u64>, why: String },     // 不可映射 → 必须显式冒烟
}
pub enum CountSrc  { Fixed(u64), Expr(MigExpr), UntilEof, UntilTerminator(Terminator), UntilCond(MigExpr) }
pub enum SizeHint  { Exact(u64), Dynamic { max: Option<u64> }, Unknown }
pub enum PtrSpace  { Off32, Off64, Rva32, Rva64, Rel32, Rel64, Va32, Va64 }
pub enum AlignPolicy { ExplicitOnly }        // P2：只有一个合法值
```

### 3.3 字段与布局

```rust
pub struct MigField {
    pub name: String, pub ty: TypeRef,
    pub at: Option<MigExpr>,        // → `@`（不移动游标）
    pub cond: Option<MigExpr>,      // → `if`
    pub attrs: MigAttrs,            // color/format/comment/hidden/assert/bits/visualize
    pub conf: f32,                  // → #[ai:conf]，与 03 §7 同刻度
    pub origin: SrcSpan,            // 源行区间，报告与"跳回源"用
    pub todos: Vec<Todo>,           // 每条 = 一个必须人工做的具体动作
}
pub struct MigLayout {
    pub name: String, pub kind: CompoundKind, pub hint: SizeHint,
    pub fixed_size: Option<u64>,    // 有则 emit `struct X : N` + #[assert(sizeof(X)==N)]
    pub fields: Vec<MigField>, pub padding_policy: AlignPolicy,
}
pub struct MigModule {
    pub layouts: Vec<MigLayout>, pub root: Option<TypeRef>,
    pub endian_default: Endian, pub image_base: Option<u64>,   // → #pragma image_base
    pub origin: OriginMeta, pub diags: Vec<Diag>,
}
```

`MigExpr` 是**结构化表达式树**（`Literal`/`FieldRef`/`BinOp`/`Call`/`Ternary`/`SizeOf`），不是字符串。无法解析的表达式降级为 `MigExpr::Raw { src }` + `Todo::ManualExpr`，emitter 输出 `// TODO(expr)` 占位并附一条必然失败的 `#[assert]`——**确保它不会静默错**。

### 3.4 前后端 trait

```rust
pub trait SourceFrontend {                    // 6 个实现
    fn id(&self) -> &'static str;                                // ksy|cheader|bt|hexpat|...
    fn detect(&self, hint: &Path, text: &str) -> f32;            // 0~1，供 --from auto
    fn parse(&self, ctx: &MigCtx, text: &str) -> Result<RawModel, Vec<Diag>>;
}
pub trait TargetBackend { fn emit(&self, m: &MigModule) -> Result<ast::Module, Vec<Diag>>; }
pub trait ExportBackend { fn emit(&self, m: &MigModule) -> Result<String, Vec<Diag>>;
                          fn fidelity(&self) -> Fidelity; }      // A/B/C + 丢失清单
```

流程固定 `frontend → RawModel → normalize → MigModule → backend`；`normalize` 是**所有语义判断的唯一发生地**（union 大小、指针空间、padding、单态化），也是唯一按源格式特化的一层。

### 3.5 报告与溯源

```rust
pub struct ConvertReport {
    pub source: SourceInfo, pub converter: ConverterInfo,   // format/url/license/sha256 + version/git rev
    pub stats: Stats,          // fields / auto / manual / opaque / byte_coverage
    pub grade: Grade,          // A|B|C + 判定依据逐条
    pub items: Vec<ReportItem>,   // 逐字段：src_span→bspec_span、依据、conf、todo
    pub verifier: Option<VerifySummary>,   // L1/L2/L3 + I1~I10 明细
    pub license: LicenseVerdict,  // pass | local_only | forbidden + 命中规则
}
pub struct OriginMeta {
    pub source_format: SourceFormat,   // ksy|cheader|bt|hexpat|wireshark|construct|manual|ai
    pub source_url: Option<String>,    // 上游仓库/规范链接，禁空串
    pub source_license: SpdxExpr,      // SPDX 或 LicenseRef-*
    pub converted_by: ConvertedBy,     // "geode-migrate" + 人类可读名
    pub converter_version: String,     // semver + git rev
    pub confidence: f32,               // 整份产物聚合置信度（字段级在 #[ai:conf]）
}
```

写两处，内容必须一致：

```cpp
// ── .bspec 头部注释块（必须在任何声明之前） ────────────────────
// origin.source_format  = ksy
// origin.source_url     = https://github.com/kaitai-io/kaitai_struct_formats/.../png.ksy
// origin.source_license = Apache-2.0
// origin.converted_by   = geode-migrate 0.4.0 (rev 9f2c3a1)
// origin.confidence     = 0.82
// origin.distribution   = index-allowed | local-only | forbidden
// ─────────────────────────────────────────────────────────────
```

```toml
origin = { kind = "imported", from = "kaitai", ref = "png@0.9",
           upstream_license = "Apache-2.0",
           converter = "geode-migrate/0.4.0", confidence = 0.82 }
```

`distribution` 是本文引入的唯一新字段，取值与 §7.4 的红线判定一一对应；`geode-content-lint` 必须读它，`local-only`/`forbidden` 一律不得进索引。

---

## 4 核心算法流程

### 4.1 主流水线

```
源文本 → [F] parse → RawModel → [N] normalize → MigModule
                                                   ├→ [E] bspec emitter → [P] printer → .bspec
                                                   │        → [V] verifier（L1/L2/I1~I10）→ grade
                                                   └→ [X] export backend → json / c / ksy
```

| 阶段 | 失败语义 |
|---|---|
| F parse | 语法不支持 → `EB101`，**已解析部分仍进 IR**（P6 精神：给部分结果） |
| N normalize | 单字段不可映射 → `MigType::Opaque` + `Todo`，**不中断** |
| E/P emit+print | 必须确定性：同 IR 两次输出逐字节相等（内容包可复现，见 15 §9） |
| V verify | L1 失败 → 换 union/位域策略重试 1 次；仍失败 → 降为 C 级骨架 |

### 4.2 幂等与顺序

输出不含时间戳、随机数、绝对路径；字段顺序 = 源顺序（顺序即字节布局，P2），绝不按字母排序；`confidence` 只依赖输入与配置，不依赖运行环境。

### 4.3 转换难点逐条（每条给 BinSpec 侧写法）

1. **C 位域顺序是实现定义** → `bitfield Flags : u8 #[bits: lsb] { ... }`。C 标准未规定 bit-field 在存储单元内的分配顺序，GCC/Clang/MSVC 在小端上普遍 LSB-first，但那是实现约定。**无法从源码判定**，故默认输出 `lsb` + `#[comment("BIT ORDER UNVERIFIED")]` + `Todo::ConfirmBitOrder`；源已明示 MSB（网络/压缩格式常见）必须人工改。这是 B 级与 C 级的分界线之一。
2. **union 默认大小（max vs 首个成员）** → 一律输出显式 `union U : N { ... }`。C/ksy 取 `max(成员)`，010 取首个成员；两者不等时补 `padding[N - first]` + `#[assert(sizeof(U) == N, "...")]`。**绝不依赖默认值**——02 §11.2 明确这条仍待定。
3. **变长数组表达式** → 四种落地：`T data[count]` / `T data[] size=n` / `T data[] until(0)` / `T data[] until($ >= end)`。C 的柔性成员 `T data[0]`、ksy 的 `size`/`size-eos`/`repeat-until` 全归入这四种。硬规则：表达式引用的字段必须已在游标之前读到，否则只能标 `Todo::Reorder`（P2 不许推测）；ksy `repeat: expr` 且无 `size` 时转 `for` 循环。
4. **指针语义（C 的 `T*` 该是 off/rva/rel/va？）** → 唯一可自动化的判据表：PE 且给了 `image_base` → `rva32/64`；ELF/DWARF 或明确"相对自身" → `rel32/64`；裸文件无 base → `off32/64`（保守默认）；源有显式 base 字段 → `va32/64` + `#pragma image_base`。字宽由类型宽度直定；**空间不可判定时 conf 上限 0.30**（对齐 03 §7）并输出 `Todo::ConfirmPtrSpace`。解引用一律带空指针守卫：`Node n @ h.next if h.next != 0;`（02 §3.3 的 S1 软失败在这里是资产）。
5. **C 编译器隐式 padding 与 P2 冲突** → 显式 `padding[n]` + `#[assert(sizeof(T) == N, "...")]`。这是**最容易把 A 级打成 C 级**的一条：① 真实二进制多是手写序列化产物、**不是** C 按 ABI 排布的结果，按 C 规则补 padding 是假阳性 padding，会让后面所有字段错位；② 默认策略即 `#pragma pack(1)` 语义，不做任何隐式补齐（`AlignPolicy::ExplicitOnly`）；③ 仅当源显式写了 `alignas`/`aligned(N)`/`#pragma pack(N>1)` 才插 `padding[k]`，k 在 normalize 期算成常量；④ 每个定长结构输出 `struct Foo : N` + 一条 `#[assert]`，让错位在解析期可见而不是静默；⑤ 报告单列"padding 决策"节：offset / 来源 / 插入字节数。
6. **字符串编码与终止符** → `str(enc="ascii", term=0)` / `str(enc="utf16le", size=n)` / `str(enc="utf8", term=0, max=256)`。三条硬规则：定长零填充字段用 `size=` **而不是** `term=`（否则吃掉定长区外字节、后续错位）；`term` 按编解码单元判定，UTF-16 的 `term=0` 即双零字节（02 §3.6）；无长度约束的 C 字符串必须显式写 `max=`（引擎会兜底为剩余文件大小，但写出来用户才看得到风险边界）。
7. **条件字段** → `if (cond) { ... }` 或 `u32 x if flags & 1;`。ksy `if:` 与 `.bt` 的过程式 `if(...)` 都要重建成**块级** `if`，并保证 `else` 侧有等价字节消费或明确的"字段不存在"语义。`.bt` 那种"用 `if` 决定读不读、再靠 `FSeek` 修正偏移"的写法无法静态判定时降级 `Todo::ControlFlow`。
8. **宏与模板参数** → 单态化。BinSpec v0.1 无泛型，故 `#define` 只做数字/字符串字面量替换，参数化类型（`.bt` `<template>`、ksy `params`）按实例化点展开成 `Base_<Arg1>_<Arg2>`。带副作用的宏函数（`#define RD(x) ReadBytes(...)+1`）不展开 → `Todo::Macro`。产物可能膨胀 2~4 倍，可接受。
9. **`#include` 与依赖顺序** → v1 输出**永远是单个 `.bspec`**：按拓扑序内联，环依赖直接 `EB202`（不尝试打破）。理由：`import` 的可见性规则尚未定稿（02 §11.5），依赖未定语义的转换器会在定稿后全部返工。报告另附"建议拆分的模块边界"。
10. **大小端标注** → 全局 `meta/endian` → 文件级 `#pragma endian little;`；块级 `be { }`/`le { }` → 逐字段前缀展开（前缀只作用于紧跟的标量，02 §3.2）；**无任何标注** → 按 LE 输出 + 头部注释写明"端序未声明，按 LE 假设" + `Todo::ConfirmEndian`。端序错的产物能编译、能跑、值全错，所以这条必须出现在报告第一屏。

### 4.4 反向导出（互操作出口）

| 出口 | 命令 | 保真度 | 保住的 | 丢失的 |
|---|---|---|---|---|
| **JSON 结构描述** | `geode export --to json` | **A** | 布局名、字段名、类型、`at`/`cond` 表达式、`size`/`term`/`enc`、`#[assert]`、`origin` | 动态长度只剩表达式文本；UI 属性折叠进 `attrs` 映射 |
| **C struct** | `geode export --to c` | **B+** | 字段名/类型/嵌套/位域/enum、显式 padding（`uint8_t _padN[k]`）、`#pragma pack(push,1)` + `static_assert(sizeof(T)==N)` | 指针空间语义（`off32`/`rva32` 退化为 `uint32_t`）、空指针守卫、条件字段（退化为注释 + union 载体）、不可常量折叠的断言、变长尾部数组 |
| **Kaitai `.ksy`** | `geode export --to ksy` | **C**（尽力而为） | `struct`/`seq`/标量/定长数组/`endian`/`terminator`/`repeat-until` | `@` 的"不移动游标"（只能靠 `instances` 的 `pos:` 近似）、预算与软失败、`#[assert]` 仅支持单表达式、指针空间分层、多态层级 |

JSON 出口最稳：纯结构描述（`{"format":"geode-struct","version":1,"layouts":[...]}`）、无执行语义，且**同时是自测工具**——`ksy → bspec → json` 与 `ksy → json` 对拍，能廉价发现 emitter 回归。所有出口同样写 `origin`（`source_format = "geode-bspec"`，`distribution` 跟随原产物）。

### 4.5 CLI

```
geode migrate --from ksy --out png.bspec        # --from auto 亦可
    --samples a.png,b.png   # 送 verifier L2
    --image-base 0x400000   # → #pragma image_base
    --dry-run               # 只打印报告摘要，不落盘
    --report r.json         # 完整逐字段报告
    --pack tpl.png          # 直出 .geopkg 目录草稿（§7.3）
    --license Apache-2.0    # 缺省继承上游；只能收紧不能放宽
    --i-have-rights         # 启用 GPL 源（§7.4），产物强制 local-only
    --min-grade B           # 低于该级不落盘
geode export --from x.bspec --to json|c|ksy --out -
```

退出码：`0` 成功达标；`2` 有阻断错误；`3` 达标但低于阈值；`4` 许可 `forbidden`；`5` 内部错误（emitter bug）。

---

## 5 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 单文件转换（ksy，≤1000 行） | < 300ms | 含 verifier L1（编译 ≤ 20ms，00 §7） |
| 单文件转换（C header，≤2000 行） | < 400ms | 同上 |
| 批量 30 个 ksy 全量 | < 20s | CI 单核，含报告 |
| 峰值内存 | < 64MB | 源 ≤ 1MB / 字段 ≤ 20k |
| 输出确定性 | 逐字节一致 | 同输入两次运行 |
| UI 影响 | 零 | 走 Job + `CancelToken`，100ms 内响应取消（00 §6） |

基准落地：`bench/migrate/` 固化 30 个源文本 + 期望产物哈希；`cargo xtask bench migrate` 只看**总耗时**与**产物哈希是否变化**——哈希变化必须由 PR 显式更新，防止转换器静默改变输出。

---

## 6 失败模式与降级

### 6.1 错误码（`EBxxx`，归"自有格式校验与迁移"，待登记进 00 §12）

| 码 | 语义 | 级别 | 处置 |
|---|---|---|---|
| `EB101` | 源语法无法解析 | Error | 部分解析进 IR，其余标 Opaque |
| `EB102` | 源超限（1MB / 40k 行） | Error | 拒转，提示拆分 |
| `EB201` | 构造不可映射（内嵌运行时/副作用宏） | Warning | 降级 Opaque + Todo，仍输出 |
| `EB202` | 类型依赖成环 | Error | 拒转该模块，输出环路径 |
| `EB203` | 游标顺序不可重建（引用未来字段） | Warning | `Todo::Reorder`，产物 C 级 |
| `EB301` | 产物未过 L1 编译 | Error | 重试 1 次后降级骨架 |
| `EB302` | 产物未过 L2 执行 | Error | 同上 |
| `EB303` | 不变量未过（I4 覆盖/I5 长度自洽） | Warning | 降 B/C，报告首屏提示 |
| `EB401` | `origin` 缺失或不完整 | Error | **拒写产物**（不可关闭） |
| `EB402` | 命中 GPL/AGPL/LGPL 且未声明权利 | Error | 拒转；给 `--i-have-rights` 与法律提示 |
| `EB403` | `local-only` 产物尝试 `--pack` | Error | 拒打包，保留本地 `.bspec` |
| `EB404` | 与已知 GPL 语料 5-gram 相似度 > 0.6 | Warning | 转人工复核（同 15 §4.8 ⑤） |
| `EB501` | CLI IO / 目标已存在且无 `--force` | Error | 不覆盖 |

### 6.2 降级路径

| 情形 | 降级 |
|---|---|
| 整段过程式代码不可映射 | 该段输出 `padding[n]`（若定长）+ `// TODO(migrate): <原文摘要>` + `#[comment]` |
| verifier 反复失败 | 输出"已知能编译的骨架"：结构名、字段名、标量类型、显式 padding，去掉表达式与断言（C 级） |
| 指针空间不确定 | `off32` 占位 + `Todo::ConfirmPtrSpace`，报告单列清单 |
| 端序未声明 | LE 输出 + 头部注释 + `Todo::ConfirmEndian` |
| 许可未定 | `distribution = local-only`，禁止 `--pack`，UI 黄条常驻（§7.4.6） |

**铁律**：宁可输出 C 级骨架并明确标注，也绝不输出"看起来完整但跑不通"的模板（对齐 03 §6）。

---

## 7 与其它子系统的契约

### 7.1 `geode-bspec`

只读 AST 构造器与编译期类型表，**不改其类型系统**；某一源构造无法表达时由 migrate 单态化，而不是给 bspec 加特性。依赖 bspec 提供稳定的 pretty-printer（P 阶段）；若不提供则由 migrate 实现并让 bspec 单测对拍——**不允许两边各写一个不一致的 printer**。产物语法必须落在 02 已定稿子集内；碰到 02 §11 的待定项（`import` 可见性、union 默认大小）一律输出显式形式规避。

### 7.2 verifier

产物落盘前必过 L1 编译 / L2 执行 / L3 不变量 I1~I10（见 03 §5）。映射：`I1`/`I2` 不过 → 不得输出（`EB301`/`EB302`）；`I4 < 40%` 或 `I5` 不过 → 最高 B 级；全过且有样本 → A 级候选。字段级 `conf` 写入 `#[ai:conf(...)]`，与 03 §7 共用刻度，但转换器只在"结构自证"信号上给分、不给语义分，**上限 0.85**。

### 7.3 与内容分发（15）

`--pack` 产出的目录草稿与 15 §3.1 结构一致：`manifest.toml` + `spec/*.bspec` + `samples/*`（模板类 ≥1）+ `LICENSE`（Apache-2.0 必附 NOTICE）+ `preview.png`。规则：

- `origin.kind` 恒为 `"imported"`，`from`/`ref`/`upstream_license` 缺一即 `EB401`。
- `license` 缺省继承上游 SPDX；`--license` 只能**收紧**（改编作品不能单方面加宽松条款）。
- 打包后必过 `geode-content-lint` 五条（15 §4.8）：SPDX 合法、白名单、GPL 硬拒 `EA301`、imported 三字段齐全、5-gram < 0.6。
- **UI 义务**：详情页显示 `origin` 六项 + 许可徽章；结构树顶部对 `local-only` 产物显示常驻提示条；列表页至少显示许可徽章，不得折叠。

### 7.4 GPL 红线操作指南

**7.4.1 三层区分（读 / 学 / 抄）**

| 行为 | 结论 |
|---|---|
| 读 `.hexpat` 了解某格式的字段顺序、magic 字节 | **允许**。字节事实不受版权保护（15 §4.8） |
| 读 `.hexpat` 学 Pattern Language 的语法设计（`@`、`#[...]`、`u24`） | **允许**。语法与思想不受保护，只保护表达 |
| 把其**结构组织、字段命名清单、注释文字、断言文案**机械搬成 `.bspec` | **不允许**。构成改编 |
| 只看二进制与格式规范重写同格式模板（clean-room） | **允许**。唯一无争议路径 |

**7.4.2 什么构成改编（GPL-2.0 §2(b) 传染）**：判定不看"是否换了语法关键字"，而看**表达是否被复制**——① 字段**命名序列**被保留（一串 `header_t`/`magic_bytes`/`num_entries` 照搬）；② **拆分粒度**被保留（4 字节拆成两个 `u16` 还是留 `u32`，作者有选择自由）；③ **注释、断言消息、`#[color]`/`#[hidden]` 的选择**被保留；④ **组织顺序与辅助结构划分**被保留；⑤ 只做了"关键字替换 + 排版重写"。第 ⑤ 种"洗稿"会被 15 §4.8 ⑤ 的 5-gram 检查打中，且法律结论不变。一旦构成改编，§2(b) 要求"基于该作品的整体"按 GPL 分发；塞进 MIT/Apache 双许可发布物或官方索引，等于声称整个分发物可按 GPL 使用；"聚合（mere aggregation）"抗辩在官方模板库这种集成式分发里站不住（15 §4.8 已裁决）。

**7.4.3 "数据 vs 作品"的争议与保守做法**：可争辩的立场是"模板只是格式事实的机械编码"。这在**命名毫无自由度、规范逐字节给出**的格式上有一点说服力；但真实模板里命名、拆分、注释、断言、辅助结构划分都有大量选择自由，立场站不住。**本项目一律按作品处理**——理由不是法律确定性，而是**风险不对称**：误判为作品只损失一批模板，误判为数据可能让整个产品被迫 GPL 或下架。

**7.4.4 安全做法三条（必须同时满足）**：① **只做语法层转换、输出全新生成物**——不复制注释、不继承命名表、不做语义润色；对 GPL 源字段名一律来自格式规范/字段语义的规范化命名，报告里记录"替换了 N 处命名"（**这是降低相似度，不是法律免责**，两者不可混淆）；② **不复制注释与命名**——`--keep-comments` 对 GPL 源在配置层强制屏蔽（非默认值）；③ **产物必写 `origin` 并提示用户复核许可**——`distribution = local-only`，UI 显示"本模板源自 GPL-2.0 材料，仅限本机使用，不得分发"，`--pack` 直接 `EB403`。

**7.4.5 绝对禁止（写进代码检查，不只是文档）**：① 把 ImHex-Patterns 目录**原样或改名后**打包进内容库；② 把 `.hexpat` 作为 `samples/` 或 `docs/` 塞进 `.geopkg`（包内出现 GPL 文本即污染整包）；③ 把 ImHex 语料克隆进本仓库当回归测试集——**连测试语料也不行**，仓库本身是可分发物；④ 索引里出现 `upstream_license = GPL-*` 的包（`EA301` 硬拒）；⑤ 转换器默认开启 `.hexpat` 前端（默认 `disable_formats = ["hexpat","wireshark","construct"]`）。

**7.4.6 不确定时：默认禁止 + "法务未定"降级路径**：许可缺失、声明与仓库 LICENSE 矛盾、来源不明，一律走同一条路——`license = UNKNOWN → distribution = local-only`：产物可落盘、可本地使用（用户自担风险）；`--pack` 拒绝（`EB403`）；UI 黄条"许可状态未定，请勿分发；已记录待人工裁决"；写入 `state/migrate/pending_review.jsonl`（源 sha256 + 理由 + 时间）。这个文件是产品侧审计线索：某类格式高频出现，说明该格式许可普遍不清，应触发一次 clean-room 重写，而不是逐案裁决。

### 7.5 `geode-project` / UI

migrate 不持有状态：UI 导入向导（M6）收集参数 → 提交 Job → 读 `ConvertReport` 渲染"需人工复核清单"（可点击定位到 `.bspec` 对应行）。UI **不显示单一"转换成功率"**，只显示 A/B/C 分级 + 人工项条数（对齐 03 §7 "宁可承认不知道"）。

---

## 8 配置项与默认值

```toml
# config.toml [migrate]
default_from            = "auto"
disable_formats         = ["hexpat", "wireshark", "construct"]
keep_comments           = false       # GPL 源强制 false，不可覆盖
max_source_bytes        = 1_048_576
max_source_lines        = 40_000
bit_order_default       = "lsb"       # 始终写进产物并附 Todo
union_size_policy       = "max"       # .bt 自动切 first-member
padding_policy          = "explicit-only"   # 唯一合法值（P2）
endian_when_unspecified = "little"    # 始终在头部注释声明假设
single_file_output      = true        # v1 永远单文件内联
rename_for_gpl_sources  = true        # 记录替换次数
conf_ceiling_unverified = 0.30        # 对齐 03 §7
confidence_floor        = 0.50        # 低于此值自动加 Todo
min_grade_to_write      = "C"
emit_origin             = true        # 不可关闭（EB401）
allow_gpl_sources       = false       # 需 --i-have-rights + 每次会话确认
pack_allowed_distribution = ["index-allowed"]
sample_bytes_limit      = 8_388_608
```

四条硬编码不可配置：`emit_origin = true`、`padding_policy = explicit-only`、GPL 源 `keep_comments = false`、`distribution != index-allowed` 时禁 `--pack`。

---

## 9 测试策略

### 9.1 测试金字塔

| 层 | 内容 | 门槛 |
|---|---|---|
| 单元 | 每个 frontend 的语法覆盖（表驱动 40+ 畸形输入）；normalize 的每个决策点（位序、union 大小、指针空间、padding 计算、单态化） | 分支 ≥ 85%，normalize 100% |
| 集成 | 30 个 ksy → bspec → 编译 → 真实样本上跑 → 结构树快照 | 全部 ≥ C 级，A+B 达标 |
| 往返 | `ksy → bspec → json` vs `ksy → json` 对拍（§4.4） | 差异全部可解释且在白名单 |
| fuzz | 三个 frontend parser（10min/PR，1h/夜；语料 = 30 真实源 + 变异） | 不 panic、不 OOM、< 60s |
| 快照 | 产物哈希（`bench/migrate/*.sha256`） | 变化需 PR 显式更新 |
| 许可 | lint 五条正负用例；GPL 源必 `EB402`；伪造 `origin` 必 `EB401` | 0 漏判 |
| 确定性 | 同输入两次运行逐字节相等（含并行） | 必过 |

### 9.2 转换质量分级

| 级 | 定义 | 判定（机器可判 + 抽检） | 去处 |
|---|---|---|---|
| **A** | 直接可用 | L1/L2 通过 + I1~I10 全过 + `manual_items == 0` + 覆盖率 ≥ 75% + 抽检字段语义正确率 ≥ 90% | 索引 `stable` |
| **B** | 需人工改 ≤ 3 处（结构正确，个别位序/编码/指针空间待确认） | L1 通过 + L2 通过或仅 `unresolvable` + `1 ≤ manual_items ≤ 3` + 覆盖率 ≥ 60% | 索引 `beta`，标"需核对 N 处" |
| **C** | 仅骨架（能编译、字段类型齐全、表达式/断言缺或占位） | L1 通过 + `manual_items > 3` 或覆盖率 < 60% | **仅本地**，不得入索引 |

分级由 `ConvertReport.grade` 自动给出，必须附逐条判定依据（不能只给字母）。

### 9.3 批量评测方法（防玄学）

1. **语料**：从 Kaitai 官方格式库（Apache-2.0）抽 **30 个**，覆盖各风险维度：位域密集（PNG/GIF）、指针密集（PE/ELF、BMP 偏移表）、变长数组密集（MP4 box、RIFF chunk）、字符串密集（ID3、TrueType name）、条件字段密集（TIFF IFD、PCAP）。
2. **ground truth**：人工**独立编写**等价 `.bspec`（不逐行翻译，避免把 ksy 的表达缺陷带进答案），并给每个样本标注"字段 → 期望字节区间"表。
3. **指标**：`A_rate`/`B_rate`/`C_rate`、`manual_items` 中位数与 p90、`byte_coverage` 均值、`field_offset_precision`、`expr_fail_count`。
4. **M5 出口条件**：`A_rate ≥ 25%`、`A+B ≥ 60%`、`manual_items p90 ≤ 5`、`byte_coverage ≥ 70%`、零 panic；写进 CI，回退即红灯（同 18 §9）。
5. **回归**：语法演进、normalize 规则、verifier 规则任一改动都跑全量并归档 JSON 报告，出趋势图。

### 9.4 里程碑交付

| 里程碑 | 交付 | 验收 |
|---|---|---|
| **M3** | crate 骨架 + IR + **ksy 前端** + **C header 前端** + emitter + `--report`（复用 M3 已就绪的 Verifier，03 A1） | 30 个 ksy：A+B ≥ 40%、无 panic、报告逐字段可读；5 个真实固件头文件 ≥ B |
| **M5** | **`.bt` 前端（骨架级）** + 反向导出（json/c/ksy）+ `--pack` + 分级自动判定 + 评测 CI | `.bt`：C 级为主、A+B ≥ 15%；json 往返零差异；`A_rate ≥ 25%` |
| **M6** | 导入向导 UI + 与 `geode-content` 打通（origin/license/签名）+ 首批 40 个 A/B 模板进 `beta` | imported 模板 origin 100% 完整；GPL 源 100% 被拒 |

---

## 10 开放问题

| # | 问题 | 倾向 | 决策点 |
|---|---|---|---|
| Q1 | 是否内置 clang 解析 C header？ | **不做**。libclang ≥ 40MB，与"静态链接优先"（00 §2）冲突；v1 自研受限 parser（单文件 + 简易 `#include` + 常量 `#define`），超纲即 `EB201` | M3 |
| Q2 | 是否支持 Wireshark Lua dissector？ | 不做前端，改"**辅助模式**"：用户跑 tshark 导出字段表 CSV，migrate 读表生成骨架——绕开执行 Lua 与 GPL | M6 |
| Q3 | 导入时是否做许可白名单**硬过滤**？ | 本地宽松、入库严格：本地只警告，`--pack`/入库硬拒。过度限制本地使用会迫使用户绕过工具，反而失去可控性 | M5 |
| Q4 | 是否提供在线转换服务？ | **不做**。上传源码即暴露私有格式；服务端成为 GPL 内容中转站（法律风险全归我们）；无商业模式 | 关闭 |
| Q5 | `MigModule` 的 serde 形式是否作为公开中间格式标准？ | 内部私有。公开即背 semver 承诺，且与 bspec AST 重复 | M5 |
| Q6 | 反向导出 C 用 C11 `static_assert` 还是运行时检查？ | C11 `static_assert` + `#pragma pack(1)`，头注释注明"仅用于文档/交叉校验，不可直接作解析器" | M5 |
| Q7 | 产物字段名强制 `snake_case`？ | 强制（对齐 02 §11.7），报告中保留原名映射 | M3 |
| Q8 | 是否对 `.bt` 过程式片段尝试抽象解释恢复声明式布局？ | v1 不做（工期不可控），先"骨架 + TODO"覆盖，M7 再评估 | M7 |

---

## 附：需回填的其它文档

1. **00 §3 / 18 §3.2**：新增 `geode-migrate`（00 图放 bspec 旁支 L1 带；18 表内层号 **2**，依赖 `core`、`bspec`），并登记进 `cargo xtask deps` 目标表。
2. **00 §12**：登记 `EBxxx`（自有格式校验与迁移，码位见本文 §6.1）；`00 §10` 子文档清单补一行 `24-migration-and-interop.md`。
3. **15 §4.8**：补"`origin.distribution = local-only|forbidden` 不得进索引（`EA301`），该字段由 `geode-migrate` 写入（见 24 §7.3）"；`manifest.origin` 增 `converter`/`confidence` 两个可选键。
4. **03 §11.1**：评测集可直接复用本文 §9.3 的 30 个 ksy 语料与 ground truth，避免两套标注。
