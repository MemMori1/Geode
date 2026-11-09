# 16 — UI 外壳 / 主题 / i18n / 字体

> 本文规范 `geode-ui` 的**容器层**：主窗口布局、面板管理、命令与快捷键系统、主题、i18n、字体、无障碍与外壳性能。
> **不涉及各视图内部**（hex 网格绘制 `见 05-render-and-hexview`，结构树 `见 02-dsl-spec`，搜索 UI `见 08-search-subsystem`，可视化 `见 13-visualizers`）。布局持久化格式 `见 17-project-and-persistence`；主题/语言包的**分发与安装** `见 15-content-distribution`。

---

## 1. 目标与非目标

| # | 目标 | 可验证判据 |
|---|---|---|
| G1 | 菜单 / 工具栏 / 命令面板 / 快捷键**四者单一事实来源** | 新增一个功能只需注册一次 `Command`，四处同时出现 |
| G2 | 布局零学习成本、可恢复 | 首次启动合理默认；崩溃重启后布局与上次一致 |
| G3 | 主题是**数据**，不是代码 | 换主题、跟随系统、热重载都不重启，不改二进制 |
| G4 | 中文与英文一等公民，无截断、无乱码、无错位 | 伪本地化后 0 截断；CJK 在 hex 网格严格占 2 列 |
| G5 | 外壳开销不挤占 hex 渲染 | 布局 + 外壳 ≤ 2ms/帧（§5） |

**非目标（明确否决）**：

- **不做可分离为独立窗口的面板**（§4.1 详述理由：viewport 焦点/DPI/输入法三重坑，收益低）。
- **不做 Vim 模式**（v1；可行性见 §4.4 末段）。
- **不做主题编辑器 GUI**（v1；手写 TOML + 热重载已足够，先验证 token 集稳定）。
- **不做 RTL 布局**（v1；不引入 `egui` 之外的布局引擎）。
- **不做自绘滚轮惯性、动画系统**：立即模式 UI 里动画易造成重绘抖动，除 §4.5 的 120ms 主题过渡外一律无动画。

---

## 2. 依赖与边界

`geode-ui` 是唯一可依赖 egui/wgpu 的 crate（00 §2 硬约束，另一处是 `app`）。它向下依赖各业务 crate，向上被 `app` 组装。

| 关注点 | 归属 | 本文态度 |
|---|---|---|
| hex 网格绘制、偏移列、字符度量实测 | `05-render-and-hexview` §3/§7 | 本文只提供**字体与度量 API** |
| 各视图内部交互 | 05 / 07 / 08 / 10 / 12 / 13 | 只定义宿主容器与生命周期 |
| 布局持久化、最近文件、工作区 | `17-project-and-persistence` | 只定义内存模型与序列化时机 |
| 主题包/语言包的下载安装 | `15-content-distribution` | 只消费 `ContentStore::resolve` |
| i18n catalog 的语法与插值 | 本文 §4.6 | — |
| 全局快捷键的系统级注册（媒体键等） | 不涉及 | 明确不做 |

工程约束：`egui` 用 `ViewportCommand` 控制窗口，不引入 `eframe` 之外的窗口库；字体走 `epaint::text::FontDefinitions`；无障碍走 `egui/accesskit`（§4.8）。

---

## 3. 数据结构与接口

### 3.1 主窗口布局

> 本节只定**区域划分、尺寸约束与固定/弹性属性**。完整的 ASCII 线框（主窗口、结构树联动、搜索结果、处理器图、diff、AI 建议面板、空状态、首次运行）、文档状态机与全部文案规范见 `25 §4`；两份文档冲突时以本节的结构约束为准，以 25 的话术为准。

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ 菜单栏 File Edit View Tools Help                     ⟳ 2 个后台任务    28px  │
├──────────────────────────────────────────────────────────────────────────────┤
│ 工具栏 [打开][保存][撤销][重做] │ [搜索][跳转 ____][结构▾][主题▾][命令⌘P] 36px│
├──────────────┬──────────────────────────────────────────┬────────────────────┤
│ 左 260px     │ 中央：标签页 + 可拆分的 hex 视图         │ 右 320px           │
│ ▸ 文档 (3)   │ ┌ [report.bin ×][disk.img ×][+] ┐        │ Inspector          │
│   disk.img   │ │ 0000│ 4D 5A 90 00 …  MZ……     │        │  值 / 类型 / 注释  │
│   report.bin │ │ 0010│ …                       │        │ ▸ 分析 / 反汇编    │
│ ▸ 结构树     │ │      (05-render-and-hexview)│        │ ▸ YARA / 可视化    │
│   ▸ DOS_HDR  │ └────────────────────────────────┘       │                    │
│ ▸ 处理器图   │                                          │                    │
├──────────────┴──────────────────────────────────────────┴────────────────────┤
│ 0x00001A3C │ 选区 256 B │ hexlify │ LE │ 4.00 GB │ 补丁 12 │ ▓▓░░ 43% │0.9.2│ 24px
└──────────────────────────────────────────────────────────────────────────────┘
```

| 面板 | 职责 | 默认 / 最小 / 最大宽 | 可关闭 |
|---|---|---|---|
| `Left` | 打开的文档列表（多标签视图 = 树）、结构树、数据处理器图（三者上下堆叠或标签化） | 260 / 180 / 480 | 是 |
| `Center` | 标签栏 + 一或多个 hex 视图（可水平/垂直拆分） | 剩余空间 / 480 / — | **否** |
| `Right` | Inspector、分析、反汇编、YARA、可视化（标签化） | 320 / 240 / 560 | 是 |
| `StatusBar` | 偏移、选区长度、编码、字节序、文件大小、补丁数、后台进度、版本 | 24px 固定高 | 否 |
| `MenuBar` / `Toolbar` | 由命令注册表生成（§4.2） | 28 / 36px | 工具栏可隐藏 |

宽高以逻辑像素（dp）计，随 `pixels_per_point` 缩放；`Center` 永不被挤压到 480 以下（否则拒绝拖动分隔条）。

```rust
pub enum PanelSlot { Left, Right, Bottom }        // Center 恒存在，不入枚举
pub struct PanelId(pub &'static str);             // "structure" / "inspector" / "yara" / "graph" …
pub struct PanelSpec {                            // 静态注册（linkme/ctor 式表）
    pub id: PanelId, pub slot: PanelSlot, pub title_key: &'static str, // i18n key
    pub default_visible: bool, pub closable: bool, pub singleton: bool,
    pub default_split: f32,                       // 同槽内相对高度，0..1
}
pub struct ShellLayout {
    pub left_width: f32, pub right_width: f32,
    pub left_panels: Vec<PanelId>, pub left_split: f32,
    pub right_panels: Vec<PanelId>, pub right_split: f32,
    pub visible: HashSet<PanelId>, pub maximized: Option<PanelId>,
    pub toolbar_visible: bool, pub statusbar_visible: bool,
}
pub struct Tab { pub doc: DocId, pub primary: ViewId, pub splits: Vec<SplitSpec>, pub title: String }
pub enum SplitDir { Horizontal, Vertical }
pub struct SplitSpec { pub dir: SplitDir, pub ratio: f32, pub view: ViewId }
```

### 3.2 命令注册表

```rust
pub struct CommandId(pub &'static str);           // "file.save" / "view.toggle_left"
pub enum CommandCategory { File, Edit, View, Nav, Search, Structure, Tools, Panel, Help }
pub struct Command {
    pub id: CommandId,
    pub title_key: &'static str,                  // i18n：menu.file.save
    pub category: CommandCategory,
    pub default_keys: &'static [KeyChord],        // 可空
    pub enabled: fn(&UiCtx) -> bool,              // 灰显依据（不是鉴权）
    pub checked: Option<fn(&UiCtx) -> bool>,      // 可勾选（如"显示结构树"）
    pub requires_confirm: Option<ConfirmSpec>,    // 破坏性操作（覆盖保存、批量替换）
    pub run: fn(&mut UiCtx) -> Result<(), UiError>,
}
pub struct CommandRegistry { map: HashMap<CommandId, Command>, order: Vec<CommandId> }
```

注册在 `linkme` 分布式切片里按 `category` 顺序稳定排序 → 菜单顺序**确定**，不依赖 HashMap 迭代顺序。`ui_schema_ver` 由 `xtask` 生成，用于 UI 快照测试（§9）。

DSL 相关命令（`bspec.fmt.document` / `bspec.lint.run` / `bspec.debug.start` / `bspec.debug.step` / `bspec.trace.export`）同样注册在本表、归 `ContextId::Debug`，**不得由 DSL 面板自建快捷键或菜单项**（见 22 §4.4）。这是「命令注册表是唯一事实来源」的硬约束，否则菜单、命令面板、快捷键三处必然漂移。

### 3.3 快捷键

```rust
pub struct KeyChord { pub key: Key, pub mods: Modifiers }   // mods: cmd/ctrl/shift/alt
pub struct Keymap { overrides: BTreeMap<CommandId, Vec<KeyChord>>, /* 空 vec = 显式解绑 */ }
pub struct KeyConflict { pub chord: KeyChord, pub claims: Vec<CommandId>, pub context: ContextId }
pub enum ContextId { Global, HexView, TextInput, SearchBox, StructureTree, Debug }
```

解析规则：**上下文优先于全局**，同上下文内后注册者报冲突（不允许静默覆盖）；文本输入焦点在 `TextInput` 上下文时不派发任何 `Global` 命令（§4.4）。

### 3.4 主题 token

三层 token，见 §4.5 与完整示例：

```rust
pub struct Theme {
    pub schema: u32, pub id: String, pub appearance: Appearance,  // Dark | Light
    pub palette: HashMap<String, Color32>,      // ① 基础调色板（命名色，如 gray-2）
    pub semantic: SemanticTokens,               // ② bg.base / fg.primary / accent / border / …
    pub domain: DomainTokens,                   // ③ hex.* / layer.* / severity.*
    pub fonts: Option<FontOverrides>,           // 主题可只改字号/行高，不改字体文件
}
pub struct DomainTokens {
    pub hex_offset: Color32, pub hex_ascii: Color32, pub hex_null: Color32, pub hex_ascii_nonprint: Color32,
    pub hex_patch_insert_bg: Color32, pub hex_patch_delete_bg: Color32,
    pub hex_patch_insert_fg: Color32, pub hex_patch_delete_fg: Color32,
    pub layer: [Color32; Layer::COUNT],          // 顺序与 00 §5.3 的 Layer 一致（当前 10 个变体）
    pub layer_bg: [Option<Color32>; Layer::COUNT],
    pub severity: [Color32; 3],                 // Error / Warning / Info
}
```

**`layer` 数组必须覆盖 `Layer` 全部 10 个变体**（Value/Search/Structure/Analysis/Selection/Bookmark/Diff/Yara/Ai/Cursor）：加载时按 `Layer::COUNT` 校验长度，缺一即拒载（`E0120 主题 token 不完整`）。这样新加 `Layer` 变体时编译器与加载器双重兜底。

### 3.5 i18n catalog

```rust
pub struct Catalog { lang: LanguageId, entries: HashMap<Key, Entry>, plural: PluralRules }
pub struct Entry { text: String, /* 已解析的 {var} 段 */ vars: Vec<(Range<u32>, VarId)>,
                   one: Option<Box<Entry>>, other: Option<Box<Entry>> }
pub struct LanguageId(pub String);              // BCP-47 子集："zh-CN"
pub struct Translator<'a> { chain: [&'a Catalog; 3], /* 当前 → en-US → key 本身 */ }
```

### 3.6 字体

```rust
pub struct FontSet {
    pub mono_family: FontFamilyId,              // hex 网格唯一可用族
    pub ui_family: FontFamilyId,                // 菜单/标签
    pub fallback: Vec<FontFamilyId>,            // 缺字回退链，≤ 4
    pub metrics: MetricsCache,                  // 每 size 的 advance/行高/ascent
}
pub struct MetricsCache { by_size: SmallVec<[(u16, FontMetrics); 8]>, generation: u32 }
pub struct FontMetrics { pub advance: f32, pub cjk_advance: f32, pub row_height: f32, pub ascent: f32, pub descent: f32 }
```

`advance` 与 `cjk_advance` **必须实测自字形**（`galley.size()`/`Font::glyph_width`），禁止 `advance = size * 0.6` 之类的假设（见 `01-competitor-feature-research §3.2`）。hex 视图只用 `mono_family`，不允许 fallback（§4.7）。

---

## 4. 核心算法 / 流程

### 4.1 面板管理

- **分隔条**：左右槽各一条竖直分隔条（命中区 6dp，视觉 1dp），槽内上下可再分一条水平条。拖动时只写 `f32` 到 `ShellLayout`，重建布局在同帧完成；松手才触发一次持久化（`见 17`，去抖动 500ms）。
- **显隐**：`Ctrl+Shift+E` 等命令切换 `visible`；面板关闭后保留在 `ShellLayout` 的槽列表里（重开时回原位）。`Center` 不可关闭——否则用户会把编辑器关没。
- **最大化**：`Ctrl+Shift+M` 暂存 `ShellLayout` 快照后只渲染目标面板，再按一次还原。**用快照而不是标记位**：否则最大化期间拖窗/改设置会污染原布局。
- **决策：默认不做可分离独立窗口（detach）。** 否决理由，逐条：
  1. egui viewport 的多窗口在 macOS 与 Windows 上的**焦点回传**不可靠：子窗口获得焦点时主窗口的 `input` 会丢一帧，命令与快捷键需要额外仲裁；
  2. **DPI 迁移**：窗口拖到另一块缩放比不同的显示器时，`pixels_per_point` 变化会让已缓存的字形图集与度量失效，需重建（§4.7）——重复重建会闪；
  3. **输入法（IME）**：多 viewport 下 CJK 候选窗锚点错位是 egui 已知问题，直接影响中文用户；
  4. 收益主要是"双显示器对照"，而**同一窗口内水平拆分**已能满足 90% 需求（拆两个视图看不同偏移）。
  保留后路：`ShellLayout` 里预留 `detached: HashSet<PanelId>` 字段（恒空，不序列化），将来若做只需实现渲染路径，不动数据模型。
- **布局持久化**：只在 ①退出、②松手 500ms、③最大化/还原、④标签增删后写盘。存 → `见 17`。

### 4.2 命令分发（单一事实来源）

```
启动：注册命令 → 应用用户 keymap（冲突检测）→ 生成菜单树 / 工具栏 / 命令面板索引
每帧：
  1) 收集输入事件 → 快捷键解析（§4.4）→ 命中的 CommandId 入队
  2) 绘制菜单/工具栏：按钮直接引用同一 Command（title_key / enabled / checked / keys 提示）
  3) 执行队列：先检查 enabled() → 需要确认则弹框 → run()
  4) 任何 run() 返回 Err → 转 Diag + 可关闭提示条（不 panic，不阻塞后续命令）
```

菜单、工具栏、命令面板**都不持有自己的文案与快捷键**，只持有 `CommandId`。这样"某功能在菜单有、工具栏没有、快捷键写错"这类不一致在结构上不可能发生。工具栏是菜单的**白名单子集**（`toolbar: true` 的注册项），用户可在设置里增删。

### 4.3 命令面板

- 触发：`Cmd/Ctrl+P`（**主**）；`Cmd/Ctrl+Shift+P` 作为别名（VSCode 肌肉记忆，零成本）。
- 打开时立即聚焦输入框，列表默认按 **最近使用**（MRU ≤ 40 条，持久化在 `state/ui.toml`，不含文件内容）排序，输入后按模糊分（子序列匹配 + 连续加权 + 词首加权）排序，取前 50。
- 每行显示：标题（i18n）、分类、快捷键（若已绑定）、禁用原因（`enabled()` 为 false 时显示灰字原因而非隐藏——**隐藏会让人以为功能不存在**）。
- `Enter` 执行、`↑↓` 移动、`Esc` 关闭；面板内不做二次确认，破坏性命令照常走 `requires_confirm`。
- 与"快速打开文件"区分：`Cmd/Ctrl+Shift+O` 走模糊文件名，`Cmd/Ctrl+P` 走命令。

### 4.4 快捷键

| 分组 | 键位 | 命令 |
|---|---|---|
| 文件 | `Cmd/Ctrl+N` / `O` / `S` / `Shift+S` / `W` / `Tab` / `Shift+Tab` | 新建 / 打开 / 保存 / 另存为 / 关闭标签 / 下一个标签 / 上一个标签 |
| 导航 | `Cmd/Ctrl+G` | 跳转到偏移（goto） |
| 导航 | `Ctrl+Home` / `Ctrl+End` / `PgUp` / `PgDn` / `←→↑↓` | 文件头 / 文件尾 / 翻页 / 移动光标 1 字节 |
| 导航 | `Alt+←` / `Alt+→` | 跳转历史后退 / 前进 |
| 导航 | `Cmd/Ctrl+Shift+G` | AI 一键猜结构 |
| 搜索 | `Cmd/Ctrl+F` / `F3` / `Shift+F3` / `Cmd/Ctrl+Shift+F` | 查找 / 下一个 / 上一个 / 跨文档搜索 |
| 搜索 | `Esc` | 清高亮 / 关搜索条 |
| 编辑 | `Cmd/Ctrl+Z` / `Shift+Z`（或 `Ctrl+Y`）/ `A` / `C` / `V` / `X` | 撤销 / 重做 / 全选 / 复制 / 粘贴 / 剪切 |
| 编辑 | `Insert` / `Backspace` / `Delete` / `Cmd/Ctrl+Shift+V` | 插入-覆盖切换 / 删前 1 字节 / 删后 1 字节 / 以 hex 文本粘贴 |
| 选择 | `Shift+方向` / `Shift+PgUp` / `Cmd/Ctrl+Shift+方向` / `Alt+Shift+方向` | 扩展 1 字节 / 扩展一页 / 到行首行尾 / 按结构单位扩展 |
| 视图 | `Cmd/Ctrl+=` / `-` / `0` / `\` / `F11` | 放大 / 缩小 / 重置缩放 / 拆分视图 / 全屏 |
| 视图 | `Cmd/Ctrl+Shift+T` / `Cmd/Ctrl+K` | 切换主题 / 切换跟随系统明暗 |
| 面板 | `Cmd/Ctrl+B` / `Cmd/Ctrl+J` / `Cmd/Ctrl+Shift+E` / `Cmd/Ctrl+Shift+M` / `Cmd/Ctrl+1..4` | 结构树 / 底部状态栏详细 / 文档列表 / 最大化面板 / 聚焦第 N 面板 |
| 工具 | `Cmd/Ctrl+Shift+D` / `Cmd/Ctrl+Shift+Y` / `Cmd/Ctrl+Shift+I` / `Cmd/Ctrl+Shift+A` | Diff / YARA 扫描 / Inspector / 分析 |

（共 34 条默认绑定；`Cmd` 在 macOS、`Ctrl` 在 Windows/Linux，由键位抽象统一映射，配置文件里写 `"cmd"` 一项两端通吃。）

- **冲突检测**：加载用户 keymap 时在同 `ContextId` 内做 chord 冲突矩阵；冲突 → 保留用户新值、把旧命令降级为无绑定，并在设置页标黄"`Ctrl+B` 原属 `view.toggle_structure`，已解除"。
- **导入 / 导出**：VSCode 风格 `keybindings.json`（`[{ "key": "ctrl+shift+d", "command": "tools.diff", "when": "geodeFocus" }]`）。导入时：`key` 按平台前缀规范化（`cmd`→`ctrl` on win/linux）、未知 `command` 收集成一份报告而不是整体失败、`when` 只支持本系统的 6 个 `ContextId` 白名单。
- **文本输入屏蔽**：`TextInput` / `SearchBox` 上下文激活时，只放行 `Esc` 与 `Cmd/Ctrl+V`（粘贴）——其余单键与 `Ctrl+字母` 一律交给输入控件，避免"打 `s` 触发保存"这类事故。`F3`/`F11` 等无冲突键仍可用。
- **AI 键位冲突（已裁决）**：`Ctrl+G` 在编辑器惯例中是**跳转偏移**，故定 `Ctrl+G = 跳转`、`Ctrl+Shift+G = AI 猜结构`。`03-ai-dsl-generation §1` 与 `10-analysis-toolkit §4` 已同步改为此键位（原 `Ctrl+G` 方案作废）。
- **Vim 模式（远期）**：可行，但要新增一个**模态输入层**——`Mode{Normal,Insert,Visual,Command}` 状态机位于快捷键解析之前，Normal 模式下把按键序列（`gg`、`3j`、`d2w`）累积进一个 ≤ 4 键的 pending buffer 再映射到 `CommandId`。风险：`Visual` 模式会绕过现有"选区随光标"模型（05 的选区由锚点维护），需给 `geode-ui` 加一层选区命令总线。工作量估 3~5 人日基础层 + 每模式调试，不进 v1。

### 4.5 主题

```toml
schema = 1
id = "dark-default"
name = { "en-US" = "Dark (Default)", "zh-CN" = "深色（默认）" }
appearance = "dark"                  # dark | light —— "跟随系统"按此字段匹配

[palette]                            # ① 基础调色板：按色相 + 明度阶梯命名，只定义不发散
gray-0 = "#0b0d10"  gray-1 = "#14181d"  gray-2 = "#1c2229"  gray-3 = "#2a323b"
gray-6 = "#9aa4b1"  gray-9 = "#e6ebf2"
blue-4 = "#3b82f6"  blue-6 = "#60a5fa"  cyan-5 = "#22d3ee"  violet-5 = "#a78bfa"
red-5  = "#ef4444"  amber-5 = "#f59e0b"  green-5 = "#22c55e"

[fg]                                 # ② 语义 token：只允许引用 palette 名
primary = "gray-9"   secondary = "gray-6"   disabled = "gray-3"   on-accent = "gray-0"
[bg]
base = "gray-0"   raised = "gray-1"   sunken = "gray-2"   hover = "gray-2"   active = "gray-3"
[border]
normal = "gray-3"   separator = "gray-2"   focus = "blue-6"
accent = "blue-4"   selection = "blue-4"   selection-text = "gray-0"

[hex]                                # ③ 领域 token
offset = "gray-6"   ascii = "gray-9"   ascii-nonprint = "gray-3"   null-byte = "gray-2"
patch-insert-bg = "#12351f"   patch-insert-fg = "green-5"
patch-delete-bg = "#3a1416"   patch-delete-fg = "red-5"

[layer]                              # 与 00 §5.3 的 10 个 Layer 一一对应且顺序一致，必须齐全
value = "gray-9"   search = "#facc15"   structure = "cyan-5"   selection = "blue-4"
analysis = "#a3e635"   # 00 §5.3 的 Analysis 层（10 变体齐全，见 26 §4.9）
bookmark = "violet-5"   diff = "#f472b6"   yara = "red-5"   ai = "#c084fc"   cursor = "#ffffff"
[severity]
error = "red-5"   warning = "amber-5"   info = "blue-6"
```

- 三层的作用不同：**palette 管"有哪些色"**（改整套主题只改这一层），**semantic 管"什么角色"**（控件与 egui `Visuals` 的映射），**domain 管"hex 里那一个字节该是什么色"**（05 每帧取用）。领域 token 允许直接写 hex 值，但 lint 会警告"绕过调色板"，避免散落魔术色值。
- egui 适配：`semantic` → `egui::Visuals`（`widgets.*.bg_fill` / `fg_stroke` / `selection.bg_fill`），`dark_mode` 由 `appearance` 决定；`hex`/`layer` 不进 `Visuals`，由 hex 视图直接查表（保持 `05` 零额外间接层）。
- **内置 5 套**：`dark-default`、`light-default`、`high-contrast`（纯黑/纯黄，AA 以上，目标 7:1）、`solarized-dark`、`nord`。前 3 套随二进制内置（只读，见 15 §4.5），后 2 套以 `.geopkg` 分发但**首启预装**。
- **热重载**：`notify` 监听 `themes/` 下当前主题文件，去抖动 250ms；重载失败**保留旧主题**并 toast（绝不能因为写错一个色号把界面刷成不可读）。重载成功后做 120ms 的交叉淡入（唯一允许的动画），可用"减少动效"关掉。
- **跟随系统明暗**：`theme.follow_system = true` 时监听平台外观事件（macOS `NSApp.effectiveAppearance`、Windows `AppsUseLightTheme` 注册表 + `WM_SETTINGCHANGE`、Linux 读 portal `color-scheme`），在 `appearance = dark/light` 的两个主题间切换；只切"用户所选主题的同 appearance 对应项"，不能乱跳。
- **主题编辑器（远期）**：先做"内存改 token → 导出 TOML"，不做完整表单；等 token 集稳定（v1.2 后）再考虑。

### 4.6 i18n

- 目录：`assets/i18n/<lang>/<namespace>.json`，命名空间 `app`、`menu`、`hex`、`search`、`structure`、`analysis`、`errors`、`content`、`settings`、`dsl`（10 个）。分文件是为了**按需加载 + 减少冲突**（多人同改一个 giant json 必冲突）。
- key 规范：`<namespace>.<area>.<item>[.<case>]`，点分层级 ≤ 5，全小写 + 连字符，例如 `view.hex.goto.title`、`menu.file.save`、`errors.zip.slip`。**key 一经发布不重命名**（翻译会丢），要改就新增 + 旧 key 保留一个版本周期。
- 插值与复数：`{name}`、`{count}`、`{offset:hex}`（格式化器白名单 `hex|dec|size|date|pct`）。复数用后缀键 `...one` / `...other`（+ `_few`/`_many` 预留给 ru/pl/ar），**不引入完整 ICU**（体积 ~30MB，对 5 语言不值）；CJK 无复数，只取 `other`。
- 回退链：`zh-TW → zh → en-US → key 本身`（缺 key 显示 `<menu.file.save>` 并 `log::warn` 一次，不显示空白——空白最难排查）。
- 运行时切换：`Cmd/Ctrl+Shift+L` 或设置页切换 → 重建 `Translator` → **发 `UiEvent::Retranslate`**；所有 UI 文案都是每帧查表（不缓存字符串），因此无需重启，也无需手动刷新面板。已生成的 `Diag.message` 不回填（历史消息保留生成时语言，避免"昨天的错误变成今天的语言"造成困惑）。
- 伪本地化（dev）：`GEODE_PSEUDO_LOCALE=1` 时把 en-US 变换为 `⟦Ĥéļļö wörļd⟧` 并**加长 30%**（`⟦… ~~~~⟧`）；两条收益：发现硬编码英文（未包裹 `t!()` 的字符串不会被变换），发现截断（加长后爆版）。
- 覆盖率 CI：`xtask i18n-check` 输出 `{lang → 缺 key / 多余 key / 占位符不一致}`；门禁 `en-US = 100%`、`zh-CN ≥ 98%`、其余 ≥ 60%（仅报告不 fail）。**占位符不一致必须 fail**（`{count}` 写成 `{cnt}` 会在运行时丢参数）。
- **DSL 诊断文案**：`dsl.err.<CODE>.short|long|action`（对应 02 §8.2 的 E1~E5）与 `dsl.lint.<RULE>.why`（对应 22 的 L001~L032）共用 `dsl` 命名空间，由 `xtask i18n-check` 一并校验覆盖；`Diag.message` 这类机器消息**不翻译**，只翻译人话层（见 22 §4.5）。
- 提取工具：源码只允许 `t!("menu.file.save")` 宏（编译期校验 key 存在，`xtask` 生成常量表）与 `t_fmt!("view.hex.goto.moved", offset = off)`；`xtask i18n-extract` 反向扫描宏生成缺失 key 的占位 JSON（值为空串 + `"__todo"` 标记），供翻译者填充。
- 语言清单：v1 `zh-CN`、`en-US`；预留 `zh-TW`、`ja`、`de`、`ru`（目录与复数规则位已留）。语言包同样可由内容库分发（`见 15`，语言包按 `theme.` 的同类机制、在 `content` 命名空间下校验 key 完备性）。
- **翻译流程**（缺了它语言包永远是半成品）：
  1. 主干只保证 `en-US` 完整；`zh-CN` 与其它语言由 PR 贡献，`xtask i18n-extract` 生成带 `"__todo"` 的占位文件作为 PR 起点。
  2. 翻译 PR 的机器门禁：占位符集合与 `en-US` 逐一相同、key 集合不多不少、`{...}` 内的格式化器名不得改写、JSON 可解析、无空串残留。
  3. 人审门禁：术语必须与 `25 §4` 的术语表一致（offset/offset 不混用"位置"、patch 不用"补丁块"等）；**含快捷键、菜单路径、命令名的文案不翻译**（一律从命令注册表取，见 §4.2）。
  4. 新语言接入的前提是"该语言的使用者愿意长期维护"，在 `assets/i18n/<lang>/MAINTAINERS.md` 里登记 @handle；无维护者的语言不合并。
  5. 发布前用伪本地化 + 目标语言 100% 覆盖跑一次截图回归，重点看工具栏与状态栏截断。

### 4.7 字体

- **hex 网格必须严格等宽且严格 2 列 CJK**：hex 视图绑定 `Sarasa Mono SC`（等宽 + CJK 恰好 2× Latin advance 的"等宽 CJK"字体），并**禁用该视图的 fallback**——一个非等宽字形混入就会让整屏十六进制错位（`01 §3.2` 的明确结论）。缺字走"渲染为 `□` + 状态栏提示"，不静默 fallback。
- **UI 文字另用一套**：`Inter`（Latin/UI，可读性）+ 系统 CJK 字体栈（macOS `PingFang SC`、Windows `Microsoft YaHei`、Linux `Noto Sans CJK SC`）→ 缺字回退到 `Sarasa Mono SC`。这样做的代价是 CJK UI 文本跨平台字面不同，收益是二进制少 ~8MB 且观感"本地化"。
- **体积预算**：`Sarasa Mono SC Regular` 子集（ASCII + Latin-1 + 常用标点 + GB2312 6763 汉字 + 假名）≈ **3.4MB**；`Inter` Latin 子集 ≈ **220KB**；UI CJK **不打包**（0B）。字体总占用上限 **4MB**，CI 里 `ls -l` 断言，超了必须走子集化重跑。
- 子集化流程：`pyftsubset`（构建期，脚本入 `xtask fonts-subset`）→ 保留 `--unicodes` 白名单 + `--layout-features='kern,liga,calt'` → 输出校验"`advance` 与 `cjk_advance` 比例仍为 2.0"（子集化破坏度量是真实陷阱）。
- **高 DPI 与度量缓存**：`pixels_per_point` 变化时 `MetricsCache.generation += 1` 并要求 epaint 重建字形图集（图集按物理像素栅格化，缩放比变了必须重建）；度量按 `(size, ppp)` 双键缓存，避免每帧 `galley` 计算。行高统一取 `round(max(metrics.row_height, ppp*size*1.35))`，避免半像素模糊。**例外**：hex 网格行高走 `05 §3.3` 的 `ceil(ascent+descent)+2*row_pad`（需要 `row_pad` 保点击性），两处不得互相套用。
- 字体图集上限：**单通道（`Alpha`/R8）2048×2048 = 4MB**，不够时先扩到 4096×4096（16MB）再拒绝新字形（渲 `□`）。不用 RGBA8——颜色在着色阶段施加，RGBA 图集是 4 倍浪费。
- 用户自定义字体：`config.ui.font_hex` / `font_ui` 支持本地路径；**hex 字体加载后必须实测等宽性**（随机取 200 个已用字形比对 `advance` 与 CJK 比例），不合格则拒绝并回退内置 + 明确报错（`E0130 字体不是等宽`）。

### 4.8 无障碍

- **键盘可达**：每条命令都有快捷键或可达的 Tab 路径（菜单栏 `Alt`/`F10` 唤起 → 方向键 → `Enter`）；面板内的自定义列表（结构树、命令面板）必须响应 `↑↓ Home End PgUp PgDn Enter Esc`——这是唯一的硬要求，用 `geode-ui` 统一的 `ListNav` 辅助实现。
- **屏幕阅读器（诚实说明）**：egui 通过 `accesskit` 向平台暴露语义树（Windows UIA / macOS NSAccessibility / Linux AT-SPI），当前限制是：① **自绘内容不在语义树里**——hex 网格是用 `Painter` 画的矩形，accesskit 看不到；② 树节点数量与每帧更新成本有冲突。因此本项目的方案是"**可访问镜像**"：为当前选区/光标所在行生成一段 `accesskit` 文本节点（`"偏移 0x1A3C：4D 5A 90 00，ASCII MZ.."`），而不是试图暴露 4000 个单元格；③ 结构树与 Inspector 用真实 widget 渲染（非自绘），天然可读。**v1 只做到这个程度**，完整 NVDA/VoiceOver 验收列为开放问题（§10.4）。
- **高对比度**：`high-contrast` 主题（纯黑底 + 纯黄/白字，目标对比度 7:1）+ 强制 1px 实线边框 + 关闭渐变/阴影。跟随系统高对比开关（Windows `SystemParametersInfo`、macOS `AXAPI`）时自动切到该主题。
- **色盲友好**：`layer.*` 与 `severity.*` **不得只靠色相区分**——Error/Warning/Info 在结构树与诊断列表里同时带图标/文字前缀；Diff 的插入/删除同时用底色 + 左侧 3px 竖条；hex 补丁行同时用底色 + `+`/`-` 标记。提供 `theme.deuteranopia = false` 开关：为 true 时把红/绿系换成 Okabe-Ito 蓝/橙系。**实现形态**：开关派生出一套命名主题 `dark-deuteranopia`（用户侧仍只是一个布尔，但快照基线可按命名主题比对，见 26 §4.9）。
- **减少动效**：`config.ui.reduce_motion = true`（跟随系统 `prefers-reduced-motion`）→ 关闭主题交叉淡入、面板展开动画、光标闪烁（改为常亮）。

---

## 5. 性能预算与基准

| 项 | 预算 | 说明 |
|---|---|---|
| 布局 + 外壳每帧 CPU | **< 2ms**（p99 < 3.5ms） | 空载 1 视图；面板全开 < 2.5ms |
| 其中：菜单/工具栏/状态栏 | < 0.4ms | 全部由命令表生成，无字符串分配 |
| 其中：主题查表 | < 0.15ms | `DomainTokens` 传引用，不逐格查 HashMap |
| 其中：i18n 查表 | < 0.3ms | 每帧 ≤ 200 次查表；`Key` 是 `&'static str` 的 interned id |
| 命令面板首帧（500 条） | < 12ms | 模糊分一次算完并缓存，输入时只增量重排 |
| 快捷键解析（每键盘事件） | < 30µs | 一次 `HashMap<KeyChord, …>` 命中 |
| 主题热重载 → 生效 / i18n 语言切换 | < 50ms / < 120ms | 不重建图集；语言切换重载 10 个 JSON（~200KB/语言） |
| 字形图集重建（ppp 变化） | < 40ms，且 ≤ 1 次/秒 | 防连续缩放导致抖动 |
| 字体图集内存 | ≤ 4MB（2048² R8），硬上限 16MB | §4.7 |
| 外壳常驻内存 | ≤ 30MB | 不含文档、不含字形图集 |
| 面板数量与重绘 | 1 个可见面板 ≈ 1 次 `ui` 子树；**隐藏面板零成本**（不构建） | 禁用面板每帧 < 100µs |

**重绘策略**：立即模式下"面板越多越慢"的来源不是绘制，而是每帧重建全部 `Ui` 子树。因此：①隐藏面板根本不构建；②非活动标签页只画标签头，视图体不构建；③egui 的 `ctx.request_repaint()` 只在必要时调用（后台任务进度、光标闪烁），空闲时无重绘（0% CPU 待机是验收项）。

---

## 6. 失败模式与降级

| 失败 | 检测 | 降级 |
|---|---|---|
| 主题 TOML 语法错 / token 缺失 / 引用不存在的 palette 名 | 加载时 schema + `Layer::COUNT` 校验 | 保留上一主题（首启则内置 `dark-default`），设置页列出缺哪些 token（`E0120`）；坏 token 以 `magenta` 高亮并给行号（**显眼而非静默**） |
| 自定义 hex 字体非等宽 / 字体文件缺失损坏 | 实测 200 字形；`FontData` 解析 | 拒绝加载 + 回退内置 + `E0130`（**绝不半用**）；文件级失败则回退系统等宽字体并提示"hex 对齐可能不精确" |
| 缺字（无字形） | 图集插入 miss | 渲 `□`，状态栏提示"当前字体缺字 N 个"，不 fallback（hex 视图） |
| 语言 JSON 缺失 / 解析失败 | 加载 | 该命名空间回退 en-US；全失败 → 显示 key 本身 + 黄条 |
| 缺 key / 占位符不匹配 | 查表 | 显示 key 名；占位符不匹配时原样输出，不 panic |
| 用户 keymap 冲突 / 非法 chord | 加载时矩阵检测 | 保留用户值、解除旧绑定、标黄说明；文件整体非法则忽略该文件 |
| 命令 `enabled()` 为 false | 每帧 | 灰显 + 悬停显示原因（不隐藏） |
| `run()` 返回 Err | 执行后 | 转 `Diag` + 可关闭提示条；命令系统不进入坏状态 |
| 布局文件损坏 / 面板 id 未知 / 几何不可能（负宽、重叠） | 反序列化 + 每帧校验 | 未知 id 丢弃、尺寸夹到 min/max、缺失字段取默认、几何归一化（**永不因布局失败而拒绝启动**） |
| 图片/字体图集分配失败（显存不足） | wgpu 错误 | 降到 1024² 图集并渲 `□`，提示"显存不足" |
| accesskit 初始化失败 | 平台 API | 静默降级为"无屏幕阅读器"，键盘可达不受影响 |
| 系统明暗事件风暴 | 事件合并 200ms | 只取最后一次；切换失败保持当前 |

总原则（对齐 00 §9）：**外壳任何组件失败都不得让窗口不可用**——最坏是"主题退回默认、某些面板关掉"，而不是白屏或 panic。

---

## 7. 与其它子系统的契约

| 子系统 | 它读本文 | 本文读它 |
|---|---|---|
| `app`（见 00 §3） | `geode_ui::run(AppHooks)` 入口、`UiCtx` | 提供 `Buffer`/`Provider`/`Job` 注册 |
| `05-render-and-hexview` | `FontSet`/`MetricsCache`/`DomainTokens`、`SplitSpec` | 视图尺寸、`ViewId` 生命周期、`request_repaint` |
| `07-data-processor-graph` / `02-dsl-spec` | `PanelSpec` 注册（左槽 `graph`、`structure`） | 面板内自绘 |
| `08-search-subsystem` | `ContextId::SearchBox` 键位屏蔽、`layer.search` 色 | 搜索条 command 注册 |
| `10/11/12/13` | 右槽面板注册与 `PanelSpec` | 面板标题 key、需要的高度 |
| `15-content-distribution` | 主题包/语言包 → `theme.toml`/`<lang>/*.json` 的加载入口 | `ContentStore::resolve`；`theme.` 包 token 完备性由此校验 |
| `17-project-and-persistence` | `ShellLayout`/`Tab`/`Keymap`/MRU 的序列化 schema | 惰性写入时机、原子写、版本迁移 |
| `geode-core`（00 §5.3） | `Layer` 枚举作为 `layer` token 的索引 | `Ann` 的 `fg`/`bg` 覆盖优先于主题 |

**硬约定**：本文新增的一切 UI 状态（可见性、宽度、MRU、主题 id、语言 id）都进 `ShellLayout`/`config.toml`，**不引入隐式全局状态**（00 §9）。任何视图不得自带主题色常量——必须查 `DomainTokens`；`xtask lint` 会 grep `Color32::from_rgb` 在 `geode-ui` 里的出现（只允许出现在主题加载器里）。

---

## 8. 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `ui.theme` | `"dark-default"` | 主题 id |
| `ui.theme.follow_system` | `true` | 跟随系统明暗（按 `appearance` 匹配） |
| `ui.theme.deuteranopia` | `false` | 色盲友好配色（红绿换蓝橙） |
| `ui.theme.hot_reload` | `true` | 主题文件热重载（仅 dev 与非打包版默认 true，打包版默认 false） |
| `ui.lang` | 系统语言 → 无匹配则 `"en-US"` | BCP-47；`ui.lang.pseudo` 为伪本地化（`GEODE_PSEUDO_LOCALE=1` 亦可） |
| `ui.font.hex` | `"builtin:sarasa-mono-sc"` | hex 视图字体（必须等宽，加载后实测） |
| `ui.font.ui` | `"builtin:inter"` | UI 字体 |
| `ui.font.size` / `ui.zoom` | `13.0` / `1.0` | 基准字号（dp）；缩放 0.6~3.0，`Ctrl+= / Ctrl+- / Ctrl+0` |
| `ui.panels.left.width` / `right.width` | `260` / `320` | 逻辑像素，夹在 180~480 / 240~560 |
| `ui.panels.toolbar_visible` / `statusbar_visible` | `true` / `true` | 状态栏关掉时保留最小 24px 提示区 |
| `ui.command_palette.mru_size` / `triggers` | `40` / `["cmd+p", "cmd+shift+p"]` | 最近使用条数；触发键 |
| `ui.keys.preset` | `"default"` | `default` \| `vscode-ish` \| 用户自定义文件 |
| `ui.reduce_motion` | 跟随系统 | 关闭动画/光标闪烁 |
| `ui.accessibility.screen_reader` | `true` | accesskit 开关（关掉省 ~0.3ms/帧） |
| `ui.statusbar.show` | `[offset, selection, encoding, endian, size, patches, tasks, version]` | 状态栏段的白名单与顺序 |
| `ui.banner.max_visible` | `2` | 顶部横幅最多同时显示条数，超出折叠为「还有 N 条」（`25 §4.4`） |
| `ui.copy.expert_mode` | `false` | 打开后错误文案附加原始 `code`/`path`，供用户贴 issue（`25 §5`） |
| `ui.ai_panel.expanded` | `false` | AI 面板默认收起，首次触发 `Ctrl+Shift+G` 时自动展开 |

---

## 9. 测试策略

| 层 | 内容 | 门禁 |
|---|---|---|
| UI 快照 | 固定 `egui::Context` 输入（尺寸/DPI 1.0、1.5、2.0）→ 渲染到离屏纹理 → 与基准 PNG 做 ≤ 0.1% 像素差比对（与 05 §9 / 18 §9 同口径；hex 网格另有一层 12 张**零容差**几何基线，见 26 §9.1） | 内置 3 套主题 + 1 套派生（deuteranopia）× 3 DPI × 3 布局（默认/全开/最大化）= 36 张基准图；分发包主题（solarized/nord）不进 CI 基线，避免依赖外部 `.geopkg` |
| 布局不变量 | 属性测试：随机拖动/显隐/最大化序列后，宽度恒在 min/max、无负宽、`Center ≥ 480` | 1 000 随机序列 |
| 快捷键冲突 / 命令完整性 | 加载 34 条默认绑定 + 一份"故意冲突"的 keymap，断言检测出且旧绑定被解除；`xtask` 断言每条 `Command` 有 `title_key`、该 key 在 en-US 存在、且在菜单或工具栏可达 | 冲突矩阵 0 漏报；0 孤儿命令 |
| i18n 覆盖率 / 伪本地化 | `xtask i18n-check`：en-US 100%、zh-CN ≥ 98%、占位符集合一致；以 `pseudo` 渲染全部快照 | 不一致即 fail；0 文本截断 |
| 主题对比度 | 脚本对 5 套主题逐 token 计算 WCAG 对比度：正文 ≥ 4.5:1、UI 元素 ≥ 3:1、`high-contrast` ≥ 7:1 | 低于阈值 fail（附 token 名与实测值） |
| 主题加载 | 恶意/破损 TOML 语料 30 份（缺 token、循环引用、非法色、超长）→ 断言全部降级且不 panic | 必须全过 |
| 字体度量 | 断言 `cjk_advance / advance == 2.0 ± 0.001`（内置字体 + 3 个 DPI + 缩放 0.6/1.0/3.0） | 否则 fail |
| 性能回归 | §5 全部项，`criterion` + CI 基线 | 退化 > 20% 红灯 |
| 无障碍 | accesskit 语义树快照（结构树节点、可访问镜像行文本）；Tab 路径遍历断言每个面板可达 | 语义树快照比对 |

---

## 10. 开放问题

1. **`Center` 之外的布局骨架是否够**：当前是"左-中-右 + 底部"。ImHex 支持任意靠边停靠，是否需要四边停靠？倾向 v1 不做——固定三槽让布局代码与持久化 schema 简单一个数量级，实际需求（结构树在左、Inspector 在右）已被覆盖。
2. **主题 token 是否需要"密度/间距"层**：`padding`/`row_height`/`radius` 是否也进主题？倾向进（否则紧凑模式要改代码），但要等 egui `Spacing` 映射稳定。
3. ~~`Ctrl+G` 归属~~ **已裁决**：`Ctrl+G` = 跳转偏移、`Ctrl+Shift+G` = AI 猜结构；03/10 已回填（见 §4.4）。
4. **屏幕阅读器的验收标准**：egui/accesskit 对自绘网格无能为力，本文的"可访问镜像"是否足以通过 NVDA/VoiceOver 实测？需要一次真实用户测试才能定"可接受"，因此 v1 只承诺"键盘全可达 + 结构树/Inspector 可读"。
5. **语言包是否走内容库分发**：走则无需发版即可加语言，但引入"翻译质量与许可证"审核负担（同 15 §4.8）。倾向：内置 `zh-CN`/`en-US` 随包，其余走内容库。
6. **命令面板是否需要"参数化执行"**（如"跳转到 0x1234"直接在面板里输参数）：VSCode 的 `>` 命令无参数，但编辑器里这很有用。倾向 v2。
7. **`MetricsCache` 与 epaint 图集的耦合**：egui 0.2x 的图集重建接口是否够用（能否只失效指定字体）需一次 spike 验证；若不够，可能要 fork `epaint` 的 atlas 逻辑，成本较高。
8. **UI 快照测试的稳定性**：字体栅格化在不同 egui 版本间会有亚像素差异，0.1% 阈值是否够？需在 CI 上跑 20 次观察噪声分布再定阈值。
