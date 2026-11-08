# 14 — 插件系统与 ABI

> 状态：Draft v0.1（未实现）。上位契约：`00-architecture-overview`（§2 技术栈定案、§3 crate 分层、§5 共享接口、§9 设计原则）。
> 归属 crate：`geode-plugin-host`（位于 `geode-ui` **下方**，不依赖任何 GUI）+ `geode-plugin-sdk`（面向插件作者的 Safe 封装）。
> 职责边界：本文拥有 ABI、加载/卸载/热重载、能力注册、内存所有权、崩溃隔离与版本策略；**不拥有**具体插件功能、不拥有签名算法实现（见 15-content-distribution）、不拥有菜单与快捷键的最终仲裁（见 16-ui-shell）。

**结论先行（5 条）**

1. **插件在 v1 不能绘制 UI，这是分层决定的，不是偷懒**。`geode-ui` 在最上层，插件宿主在它下面（00 §3），插件天然拿不到 `egui`。v1 走"数据类插件 + 声明式面板"，v1.1 的 egui shim 与 v2 的独立进程各自留了口子，见 §1.3。
2. **ABI 只有 C 内存布局**：`#[repr(C)]` + 函数指针表 + `(ptr,len)` 字符串 + 不透明 `u64` 句柄。绝不出现 `String`/`Vec`/泛型/trait object。
3. **所有权规则是本子系统最容易出事的地方**，因此写成 8 条硬规则（§3.4），逐条可测。
4. **v1 插件崩溃 = 应用崩溃，这一点如实告知用户**，不假装沙箱。缓解手段（`catch_unwind`、看门狗、子进程预检）与真正的隔离方案（v2 独立进程）分开写。
5. **插件 = 任意代码执行**。签名、权限声明、来源提示三件套在首次加载时强制走一遍（签名算法见 15）。

---

## 1. 目标与非目标

### 1.1 目标

| # | 目标 | 判据 |
|---|---|---|
| G1 | 不是本仓库维护者也能扩展产品 | 第三方用 `geode-plugin-sdk` 在 30 分钟内写出可加载的插件 |
| G2 | ABI 跨编译器/跨 Rust 版本稳定 | 用 Rust 1.8x 编译的插件能被 1.9x 编译的宿主加载（同一 major） |
| G3 | 一个坏插件不影响其它插件与核心功能 | 加载失败/版本不符/签名缺失 → 该插件禁用，应用正常启动；崩溃后下次启动能定位到具体插件并自动列入禁用名单 |
| G4 | 能力面覆盖 00 中所有"可扩展点" | Provider / Visualizer / 命令 / 处理器节点 / BinSpec 函数 / 反汇编 / 主题 / 导入导出 |
| G5 | 不破坏分层 | `geode-plugin-host` 的 `Cargo.toml` 里没有 `egui`/`wgpu`，CI 强制检查 |

### 1.2 非目标

| 否决项 | 理由 |
|---|---|
| v1 支持 WASM 插件 | 数据拷贝与性能代价过大（00 §2 已裁决）；但 v2 重估（§10） |
| 插件直接依赖 `geode-ui` 或任何 Rust crate 的内部类型 | Rust 无稳定 ABI，且会形成"插件反向依赖 UI"的死结 |
| 插件市场/一键安装 | 属 15-content-distribution |
| 插件在 v1 提供自定义渲染管线（shader） | 与 13 的分层同因：绘制归 `geode-ui` |
| 插件热更新 ABI 而不重启 | 破坏"已加载代码不可变"的前提 |

### 1.3 核心矛盾的裁决：插件能不能画 UI

**矛盾陈述**：00 §2 要求「除 `geode-ui` 与 `app` 外任何 crate 不得依赖 GUI」；00 §3 又要求插件宿主 `geode-plugin-host` 位于 `geode-ui` **之下**。于是插件（作为宿主的被加载者）在依赖图中永远无法引用 `egui`。若强行让插件画 UI，只有三条出路：拉平分层（不可接受）、把 egui 塞进宿主（违反 §2）、或者让宿主通过 ABI 转出 egui 的能力（可行但昂贵）。

**决策表**

| 方案 | 插件能自由绘制? | 分层 | 维护成本 | 采用 |
|---|---|---|---|---|
| **A. 数据类插件 + 声明式 UI**（插件提交控件树描述，宿主渲染） | 否，受限于控件集 | 完好 | **低**：schema 一页纸，控件集扩充不影响 ABI | ✅ **v1** |
| **B. egui 版本锁定通道**（宿主导出 egui 的 C shim，插件用同一版本编译） | 是 | 需要 geode-ui 额外导出一层 shim | **高**：见下方论证 | ❌ 暂不采用（v1.1 候选） |
| **C. 独立进程 + 共享内存 + 插件自己的窗口** | 是（自有窗口，不能内嵌） | 完好 | 中高：IPC、生命周期、双窗口焦点 | 🔜 **v2** |
| **D. 插件链接 `geode-ui` crate** | 是 | **崩塌** | 灾难 | ❌ 永不 |

**为什么否决 B（不是不可能，是不划算）**

1. egui 是**立即模式**库：`Ui` 的 API 面约 200 个方法、30 余个类型，全部要塞进 C shim 才能满足常见插件；这层 shim 本身就是一个需要版本化维护的 ABI。
2. egui 的 minor 版本经常带 breaking change（`Ui::add` 签名、`Response` 字段、`Style` 结构都动过）。若要"插件用同一版本编译"，宿主升级 egui = 全部插件必须重编译，**生态会被版本锁死**；且同进程两个 egui 版本 = 符号冲突（Rust 默认不导出符号，导出 shim 就必须导出，冲突立刻出现）。
3. 成本估算：shim 首次 0.5 人月，之后 0.3 人月/年；而"某插件在新版本突然加载失败"的支持成本更高。相比之下 v1 的声明式控件树覆盖了 90% 的真实需求（面板、表格、按钮、滑块、图片、文本）。
4. 保留口子：若 v1 上线后"需要自由绘制"的插件占比超过 20%（可从插件仓库统计），再启动 B——§3.1 的 `struct_size` 机制已保证前向兼容，不需破坏 ABI。

**声明式 UI 的具体形态（方案 A 落地）**

```c
typedef enum { HEXW_LABEL, HEXW_VALUE, HEXW_BUTTON, HEXW_CHECKBOX, HEXW_SLIDER,
               HEXW_TEXT, HEXW_TABLE, HEXW_IMAGE, HEXW_COMBO } GeodeWidgetKind;
typedef struct {                        // 一个控件；id 由插件决定，事件回传用
    uint32_t kind, id;
    const char* text; uint32_t text_len;
    double   num;                       // 滑块值 / 图片宽
    uint32_t rows, cols;                // 表格维度（数据在 GeodePanelDesc.blob）
} GeodeWidget;
// GeodePanelDesc{ title(ptr,len); widgets: *const GeodeWidget + count; blob: *const u8 + blob_len }
```

宿主侧：`ui_panel_define(&desc)` 覆盖该插件面板的当前内容；用户交互以 `GeodeUiEvent { plugin_id, widget_id, kind, value }` 回调 `CommandVTable::on_ui_event`。插件不持有 UI 对象、不参与布局、不接触字体与主题（主题与 i18n 归 16）。

**演进路径**：v1 方案 A →（v1.1，条件触发）方案 B 作为"高级通道"与 A 并存 →（v2）方案 C，进程隔离顺便解决崩溃问题，B 可下线。

---

## 2. 依赖与边界

### 2.1 在依赖图中的位置

```
          app
           │
        geode-ui ──────────────┐   ← 唯一能碰 GUI 的层
           │                 │
 ┌─────────┼────────┬────────┴──────┐
 ▼         ▼        ▼               ▼
geode-project geode-ai geode-content geode-plugin-host  ← 本子系统（无 GUI 依赖）
                                    │
     geode-provider / geode-bspec / geode-proc / geode-annot / geode-core
```

- **硬约束**：`geode-plugin-host` 只依赖 `geode-*` 内核 crate + `libloading`（`dlopen` 封装）+ `ed25519-dalek`（验签，见 15）+ `semver`。**禁止**依赖 `egui`、`wgpu`、`geode-ui`。
- **反向依赖的处理**：插件注册的能力（可视化器、处理器节点、命令）由宿主暴露为 trait object，`geode-ui` 在**上层**把它当作普通实现来用。信息流永远是"UI 向下读注册表"，不是"宿主向上通知 UI"。

### 2.2 与相邻子系统的契约

| 子系统 | 关系 |
|---|---|
| 13-visualizers | 插件的 `VisualizerVTable` 被包装成 `Arc<dyn Visualizer>`，签名与 §3.3 一致 |
| 07-data-processor-graph | 插件可注册处理器节点（`ProcessorNodeVTable`），节点图引擎在下层，插件节点与内置节点地位相同 |
| 02-dsl-spec | 插件可注册 BinSpec 内建函数（`BspecFnVTable`），但**必须声明纯度与预算**（§3.3），且不能引入 I/O |
| 06-data-providers | 插件 Provider 必须实现 `read_at` 的越界语义（00 §5.1 铁律） |
| 11-disassembler | 插件可注册反汇编引擎（`DisasmVTable`），与 Capstone 并列 |
| 15-content-distribution | 提供 Ed25519 签名与公钥库；本子系统只做"验或不验"的决策与 UX |
| 16-ui-shell | 命令/菜单/快捷键由 16 的命令系统仲裁；插件只提交 `CommandVTable` |
| 17-project-and-persistence | 工程文件记录"启用了哪些插件（id + 版本）"，缺失插件时的降级 UX 归 17 |

---

## 3. 数据结构与接口

### 3.1 ABI 基础

```c
#define GEODEPLUG_ABI_MAJOR 1
#define GEODEPLUG_ABI_MINOR 0
#define GEODEPLUG_ABI_VERSION ((uint32_t)(GEODEPLUG_ABI_MAJOR << 16) | GEODEPLUG_ABI_MINOR)

typedef struct { uint16_t major, minor, patch; } GeodeSemver;
typedef struct { uint32_t struct_size; uint32_t abi_version; } GeodeVTableHeader;  // 每个 vtable 的首成员

// 插件元信息：必须是插件的静态数据，生命周期到进程结束
typedef struct {
    GeodeVTableHeader hdr;
    const char* id;            uint32_t id_len;        // 反向域名 "acme.waveform"
    const char* name;          uint32_t name_len;
    GeodeSemver   version;                              // 插件自身 semver
    const char* author;        uint32_t author_len;
    const char* license;       uint32_t license_len;  // SPDX 标识符
    const char* description;   uint32_t description_len;
    GeodeSemver   host_min, host_max;                    // 要求宿主版本区间（上界开）
    uint32_t    permissions;                          // 见 §3.2 / §7.3
    const GeodeCapability* caps; uint32_t cap_count;
} GeodePluginDesc;

GEODEPLUG_API const GeodePluginDesc* geodeplug_entry(const GeodeHostApi* host);   // 唯一入口符号
```

**兼容规则（必须逐条实现）**

| 规则 | 内容 | 违反时 |
|---|---|---|
| R1 | 宿主接受 `plugin.major == host.major` 的插件，忽略 `plugin.minor` 高低（minor 只增不改） | major 不等 → 拒绝加载 |
| R2 | 所有 vtable 用 `struct_size` 做"取小"读取：宿主只读 `min(自己的大小, plugin 报的大小)` 这段 | 尺寸为 0 → 拒绝 |
| R3 | 新字段只能**追加**到 vtable 末尾；已有字段的语义永不改 | 破坏性改动 → major+1 |
| R4 | `geodeplug_entry` 返回的 `GeodePluginDesc` 必须是**插件的静态数据**，生命周期到进程结束；入参 `const GeodeHostApi*` 只在 entry 期间保证有效（指针本身恒定，插件可保存副本） | 动态分配 → 拒绝（宿主不 free） |

### 3.2 HostApi（宿主提供给插件的唯一通道）

```c
typedef struct GeodeHostApi {
    GeodeVTableHeader hdr;
    void*  (*alloc)(size_t len, size_t align);                       // ── 内存（§3.4 规则 1/3）
    void   (*free)(void* p, size_t len, size_t align);
    GeodeDocHandle (*doc_current)(void);                              // ── 文档字节（只读快照）
    uint64_t (*doc_len)(GeodeDocHandle h);
    uint64_t (*doc_version)(GeodeDocHandle h);                        // 陈旧校验（00 §6 规则 3）
    int32_t  (*doc_read)(GeodeDocHandle h, uint64_t off, uint8_t* out, size_t len);
    int32_t  (*ann_insert)(GeodeDocHandle h, const GeodeAnnDesc* d, uint32_t* out_id);  // ── 标注 00 §5.3
    void     (*ann_remove)(GeodeDocHandle h, uint32_t ann_id);
    void (*diag)(const GeodeDiagDesc* d);                              // ── 诊断 / 日志
    void (*log)(int32_t level, const char* ptr, uint32_t len);
    int32_t (*cfg_get)(const char* k, uint32_t k_len, uint8_t* buf, uint32_t* io_len);  // ── 配置
    int32_t (*cfg_set)(const char* k, uint32_t k_len, const uint8_t* v, uint32_t v_len);
    uint64_t (*job_submit)(const GeodeJobDesc* d);                     // ── 后台任务
    void     (*job_cancel)(uint64_t job_id);
    void (*ui_toast)(int32_t level, const char* ptr, uint32_t len);  // ── 声明式 UI（§1.3 方案 A）
    void (*ui_panel_define)(const GeodePanelDesc* desc);
} GeodeHostApi;
```

**不允许的能力（刻意不提供）**：打开/读写任意文件、网络、执行子进程、读环境变量、访问宿主内存里除 `doc_read` 之外的任何数据、注册全局热键、修改他人插件的状态。插件若需要文件，只能通过"导出"能力请求宿主代写（宿主弹窗确认）。

### 3.3 能力（capability）模型

```c
typedef enum {
    HEXCAP_PROVIDER = 1, HEXCAP_VISUALIZER, HEXCAP_COMMAND, HEXCAP_PROC_NODE,
    HEXCAP_BSPEC_FN,     HEXCAP_DISASM,     HEXCAP_THEME,   HEXCAP_IO_CODEC,
} GeodeCapKind;

typedef struct { GeodeVTableHeader hdr; uint32_t kind; const void* vtable;
                 const char* id; uint32_t id_len; } GeodeCapability;
```

| 能力 | v1 | 主要函数指针（草案） |
|---|---|---|
| Provider | ✅ | `open(info_out) → handle`、`read_at(h, off, buf, len)`、`write_at`(可空)、`caps()`、`poll_change()`、`close(h)` |
| Visualizer | ✅ | `matches(field_desc) → cfg`、`measure(cfg, bytes) → size_est`、`render(cfg, bytes, sink)`、`reverse(cfg, x, y) → range` |
| Command | ✅ | `list() → [cmd_desc]`、`invoke(cmd_id, ctx)`、`is_enabled(cmd_id, ctx)`、`on_ui_event(ev)` |
| ProcessorNode | ✅ | `describe() → ports/params`、`process(chunk_in, chunk_out, params)`、`reset()` |
| BspecFn | ✅ | `describe() → name/arity/purity`、`call(args, out)`；**必须 `purity = pure` 且无 I/O**，否则拒绝注册 |
| Disasm | ⏳ v1.1 | `arches()`、`disasm_one(bytes, addr, out)`、`regs()` |
| Theme | ✅ | `pack() → toml_bytes`（只提供主题文本 + 资源清单，不直接改 UI 状态） |
| IO Codec | ✅ | `can_import(magic) → bool`、`import(bytes) → doc`、`export(doc, sink)` |

**Provider 是唯一在热路径上的能力**，因此有一条额外约束：插件 `read_at` 会被包装成"受预算计量的调用"，每次调用前后打点，p99 超过 20µs/64KB 即在 UI 上提示"插件数据源性能异常"（§5）。视觉化器与命令不在 hex 渲染主循环里，无此约束。

### 3.4 内存所有权规则（8 条，逐条可测）

1. **谁分配谁释放**。宿主不 `free` 插件分配的指针，插件不 `free` 宿主分配的指针；两侧各有一套 allocator（每个 cdylib 自带一份 std），跨侧释放必然崩。
2. **宿主借出的内存只在当前回调内有效**。`doc_read` 写入的是调用者提供的 `out` 缓冲，没问题；但 `bytes`/`blob` 类借出指针在回调返回后立即失效。插件要留用必须自行拷贝。
3. **宿主将长期持有的内存必须由 `host->alloc` 分配**，由宿主用 `host->free(p, len, align)` 释放，`len` 与 `align` 必须与分配时完全一致。
4. **字符串一律 `(ptr, len)`**，UTF-8，**不保证 NUL 结尾**；`len == 0` 时 `ptr` 允许为 `NULL`。宿主侧必须按 `len` 转 `&str`，禁止 `strlen`。
5. **句柄（`GeodeDocHandle`/`JobId`/`AnnId`）是不透明 `u64`**，可能已失效；宿主保证不复用句柄值（单调递增）。插件每次使用前必须处理 `GEODE_ERR_STALE_HANDLE`。
6. **回调期间文档可能被关闭或编辑**：`doc_read` 可能返回 `GEODE_ERR_DOC_CLOSED`；插件不得把 `GeodeDocHandle` 存到回调之后，必须每次用 `doc_current()` 取。
7. **出参只在成功时写**：`int32_t` 返回 0 = `GEODE_OK`，负数为 `GeodeStatus`（`GEODE_ERR_INVALID_ARG=-1`、`GEODE_ERR_OUT_OF_BOUNDS=-2`、`GEODE_ERR_BUDGET=-3`、`GEODE_ERR_DOC_CLOSED=-4`、`GEODE_ERR_STALE_HANDLE=-5`、`GEODE_ERR_UNSUPPORTED=-6`）。插件返回的缓冲区在失败时宿主不读取也不释放。
8. **异常绝不跨越 ABI 边界**：插件必须在自己的每个边界函数里 `catch_unwind`；宿主也会在**每个** ABI 入点上包一层 `catch_unwind`（因此宿主与插件都必须 `panic = "unwind"`，cdylib 用 `panic = "abort"` 会被加载器拒绝并在 UI 说明原因）。

### 3.5 Rust 插件 SDK（`geode-plugin-sdk`）

SDK 把全部 `unsafe`、vtable 布局、`(ptr,len)` 转换、`catch_unwind`、错误码映射收进宏与类型里，插件作者只写安全 Rust：

```rust
#[geodeplug(id = "acme.waveform", version = "0.2.1", host = "1.0..2.0", license = "MIT")]
fn entry(ctx: &mut HostCtx) -> Caps {
    Caps::new()
        .visualizer("waveform", WaveformViz)             // 实现 Visualizer trait 即可
        .command("waveform.export_csv", cmd_export)      // 菜单项与快捷键交给 16 仲裁
}

impl Visualizer for WaveformViz {                        // 与 13 §3.2 的 trait 一一对应
    fn id(&self) -> &'static str { "waveform" }
    fn matches(&self, f: &FieldDesc) -> Option<Config> { ... }
    fn measure(&self, cfg: &Config, data: &[u8]) -> Result<SizeEstimate> { ... }
    fn render(&self, cfg: &Config, data: &[u8], out: &mut dyn RenderCtx) -> Result<()> { ... }
}
```

`#[geodeplug]` 展开为：`#[no_mangle] pub extern "C" fn geodeplug_entry` + 静态 `GeodePluginDesc` + 每个 trait 方法一个 `extern "C"` 蹦床（内含 `catch_unwind` 与错误码转换）+ 参数合法性校验。SDK 自带 `sdk-conformance` 测试套件，插件作者 `cargo test` 即验证 ABI 契约。

---

## 4. 核心算法 / 流程

### 4.1 加载流程

```
① 扫描目录（配置的 plugin_dirs + 用户目录），收集 *.geoplug / *.dll / *.dylib
② 读同名 manifest（plugin.toml：id/name/version/permissions/capabilities/签名附属信息）
   —— 只用于 UI 展示与"是否需要提示"，一切以加载后自报为准
③ 校验：文件哈希 + Ed25519 签名（公钥库来自 15）；未签名 → 按配置决定拒绝/警告/仅开发模式放行
④ dlopen（RTLD_NOW|RTLD_LOCAL；Windows 绝对路径）→ dlsym "geodeplug_entry" → 版本校验
   （major 相等；plugin.host_min <= host_version < plugin.host_max）；任一步失败即弃
⑤ 调用 geodeplug_entry(host_api)，取回 GeodePluginDesc（包 catch_unwind + 5s 超时看门狗）
⑥ 逐条校验能力：kind 已知、vtable.struct_size 合理、id 唯一、BspecFn 的 purity 合法，
   成功者才注册（单个能力失败不影响其它能力），然后标 Loaded 并写持久化记录
```

**关键决策**

- **`RTLD_LOCAL` 而非 `RTLD_GLOBAL`**：避免插件之间的符号互相污染（两个插件各自带一份 `serde`/`rayon` 也不会互相踩）。**Windows 必须绝对路径 + `LOAD_WITH_ALTERED_SEARCH_PATH`**：否则 DLL 搜索顺序可被同目录的恶意同名 DLL 劫持（这是本子系统最容易被利用的入口）。
- **macOS**：若宿主启用了 Hardened Runtime 并做公证，加载第三方未签名 dylib 需要 `com.apple.security.cs.disable-library-validation` 权限。**这是在"公证"与"第三方插件"之间的真实取舍**：v1 选择带上该 entitlement（并在 §7 向 15/18 声明这是安全妥协），v2 的进程隔离方案可以去掉它。
- **`dlopen` 前不 `chdir`、不继承工作目录语义**，所有路径先 `canonicalize` 并检查是否落在允许目录内。

### 4.2 使用期与看门狗

```
插件调用 = 边界包装(catch_unwind) + 计时 + 看门狗登记 + 结果校验
看门狗线程：每 500ms 扫描"已登记但超时（默认 5s）"的调用
   → 超时：写诊断 + 把插件标为 Suspect + 在 UI 顶部提示"插件 X 无响应"；不能强制终止
     （同进程无安全手段），只能提示用户"禁用该插件并重启"
```

坦白：**同进程内无法抢占一个死循环的插件**。看门狗只能"发现并提示"，不能"救回"。这条必须在 §6 写明，也是 v2 进程隔离的主要动机。

### 4.3 卸载

```
① 反注册：从所有能力表移除该插件的条目（此后新查询看不到它）
② 等待引用计数归零：所有 Arc<dyn Capability> 都被丢弃、所有 in-flight Job 结束（最长等 5s，超时则放弃卸载）
③ 清理宿主侧持有的插件内存（用 host->free 释放插件交来的缓冲）
④ 决定是否 dlclose：v1 默认 **不真正 dlclose**，只把映射标记为"退役"
```

**为什么不 `dlclose`**：Rust 的 cdylib 卸载会跑 `std` 的 TLS 析构与 `at_exit` 钩子，而其它线程可能仍持有指向该映射的返回地址（尤其 `rayon` 线程池里的任务）；`dlclose` 之后这些线程一执行就跳进野指针。业界（包括多个编辑器）的经验是**卸载动态库的收益远小于风险**。代价：卸载后地址空间里留一份映射（通常 1~8MB），插件数少时可接受。热重载因此改为"新文件名加载新版 → 切换引用 → 旧版退役"，同一插件反复热重载 50 次会累计映射，开发模式下会提示重启。

### 4.4 崩溃隔离现状（诚实版）

| 现象 | v1 结果 | 缓解 |
|---|---|---|
| 插件 panic（unwind） | 被 `catch_unwind` 拦下，调用返回 `GEODE_ERR_INTERNAL`，插件标记 Suspect | 完全可用 |
| 插件 panic（abort） | **整个应用 abort** | 加载时检查 cdylib 的 panic 策略，abort 一律拒绝加载 |
| 段错误 / 空指针 / 越界写 | **整个应用崩溃** | 崩溃处理器写 minidump + 记录"当时正在调用的插件"；下次启动弹窗"上次崩溃可疑插件 X，是否禁用" |
| 死循环 | UI 卡死 | 看门狗提示；同时所有插件调用都走"不在 UI 线程"的间接调用路径，卡的是后台线程 |
| 内存泄漏 / 无限分配 | 宿主 OOM | 插件侧分配走 `host->alloc`，宿主可统计每插件分配量并在超阈值（默认 512MB）时拒绝继续分配 |

**"在子进程预检插件"（v1 可选，默认开启）**：插件首次加载前，宿主 fork 一个子进程，在其中 `dlopen` + 调 entry + 调用插件自报的 `self_test`（跑几个只读样本），5s 超时。子进程崩溃 → 判定"该插件在此平台不稳定"，不加载。代价约 80ms/插件，可配置关闭。它挡不住"只在特定数据上崩"的情况，但能挡住"装载即崩"与"ABI 根本对不上"两类占比最高的问题。

---

## 5. 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 单个插件加载（含 dlopen + entry） | < 15ms | 本地 SSD，未签名校验不含 |
| 启动时 20 个插件总扫描 | < 100ms | 仅读 manifest；`dlopen` 延后到 idle 任务 |
| ABI 单次调用开销 | < 200ns | 空调用往返（含 `catch_unwind`） |
| `doc_read` 吞吐 / 插件 Provider `read_at` p99 | > 1 GB/s；< 20µs / 64KB | 64KB 块命中页缓存；超阈值 → UI 警告，不自动禁用 |
| 插件 Visualizer 解码 | 同 13 §5（缩略图 < 8ms、全尺寸 < 250ms） | 超时按 13 §6 降级 |
| 声明式面板一次提交 | < 0.3ms | ≤200 控件 |
| 能力表查询（按 kind + id） | < 100ns | HashMap 命中，读侧无锁（`arc_swap`） |
| 每插件可观测堆占用上限 | 512MB（默认） | 超限拒绝分配 |

**三条规则**

1. **UI 线程绝不直接进插件**。所有插件调用（除 `matches()` 这类"已确认是纯计算且 <10µs"的少数）都经 Job 或专用插件线程；这条与 00 §6 规则 1 同源。
2. **能力表读路径无锁**：用 `arc_swap::ArcSwap<Registry>`，注册/反注册走写侧拷贝，避免 hex 渲染线程与注册表争锁。
3. **数据不整块拷贝**：`doc_read` 一次只读调用方要的区间；插件处理大区间必须分块循环（宿主强制单次 ≤ 4MB，超出返回 `GEODE_ERR_INVALID_ARG`）。

---

## 6. 失败模式与降级

| 失败 | 检测 | 降级 | 码 |
|---|---|---|---|
| 目录里有非法文件 | dlopen 失败 | 跳过 + 日志；不影响其它插件 | `E9001` |
| manifest 与自报元信息不符 | ⑥ 校验 | 以自报为准 + 警告（manifest 只作展示） | `E9002` |
| ABI major 不符 / 要求宿主版本高于当前 | 载入 ④ | 拒绝加载，UI 明确写"需要宿主 1.x，当前 2.0，请更新插件"/"需要更新的宿主版本" | `E9003` `E9004` |
| 签名缺失/无效 | ③ | 默认拒绝；用户可在"高级"里显式放行单个文件（记入信任列表，含哈希） | `E9005` |
| 能力 id 冲突 | ⑥ | 后注册者被拒（内置优先，其次按加载顺序） | `E9006` |
| 入口函数 panic / 调用期 panic | `catch_unwind` | 入口 panic → 该插件禁用，应用正常启动；调用 panic → 返回错误并标 Suspect，连续 3 次自动禁用 | `E9007` |
| 插件死循环 | 看门狗 5s | 提示"插件 X 无响应，建议禁用并重启" | `E9008` |
| 插件进程崩溃 | 退出码/信号 | 崩溃处理器记录可疑插件 → 下次启动弹窗禁用建议 | `E9009` |
| 插件超配额分配 | `alloc` 统计 | 拒绝分配，调用返回 `GEODE_ERR_BUDGET` | `E9010` |
| 插件未实现必需函数 | vtable 校验 | 该能力不注册（插件其余能力仍可用） | `E9011` |
| 热重载失败 | 新版 entry 失败 | 回滚到旧版（旧映射仍在），提示重启以彻底清理 | `E9012` |

`E9xxx` 为插件段，已登记于 `00-architecture-overview §12`（与 13 的 `E8xxx`、15 的 `EAxxx` 同批分配）。

---

## 7. 与其它子系统的契约

### 7.1 本子系统写

| 目标 | 内容 |
|---|---|
| 各能力注册表（宿主内部） | `arc_swap` 单例，UI 向下读 |
| 16 的命令系统 | 插件命令以"外部命令源"登记，菜单路径与快捷键由 16 仲裁，冲突时内置优先 |
| 13 的 `VisualizerRegistry` | 包装后的 `Arc<dyn Visualizer>`（`VizSource::Plugin`） |
| `geode-annot` | 经 HostApi 的 `ann_insert`，`Layer` 由宿主按能力类型限定（插件不能写 `Layer::Structure`） |
| `geode-core` 诊断 | `GeodeDiagDesc` → `Diag`，`code` 用 `E9xxx`，`range` 必填 |
| 17 工程文件 / 崩溃转储目录 | 记录启用插件的 `(id, version, hash)`，缺失时不阻塞打开；记录可疑插件供下次启动读取 |

### 7.2 本子系统读

15 的公钥库与签名接口、16 的当前文档/视图上下文、00 §5.4 的 Job 系统。

### 7.3 明确的安全事实（必须写进用户文档）

> 加载插件 = 允许该插件以你的用户权限执行任意代码。宿主不提供系统级沙箱（无 seccomp/无 AppContainer），ABI 只约束"插件通过宿主访问数据"，不约束"插件自己 syscall"。唯一的真实隔离方案是进程隔离（v2）。

---

## 8. 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `plugin.enabled` | true | 总开关（关掉后不扫描不加载） |
| `plugin.dirs` | `[<数据目录>/plugins]` | 只加载这些目录下的文件（canonicalize 后校验前缀） |
| `plugin.allow_unsigned` / `plugin.require_signature` | false / true | 二者共同决定验签策略：生产必须 `require_signature=true`；`allow_unsigned=true` 仅限开发模式且 UI 必须常驻警示条（企业策略可强制） |
| `plugin.max_count` | 64 | 超出时只加载签名有效的前 N 个 |
| `plugin.load_timeout_ms` | 5000 | entry + self_test |
| `plugin.call_timeout_ms` | 5000 | 看门狗阈值 |
| `plugin.preflight_subprocess` | true | 子进程预检（§4.4） |
| `plugin.hot_reload` | false（开发模式默认 true） | |
| `plugin.mem_quota_mb` | 512 | 每插件分配上限 |
| `plugin.max_call_buf` | 4 MiB | 单次 `doc_read` 上限 |
| `plugin.suspect_disable_after` | 3 | 连续调用失败次数阈值 |
| `plugin.network` | **不可配置，恒拒绝** | ABI 不提供 |

---

## 9. 测试策略

| 层次 | 内容 | 门禁 |
|---|---|---|
| ABI 兼容矩阵 | 用 v1.0/v1.1/未来 v1.2 的 SDK 各编译一个 fixture 插件，与当前宿主两两组合加载 | 同一 major 全绿，跨 major 全部明确拒绝 |
| 编译期契约 | `static_assert` 等价物：`offsetof`/`size_of` 断言全部 `#[repr(C)]` 结构与 C 头一致（用 `bindgen` 生成的头做对照） | 差异 = 0 |
| 故意崩溃 | 1) panic 插件 2) abort 插件 3) `mem::forget` 泄漏插件 4) 死循环插件 5) 越界写插件 | 1/3/4 必须不崩宿主；2/5 明确记录为"预期崩溃"并验证崩溃处理器产出正确插件名 |
| HostApi fuzz / 所有权 | 用 fuzz 插件对每个回调传入 `NULL`、`len=0`、`len=u32::MAX`、越界 offset、已失效句柄；泄漏检测（每插件分配/释放计账配平）；跨侧 free 检测（debug 构建用哨兵字节） | 全部返回错误码且无 UB；配平误差 = 0 |
| 性能 | §5 全表入 `bench/` | 退化 >15% 红灯 |
| 签名 | 篡改一个字节、替换签名、使用过期/吊销公钥 | 全部拒绝，且 UX 文案可读 |
| 平台 | macOS（公证 + entitlement）、Windows（DLL 劫持用例）、Linux（相对路径/符号污染用例） | 全绿 |
| 大规模 | 同时加载 64 个插件 + 打开 100GB 文档 | 启动 < 3s，滚动帧时间不回退超过 1ms |

---

## 10. 开放问题

1. **卸载与双 `std`**：当前决策是"永不真正 `dlclose`"，但开发模式反复热重载会累积映射、`Arc` 泄漏会延长生命周期——是否在进程退出前统一 `dlclose`（跑一遍析构以暴露泄漏）？另外每个 cdylib 自带一份 `std`（`TLS`、`panic` 运行时、`rayon` 全局池互不相识），插件的线程池可能劫持宿主 CPU，是否强制插件只用宿主提供的线程池？需实测。
2. **`abi_stable` 是否值得**：它能让插件用 Rust 类型（`RString`/`RVec`），代价是 ABI 面大幅膨胀且版本敏感。v1 选裸 C ABI。若第三方插件开发者的最大抱怨是"手写 unsafe 太痛"，再评估（SDK 已能遮掉大部分）。
3. **v1.1 的 egui shim 成本要量化**。§1.3 的触发条件（20% 插件需要自由绘制）是拍脑袋的，应从插件仓库的真实需求统计。
4. **macOS 公证与第三方插件的取舍**：带 `disable-library-validation` 会削弱整体加固，是否改为"v1 只允许公证过的插件"（等于生态封闭）？
5. **v2 进程隔离的具体形态**：共享内存环形缓冲 + 每能力一条请求/响应通道？插件窗口是独立顶层窗口还是宿主管理的子窗口（后者在 Wayland 上很麻烦）？WASM 是否作为"轻量、低可信度要求"的第二通道回归（纯计算的 BinSpec 函数与主题，性能代价可接受、安全收益巨大）？
6. **权限声明的可信度**：`permissions` 字段完全靠插件自报，宿主无法验证（ABI 层面没有手段）。是否干脆去掉，改为"所有插件一律视为全权限"+ 首次加载强制弹窗？
