# BinSpec — 二进制描述语言 规范草案 v0.1

> 状态：Draft，未实现。目标：定义一门**人类易写、机器可验证、AI 可生成**的二进制结构描述语言。
> 文件扩展名：`.bspec`

---

## 1. 设计原则（每条都要能用来裁决后面的分歧）

| # | 原则 | 推论 |
|---|---|---|
| P1 | **贴 C** | 会 C 的人零成本上手。`{ }`、`;`、`struct`、`if`、C 运算符。不做"更优雅但更陌生"的语法（这是 010 赢下市场的根本原因） |
| P2 | **字节即真理** | 语言描述的是**字节布局**，不是内存布局。**绝不自动对齐、绝不隐式 padding**。C 的 struct 对齐规则在这里是毒药 |
| P3 | **一切可回链** | 每个解析出的值都携带 `(offset, size, path)`。没有这个，UI 双向联动做不出来，AI 也无法归因 |
| P4 | **显式优于推断** | 字节序、长度、终止符、位序都要能显式写出来。宁可啰嗦，不可含糊 |
| P5 | **默认安全** | 越界读 = 错误返回而非崩溃；无限循环有预算；解析永远可中断、可取消 |
| P6 | **错误可恢复** | 单个字段错不终止整个解析。产出"部分结果 + 精确错误清单"，而不是"失败" |
| P7 | **AI 友好** | 语法无歧义、报错为机器可读 JSON、允许冗余显式声明（AI 写冗余显式反而更准） |

**AI 友好 ≠ 人类友好，冲突时按顺序 P1 → P7 裁决。** 但本设计的经验是：显式、无歧义的语言对两者都更好。

---

## 2. 词法

- 注释：`//` 行注释，`/* */` 块注释。**注释是 AI 输出里最有价值的信号，必须能在 UI 里当文档显示**
- 标识符：`[A-Za-z_][A-Za-z0-9_]*`
- 字面量：`123`、`0x1F`、`0b1010`、`0o17`、`1.5`、`1.0e-3`、`"str"`、`'c'`、`null`、`true`、`false`、`inf`
- 字符串支持转义 `\n \t \\ \" \x41 \u{1F600}`
- 数值允许 `_` 分隔：`0xDEAD_BEEF`

---

## 3. 类型系统

### 3.1 基础类型（全部占固定字节数）

```
u8  u16 u24 u32 u48 u64 u128
s8  s16 s24 s32 s48 s64 s128
f16 f32 f64
bool            // 1 字节，0=false，非0=true
char            // 1 字节，按 ASCII/Latin-1 显示
byte            // 1 字节，无符号，按字节显示
```

`u24`/`u48`/`s24` 不是凑数——PE、DVB、某些动画格式里真实存在，缺了只能退化成 `u8 x[3]` + 手算，体验断崖。

### 3.2 字节序

三层控制，从粗到细：

```cpp
#pragma endian little;        // 默认，文件级
struct H { be u32 magic; le u32 size; };   // 前缀覆盖，字段级
```

- 缺省 little（绝大多数真实文件是 LE）
- 前缀 `be` / `le` 只对紧跟的那个类型声明的**标量**生效（不递归到 struct 内部——struct 内部有自己的 pragma/前缀）
- `#pragma endian` 只在当前作用域（文件/struct）生效，可在 struct 内重新声明（很多格式里嵌套结构字节序相反）

### 3.3 偏移/指针类型

二进制里的"指针"有四种语义，全部内置，不靠用户手算：

```
off32  off64       // 文件绝对偏移
rva32  rva64       // 相对镜像基址（PE）
rel32  rel64       // 相对自身位置（ELF、DWARF）
va32   va64        // 绝对虚拟地址（需用户提供 base）
```

```cpp
#pragma image_base 0x400000;   // 供 rva/va 使用

struct PEHeader {
    u32 signature;
    rva32 entryPoint;          // 值 = 0x1234，语义 = image_base + 0x1234
};
```

解引用：用 `@` 跳过去读。

```cpp
Node node @ 0x10;
Node next @ node.next;                  // node.next 是 off32，直接当偏移用
char name[] @ next.name if next.name != 0;   // 空指针守卫
```

> **语义规则 S1（软失败）**：`@` 的目标超出文件范围时，**不是硬错误**，而是把该字段标记为 `unresolvable`，记录原因，并继续解析下一个字段。逆向时指针经常是坏的，硬失败等于不可用。

### 3.4 复合类型

```cpp
// 变长结构（读到自然结束）
struct Header {
    char magic[4];
    u32  length;
    u16  type;
};

// 定长结构：必须恰好 N 字节，不足补隐式 padding，超出报错
struct DirEntry : 32 {
    char name[24];
    u32  offset;
    u32  size;
};

union Value {           // 所有成员同偏移；大小 = max(成员) 或显式指定
    u32 raw;
    struct { u16 lo; u16 hi; } parts;
};

union Packed : 8 {
    u64 raw;
    struct { u8 tag; u8 data[7]; } v;
};

enum Format : u16 {
    Unknown = 0x0000,
    Png     = 0x8950,
    Jpeg    = 0xFFD8,
};

bitfield Flags : u8 {
    compressed : 1;
    encrypted  : 1;
    _          : 6;      // 未命名位，跳过
};
```

### 3.5 位域位序（必须显式）

位域的"第 0 位"是 MSB 还是 LSB，**是二进制格式里最经典的歧义**，不同规范默认相反。

```cpp
bitfield Flags : u8 #[bits: lsb] { ... };   // 默认 lsb（x86 习惯）
bitfield Flags : u8 #[bits: msb] { ... };   // 网络/压缩格式常见
```

### 3.6 字符串

字符串不是 `char[]` 的语法糖，是独立参数化类型（终止符/长度/编码必须显式）：

```cpp
str(enc="utf8",  term=0)     name;      // 0 结尾
str(enc="utf16le", term=0)   wideName;  // 双 0 结尾
str(enc="utf8",  size=len)   body;      // 定长
str(enc="ascii", term=0, max=256) label; // 有上限，防止无终止符时读到文件尾
```

`max` 缺省时，运行时生效的上限 = 剩余文件大小（由引擎强制，用户无法解除）。

### 3.7 类型别名

```cpp
using VCInt = s32;      // 只是显示友好，语义同 s32
using DWORD = u32;
```

---

## 4. 语句

### 4.1 成员声明（消费字节）

```cpp
<type> <name> [ <array> ] [ @ <offset> ] [ if <cond> ] ;
```

```cpp
u32       width;                // 顺次读 4 字节
u8        data[16];             // 定长数组
u8        data[count];          // 表达式定长（count 是前面读到的字段）
u8        data[];               // 读到文件尾
char      s[] until(0);         // 读到终止符
Rect      recs[] until($ >= tableEnd);
padding   [4];                  // 跳过 4 字节（不产生值，但占字节，参与 size 计算）
```

### 4.2 位置声明（顶层/跳转）

```cpp
Header    hdr @ 0x00;
Section   sects[count] @ sectTable;
u8        trailer @ $size - 8;   // 从末尾往前
```

`@` 只改变**这次读取的起始位置**，读完不改变当前游标（除非表达式里显式使用 `$`）。

> **语义规则 S2（不移动游标）**：`X a @ expr;` 等价于"在 `expr` 处读一个 `X`"，读完后游标回到原处。这与 ImHex 的 `@` 语义一致，也是唯一能让"散布在文件各处的结构"被独立描述的模型。

### 4.3 计算值（不消费字节）

```cpp
let pixelCount = hdr.width * hdr.height;
let isCompressed = (flags.compressed == 1);
```

`let` 值参与 `sizeof` 计算？——**不参与**。显式区分"消费字节"和"纯计算"是 P4 的直接体现，也避免 AI 混淆。

### 4.4 控制流

```cpp
if (hdr.version >= 2) {
    ExtHeader ext;
} else {
    padding[8];
}

match (hdr.type) {
    Format::Png  => { PngChunk chunk; }
    Format::Jpeg => { JpegSegment seg; }
    _            => { u8 rest[]; }        // 兜底，强烈建议总写
}

for (u32 i = 0; i < hdr.count; i = i + 1) {
    Entry e;
}

while ($ < hdr.dataEnd) {
    Chunk c;
}
```

### 4.5 函数

```cpp
fn crc32Region(off: u64, len: u32) -> u32 {
    return std::checksum::crc32(off, len);
}

fn readChunks(tag: u32) {
    while (peek(u32) != tag) { Chunk c; }
}
```

限制：无闭包、无堆分配、无 I/O 副作用（除读字节）、无外部调用。**这是沙箱的边界，不能为了表达力突破。**

### 4.6 断言（AI 和人类都最需要的东西）

```cpp
#[assert(hdr.length == $size - 8, "length 字段与文件实际大小不符")]
```

断言失败 = warning 级别（不是 error），因为真实世界里很多文件确实不符合自己的规范。但断言是**验证闭环的关键信号**，见 AI 文档。

---

## 5. 表达式

### 5.1 内置量

| 记号 | 含义 |
|---|---|
| `$` | 当前读取偏移（游标） |
| `$size` | 文件总大小 |
| `$start` | 当前所在 struct/union 的起始偏移 |
| `$end` | 当前所在 struct/union 的结束偏移（仅定长结构可用） |
| `sizeof(T)` | 类型 T 的字节长度（变长类型返回值 = 当前实例长度） |
| `count(T)` | 数组/map 的元素个数？ — **v0.1 移除，用 `len(arr)`** |
| `len(x)` | 字符串字节长度 / 数组元素数 |

### 5.2 运算符

完整 C 优先级与语义：`|| && | ^ & == != < <= > >= << >> + - * / % ! ~ - +`、三元 `?:`、成员 `.`、索引 `[]`。

补充：`**` 幂运算；`//` **不是**注释（注释已占用）。

整数除法向零截断；除零 = 运行期错误（带偏移）；溢出按类型位宽回绕（与文件格式的实际行为一致，不 panic）。

### 5.3 内建库（`std::`）

```
std::mem::read_u8/u16/u32/u64(off)        随机读（AI 用来试探）
std::mem::read_bytes(off, len)
std::mem::find(pattern, from)              位置搜索
std::checksum::crc32(off, len, poly?, init?)
std::checksum::md5/sha1/sha256(off, len)
std::string::utf8_to_str(bytes)
std::convert::leb128_decode(off)
std::convert::varint_decode(off)
std::time::unix32(v) / unix64(v) / dos(v)  时间戳格式化
std::guid::from_bytes(v)                   GUID 格式化
std::math::*
```

内建库要**小而确定**——每个函数都必须可 O(1) 或线性、有明确预算、结果可缓存。不做 `std::file::write` 这类东西（沙箱原则）。

---

## 6. 属性（Attribute）

`#[name(args)]` 附着在声明上，全部可选，全部影响 UI 或验证，不影响字节消费。

| 属性 | 作用 |
|---|---|
| `#[color("red")]` / `#[color("#3a7")]` | 该字段在 hex 视图里的背景色 |
| `#[format("hex"\|"dec"\|"oct"\|"bin"\|"date"\|"guid"\|"ipv4")]` | 值的显示格式 |
| `#[comment("...")]` | 侧栏/悬浮提示 |
| `#[hidden]` | 不在结构树里显示（padding 等噪音） |
| `#[optional]` | 解析失败时静默（等价于 try） |
| `#[assert(expr, "msg")]` | 断言 |
| `#[bits: msb\|lsb]` | 位域位序 |
| `#[visualize("image/png"\|"audio/wav"\|"coords/xyz"\|"timeline")]` | 交给宿主的可视化器渲染 |
| `#[alias("DWORD")]` | 显示别名 |
| `#[ai:conf(0.0~1.0)]` | **AI 生成时写入的置信度**（见 AI 文档 §4） |
| `#[ai:note("...")]` | AI 的推理备注，UI 里可折叠显示 |

---

## 7. 执行模型

### 7.1 编译流水线

```
source → lexer → parser → AST → 语义检查 → 字节码 → VM 执行
                                    ↓
                          符号表 / 类型表 / 依赖图
```

**编译到字节码，不做 AST 树遍历**，理由：
- 指令计数即预算，粒度精确、实现简单（`steps += 1`）
- 取消（Esc / 编辑 / 换文件）在当前指令边界检查，延迟 < 1µs
- 栈深限制天然由 VM 帧栈控制
- 单步调试 / 断点 / 覆盖率（哪些字段被读过）都是字节码层面的廉价操作

### 7.2 预算（Budget）—— 沙箱的唯一保障

```rust
struct Budget {
    steps:     u64,   // 默认 50_000_000
    wall_time: Duration, // 默认 5s（可配 1~60s）
    mem_vars:  usize, // 默认 1_000_000 个变量
    depth:     usize, // 默认 256 层嵌套/递归
    output:    usize, // 默认 64MB 解析结果
}
```

超预算 → `E2001 执行预算耗尽`，附带**已解析的部分结果**和"卡在哪个循环的第几次迭代"。这条信息对调试模板至关重要。

`#[budget(steps = "100M")]` 允许单文件放宽，但用户在设置里设的硬上限不可被模板突破。

### 7.3 解析结果模型（P3 的落地）

```rust
struct Node {
    name:     String,
    type_name: String,
    offset:   u64,
    size:     u64,          // 消费的字节数；let 值为 0
    value:    Value,        // Scalar / Compound / String / Bytes / Unresolved
    children: Vec<Node>,
    attrs:    Attrs,        // color / format / ai:conf / ...
    errors:   Vec<Diag>,    // 该节点范围内的诊断
    path:     Vec<usize>,   // 结构树路径，用于双向联动
    src_span: SrcSpan,      // 生成该节点的源码区间；与 offset/size 严格不同型，禁止互赋（见 22 §3.3）
}
```

`offset + size` 是双向联动的全部秘密：**点结构树 → 用 `(offset,size)` 在 hex 视图选中；点 hex → 用区间树反查最深命中的 node。**

### 7.4 增量重解析

编辑后不重跑全文：
1. 顶层每个 `@` 声明是独立"解析单元"
2. 维护 `unit → dirty_ranges` 映射
3. 编辑发生在某 unit 的 `[offset, offset+size)` 内 → 只重跑该 unit
4. **插入/删除导致文件长度变化 → 全部失效**（此时给用户明确的"正在重新解析"提示，或让用户手动触发）

---

## 8. 错误模型

### 8.1 诊断结构（机器可读，AI 的修复输入）

```json
{
  "severity": "error",
  "code": "E1004",
  "message": "magic 不匹配",
  "offset": 4096,
  "length": 4,
  "expected": "0x50 0x4B 0x03 0x04",
  "actual":   "0x00 0x00 0x00 0x00",
  "path": ["ZipArchive", "localHeader"],
  "suggestion": "该区域不是 ZIP local file header；文件可能被截断，或偏移字段指向了数据区",
  "recoverable": true
}
```

必备字段：`offset` / `length` / `expected` / `actual` / `path` / `recoverable`。少任何一个，UI 都做不到"在 hex 视图里精确标红那个字节"，AI 也做不到自修复。

### 8.2 错误码空间

| 段 | 含义 |
|---|---|
| `E1xxx` | 数据不匹配（magic 错、枚举值未知、断言失败） |
| `E1006` | 数据不匹配·断言：`#[assert]` 或 `assert()` 失败（**不归 E5**，E5 只表示前置上下文缺失） |
| `E2xxx` | 执行限制（预算耗尽、栈溢出、取消） |
| `E3xxx` | 内存/边界（越界读、解引用失败、读取超限） |
| `E4xxx` | 语法/类型（解析错误、类型不匹配、未定义符号） |
| `E5xxx` | 前条件（未提供 image_base、未提供所需上下文） |

### 8.3 恢复策略

- 单个成员解析失败 → 标记该成员 `unresolvable`，游标按"声明的定长大小"前移（定长类型可继续；变长类型无法恢复 → 终止当前 struct）
- struct 内失败 → 冒泡为该 struct 节点的 `errors`，**不终止上层**
- 顶层多个 `@` 单元互相独立，一个失败不影响其他

---

## 9. 与竞品特性对比

| 特性 | BinSpec（本设计） | 010 Template | ImHex PL | Kaitai Struct |
|---|---|---|---|---|
| 语法接近 C | ✅ 强 | ✅ 强 | ⚠️ 中（`@`/`#[..]` 风格独特） | ⚠️ 中（YAML 式类型定义） |
| 显式禁止隐式对齐 | ✅ | ✅ | ✅ | ✅ |
| 字节序前缀 | ✅ `be u32` | ✅ `BE u32` | ✅ `be u32` | ✅ `be` 块 |
| 位置声明 `@` | ✅ | ❌（需 `FSeek`） | ✅ | ❌（`pos:`） |
| u24/u48 | ✅ | ✅ | ✅ | ⚠️ |
| 位域位序显式 | ✅ `#[bits]` | ✅ | ✅ | ✅ |
| 指针语义分层（off/rva/rel/va） | ✅ | ❌ | ❌ | ❌ |
| 空/坏指针软失败 | ✅ 内建 | ❌ 硬失败 | ⚠️ try | ❌ |
| 沙箱预算内建 | ✅ 一等公民 | ⚠️ 有限 | ✅ | N/A（生成代码） |
| 诊断 JSON 化 + 字节级定位 | ✅ | ❌ | ⚠️ 文本 | N/A |
| 结构 ↔ 字节双向回链 | ✅ 一等公民 | ✅ | ✅ | N/A |
| 实时编辑反馈 | ✅ | ✅ | ✅ | ❌（离线生成代码） |
| 增量重解析 | ✅ 设计内 | ⚠️ | ⚠️ | N/A |
| AI 可生成性（语法无歧义 + 可验证） | ✅ 明确目标 | ⚠️ | ⚠️ | ✅（结构清晰但表达力弱） |
| 断言作为一等语法 | ✅ | ✅ | ⚠️ | ⚠️ |

**差异化的三条**：
1. **指针语义分层 + 软失败**——真实逆向场景里坏指针是常态，现有工具全都会中断解析
2. **诊断机器可读**——同时服务 UI 和 AI 修复闭环
3. **为 AI 生成而设计**——语法无歧义、允许冗余显式、报错结构化

---

## 10. 完整示例：ZIP Local File Header

```cpp
#pragma endian little;

struct ZipLocalHeader : 30 {
    u32   signature;              // 0x04034B50
    u16   versionNeeded;
    u16   flags;                  // #[format("hex")]
    u16   method;                 // 0=store, 8=deflate
    u16   modTime;                // #[format("dos-time")]
    u16   modDate;
    u32   crc32;
    u32   compressedSize;
    u32   uncompressedSize;
    u16   nameLen;
    u16   extraLen;

    char  name[]   size=nameLen;
    u8    extra[]  size=extraLen;
    u8    data[]   size=compressedSize;

    #[assert(method == 0 || method == 8, "unsupported compression method")]
    #[assert(signature == 0x04034B50, "not a local file header")]
};

struct ZipArchive {
    // 顶层单元：先定位第一个 header
    ZipLocalHeader first @ 0x00;

    // 顺次读后续（真实 ZIP 需要中心目录，这里演示 until 语法）
    ZipLocalHeader rest[] until($ >= $size - 22);

    // 尾部的 EOCD
    struct {
        u32 signature;    // 0x06054B50
        u16 diskNum;
        u16 cdDiskNum;
        u16 entriesOnDisk;
        u16 totalEntries;
        u32 cdSize;
        u32 cdOffset;
        u16 commentLen;
        char comment[] size=commentLen;
    } eocd @ $size - 22;
};

ZipArchive zip;
```

---

## 11. 待定问题（需要在实现前拍板）

> 第 1、5、7 条已被 22 的工具链消费（分别决定 lint 规则 L006 / L015 / L023 的等级与话术，见 22 §4.2）。拍板前按本稿倾向执行，不要在实现里各自决定。

1. **`@` 是否移动游标**：本稿定为"不移动"（与 ImHex 一致），但会让"顺序读 + 偶尔跳转"的写法变啰嗦（需 `@ $`）。备选：引入 `seek()` 语义。**倾向保持不移动**，因为可组合性远重要于简洁。
2. **union 的默认大小**：`max(成员)` 还是"最后一个成员"？本稿取 `max`，但 010 取首个。会影响真实格式的解析正确率，需实测。
3. **递归 struct 的深度上限**：链表解析天然递归，256 层够不够？可能需要 4096。
4. **是否需要 `switch` 的 fallthrough**：倾向不需要（真实格式里几乎没用，反而制造歧义）。
5. **模板的"库"机制**：`import "std/zip.bspec";` 的符号可见性规则未定（全局 vs 显式）。
6. **是否需要"写"能力**（模板反向序列化 → 生成字节）？成本极高（等长、字节序、padding 全部要反推），建议 v2 再做，但**语法要预留**（比如 `#pragma writable`）。
7. **AI 生成字段名的命名规范**是否要强制（snake_case / camelCase）——影响 diff 和用户熟悉度。
