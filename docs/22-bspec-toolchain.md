# 22 — BinSpec 工具链（fmt / lint / LSP / 语法高亮 / 调试器）

> 本文**拥有**：`geode bspec` 命令矩阵、格式化规则集、lint 规则表、语言服务器能力与增量编译接口、编辑器语法资产（TextMate / tree-sitter）、VM 调试器与执行轨迹、错误码→用户话术映射。
> 本文**不得涉及**：BinSpec 语言语义（`见 02`）、AI 生成与验证闭环内部（`见 03`）、UI 布局与 i18n 加载机制（`见 16`）。
> 核心判断：**03 解决"AI 写得出"，本文解决"人写得顺"。DSL 的成败一半在这里。**

## 0 决策速览

1. **一份源码、三种消费者**：fmt / lint / LSP 全部复用 `geode-bspec` 的 lexer + parser + 符号表，**不写第二套解析器**。唯一例外是 tree-sitter（让没有编译器的编辑器也能高亮），用差分测试钉死（§4.6）。
2. **lint 是 verifier 的第 0 层**：`L0 lint → L1 编译 → L2 执行 → L3 不变量`；AI 输出与人类手写走**同一套规则、同一套等级**（§7.1）。
3. **源码位置 ≠ 数据区间**：新增 `SrcSpan` + `LineIndex`，与 00 §4 的 `ByteRange` 严格分型、禁止互赋——这是 LSP 与结构树联动的头号崩溃源。
4. **`fmt` 是纯函数且必须幂等**：`fmt(fmt(x)) == fmt(x)` 进 CI；`parse(fmt(x))` 的 token 流必须与 `parse(x)` 等价。
5. **格式化只动空白**：不改标识符、不排序、不增删声明、不换进制、不重写注释文本。有争议的"美化"推给 lint 的 quickfix。
6. **诊断先到、精确后到**：LSP 200ms 内先吐语法级诊断，语义/运行级超预算即转后台补发第二次 `publishDiagnostics`，永不阻塞输入。
7. **调试器是一等公民**：`geode bspec trace --step-limit` 与 GUI 调试器共用同一 `DebugSession`，轨迹 JSONL 可入 CI 回归。

## 1 目标与非目标

| # | 目标 | 验收判据 |
|---|---|---|
| G1 | 写 BinSpec 的手感 ≈ "写 C + 一个懂格式的助手" | 30 分钟教程后能不看文档写出可解析 PE 头的最小模板 |
| G2 | 错误翻译成人话并给出下一步 | 每条 E1~E5 诊断都有非空 `user_text` + `action`，无 key 缺失 |
| G3 | 模板风格统一 | `bspec fmt --check` 在 CI 上 0 差异；模板库（`见 15`）批量校验 100% 通过 |
| G4 | GUI / CLI / CI 诊断结论一致 | 三处跑同一 `lint + compile`，结果集字节级相同（差分测试） |
| G5 | 能像调试程序一样调试解析 | 断点/单步/预算余量/游标可见，停下时联动帧 ≤ 16ms |
| G6 | 可脱离 GUI 发布 | `geode-bspec-cli` 三平台静态二进制 < 8MB，无 GUI 环境可 `check` |

**非目标**：N1 不做通用 IDE（构建系统、包管理、多语言工程）；N2 不做 LSP 全协议（只做 §3.6 的 10 项 + 生命周期，`workspace/*` 重型能力留到 v2）；N3 fmt 不做语义重构（折常量、提 struct、内联 `let` 只能作 lint 建议）；N4 v1 不支持"写回字节"的模板（02 §11 未拍板，工具链不留后门）；N5 不为调试另写解释器（直接跑真实字节码并插钩子，两套执行器必然漂移）。

## 2 依赖与边界

| crate | 内容 | 允许依赖 | 禁止 |
|---|---|---|---|
| `geode-bspec` | lexer / parser / 语义检查 / 字节码 / VM / `StructureIndex` | `geode-core`、`geode-provider`（只读） | GUI |
| `geode-bspec-fmt` | 纯函数格式化器（`&str → String`），**零 I/O、零时钟** | `geode-bspec`（只读 AST + token 流） | 其它一切 |
| `geode-bspec-lint` | 规则引擎、规则表、诊断→话术映射 | `geode-bspec`、`geode-bspec-fmt`（生成 fix） | GUI |
| `geode-bspec-cli` | `geode bspec *` 的实现体，被 `app` 与独立二进制共用 | 上述三者 | GUI |
| `geode-bspec-lsp` | LSP 服务器（`bspec-lsp` 二进制） | 上述三者 | GUI、`geode-ui` |

**分层铁律**（00 §2/§3 推论）：以上全部**不得依赖 `geode-ui`**；LSP 只能通过 `geode-core::Diag` 与 `Arc<StructureIndex>` 与 GUI 对齐语义。独立二进制可依赖 `geode-provider`（`check --run` 要打开真实文件），但只读，不依赖 `geode-buffer` 的编辑能力。

上游契约（读）：`geode-core`（00 §4）的 `Diag`/`Severity`/`ByteRange`/`Path`；`02` §2~§6 的词法、类型、字节序、属性、内置量（高亮与补全的**唯一**词表来源）；`02` §7.1/§7.2 的编译流水线与 `Budget` 字段（调试器余量、`--step-limit` 语义）；`02` §7.3/§7.4 的 `Node` 与 unit/`dirty_ranges`；`16` §3.5 的 `dsl`/`errors` 命名空间；`03` §5.1 的 L1~L3 分层（lint 插为 L0 的位置）。下游：`geode-ui`（模板编辑器、结构树、Inspector）、`app`（命令注册，`见 16 §3.2`）、`geode-content`（模板入库校验，`见 15`）、`geode-ai`（lint 结果喂回修复循环，`见 03 §6`）。

## 3 数据结构与接口

### 3.1 工具矩阵

| 工具 | 形态 | 典型调用 | 依赖能力 | 主要使用者 |
|---|---|---|---|---|
| `geode bspec check` | CLI | `check t.bspec [--run dump.bin] [--json]` | 语义检查（L1）+ 可选 VM 执行（L2） | CI、pre-commit、AI verifier |
| `geode bspec fmt` | CLI | `fmt t.bspec [--check] [--diff] [-w]` | lexer + token 流（不需完整编译） | 人、CI、AI 后处理 |
| `geode bspec lint` | CLI | `lint [--level warn] [--json] [--fix] [--max-warn 0]` | AST + 符号表 + 类型表 + magic 库 | CI、模板库入库 |
| `geode bspec stat` | CLI | `stat t.bspec --run dump.bin` | VM 结果 + `StructureIndex` | 评测、模板调优 |
| `geode bspec explain` | CLI | `explain t.bspec:12:7` / `explain E1004` | 推导链（字节序来源、类型大小、诊断码） | 新手、答疑 |
| `geode bspec trace` | CLI | `trace t.bspec --run d.bin --step-limit N --out t.jsonl` | VM 单步 + 事件流 | 调试、CI 回归 |
| `bspec-lsp` | stdio 服务器 | 编辑器 `--stdio` 拉起 | 上述全部 + 文件缓存 | VSCode / Zed / Neovim / Emacs |
| `vscode-bspec` / `zed-bspec` | 编辑器扩展 | marketplace / extension registry | `bspec-lsp` + 语法资产 | VSCode 用户 / Zed 用户（WASM） |
| `tree-sitter-bspec` / `BinSpec.tmLanguage.json` | 语法资产 | `editors/` | 独立 grammar / 正则 scope | Zed、Helix、Neovim / Sublime、GitHub |

`stat` 输出字段固定：`lines / bytes / tokens / units / nodes / max_depth / coverage_pct / steps_used / wall_ms / slowest_std_calls[5] / lint_counts{off,warn,error}`。`coverage_pct` 与 03 §5.2 的 I4 同量同式（`Σ 顶层 unit 覆盖字节 / file_size`，重叠区间只算一次）。

### 3.2 源码位置与数据区间分离

```rust
pub struct SrcSpan { pub start: u32, pub len: u32 }   // 源码 UTF-8 偏移，**永不是数据偏移**
pub struct LineIndex { /* 行起始表 + 全 ASCII 快速位图 */ }
impl LineIndex { pub fn new(src: &str) -> Self;
                 pub fn to_pos(&self, sp: SrcSpan) -> (Pos, Pos);   // 1-based line / 0-based utf16 col
                 pub fn to_span(&self, p: Pos) -> Option<SrcSpan>; }
pub struct SourceFile { pub uri: SourceUri, pub text: String, pub index: LineIndex, pub version: i64 }
```

`Diag.range: ByteRange` 是**数据**区间（00 §4 硬要求，UI 要在 hex 视图标红）；源码侧一律 `SrcSpan`。`Lint`/`LspDiag` 只带 `SrcSpan`，要定位数据区时经 `Node.src_span` 反查（§7.3 的加字段请求）。

### 3.3 格式化接口

```rust
pub struct FmtOptions { pub indent: u8 /*4*/, pub max_width: u16 /*100，显示列*/,
                        pub attr_break: AttrBreak,      // Preserve | OnePerLine | Inline
                        pub blank_between_top: u8 /*1*/, pub comment_gap: u8 /*1*/ }
pub struct FmtOutput { pub text: String, pub edits: Vec<TextEdit>,   // 只覆盖空白/换行 span
                       pub ast_equal: bool, pub changed: bool }
/// 纯函数：同输入 + 同 options 必须逐字节同输出。
pub fn format(src: &str, opt: &FmtOptions) -> Result<FmtOutput, FmtError>;  // FmtError = Syntax{at,code} | Unsupported
```

语法错误一律 `Err`：**fmt 不猜测**，残缺源码原样交还并附首个错误位置。

### 3.4 lint 接口与规则表

```rust
pub struct LintRule { pub id: RuleId, pub level: LintLevel,   // Off | Info | Warn | Error（出厂等级，error 不可下压）
                      pub fix: FixKind,                      // None | Safe | Suggest | QuickFixOnly
                      pub doc_key: &'static str,             // i18n：dsl.lint.L016.why
                      pub run: fn(&LintCtx, &mut Vec<Lint>) }
pub struct Lint { pub rule: RuleId, pub span: SrcSpan, pub msg_key: &'static str, pub args: Vec<(&'static str, String)>,
                  pub related: Vec<SrcSpan>, pub fix: Option<Fix> }
pub struct LintConfig { pub overrides: BTreeMap<RuleId, LintLevel>,
                        pub allow_spans: Vec<SrcSpan> /* 行内 // bspec-allow(L016) */ }
pub fn lint(src: &str, cfg: &LintConfig, ctx: &LintCtx) -> Vec<Lint>;
```

`LintCtx` 惰性构建，昂贵部分只在相关规则开启时才算：`tokens`、`ast`、`symbols`（含未定义引用表）、`types`、`units`（`@` 单元及静态可达大小）、`magic_hits`（`见 10`）、`vm_trace`（仅 `--run`）。

| id | 等级 | 说明 | 自动修复 |
|---|---|---|---|
| L001 | error | **引用未定义符号**（字段/类型/函数/`let`）；给最近候选名（编辑距离 ≤ 2） | 否（`Suggest` 改名） |
| L002 | error | **字段名重复**（同一 struct/union 作用域）；union 同名视图可显式降为 off | 否 |
| L003 | error | 定长 struct 声明大小 < 成员静态大小之和（`struct X : 8` 却有 `u32 a; u32 b;`） | 否 |
| L004 | warn | **数组长度为字面量 0**（`u8 a[0]`），几乎总是笔误 | 是（删除声明，`Safe`） |
| L005 | warn | **`[]` / `until()` 无终止条件或上界**：可能读到文件尾或永不终止 | 否（建议补 `max=` 或 `$ < end`） |
| L006 | warn | **`@` 位置声明疑似想推进游标**：`@` 之后紧跟同类声明且偏移递增；话术固定含"`@` 不移动游标"（02 §4.2 S2） | QuickFix：改为顺序声明或补 `u8 _pad[n] @ $;` |
| L007 | warn | `while` / `for` 循环体**无字节消费**（无成员声明、`$` 不变）→ 死循环风险 | 否 |
| L008 | warn | **struct 大小无法静态确定却用作数组元素**（`Var v[n]`）：数组总长不可预测、`$end` 不可用、预算易爆 | 否 |
| L009 | warn | **长度字段未被任何 `#[assert]` 交叉校验**（字段存在但无断言引用它）→ 03 §5.2 的 I5 失效 | Suggest：生成断言骨架 |
| L010 | warn | **`if` 条件恒真/恒假**（字面量、`x == x`、非零常量） | 是（恒真留块去 `if`；恒假注释块，`Suggest`） |
| L011 | info | **空的条件块**（`if (...) { }`） | 否 |
| L012 | warn | **嵌套深度 > 6**（if/for/while/match/内联 struct 累加）→ 拆 `fn` 或命名 struct | 否 |
| L013 | warn | **`str()` 未给 `max`**：缺省上限 = 剩余文件大小（02 §3.6），一个坏终止符就吃满预算 | 是（插入 `max=4096`，`Suggest`） |
| L014 | warn | **指针类型与 `#pragma image_base` 不一致**：用了 `rva*`/`va*` 但无 pragma（迟早 E5001） | 是（插占位 pragma + 注释） |
| L015 | warn | **命名不符 snake_case**（字段/`let`/`fn`）；类型应 PascalCase，`std::` 调用除外 | Suggest：重命名 + 同步引用 |
| L016 | warn | **`#[bits]` 位序未显式指定**：依赖默认 `lsb`，跨格式复用会静默出错（02 §3.5 要求显式） | 是（插入 `#[bits: lsb]`，`Safe`） |
| L017 | info | **magic 已识别但模板未声明**：magic 库命中某格式签名，模板未覆盖其首字段 | 否（建议 `--magic` 生成骨架） |
| L018 | warn | **未使用的 `let`**（无任何后续引用） | 是（删除，`Safe`） |
| L019 | warn | **未使用的类型定义**（无人引用且非顶层单元） | 否（保守：可能供 `import` 外部使用） |
| L020 | warn | `match` **缺 `_` 兜底分支**（02 §4.4 强烈建议） | Suggest：追加 `_ => { }` |
| L021 | warn | **数组长度表达式可能为负或非整数**（`u8 a[x - y]` 无符号下溢） | 否 |
| L022 | warn | **数组长度字面量 > 1e6**（E2 预算与结果体积风险） | 否 |
| L023 | info | **单文件 > 2000 行**，建议按格式拆文件 + `import`（机制见 02 §11） | 否 |
| L024 | warn | **字节序前缀施加在复合类型上**（`be Header h;`）：02 §3.2 明确前缀只对标量生效 | Suggest：删前缀或下沉 pragma |
| L025 | info | 同一作用域重复 `#pragma endian` 且值相同（冗余） | 是（删后一条，`Safe`） |
| L026 | info | **文件级 `#pragma endian` 缺失**（依赖默认 LE），P4 显式原则建议写出 | 是（插入 `#pragma endian little;`，`Safe`） |
| L027 | warn | **`sizeof(T)` 用在变长类型**上（返回当前实例长度，极易误读） | 否 |
| L028 | warn | **数字字面量超出目标类型位宽**（`u8 v = 300` 静默回绕，02 §5.2） | 否 |
| L029 | warn | `#[assert]` **消息为空或缺失**：失败时用户只看见表达式 | Suggest：生成消息模板 |
| L030 | warn | **`#[optional]` 与 `#[assert]` 同字段并用**（静默失败 vs 必须报） | 否 |
| L031 | info | 注释含 `TODO` / `FIXME` / `XXX`（进模板库质量分，不阻断） | 否 |
| L032 | warn | **只有类型定义、没有顶层实例**：模板跑起来不产出结构（新手最常见"没反应"） | Suggest：追加 `<TopType> root;` |

规则总量 **32 条封顶**：新增必须先拿掉一条或默认 `off`——规则表膨胀是 lint 工具死亡的常规路径。

### 3.5 增量编译接口

```rust
/// 源码编辑驱动的增量编译（与 02 §7.4 的"数据编辑驱动重解析"正交，可叠加）。
pub trait Compiler {
    fn compile(&mut self, file: &SourceFile) -> CompileOutput;
    /// dirty 是源码 span；返回全文件诊断（未脏部分复用上次结果）。
    fn compile_incremental(&mut self, file: &SourceFile, dirty: &[SrcSpan]) -> CompileOutput;
    fn last_generation(&self) -> u64;
}
pub struct CompileOutput { pub generation: u64, pub diags: Vec<LspDiag>,   // 含 SrcSpan + 可选 ByteRange
                           pub symbols: Arc<SymbolTable>, pub types: Arc<TypeTable>,
                           pub bytecode: Option<Arc<Program>>, pub units: Arc<[UnitInfo]>,  // @ 单元 → 静态可达大小
                           pub timing: StageTiming }       // lex/parse/resolve/check/codegen
```

失效模型：**按声明块粒度**。编辑落在某顶层声明内 → 只重解析该声明 + 重做引用它的符号解析；改动 `#pragma` 或类型定义 → 全文件重做语义检查（pragma 是全局状态，02 §3.2）；语法错误时**保留上一次成功的 AST** 并标 `stale`，供 hover/goto 继续工作。编译失败也必须返回部分 `symbols`（由错误恢复后的部分 AST 构建），否则输入两个字符就全屏失去提示。

### 3.6 LSP 能力矩阵

| 能力 | LSP 方法 | 依赖编译器的能力 | 需真实数据 | 响应预算 | 无数据时降级 |
|---|---|---|---|---|---|
| 诊断 | `publishDiagnostics` | 语法级：lexer+parser；语义级：符号表+类型表；运行级：VM | 运行级需要 | 语法 10ms / 语义 80ms / 运行级 200ms 后转后台 | 只发前两级，标 `diags_run_pending` |
| 悬停 | `hover` | `Node` 类型 + 生效字节序（pragma 求解）+ 属性的 i18n 说明 | 否（有则加值） | 50ms | 显示类型与文档，值显示 `—` |
| 补全 | `completion` | 作用域符号表 + 类型表 + `std::` 目录 + 属性名表 | 否 | 100ms | 纯词表补全（02 §2~§6 静态表） |
| 跳转定义 | `definition` | 符号表（声明 span） | 否 | 50ms | `stale` AST 上仍可用 |
| 查找引用 | `references` | 符号表反向边 + 全文件扫描 | 否 | 150ms | 仅当前文件（v1 不跨文件） |
| 重命名 | `rename` + `prepareRename` | 符号表反向边（`std::` 内建名不可改名） | 否 | 200ms | 拒绝并给原因 |
| 文档符号 | `documentSymbol` | AST 顶层声明 + struct 成员（两级树） | 否 | 30ms | 语法错误时用上次成功 AST |
| 格式化 | `formatting` / `rangeFormatting` | `geode-bspec-fmt`（只需能 lex，容错高） | 否 | 100ms | 语法错误 → 空 edits（不猜测） |
| 内联提示 | `inlayHint` | 类型表 + 静态大小 + 字节序求解 + 预算估计 | 否 | 100ms | 只显示静态可算大小 |
| 代码动作 | `codeAction` | lint 的 `Fix` + 编译器 quickfix（`@ $` 补丁/断言骨架/`max=`） | 否 | 100ms | 只给 `Safe` 级修复 |

**缺席说明**：不做 `semanticTokens`（v1 用 TextMate + tree-sitter 已够，双上色会打架）、不做 `workspace/symbol`（模板检索是 15 的活）、不做跨文件 `rename`。

## 4 核心算法流程

### 4.1 fmt 流水线

```
src ─lex─► token 流（含 trivia：注释/换行/空格，全部保留）；语法错误 → Err(Syntax)，原样返回
           ▼
      空白决策：每个 token 间隙求 gap = 0 | 1 | 换行+缩进 n
           ▼
      trivia 重定位（注释按附着规则挂到锚 token）→ 校验 token 流等价 → FmtOutput
```

关键点：**fmt 在 token 流上工作，不在 AST 上重打印**。AST 重打印会丢注释、丢空行、丢手工对齐——三条都会立刻招致"格式化毁了我的文件"。

### 4.2 fmt 规则集

| id | 规则 | 细则 |
|---|---|---|
| R1 | 缩进 | 4 空格；tab 展开（按 4 列对齐），删行尾空白；文件尾单个换行 |
| R2 | 字节序前缀 | `be` / `le` 与类型间**恰好一个空格**：`be u32`；强制小写；`beu32` 拆开 |
| R3 | 声明形状 | `<type><空格><name><后缀><;>`；后缀顺序固定 `[数组]` → `@ offset` → `if cond` |
| R4 | 行宽与长数组 | 100 **显示列**（CJK 记 2）。超宽断行优先级：① 属性链每属性独占一行（缩进 +4）② `if` 条件换行（运算符前置，续行 +8）③ **数组后缀永不拆行**——`u8 data[16384];` 这类行**保持单行**，折行会与 hex 视图的行对不上（用户读的是 `[n]` 整体）；仅 `until(expr)` 的长表达式可折 |
| R5 | padding 对齐 | `padding[n]` 的 `[` 与其上方最近成员的数组起始列对齐（仅当该组全部成员都有 `[...]`）；`n` 用十进制，不补零、不加 `_` |
| R6 | 注释保留 | 行尾注释与代码间保留 1~2 空格（原文 1 就 1、2 就 2——**不改用户的列对齐意图**）；`//` 后补 1 空格；块注释逐字保留，不重排、不加星号 |
| R7 | 空行 | 顶层声明间 1 空行；struct 成员间 0（保留用户单个分组空行，最多 1）；连续空行压成 1；pragma 区后强制 1 空行 |
| R8 | 最小 diff | 只改 token 间空白：不改标识符大小写、不排序成员与属性、不增删声明、不转进制（`0x1F` 不变 `31`）、不改引号、不合并行注释 |
| R9 | 幂等性 | `fmt(fmt(x)) == fmt(x)` 逐字节成立；对已规范文件 `fmt(x) == x`（`--check` 即基于此） |

`--check` 语义：有差异 → 退出码 1 + unified diff（`--diff`）+ 差异行数；语法错误 → 退出码 2。**CI 用退出码做门禁**。

### 4.3 lint 执行分层

```
① 语法层（有 AST 即跑）        L004 L011 L012 L016 L021 L022 L024 L025 L026 L028 L031
② 符号层（要符号表/类型表）    L001 L002 L015 L018 L019 L020 L023 L027 L030 L032
③ 语义静止分析（要静态大小）   L003 L005 L006 L007 L008 L013 L014
④ 数据相关层（要 magic / VM）  L009 L017  + 合并 03 的 I4/I5/I9 结果呈现
```

①②③纯静态、零 I/O、可离线，是 CI 与 AI verifier 的公共子集；④需外部数据，只在 GUI 里有意义。等级规则：`error` 永远拒绝；`warn` 允许用户 `override` 到 `off`，但**模板库入库（`见 15`）与 AI 输出（`见 03`）强制出厂等级**，否则"能过 CI 的模板"标准会分叉。

### 4.4 LSP 会话与 200ms 预算

```
onDidChange(version=n) ─► 150ms debounce（期间新编辑 → generation++，撤销旧请求）
  ① 语法级（lex+parse，通常 <10ms）──► publishDiagnostics(vN, 语法)
  ② 语义级（符号/类型/检查，目标 <80ms）──► publishDiagnostics(vN, 语法+语义)
  ③ 运行级：≤200ms 并入第 2 次发布；>200ms 进后台 Job（00 §5.4），完成后按 version+序号补发，不覆盖为旧
```

硬约束：① 请求处理不得阻塞读消息循环，worker 线程池 + `CancelToken` 协作取消（00 §5.4）；② 超时即"先吐部分结果 + 转后台"，**不返回 error、不返回空**；③ 所有结果带 `generation`，与 `SourceFile.version` 不匹配即丢弃（00 §9 原则 2）；④ `$/cancelRequest` 必须响应，取消后 50ms 内停算；⑤ 运行级由 `bspec.lsp.data_source` 指定文件，无文件时静默跳过该层。

### 4.5 VM 调试器（DX 的大头）

```rust
pub enum StopReason { Breakpoint(BpId), Step, AssertFailed(Diag), Budget(BudgetKind), Diag(Diag), Done, Cancelled }
pub enum Breakpoint { SourceLine { file: SourceUri, line: u32 },  UnitOffset { unit: UnitId, offset: u64 },
                      ByteRange { range: ByteRange },             // 命中即停（联动 05）
                      FieldHit { path: PathPattern, nth: u32 } }  // 该字段第 nth 次被读
pub struct Frame { pub unit_id: UnitId, pub path: Vec<Seg>, pub cursor: u64,
                   pub start: u64, pub end: Option<u64>, pub pc: u32 }
pub struct DebugState { pub reason: StopReason, pub frame: Frame, pub budget: BudgetSnapshot,  // steps/wall/depth/output 余量
                        pub parsed: NodeRef,          // 已构建结构树（Arc，只读）
                        pub current: Option<NodeRef>, // 正在解析的字段
                        pub pc_window: [Instr; 8] }
pub trait DebugSession {
    fn set_breakpoint(&mut self, at: Breakpoint) -> BpId;
    fn continue_(&mut self) -> DebugState;
    fn step_instr(&mut self) -> DebugState;    // 字节码粒度
    fn step_stmt(&mut self) -> DebugState;     // 源码声明粒度
    fn step_over(&mut self) -> DebugState;     // 跳过整个 struct 内部
    fn back_to(&mut self, seq: u64) -> DebugState;   // 重放到第 N 步（不做反向执行）
    fn export_trace(&self, sink: &mut dyn TraceSink) -> Result<(), TraceError>; }
```

| 面板 | 内容 | 联动 |
|---|---|---|
| 断点列表 | 4 类断点、命中次数、启用开关 | 双击 → 源码跳转；小色块 → hex 定位 |
| 单步控制条 | 继续/逐指令/逐语句/跳出/重放到第 N 步 | 新上下文 `ContextId::Debug`（需 16 登记） |
| 游标与偏移 | `$`、`$start`、`$end`、当前 unit 的 `[base, base+static_size)` | 停下瞬间 `reveal_range(offset,size)`（`见 05 §4`），**只停住时滚，不逐指令滚** |
| 预算余量 | 四条进度条 + 剩余步数估计（滑动窗口） | 余量 < 10% 转 warn 色并提示"可能陷入循环" |
| 结构树实时高亮 | 已解析节点正常显示；正在解析节点半透明 + 呼吸色 | 点节点 → 在该 offset 下静态断点 |
| 字段值面板 | 复用 Inspector 值解码（`见 10`）+ 属性（`format`/`color`/`ai:note`） | 与 hex 视图选区双向同步 |
| 执行轨迹 | 导出 JSONL；时间轴拖动 → 重放到该步并渲染当时的树 | 与单步控制条共用 `back_to` |

轨迹每事件一行，字段稳定供 CI 比对：`{"seq":183,"steps":18340,"wall_us":2100,"unit":2,"path":["ZipArchive","localHeader"],"op":"read_u32","cursor":4096,"len":4,"endian":"little","value":"0x04034B50","budget_left":49816660}`

`trace` 三个开关：`--step-limit N`（到 N 步正常退出，**退出码 0**）、`--filter <path-glob>`（只记匹配路径，控体积）、`--out -`（默认 stdout）。

与 02 §7.4 的配合：**调试模式自动关闭增量重解析**（每条语句后重解析等于原地自爆）。源码被编辑时断点按 `(unit_id, bytecode_offset)` 重定位，**不按行号**——行号在重排后毫无意义，这是调试器最容易做错的一条。数据被编辑（04 的 `version` 变化）时：定长结构 → 重放到第 N 步；长度变化 → 提示"文件长度已变，需重启会话"并置灰继续。

### 4.6 语法高亮

TextMate scope 清单（`editors/textmate/BinSpec.tmLanguage.json`，scope 名唯一权威表）：

| 语法元素 | scope |
|---|---|
| 关键字 `struct/union/enum/bitfield/using/fn/if/else/match/for/while/let/return/import` | `keyword.control.bspec` |
| `padding` / `#pragma` 名与参数 | `storage.type.bspec` / `keyword.other.pragma.bspec` + `variable.parameter.pragma.bspec` |
| 基础类型 `u8..u128 / s8..s128 / u24 / u48 / f16 f32 f64 / bool char byte` | `storage.type.bspec` |
| 指针类型 `off32/off64 / rva32/64 / rel32/64 / va32/64` | `storage.type.pointer.bspec` |
| 字节序前缀 `be` / `le` | `storage.modifier.endian.bspec` |
| 数字字面量（`0x` / `0b` / `0o` / 十进制 / `_` 分隔 / 浮点 / 位宽后缀 `0x10u32`） | `constant.numeric.bspec`；`0x` 前缀单列 `constant.numeric.hex.bspec`；后缀 `constant.numeric.suffix.bspec` |
| 字符串 / 字符 / 编码参数 `enc=utf8` `term=0` `max=256` `size=` | `string.quoted.double.bspec` / `constant.character.bspec` / `variable.parameter.encoding.bspec`（值取白名单 `utf8/utf16le/utf16be/ascii/latin1`） |
| 内置量 `$` `$size` `$start` `$end` / `sizeof` / `len` / `std::` 路径 | `variable.language.builtin.bspec` / `support.function.builtin.bspec` / `entity.name.namespace.std.bspec` + `support.function.std.bspec` |
| 属性 `#[...]` 名与括号 | `entity.other.attribute-name.bspec` + `punctuation.definition.attribute.bspec` |
| 位置声明 `@` / 定长标注 `: 32` | `keyword.operator.position.bspec` / `constant.numeric.size.bspec` |
| 注释 `//` `/* */`，文档型 `///` | `comment.line.double-slash.bspec` / `comment.block.bspec` / `comment.line.documentation.bspec` |

**为什么 tree-sitter 要单独维护**（不是冗余）：① **增量解析**——TextMate 是逐行正则，改一个字符要重扫到文件尾；tree-sitter 增量 LR 在 2000 行文件上单键编辑 < 1ms，没有它大模板在 Zed / Helix 里输入发涩。② **结构选择与折叠**——struct/union/match 分支折叠、`textobjects`（选中/交换成员）、`locals.scm` 作用域着色都要求真实语法树。③ **编辑器生态**——Neovim(nvim-treesitter)、Zed、Helix、Emacs(treesit) 的高亮与折叠全走 tree-sitter，只发 TextMate 等于放弃这些用户 80% 的体验。

一致性测试（两套 parser 不许漂移）：

```
语料 = 本文示例 + 15 模板库全量 + 02 全部代码块 + 20 的畸形样本（≥200 个 .bspec）
① 编译器 parser → token 流 T_c（含 span 与嵌套深度）
② tree-sitter    → token 流 T_t（用 node-types.json 映射到同一套 token 名）
③ 比较 T_c 与 T_t：token 名序列 / span / 嵌套深度逐项相同 → 通过；差异即红灯（默认以编译器为准，
   除非该构造在 02 里未定义——则先补 02 再改两边）
```

唯一白名单差异：语法错误输入的错误恢复形态（两者恢复策略不同，只比较错误起点之前的 token）。

### 4.7 错误码 → 人话映射

三段式：`机器消息`（英文、稳定、进 `Diag.message`，**不翻译**）→ `用户话术`（i18n，短句 + 一句为什么）→ `建议动作`（可点的一条命令或 quickfix）。映射表是 **i18n 资源的一部分**（`见 16 §3.5/§4.6`），key 规范：

```
dsl.err.<CODE>.{short,long,action}   # 一句话 ≤40 字 / 为什么 ≤120 字（插值 {expected}/{actual}/{offset}）/ 建议动作 ≤40 字
dsl.lint.<RULE>.why                  # lint 规则说明（L001~L032 各一条）
```

初始 20 行样例（v1 上线前扩到 E1~E5 全码，缺 key 由 `xtask i18n-check` 拦下）：

| code | 机器消息（英文） | 用户话术（zh-CN） | 建议动作 | 相关 |
|---|---|---|---|---|
| E1001 | enum value not in declaration | 该字段的值不在枚举定义里，格式可能变了 | 用 hex 视图确认这几字节，或给枚举补 `_` 分支 | 02 §3.4 |
| E1002 | invalid byte sequence for encoding | 这段字节不是合法 utf8 文本，可能被截断或不是字符串字段 | 改 `enc=` 或加 `max=` 限制长度 | 02 §3.6 |
| E1004 | magic mismatch | 文件头与本格式签名不符，这里可能不是该结构起点 | 检查偏移字段指向的位置，或换个模板 | 02 §8.1 |
| E1005 | assertion failed | 断言没通过：这条规则在这个文件上不成立，但不一定算错 | 看断言两侧实际值；若是规范允许的例外就放宽断言 | 02 §4.6 |
| E1006 | bool field value not 0/1 | 布尔字段读到了 0/1 之外的值 | 改用 `u8` + `#[format("bin")]` | 02 §3.1 |
| E2001 | step budget exhausted | 执行步数用完了，模板里大概有死循环或超大循环上界 | 看"卡在第几次迭代"，给 `[]` 补终止条件 | 02 §7.2 |
| E2002 | wall time budget exceeded | 执行超时（默认 5 秒） | 缩小 `[]` 范围，避免在循环里调 `std::checksum` | 02 §7.2 |
| E2003 | recursion/depth limit reached | 嵌套或递归超过 256 层，通常是指针链没终止 | 给链表解析加 `if (p != 0)` 守卫与深度计数 | 02 §7.2 |
| E2004 | output size limit exceeded | 解析结果超过 64MB，结构树会拖垮界面 | 给大数组加 `#[hidden]` 或用 `padding` 跳过 | 02 §7.2 |
| E2005 | cancelled by user | 解析被取消（按了 Esc 或切换了文件） | — | 02 §7.1 |
| E3001 | read out of bounds | 读取超出文件末尾，多半是长度字段不可信 | 检查长度字段，或读 `min(len, $size - $)` | 02 §8.3 |
| E3002 | resume target outside file | `@` 指向的位置不在文件里，该字段已标记"无法解析"，其余照常 | 无需处理；大量出现说明指针类型选错了 | 02 §3.3 S1 |
| E3003 | allocation exceeds limit | 试图读的长度超过单次上限 | 分段读，或确认长度字段的字节序 | 02 §7.2 |
| E3004 | division by zero | 表达式里除零（常见于用 0 当除数算元素个数） | 给除数加非零判断，或改用 `until()` | 02 §5.2 |
| E4001 | unexpected token | 语法错误：这里不该出现这个符号 | 看高亮括号配对；`bspec fmt` 会指出第一个错误位置 | 02 §2 |
| E4002 | type mismatch | 类型不匹配（把 `str` 当整数用，或数组长度不是整数） | 用 `sizeof()` / `len()` 取值，别混用 | 02 §3 |
| E4003 | undefined symbol | 用了没定义的符号（字段忘了 `let`、类型名拼错） | 用补全列表确认名字；`bspec lint` 会给最近候选 | 02 §7.1 |
| E4004 | fixed struct size mismatch | 定长结构声明的大小与成员实际大小对不上 | 调整 `: N` 或成员，差额在提示里 | 02 §3.4 |
| E5001 | image_base not provided | 模板用了 `rva`/`va` 指针，但没给 `#pragma image_base` | 补一行 `#pragma image_base 0x400000;` | 02 §3.3 |
| E5002 | required context missing | 缺少解析所需前置信息（如外部长度表） | 在模板里用 `let` 从文件内推导 | 02 §3.3 |

写作规范：**不许说 "invalid"**，要说"什么、在哪、大概为什么、你可以做什么"；`action` 必须能一键执行或明确写出命令；`long` 可插值 `{offset}/{expected}/{actual}`，禁止把整段十六进制塞进话术（hex 由 UI 单独渲染）。

## 5 性能预算与基准

| 场景 | 预算 | 条件 | 退化动作 |
|---|---|---|---|
| `bspec fmt` 单文件 | < 5ms | 1000 行 / 60KB | > 20ms 告警（说明走成了 AST 重打印） |
| `bspec check`（L1）/ `bspec lint` 全规则 | 20ms / 15ms | 1000 行（同 00 §7），lint 不含 `--run` | 红灯 |
| 增量编译（单键编辑） | < 10ms p95 | 2000 行，改动 1 token | 红灯 |
| LSP 诊断（语法级 / 语义级 / 悬停 / 补全） | 10ms / 80ms / 50ms / 100ms p95 | 打开 2000 行，含冷缓存；补全时符号 ≤ 2000 | 红灯 |
| 调试单步 / 停下时的联动帧 | < 1ms CPU / ≤ 16ms | 停走不重放全文；停下 → hex 视图 + 结构树高亮 | 红灯（同 00 §7） |
| `trace` 事件开销 / 体积 | < 5% 额外 wall / < 100MB | 50M 步上限、`--filter` 生效 | 达上限即截断并置 `truncated` |
| LSP 常驻内存 | < 120MB | 5 个 2000 行文件 | — |

测量点：`criterion`（核心函数）+ `bench/lsp_latency.rs` 用脚本化 LSP 帧序列测端到端 p50/p95。**延迟必须测"收到 `didChange` 到发出 `publishDiagnostics`"，不是测函数耗时**（否则永远好看）。

## 6 失败模式与降级

| # | 失败 | 降级 | 用户可见性 |
|---|---|---|---|
| 1 | 源码语法错误 / fmt 破坏语义（`ast_equal == false`） | 一律**返回原文**、不产生 edits（宁可格式化失败，不可改坏代码）；`--check` 退出码 2（区别"有差异"的 1） | 提示"先修复语法错误再格式化" + 上报片段 |
| 2 | lint 规则自身 panic | 规则级 `catch_unwind` + 该规则本会话 `off` + 追加 `L000 内部错误` | 状态栏图标 + 可复制的错误报告 |
| 3 | 运行级数据不可用（超 200ms / 未绑定文件） | 先发语法+语义，运行级后台补发（§4.4）；无文件只做静态层，`inlayHint` 值列显示 `—` | 带 `pending` 的浅色下划线；悬浮提示"未绑定数据文件" |
| 4 | 增量失效判断算漏 / 语法资产与编译器漂移 | 命中 pragma 或类型定义一律全量重算、`generation` 不匹配结果全丢；tree-sitter 以编译器为准并由 CI 红灯阻断发版 | 无（静默正确） |
| 5 | i18n key 缺失 | 回退链 `zh-CN → en-US → key 本身`（`见 16 §4.6`），同时保留英文机器消息兜底 | 可见（刻意可见：空白最难排查） |
| 6 | 调试中文件长度变化 / 预算耗尽 | 长度变：冻结会话置灰"继续"要求重启；预算耗尽：停在耗尽点，展示部分结构树 + "卡在第 N 次迭代"（02 §7.2） | 一次性对话框；停下即显示，不许静默退出 |
| 7 | LSP 崩溃 | 编辑器侧自动重启（≤3 次/5 分钟），持久化崩溃前的规则 `off` 状态 | 编辑器原生气泡 |
| 8 | `--step-limit` 截断轨迹 | 末行写 `{"seq":null,"truncated":true,"reason":"step_limit"}` | CLI 退出码 0 + stderr 一行提示 |

## 7 与其它子系统的契约

### 7.1 与 `03`：**lint 是 verifier 的第 0 层**

```
L0 lint ← 本文提供（静态、零 I/O、< 15ms）  →  L1 编译 / L2 执行 / L3 不变量 ← 见 03 §5.1
```

写死六条：
1. **同一套规则、同一套等级**：AI 生成与人类手写走**完全相同**的 `LintConfig`（出厂等级，§4.3）。禁止"AI 模式放宽 lint"——这是最容易发生的腐烂。
2. **L0 error 即拒绝**：任一 `L001~L003`、`L028` 级 error 直接判候选不合格，不进 L1，省一次编译与一次模型调用。
3. **warn 计入置信度**：`LintReport.warn_count` 作为 03 §5.3 `score` 的扣分项（建议每条 warn −0.5，权重由 03 拥有），并在 UI 以 `#[ai:note(...)]` 旁注。
4. **fmt 归一化是 best-of-N 去重与最小修复 diff 的前提**：候选比较前先 `fmt`；`fmt(x) == fmt(y)` 可判等（免费去掉"只差空白"的候选）。
5. **修复循环的输入必须并入 `Lint`**（`rule/span/msg_key`）：`L018 未使用的 let`、`L006 @ 疑似想推进游标` 这类问题编译器不报错，只靠 L1~L3 永远修不掉。
6. **禁止第四套 parser**：03 的 verifier 不得自建词法/语法检查，一切静态判断调用 `geode-bspec-lint`。

### 7.2 与 `02`（语言规范）

本文不定义任何语法语义。02 改了词法/属性/类型，则 TextMate scope 表、tree-sitter grammar、补全词表、lint 规则必须同步改——**由 02 的 PR 触发**（`cargo xtask docs` 检查：02 属性表行数变化 → §4.6 scope 表必须同时改，否则红灯）。02 §11 待定项对本工具链的直接影响：① `@` 是否移动游标（决定 L006 话术形态）；② `import` 可见性规则（决定 §10 的 include 解析与 L023 建议）；③ AI 字段命名是否强制（决定 L015 是 warn 还是 off）。

### 7.3 与 `05`（渲染 / 双向联动）

- 调试器停下 → `reveal_range(offset, size)` + 选中 + 入 `pinned_nodes`（`见 05 §4`）；**只在停住时滚，不逐指令滚动**（否则画面抖动，比不联动更糟）。反向：hex 视图点击 → 反查 `Node`，命中 `@` 单元首字节时提示"该结构解析入口在这一行"。
- 05 只持 `NodeRef` 与 `Arc<StructureIndex>`，**不感知 `SrcSpan`**；桥在本 crate。为此请求 **02 §7.3 的 `Node` 增加 `src_span: SrcSpan` 字段**（只加字段、不改语义，符合 00 §4 的"可加字段"规则）。

### 7.4 与 `16`（i18n / 命令注册表 / 字体）

- 话术表属 i18n 资源（`errors` + `dsl` 命名空间），key 一经发布不重命名（`见 16 §4.6`）；本文拥有 key 命名规范与初始表，16 拥有加载与覆盖率门禁。
- 五条命令进 `CommandRegistry`（`见 16 §3.2`）：`bspec.fmt.document` / `bspec.lint.run` / `bspec.debug.start` / `bspec.debug.step` / `bspec.trace.export`；快捷键走新上下文 `ContextId::Debug`（需 16 登记）。
- 源码编辑器用 `mono_family` 且**禁用 fallback**：缩进与对齐靠等宽，混入比例字形会毁掉 fmt 的视觉预期（`见 16 §4.7`）。诊断面板"问题列表"由 16 拥有，本文只提供 `Vec<Diag>` 与跳转用 `SrcSpan`。

### 7.5 与 `15`（模板库）和 `10`（magic 库）

模板 PR 必须过 `bspec check`（L1）、`bspec fmt --check`（0 差异）、`bspec lint --max-warn N`（0 error）；不通过即不上架，拒绝理由回传作者并附 `explain` 输出。L017 依赖 magic 签名表 `MagicHit { format_id, at: u64, confidence }`，工具链只读、`geode-analysis` 是唯一 owner，不维护第二份。

### 7.6 与 `18`：CI 集成

新增 `xtask bspec-gate`（仓库内全部 `.bspec` 跑 check+fmt+lint）、`xtask ts-diff`（tree-sitter 差分）、扩展 `xtask i18n-check`（话术 key 完备性）。三者并入 `ci.yml` 既有 `docs`/`test` job，不新增独立 job（18 §5 目标 < 12 分钟绿灯）。**模板仓库批量校验**用可复用 composite action：

```yaml
# .github/workflows/bspec.yml（composite action，模板仓库与主仓共用）
- {name: Install CLI, run: "curl -fsSL https://get.geode.dev/install.sh | sh -s -- --no-gui"}  # 无 GUI 依赖
- {name: Check, run: "geode bspec check --json docs/templates/**/*.bspec > check.json || (cat check.json && exit 1)"}
- {name: Format gate, run: "geode bspec fmt --check docs/templates/**/*.bspec"}   # 有差异退出码 1
- {name: Lint gate, run: "geode bspec lint --level warn --max-warn 0 docs/templates/**/*.bspec"}
- uses: ./.github/actions/bspec-audit                                            # 对 content repo 全量跑
  with: {repo: geode-content/templates, fail-on: error}
```

## 8 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `bspec.fmt.indent` / `max_width` | `4` / `100` | 允许 2（模板库禁止）；宽度按显示列（CJK 记 2） |
| `bspec.fmt.attr_break` | `"preserve"` | `preserve` \| `one_per_line` \| `inline` |
| `bspec.fmt.blank_between_top` / `comment_gap` / `align_comments` | `1` / `1` / `false` | 顶层空行数 / `//` 后空格数 / 行尾注释列对齐（§10 争议项） |
| `bspec.fmt.on_save` | `true` | 编辑器侧保存即格式化 |
| `bspec.lint.level.<RULE>` / `max_warn` / `run_on` | 规则表默认 / `0` / `"edit"` | 等级可上抬到 `error`，`error` 不可下压到 `off`；CI 门禁阈值；触发时机 `edit`\|`save`\|`manual` |
| `bspec.lsp.debounce_ms` / `timeout_ms` | `150` / `200` | debounce 50~500；超时即转后台 |
| `bspec.lsp.inlay_hints.max_items` / `data_source` | `400` / `null` | 一屏内联提示上限 / 运行级诊断绑定文件 |
| `bspec.debug.step_limit` | `50_000_000` | 与 02 §7.2 的 steps 同名同义，不另立上限 |
| `bspec.trace.max_events` / `filter` | `1_000_000` / `null` | 轨迹上限与路径 glob（`stat.coverage_formula` 固定为 03 §5.2 的 I4 同式） |

配置位置：用户级 `config.toml` 的 `bspec.*` 段 + 项目根 `.bspec.toml`，**项目级优先**（团队统一风格）。但 `error` 等级与 `fmt.indent = 4` 在模板库内不可覆盖。

## 9 测试策略

| 层 | 内容 | 门禁 |
|---|---|---|
| fmt 幂等（属性测试） | 随机合法 AST → 打印源码 → fmt 两遍 → `fmt(fmt(x)) == fmt(x)` | 10 000 例 0 失败 |
| fmt 等价 / 黄金文件 / 最小 diff | 语料全量 `token_stream(parse(fmt(x))) == token_stream(parse(x))`，注释多重集不变；已规范部分 `fmt(x) == x`；随机插删一个空格 → edit 集只覆盖该间隙 | 100%；抽样 500 例 |
| lint 规则正确性 + 自动修复 | **每规则 ≥5 正例 + 5 反例**（反例更重要：漏报危害小，误报会让人关掉工具）；fix 应用后不再触发且 AST 等价于人工修复版，同输入两次运行结果逐项相同 | 32 × 10 = 320 例；100% |
| tree-sitter 差分 | §4.6 的 token 流比较，语料 ≥200 文件 | 100%（白名单外零差异） |
| LSP 协议 + 延迟 | 脚本化帧序列（init→open→edit→completion→format→close）：`id` 对应、`version` 单调、取消生效；同脚本内测 §5 各项 p50/p95 | 全绿；延迟退化 >20% 红灯 |
| 调试器确定性 | 同模板 + 同数据 + 同 `--step-limit`，轨迹 JSONL 逐字节可比（剔除时间字段）；CLI 与 GUI 停下位置相同（比 `seq`/`path`） | 100% |
| 话术完备性 + 性能回归 | `xtask i18n-check`：E1~E5 全码 + L001~L032 的 `short/long/action/why` 在中英均存在、占位符集合一致；§5 全部条目对 `bench-baseline.json` | 缺失即 fail；退化 >20% 红灯 |
| 畸形源码 | 20 语料：超深嵌套、超长单行、非法 UTF-8、NUL 字节、10MB 单文件、循环 `import` | 不 panic、不 OOM、有界耗时（fuzz 语料与 20 共用） |

## 10 开放问题

1. **格式化抑制注释与注释列对齐**：是否支持 `// fmt: off/on` 与行内 `// bspec-allow(L016)`？倾向都支持（手写对齐的查找表、位域图示必须保住），但抑制区间要在结构树里可见，否则会变成"这块为什么格式化不了"的长期困惑；lint 抑制次数进模板库质量分（抑制 = 欠债）。行尾注释列对齐（`bspec.fmt.align_comments`）好看但放大 diff（改一个字段名导致整块注释列移动），与 R8（最小 diff）冲突，倾向默认不动，等真实抱怨再议。
2. **语言服务器是否内嵌进 GUI**：GUI 的模板编辑器是复用进程内 `bspec-lsp`（省 IPC、可直接持 `Arc<StructureIndex>`），还是直接调库接口、LSP 只对外？倾向后者，代价是两条代码路径，需"能力矩阵一致性测试"防漂移。
3. **include / import 路径解析规则**：02 §11 第 5 条未定。需拍板：相对当前文件还是项目根？是否允许 `std/` 之外的绝对路径？搜索路径是否进 `.bspec.toml`？这直接决定 L023 的适用性与"跨文件跳转定义"是否需要（当前明确不做）。
4. **编辑器覆盖面**：是否为 Neovim / Emacs 提供 LSP 配置示例（各 10 行 lua/elisp）？成本低但带来"配置不生效"的支持负担；倾向提供并标注"社区维护、不承诺跟随版本"，同时把 `bspec-lsp --health` 做得足够自解释。
5. **命名规范是否强制**（02 §11 第 7 条）：强制则 L015 可升 `error` 并自动修复；不强制则保持 `warn`。真实分歧是导入外部模板时改名会让原作者文档、截图、issue 全对不上。倾向 `warn` + 项目级可抬。
6. **tree-sitter grammar 的维护归属**：手工同步是长期负债。能否从编译器 parser 的构造器 DSL 生成 `grammar.js`？技术上可行但生成的 grammar 往往不可读、难优化。倾向先接受手工同步 + §4.6 差分测试兜住。
7. **调试器是否需要反向执行**：当前只有"重放到第 N 步"（本质是重新执行），遇到 `std::checksum` 这类昂贵调用会很慢。是否每 10 000 步存 VM 快照？内存代价高且结构树增量难序列化，倾向 v2 评估。
