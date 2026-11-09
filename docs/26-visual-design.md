# 26 — 视觉设计（栅格 / 字阶 / 网格几何 / 组件态 / 图标 / 主题表）

> 本文是 `geode-ui` 的**视觉常数与绘制规格**：栅格刻度、字阶、hex 网格像素几何、游标与选区外观、组件四态、图标集、右键菜单结构、空/加载/错误/只读外观、四套主题完整色值、DPI 重建、无障碍镜像、像素回归基线。
> **算法归 05**（乘法几何、高亮合成、否决 alpha 叠加、虚拟滚动，见 `05 §4.3/§4.7`）；**token 类型归 16**（`Theme`/`DomainTokens`/`FontSet`，见 `16 §3.4/§3.6`）。本文只做三件事：**给取值**、**给绘制规格**、**给判据**。冲突时以 `00` 为准（§4 类型、§5.3 Layer 10 变体、§7 预算），其次 05（几何算法），再次 16（token 类型）。
## 0 决策速览

| # | 决策 | 理由（否决了什么） |
|---|---|---|
| D1 | 间距/尺寸以 **4dp** 为基数（`UNIT=4.0`），字阶受控为 **11/13/15/18/24** | 否决"每个面板自定字号与留白"：基线会随面板作者变化，45 张外壳快照随即失去意义 |
| D2 | hex 单元格**永远乘法**：`x(n)=hex_x0+(n/g)*group_pitch+(n%g)*byte_w`；三列共用同一个 `FontMetrics` 与基线 | 否决 `x += w`（960 次累加会把量化误差累积成斜列）与"两套字体再补偿基线"（`05 §4.3` 要求整帧 1 张图集，同族同 size 时基线天然重合） |
| D3 | **否决奇数行斑马纹** | `Value` 层（`00 §5.3`）本身是"u8/u16… 的交替底色"，斑马纹抢同一通道；行边界改由可选 `separator=Grid` 1px 行线表达 |
| D4 | 组件四态**只用 semantic token**，本文不新增色值；`hex.*`/`layer.*` 专属色才进 domain | 否决"顺手加色号"：`16 §7` 的 `xtask lint` 已 grep `Color32::from_rgb`，本决策让它永远 0 命中 |
| D5 | 面板标题 **15dp Regular + 留白分级**，不引入 Bold | 否决 Bold：`16 §4.7` 字体预算 4MB、现占约 3.6MB，加一个 Inter SemiBold 子集 ≈ +220KB，收益只是"标题更粗" |
| D6 | 图标 **48 个矢量 path**（16×16、1.5dp 描边、≤8 段），禁图标字体与 emoji | 否决图标字体：多一个字体文件、多一次图集争用，缺字时无法降级；emoji 跨平台形状不同，快照必挂 |
| D7 | 主题 **4 套完整 hex 表**；dark 与 `16 §4.5` **逐字一致不得改动**，仅补缺失的 `layer.analysis` | 否决"重排一套更漂亮的深色"：16 的深色已被 45 张基线锁定，改一个色号就是全量返工 |
| D8 | 像素回归两层记账：**网格 12 张**（§9.1，`ppp` 固定 1.0，锁几何）+ **外壳 45 张**（`16 §9`，锁主题×DPI×布局） | 否决"一套快照管全部"：几何判据要求 ppp 固定才能逐像素比，主题判据要求 ppp 变化才能查出缩放问题，前提相反 |
## 1 目标与非目标

| # | 目标 | 可验证判据 |
|---|---|---|
| G1 | 视觉常数**单一来源** | 视觉模块外 grep 到间距/字号字面量即 fail |
| G2 | 任何 `ppp` 与 `bpr` 下列基线不漂移 | 12 张快照逐像素一致；`ppp∈{1,1.5,2,3}` 下相邻列 x 误差 ≤0.5 设备像素 |
| G3 | 主题是数据：每对前景/底色都过阈值 | 正文 ≥4.5:1、大字(≥18dp) ≥3:1、控件边界 ≥3:1、high-contrast ≥7:1（`16 §9`） |
| G4 | 语义**不依赖色相单通道** | 每个 `layer.*` 至少一条装饰通道（`05 §4.7` 步骤 4）；deuteranopia 下 10 层仍两两可辨 |
| G5 | 图标不依赖任何字体文件 | 产物内 grep 无图标字体；假字体测试下图标完整 |
| G6 | 四类状态都有画面与出口 | 每种状态有快照 + `25 §4.3` 对应的可用 `CommandId` |

**非目标（明确否决）**：① **不做动画系统**——除 `16 §4.5` 的 120ms 主题交叉淡入与 530ms 游标闪烁外无过渡，hover/pressed 立即生效；② **不做位图图标**——全矢量随主题着色，避免 @2x/@3x 与高 DPI 模糊；③ **不做主题编辑器**——手写 TOML + 热重载（`16 §4.5`）；④ **不定义网格算法**——偏移↔行映射、虚拟滚动、高亮合成、选区裁剪全是 05 职责，本文只供常数；⑤ **不新增控件类型**——用 egui 原生 widget 的 `Visuals` 映射（`16 §4.5`）。
## 2 依赖与边界

```text
26（本文：取值与判据）
  ├─ 00 §4/§5.3/§7 …… ByteRange / Layer 10 变体顺序 / 全局预算
  ├─ 05 §3.2/§3.3/§4.3/§4.5/§4.7/§6 …… Layout 字段、合成规则、装饰像素、降级表
  ├─ 16 §3.1/§3.4/§3.6/§4.5/§4.7/§4.8/§8 …… 布局像素高、token 类型、FontSet、主题、字体、无障碍
  ├─ 25 §4.2/§4.3/§4.4/§5 …… 线框、状态机、三段式话术、术语（游标/选区/补丁）
  └─ 被消费：geode-ui（绘制）、18（lint 与快照门禁）、21（基准口径）
```

| 边界 | 归属 | 本文态度 |
|---|---|---|
| `Layout`/`cell_w` 的**算法** | `05 §3.2/§4.3` | 复用其字段，只补两个派生量（`intra_gap`、`baseline_dy`），不改语义 |
| 高亮合成、对比度修正、装饰通道 | `05 §4.7` + `ADR-0009` | 一字不改；本文只规定装饰的像素尺寸与层的色值 |
| `Theme`/`FontMetrics` 类型 | `16 §3.4/§3.6` | 不加类型、不加字段；只补取值（`layer.analysis`） |
| 命令与快捷键 | `16 §3.2/§4.4` | 右键菜单只列 `CommandId`，未登记者标 `(待)` |
| 文案与状态语义 | `25 §4.3~§4.4` | 不写一句面向用户的字符串，只写"放哪、多大、什么色" |

**硬边界（CI 强制）**：① 视觉模块外禁 `Color32::from_rgb`、字号与间距字面量；② 禁新建 `geode-visual-*` 之类 crate（视觉常数是 `geode-ui` 内部模块，`00 §2`）；③ hex 布局路径内禁 `x += ` 与裸 `f32` 偏移（`05 §7.3`）；④ 不新增诊断段，只用 `E0120`（主题 token 不完整/不可用）与 `E0130`（字体非等宽），语义同 `16 §6`。
## 3 数据结构与接口

### 3.1 栅格、字阶与网格几何

```rust
pub const UNIT: f32 = 4.0;  // 4dp 栅格基数；视觉模块内所有间距/尺寸必须是它的整数倍

/// 受控字阶。调用点禁止写字面量字号；新增级别必须先加变体并在 §4.2 补用途行。
pub enum TypeStep { Caption11, Body13, Title15, Heading18, Display24 }
impl TypeStep {
    pub fn dp(self) -> f32;                        // 11/13/15/18/24（dp，再乘全局 zoom）
    pub fn line_h(self, fm: &FontMetrics) -> f32; // ceil_pp(max(fm.row_height, 1.35*dp))，见 16 §4.7
}

/// hex 网格几何：05 §3.2 `Layout` 的超集（`off_w`/`hex_w`/`ascii_w` 沿用同名派生字段），
/// 全部由 Layout + FontMetrics 纯函数导出，每帧重算。
pub struct GridMetrics {
    pub cell_w: f32,      // 单字符 advance = max(advance("0","F","W","-"))，量化到 1/ppp 整数倍
    pub byte_w: f32,      // = 2*cell_w + nibble_gap（05 §3.2）
    pub nibble_gap: f32,  // 半字节间隙，默认 0.0；>0 时 high/low 分色
    pub intra_gap: f32,   // 【本文新增】组内字节间隙，恒 0.0，理由见 §4.3
    pub group_bytes: u8,  // = HexViewConfig.grouping，取 1/2/4/8
    pub group_gap: f32,   // 组间间隙，默认 6.0；grouping=1 时它即字节间隙（05 §3.3）
    pub group_pitch: f32, // 一组占宽 = group_bytes*(byte_w+intra_gap) + group_gap（05 同名）
    pub hex_x0: f32,      // hex 列左缘 = UNIT*2 + off_w + UNIT*2
    pub ascii_x0: f32,    // ASCII 列左缘 = hex_x0 + hex_w + UNIT*4
    pub baseline_dy: f32, // 行内基线 = row_pad + fm.ascent；hex/offset/ascii 三列共用
}
```

### 3.2 游标、选区与四态

```rust
/// 游标外观（05 §4.5 的视觉侧；写 Layer::Cursor 的仍是 05，本 crate 只读）
pub struct CaretPaint {
    pub block_w: f32,    // 覆盖模式：一个 nibble 宽 = cell_w（ASCII 列 = ascii_cell_w）
    pub hollow_w: f32,   // 插入模式：空心框描边宽 = 2.0dp（05 §4.5"2px 空心框占半格"）
    pub blink_ms: u32,   // 530（05 §8）；占空比 50%（亮 265 / 灭 265）
    pub idle_alpha: f32, // 失焦态 0.35（不写 Cursor 层，改画 1dp fg.secondary 竖线）
    pub buddy_dim: f32,  // 同字节另一 nibble 的前景亮度 ×0.65（即降 35%，05 §4.5）
}

/// 组件四态绘制指令。颜色只能是 token 引用，不得就地写色（16 §7）。
pub struct StatePaint {
    pub fill: Tok,                  // 底色
    pub fg: Tok,                    // 文字与图标色
    pub stroke: Option<(Tok, f32)>, // 边框：token + 宽度（dp）
    pub alpha: f32,                 // 仅对 fg 与图标生效（§4.5 否决容器整体降 alpha）
    pub dy: f32,                    // pressed 态整体下移，默认 1.0dp
}
pub enum Tok { Semantic(SemanticId), Domain(DomainId), Transparent } // token 引用
```

### 3.3 主题取值（本文对 `16 §3.4` 的唯一增量）

**不加类型、不加字段**；`DomainTokens.layer` 的 10 个槽位顺序固定为 `00 §5.3` 声明序：
`0 Value · 1 Search · 2 Structure · 3 Analysis · 4 Selection · 5 Bookmark · 6 Diff · 7 Yara · 8 Ai · 9 Cursor`。
`16 §4.5` 的 `[layer]` 只写了 9 项（缺 `analysis`），本文在 §4.9 补齐第 10 项；`layer_bg` 全 `None`（10 层只用前景/底纹/装饰通道）。`IconId` 是稳定标识，Command 只引用 id 而非字符。
## 4 核心算法流程

### 4.1 栅格与间距刻度

| 刻度 | 用途 | 出现处 |
|---|---|---|
| **2** | 分隔线两侧余量、菜单栏垂直余量（28 = 24 + 2×2）、焦点环内间隙 | 菜单栏、面板分隔线 |
| **4** | 控件纵向内边距、图标有效绘区留白、字形与边框间距、列表行纵内边距 | 按钮、复选、树行 |
| **8** | 控件横向内边距、图标与文字间距、状态栏段间距、**三栏 gutter** | 工具栏、状态栏、面板之间 |
| **12** | 面板内边距、列表行水平内边距、区块内块间距、横幅内边距 | 左/右栏、Inspector、搜索结果 |
| **16** | 区块之间、**hex 列与 ASCII 列之间**、空状态元素之间 | hex 视图、空态 |
| **24** | 控件标准高度、图标按钮边长、最小指针目标、状态栏高、面板标题行高 | 全 UI |
| **32** | 分区之间、空状态元素块间距、首次运行步骤之间 | 空态、引导、设置页 |

**装饰像素例外**：`1px`（Search 下划线）、`2px`（Structure 行底色带）、`3px`（Yara 左带、Diff 左条）是 `05 §4.7` 规定的**固定像素通道**，不受 4 刻度约束——它们表达"这是哪一层"，必须恒定才能做通道隔离。

**内外边距规则**：① **外边距**由宿主面板的 12dp padding 提供，视图自身不再设外边距（避免双重留白，也让快照可预测）；② 相邻面板**不用双 padding**，只留 8dp gutter，1dp 分隔线画在正中（命中区 6dp，`16 §4.1`）；③ 嵌套容器深度 1 用 16、深度 ≥2 降到 8，padding 不叠加；④ 面板底边距 8，不与 24dp 状态栏顶死。

**三栏 gutter**：左栏右缘 →8dp→ 1dp 线 →8dp→ 中栏；（中栏|右栏）同构。中栏内部：8dp → 偏移列 → 8dp → hex 列 → 16dp → ASCII 列 → 8dp。这些数字都是 `UNIT` 整数倍，且任何一处之和必须等于相邻两列的 x 差（§5 有不变量断言）。

**像素高对接**：菜单 28 = 24+2×2、工具栏 36 = 6+24+6、状态栏 24、左栏 260、右栏 320（`16 §3.1`）都可被 `UNIT` 整除或由 2dp 补足，故改缩放只需换 `UNIT` 乘数，不动结构。

### 4.2 字阶与行高

| 级 | dp | 用途（出现处） | `line_h` |
|---|---|---|---|
| `Caption11` | 11 | hex 网格 `Compact` 预设（`05 §8`）、状态栏次段、图层条、tooltip 明细、快捷键提示 | 15 |
| `Body13` | **13（基准）** | 默认 hex 网格、面板正文、列表项、菜单、按钮、右键菜单 | 18 |
| `Title15` | 15 | **面板标题**、标签页标题、hex 网格 `Wide` 预设 | 20 |
| `Heading18` | 18 | 横幅首句、空状态子标题、`LowVision` 预设、设置页分区标题 | 24 |
| `Display24` | 24 | 首次运行欢迎标题、无文档空状态主标题 | 32 |

**进 hex 网格的只有 11/13/15/18**——正好对应 `05 §8` 的 `Compact`/`Default`/`Wide`/`LowVision` 四预设；**24 永不进网格**（行高超过可点击性所需，且 960 cell 的照排成本随字号超线性上升）。**面板标题只用 15**，永不进网格、不用于正文。

```text
line_h(step) = ceil_to_device( max(fm.row_height, 1.35 * step.dp * zoom) )   // UI/文本行（16 §4.7）
row_h(hex)   = ceil_to_device( fm.ascent + fm.descent ) + 2 * row_pad       // hex 网格（05 §3.3）
cell_w       = ceil_to_device( max_advance )  ；  cjk_advance 必须 = 2 × advance
```

- 行高与字号**不受 4 刻度约束**，只受"整设备像素"约束：`ceil_to_device(v) = ceil(v*ppp)/ppp`。行高不取整会让字形底边落在半像素上，整屏发虚。
- 全局 `zoom ∈ 0.6~3.0`（`16 §8`）作用在 `step.dp`；`05 §4.2` 的网格内 `Ctrl+滚轮` 只改网格 `font.size`（8..=28），两者**相乘后 clamp 到 8..=28**，缩放时保持"游标字节的屏幕相对位置"。
- `FontMetrics` 必须实测（`16 §3.6` 禁 `advance = size*0.6`）；本文只用它的四个字段，不做再推导。

### 4.3 hex 网格几何

```text
group(n) = n / group_bytes     slot(n) = n % group_bytes
x(n)     = hex_x0 + group(n)*group_pitch + slot(n)*byte_w     // 两级乘法，非累加
nib_x(n,k) = x(n) + k*cell_w  （k∈{0=High,1=Low}）    x_ascii(c) = ascii_x0 + c*cell_w
```

- **组间距（每 8 字节的空隙）**：`grouping=8` 时 `group_pitch = 8*byte_w + 6`，即每 8 字节一个 6dp 空隙；`grouping=1`（05 默认）时 `group_pitch = byte_w + 6`，即每字节间隙 6dp。**组内间隙 `intra_gap` 恒 0**（本文新增量）：组内靠两字形紧排自成视觉单元，再加内间隙就数不出"这是一组 8 字节"。
- **三列宽度**：`off_w = max(6, ceil(log16(len)))*cell_w + 2*UNIT`（最小 6 位防抖动，`05 §3.3`；100 GiB 时 11 位 `0x18_6A0_0000`，宽度只跳变一次）；`hex_w = ceil(bpr/group_bytes)*group_pitch - group_gap`（**尾部不留缝**，使 ASCII 列起点只依赖 `bpr`/`grouping`，与行末空位无关）；`ascii_w = ascii_glyphs*cell_w`（UTF-16 时 `ascii_glyphs = bpr/2`）。
- **对齐基准线**：三列共用 `baseline_dy = row_pad + fm.ascent`。hex 列与偏移列走图集 quad（`y = row_y + baseline_dy - glyph_ascent`），ASCII 列走 galley（`y = row_y + row_pad`，同族同 size 时 galley 顶即字形 ascent 顶）。前提是两列同族同 size（`05 §4.3` 硬要求整帧 1 张图集），故不需基线补偿；一旦给 ASCII 配了别的 family，会先看到整列错位——这就是 05 禁 fallback 的原因。
- **单元格内边距**：横向 **0**（advance 即格宽，任何内边距都会破坏 `x=n*cell_w`），纵向 `row_pad = 2.0`。字节命中区 = `byte_w × row_h`，恒 ≥ `a11y.min_target_px`。
- **决策：否决奇数行斑马纹。** ① `Value` 层按 `00 §5.3` 就是"u8/u16… 的交替底色"，与斑马纹抢同一通道，叠加后分不清"这是第 2 行"还是"这两个字节是 u16"；② 斑马纹亮度差须 ≤4% 才不干扰高亮，而该差值在 high-contrast 下直接失效，等于该主题没有斑马纹；③ 行边界有更便宜的通道：`separator=Grid` 的 1px 行线（+960 条线在预算内）。分组底纹只画组边界，不画奇数行。

### 4.4 游标与选区视觉

| 种类 | 几何 | 颜色/通道 | 备注 |
|---|---|---|---|
| **插入游标（ASCII / `Nibble::Whole`）** | 竖线 `2dp` 宽 × `fm.row_height` 高，x = 字节左缘 + `cell_w/2` | `fg.primary`；失焦 `fg.secondary`@0.35 | 闪烁 530ms、50% 占空比；`reduce_motion`→常亮 |
| **插入游标（hex，覆盖模式）** | 实心块 `cell_w × fm.row_height`，无圆角，垂直居中 | 底 `layer.cursor`，前景取该底的对比色（对比度校验最后一步以 Cursor 为准，`05 §7.3`） | 光标是本行唯一"不透明最高层" |
| **插入模式空心框** | `2dp` 描边空心矩形，占半格：`cell_w/2+1` 宽 | 描边 `layer.cursor`，内部透明 | 与覆盖模式实心块区分（`05 §4.5`） |
| **半字节 nibble 高亮** | 当前 nibble 矩形 `cell_w × fm.row_height`（`nib_x(n,k)` 起算）；另一 nibble 留在原位 | 当前 nibble 底 = `layer.cursor`；**另一 nibble 前景亮度 ×0.65**（降 35%） | 双通道：位置（左右半格）+ 亮度，色盲可用 |
| **字节选区** | 每行相交区间：起 `x(n)`、宽盖 `byte_w`（含 `nibble_gap`）；跨行首行裁左、末行裁右 | `Layer::Selection` 底，opacity 0.85（`05 §3.3`）；前景不动 | 相邻行矩形**纵向无缝相接**（用 `row_h` 而非字形高），否则每行留 2×`row_pad` 白缝，成米粒状 |
| **块选区（矩形）** | `cols(c0,c1)` → `[x(c0), x(c1)+byte_w)`；y = 可见行 ∩ 选区行范围，逐行无缝 | 同 Selection 底 **+ 1dp `border.normal` 勾边** | 勾边是它与线性选区的唯一区分通道（不靠颜色）；`Insert` 模式降级线性（`05 Q3`） |
| **非连续选区（搜索结果）** | 每段独立矩形（同"字节选区"），不勾边；段间空隙 < `cell_w` 时合并为一个 rect | 搜索结果本身是 `Layer::Search` 的 Ann；"当前命中"额外 1dp 下划线 | 渲染层不认识"命中"概念（`05 §7.3`）；每帧 rect 数受可见行钳制 |

**失焦态**：窗口失活时**不写** `Layer::Cursor`（`05 §7.2`：非活动视图不写该层），改画 1dp `fg.secondary`@0.35 细竖线——"失焦"与"活动但闪灭"因此可区分（粗细 + 颜色双通道）。**否决**：① 圆角游标块（半像素 + 每帧多 4 段 path，13dp 下还吃掉笔画）；② 光标发光/阴影（无动画系统，且与 high-contrast"关阴影"冲突）；③ 选区完全不透明（会盖掉 Yara/Diff 底纹，违反 `05 §4.7` 层级定色）。

### 4.5 组件四态与焦点环

| 态 | `StatePaint` | 取值（dark-default） | 附加规则 |
|---|---|---|---|
| Normal | `fill=bg.raised`、`fg=fg.primary`、`stroke=(border.normal,1)` | `#14181d`/`#e6ebf2`/`#2a323b` | 无阴影、无渐变 |
| Hover | `fill=bg.hover`、`fg=fg.primary` | `#1c2229` | 立即生效（0 过渡）；指针停留不改尺寸 |
| Pressed | `fill=bg.active`、`fg=fg.primary`、`dy=1` | `#2a323b` | 用 1dp 下移表达按下，**不缩放**（缩放会重新栅格化、发虚） |
| Disabled | `fill=bg.raised`、`fg=fg.disabled`、`alpha=0.45`（图标 0.35） | `#2a323b`@0.45 | hover 无反馈；tooltip 常驻显示禁用原因（`16 §6`，**不隐藏**） |
| Focus | `stroke=(border.focus,2)` + 1dp 间隙外扩 | `#60a5fa` | 见下 |

- **焦点环规格**：`2dp` 实线，贴在控件外缘外扩 `1dp`（不进内边距，避免挤压文字）；圆角 = 控件圆角（本文唯一允许的圆角 = `UNIT/2 = 2dp`，仅按钮与输入框）。high-contrast 改**双环**：外 2dp `#ffffff` + 内 1dp `#000000`，保证任何底色上都有 ≥1 条达 7:1 的边。优先级：焦点 > hover > selection，四态可叠加（focus+hover = 焦点环 + hover 底）。
- **禁用态透明度（决策）**：只对 `fg` 与图标施 alpha（0.45/0.35），**底色保持不透明**。否决"整个控件 alpha 0.4"：底色一起褪会让边界消失，用户看不出"这里有个按钮只是不能用"；且嵌套容器会叠加两次 alpha，深浅不可预测。
- **色的来源**：四态全走 semantic（`fg.*`/`bg.*`/`border.*`/`accent`/`selection`），`16 §4.5` 已有 `bg.hover`/`bg.active`/`border.focus`/`fg.disabled`，**本文不新造 semantic 名**；到 `egui::Visuals` 的映射在 16，本文不重复。**自绘网格没有"控件四态"**：单元格 hover 只做 1dp `border.normal` 内描边（不填色，否则抢 Value 层通道），pressed 由选区表达，disabled 不存在（只读见 §4.8）。

### 4.6 图标集（48 个，三组）

规格：**16×16 用户坐标网格**，有效绘区 12×12（四周留 2dp），描边 **1.5dp**、端点与拐角 `round`，坐标对齐 0.5（`ppp=1` 时落在像素中心）；填充只允许 ≤4dp² 小面积；着色 `fg.secondary`（hover `fg.primary`，disabled@0.35）；命中区 ≥24dp。**禁图标字体、禁 emoji、禁位图。**

- **组 A 文件操作（16）**：`ic.open` 托盘+右上出箭头 · `ic.new` 页框+`+` · `ic.save` 托盘+下箭头 · `ic.save_as` 托盘+短笔 · `ic.close` 对角叉 · `ic.undo` 左弧+尾 · `ic.redo` 水平镜像 · `ic.revert` 3/4 弧+逆时针箭头 · `ic.export` 页框+出箭头 · `ic.import` 页框+入箭头 · `ic.recent` 圆+两指针 · `ic.copy` 双错位 Rect · `ic.paste` 剪贴板+小舌 · `ic.cut` 剪刀（双圆+交叉线） · `ic.find` 圆+柄 · `ic.replace` 放大镜+`→`
- **组 B 视图控制（16）**：`ic.hex_grid` 4 个 2×2 Rect · `ic.ascii_col` 3 横+右竖 · `ic.panel_left` 外框+左 1/3 实心 · `ic.panel_right` 镜像 · `ic.split_h` 外框+中竖 · `ic.split_v` 外框+中横 · `ic.zoom_in` 放大镜+`+` · `ic.zoom_out` 放大镜+`-` · `ic.zoom_reset` Rect+两侧竖 · `ic.sun` 圆+8 射线 · `ic.moon` 双弧月牙 · `ic.layers` 3 个错位平行四边形 · `ic.grid_lines` 2 竖 2 横 · `ic.bracket` 左括号+缩进横 · `ic.pin` 圆头+针 · `ic.fullscreen` 4 外角折线
- **组 C 状态指示（16）**：`ic.error` 圆+`✕` · `ic.warning` 三角+`!` · `ic.info` 圆+`i` · `ic.ok` 对勾 · `ic.running` 3/4 弧（缺口右上） · `ic.stopped` 圆+横线 · `ic.readonly` 锁（Rect+上半环） · `ic.volatile` 闪电折线 · `ic.hole` 方框+2 条 45° 线 · `ic.unreadable` 方框+`!` · `ic.dirty` 实心小圆 · `ic.conflict` 两个相反箭头 · `ic.ai_hit` 菱形+中心点 · `ic.yara_hit` 盾形+中竖 · `ic.search_hit` 放大镜+实心点 · `ic.bookmark_hit` 书签（下方缺口）
- 每个图标 ≤8 段。**为何 48 而非 64**：留 16 个余量给 v1.1 插件面板（`14` 的声明式控件树只允许引用既有 `IconId`，新增须回本表）；`IconId` 发布后不改义（同 `CommandId`）。**severity 徽标不是图标**：`✕ ! i`（`25 §6.2`）是**文本前缀**，随字号缩放且必可读，图标只作视觉加强——否决"只用彩色圆点区分 severity"（色盲 + 小字号下无法区分）。

### 4.7 右键菜单结构（只列 `CommandId`）

`(待)` = 尚未在 `16 §3.2` 登记、需回填注册表；未标者为 `16 §4.4` 已有命令。本文只引用，**不定义** `title_key`/快捷键/`enabled`。

```text
【hex 视图】命中判定 = 该偏移最深 Ann → 最深结构节点（05 §4.8）
├ 复制/粘贴  edit.copy · edit.copy_hex(待) · edit.copy_ascii(待) · edit.copy_c_array(待) · edit.paste · edit.paste_hex(待)
├ 选区  edit.select_all · structure.select_unit(待) · view.select_block(待) · edit.copy_offset(待)
├ 跳转  nav.goto · nav.goto_selection_end(待) · nav.back · nav.forward
├ 结构  structure.reparse_from_here(待) · structure.reveal_in_tree(待) · bspec.edit_node_type(待)
├ 标注  annot.bookmark_add(待) · annot.bookmark_remove(待) · annot.note_edit(待)
├ 视图  view.toggle_ascii(待) · view.set_bpr.{1,2,4,8,16,32,64}(待) · view.set_grouping.{1,2,4,8}(待) · view.layers(待) · view.toggle_grid(待)
└ 工具  tools.diff · tools.disassemble_at(待) · tools.viz_show(待) · yara.scan_range(待)

【结构树】命中判定 = 节点行（真实 widget，非自绘，16 §4.8 可读）
├ 跳转  nav.goto_node(待) · structure.select_bytes(待) · structure.reveal_in_hex(待)
├ 复制  edit.copy_path(待) · edit.copy_value(待)
├ 展开/固定  structure.expand_all(待) · structure.collapse_all(待) · structure.pin(待)
├ 模板  bspec.edit_node_type(待) · structure.reparse_from_here(待) · bspec.lint.run
└ 工具  tools.viz_show(待) · structure.explain_low_confidence(待)

【搜索结果】命中判定 = 结果行（真实 widget）
├ 跳转  search.goto_hit(待) · search.next · search.prev
├ 范围  search.scope_from_hit(待) · search.scope_selection(待) · search.clear_highlights(待)
├ 命令  search.stop(待) · search.rerun(待，对应 25 §4.2 线框 3 的「重新搜索」) · search.export(待)
```

- 菜单项**顺序 = 分组顺序**；分组间画 1dp `border.separator`。`16 §3.2` 的 `category` 只决定顶层菜单，不决定右键分组（分组由本节固定）。所有项走 `enabled()`（灰显 + 悬停原因），**不隐藏**（`16 §4.3`）；`requires_confirm` 同样生效。
- 组内**最多 8 项**，超出折叠进"更多…"：hex 视图右键使用频率最高，长度直接决定操作成本。

### 4.8 状态视觉（空 / 加载 / 错误 / 只读）

| 状态 | 触发 | 画面 | 出口 |
|---|---|---|---|
| **空态（0 字节文件）** | `Ready` 且 `len()==0`（**不是** `DocState::Empty`，那是"未打开文档"，`25 §4.2` 线框 8） | 网格区居中三行：24dp 标题「文件为空（0 字节）」+ 13dp 说明「可在末尾插入数据」+ 24dp 高按钮组；偏移列仍画 `00000000` 一行，首格是可点击插入点（`Insert` 可写，`insert(at=0)`）；ASCII 列空白。**不画斜纹、不画 `??`** | `edit.insert_byte`(待) · `file.save_as` |
| **加载态（块未就绪）** | `BlockState::Loading` 无 ghost | 45° 斜纹，周期 8dp、线宽 2dp，底色 `bg.sunken`、纹色 `fg.secondary`@8%；有 ghost 时叠 `alpha=0.25` 的旧内容（`05 §4.4`）；行末 8×8 角标 `ic.running` | 无（自动重试，`05 §6 #1`） |
| **错误态（不可读区间）** | ① `BlockState::Failed`（读失败）② `Holes`（源声明空洞） | **两种纹理必须不同**：`Failed` = 平涂 40% 灰 + 行左缘 3dp `severity.error` 竖条 + 角标 `ic.unreadable`；`Holes` = 45°与 135° **交叉网纹**（`severity.error`@20%）+ 角标 `!` | `file.retry` · `export_readable_part`(待) |
| **只读态（`ReadOnly` 降级）** | `25 §4.3` 的 `ReadOnly`（权限/占用/设备忙/长度未知） | ① 状态栏左缘 8dp 宽 `severity.warning` 竖条 + `ic.readonly` + 文案「只读 · 无法写入（权限不足）」（全文见 `25 §4.3`）；② `file.save` 进 disabled 四态 + 悬停原因；③ hex 网格**不降灰**（数据仍有效），仅编辑相关控件降级；④ 编辑键首次触发 → 120ms `severity.error` 边框脉冲 + 一次性横幅（`25 §6.1`） | `file.save_as` · `file.reopen_readwrite`(待) |

**只读否决"整体降饱和"**：内容也灰掉会让用户以为"数据不可信"（`25 §4.3`：只读 ≠ 错误），真正受限的只有写入路径。**三种纹理的区分通道**：加载 = 单斜纹（中性色）；空洞 = 交叉网纹（error 色）；失败 = 平涂 + 左竖条 + 图标——形状/颜色/位置各占一个通道，色盲与灰度打印下都可分。

### 4.9 主题完整表（4 套）

**① palette（基础命名色）**——`dark` 列与 `16 §4.5` 逐字一致；未列入表中的 domain 值（`#facc15`/`#f472b6`/`#a3e635`/`#c084fc` 等）按 16 的做法直写 hex，不改任何既有取值。

| 名 | dark-default | light-default | high-contrast | dark-deuteranopia |
|---|---|---|---|---|
| `gray-0` | `#0b0d10` | `#ffffff` | `#000000` | `#0b0d10` |
| `gray-1` | `#14181d` | `#f7f8fa` | `#000000` | `#14181d` |
| `gray-2` | `#1c2229` | `#eef1f4` | `#1a1a1a` | `#1c2229` |
| `gray-3` | `#2a323b` | `#d7dce2` | `#ffffff` | `#2a323b` |
| `gray-6` | `#9aa4b1` | `#6b7280` | `#ffffff` | `#9aa4b1` |
| `gray-9` | `#e6ebf2` | `#111418` | `#ffffff` | `#f2f2f2` |
| `blue-4` | `#3b82f6` | `#2563eb` | `#66ccff` | `#0072b2` |
| `blue-6` | `#60a5fa` | `#1d4ed8` | `#99ddff` | `#56b4e9` |
| `cyan-5` | `#22d3ee` | `#0e7490` | `#00ffff` | `#56b4e9` |
| `violet-5` | `#a78bfa` | `#7c3aed` | `#cc99ff` | `#cc79a7` |
| `red-5` | `#ef4444` | `#dc2626` | `#ff8080` | `#d55e00` |
| `amber-5` | `#f59e0b` | `#b45309` | `#ffcc66` | `#e69f00` |
| `green-5` | `#22c55e` | `#15803d` | `#99ff99` | `#009e73` |

**② semantic（只引用 palette 名或直写 hex，`16 §4.5` 硬约定）**

| token | dark-default | light-default | high-contrast | dark-deuteranopia |
|---|---|---|---|---|
| `fg.primary` / `fg.secondary` | `gray-9` / `gray-6` | `gray-9` / `gray-6` | `gray-9` / `gray-6` | `gray-9` / `gray-6` |
| `fg.disabled` | `gray-3` | `#9aa4b1` | `gray-6` | `gray-3` |
| `fg.on-accent` / `selection-text` | `gray-0` | `gray-0` | `gray-0` | `gray-0` |
| `bg.base` / `bg.sunken` | `gray-0` / `gray-2` | `gray-0` / `gray-2` | `gray-0` / `gray-2` | `gray-0` / `gray-2` |
| `bg.raised` | `gray-1` | `gray-1` | `gray-0` | `gray-1` |
| `bg.hover` / `bg.active` | `gray-2` / `gray-3` | `gray-2` / `gray-3` | `gray-3` / `gray-3` | `gray-2` / `gray-3` |
| `border.normal` / `border.separator` | `gray-3` / `gray-2` | `gray-3` / `gray-2` | `gray-3` / `gray-6` | `gray-3` / `gray-2` |
| `border.focus` | `blue-6` | `blue-6` | `gray-9` | `blue-6` |
| `accent` / `selection` | `blue-4` | `blue-4` | `blue-4` | `blue-4` |

**③ domain（`hex.*` / `layer.*` / `severity.*`）**——`layer` 10 行顺序 = `00 §5.3` 声明序，缺一即 `E0120`。

| token | dark-default | light-default | high-contrast | dark-deuteranopia |
|---|---|---|---|---|
| `hex.offset` | `gray-6` | `gray-6` | `gray-9` | `gray-6` |
| `hex.ascii` / `hex.ascii-nonprint` | `gray-9` / `#6b7684` | `gray-9` / `#9aa4b1` | `gray-9` / `#bcbc00` | `gray-9` / `#6b7684` |
| `hex.null-byte` | `#2a323b` | `#c7d0da` | `#8a8a00` | `#2a323b` |
| `hex.patch-insert-bg` / `-fg` | `#12351f` / `green-5` | `#d9f2e0` / `green-5` | `#004400` / `green-5` | `#12351f` / `green-5` |
| `hex.patch-delete-bg` / `-fg` | `#3a1416` / `red-5` | `#fbe0e2` / `red-5` | `#440000` / `red-5` | `#3a1416` / `red-5` |
| `layer.value` (0) | `gray-9` | `gray-9` | `gray-9` | `gray-9` |
| `layer.search` (1) | `#facc15` | `#eab308` | `#ffff00` | `#f0e442` |
| `layer.structure` (2) | `cyan-5` | `cyan-5` | `cyan-5` | `cyan-5` |
| `layer.analysis` (3) | `#a3e635` | `#4d7c0f` | `#00ff88` | `green-5` |
| `layer.selection` (4) | `blue-4` | `blue-4` | `blue-4` | `blue-4` |
| `layer.bookmark` (5) | `violet-5` | `violet-5` | `violet-5` | `#cc79a7` |
| `layer.diff` (6) | `#f472b6` | `#db2777` | `#ff99ff` | `amber-5` |
| `layer.yara` (7) | `red-5` | `red-5` | `red-5` | `red-5` |
| `layer.ai` (8) | `#c084fc` | `#9333ea` | `#cc99ff` | `#cc79a7` |
| `layer.cursor` (9) | `#ffffff` | `#0a0a0a` | `#ffffff` | `#ffffff` |
| `severity.error/warning/info` | `red-5`/`amber-5`/`blue-6` | `red-5`/`amber-5`/`blue-6` | `red-5`/`amber-5`/`blue-6` | `red-5`/`amber-5`/`blue-6` |

**暗淡档（dim）的下限——修正值，别再退回高亮色**：`hex.ascii-nonprint` 与 `hex.null-byte` 都不是普通文字色，它们的对比度必须**够暗以退到背景里、又够亮以数得清**。实测（`docs/img/mockup.html` 像素取样）：原表给 dark 的 `gray-2`（`#1c2229`）在 `#0b0d10` 上只有 **1.5:1**，全零区直接糊成黑块；`gray-3` 作 non-print 也是 1.5:1，点号看不见。故定档：**null-byte ≥1.5:1 且必须与普通字节一眼可分**（dark `#2a323b` = 1.5:1，靠"几乎不可见"表达"这是 0"），**non-print ≥3:1**（dark `#6b7684` = 4.2:1，点号要能数），**HC 下 dim 允许降到 ≥4.5:1**（其余 HC 文字仍 ≥7:1）。这一条与 `05 §4.7` 的"前景层强制 ≥4.5:1"不冲突：dim 档属于**装饰性前景**，不上报可读性错误。

- **deuteranopia = Okabe-Ito 8 色板**（`05 §3.3` 的 `colorblind_safe`）：10 个 layer 只有 8 个可用色相，故 `layer.ai` 与 `layer.bookmark` 同色、`layer.analysis` 与 severity 的绿同族，靠**装饰通道**（Ai = 斜纹、bookmark = 无底纹只画图钉）区分——这正是 `05 §4.7`"每层必带非色相冗余通道"的用途。
- **high-contrast** 目标 7:1：`bg.base` 纯黑、前景取纯色或近纯色；关渐变与阴影，边框强制 1px 实线，焦点环改双环（§4.5）。
- **热重载与主题包不变**：120ms 交叉淡入、`notify` 250ms 去抖、失败保留旧主题（`E0120`）。`solarized-dark`/`nord` 走 `.geopkg` 分发，**不进像素基线**（§9）。

### 4.10 DPI 与缩放

```text
ppp 变化（跨显示器迁移 / 系统缩放 / Ctrl+= 引起的 zoom）
① MetricsCache.generation += 1        // 16 §4.7：度量按 (size, ppp) 双键缓存
② 字形图集重建，按 ppb_bucket ∈ {1.0,1.5,2.0,3.0} 分桶，最多 4 份（05 §4.3、16 §5）
③ cell_w / row_h / 所有 UNIT 派生量重算（纯函数、无状态）
④ row_h 取整 ceil_to_device；若 fm.row_height + 2*row_pad > row_h，则 row_h += 1
⑤ 整帧重建一次（05 §4.3 的整帧重建条件含 DPI）
```

- **防半像素模糊**：几何在逻辑像素上算，最后一步才乘 `ppp` 并取整到设备像素；**禁止**在中间量上取整（会累积成 1px 列错位）。`cell_w` 量化到 `1/ppp` 整数倍。
- **图集重建节流** ≤1 次/秒（`16 §5`）：跨屏拖动时"最后一次 ppp 生效"，中间值只置重建标记，不重建图集。
- **行高与字形不同步**：`05 §6 #5` 已规定取整后裁切字形则 +1px；本文补判定——取整损失 >0.35dp 才 +1，否则不补（避免 1.5x 下每行白吃 1px 造成"稀疏"）。**zoom 与网格 `font.size` 的合成**：`effective_size = clamp(font_size * zoom, 8.0, 28.0)`，两者同改时 `row_h` 跟随重算，锚定原则见 `05 §4.2`。

### 4.11 无障碍

| 项 | 阈值/规格 | 来源 |
|---|---|---|
| 正文对比度 | ≥ **4.5:1** | `16 §9` |
| 大字（≥18dp；本文无 Bold 故不取"14dp 粗体"档） | ≥ **3:1** | WCAG AA 大字档 |
| 控件边界/图标/焦点环 | ≥ **3:1** | `25 §6.2` |
| high-contrast 主题 | ≥ **7:1** | `16 §4.8/§9` |
| 最小指针目标 | **24dp**；分隔条命中 6dp；hex 行高下限 18dp | `25 §6.2` |
| 色盲可辨 | 10 个 layer 两两之间至少一个非色相通道不同 | `05 §4.7` 步骤 4 |

**可访问镜像文本的组织**（自绘网格不进 accesskit 语义树，`16 §4.8`；播报粒度见 `25 §6.2`）：

```text
行镜像  「偏移 0x1A30：4D 5A 90 00 03 00 00 00，ASCII MZ..」
        顺序固定：偏移 → 本行已加载字节（≤ bpr，超 16 截断加「等 N 字节」）→ ASCII
区域镜像「区域：hex 视图，第 4127 行，共 60 行可见，游标在第 3 字节，选区 256 字节」
节点镜像「字段 name，偏移 0x1A3C，长度 4，值 3，置信 0.82」
```

- **只播三类位置**：游标行、选区首行、选区末行（`25 D6`），**不逐格播报**；节流 300ms（`25 §8` 的 `announce_throttle_ms`），"行未变 + 输入未停"不重播。**单条 ≤120 字符**（超出截断但保留偏移与长度字段）：中英混排 + 十六进制串的朗读时长近似线性，120 字符 ≈ 6 秒，再长就从"播报"变"念经"。
- token 名**不进播报**（听到"搜索命中"而非 `layer.search`）；severity 播文字而非图形（"警告"而非"黄色三角"）。`reduce_motion`：关主题交叉淡入、关游标闪烁（改常亮）、关面板展开过渡（`16 §4.8`）。
## 5 性能预算与基准

| 项 | 预算 | 说明/门禁 |
|---|---|---|
| `GridMetrics` 重算 + 三列几何 | p50 0.15ms / p99 0.30ms | 落在 `05 §5.1` 的"布局 0.45/1.0ms"内；纯函数、零分配 |
| token 查表 | **0 次** HashMap 命中 | `DomainTokens`/`SemanticTokens` 按 `&` 传入绘制（`16 §5`） |
| 图标绘制（≤48 个） | <0.12ms/帧 | ≤8 段/图标 ⇒ ≤384 线段；一次批量提交 |
| hover/pressed 变化 | <0.05ms | 只脏受影响控件，用 `05 §4.3` 的行 Shape 缓存 |
| 对比度校验（全 token 对） | <2ms，仅启动/主题变更/色盲开关 | 4 主题 × ≈40 对，数值随 `FrameReport` 上报 |
| 字形图集重建（ppp 变化） | <30ms（`05 §5.2`）/ <40ms（`16 §5`），≤1 次/秒 | 取更严者 |
| 12 张快照套件墙钟 | <90s | 含图集构建，CI 可拆 3 片并行 |
| 单张快照峰值内存 | <400MB | 1280×800 RGBA ≈ 4MB + 图集 4MB |

**不变量（CI 断言）**：① `hex_x0 + hex_w + UNIT*4 == ascii_x0`（列间距闭合）；② `off_w + UNIT*2 + hex_w + UNIT*4 + ascii_w + UNIT*2 == 视图内容宽度`（无溢出、无尾随空白）；③ 所有 padding/尺寸/图标边长 `% UNIT == 0`（装饰像素 1/2/3 除外）；④ `cjk_advance / advance == 2.0 ± 0.001`。
## 6 失败模式与降级

| # | 失败 | 检测 | 降级 |
|---|---|---|---|
| 1 | 主题 token 缺失 / 引用不存在的 palette 名 / `layer` 不足 10 项 | 加载时 schema + `Layer::COUNT` 校验（`16 §3.4`） | 保留上一主题（首启用内置 `dark-default`），设置页列缺项，坏 token 以 `magenta` 高亮并给行号（`E0120`） |
| 2 | 对比度不达标（含自定义主题） | 启动/主题变更时全对校验 | 前景沿 L 轴二分修正（≤8 次，`05 §4.7`）；仍不足则以纯黑/纯白兜底 + 一次性横幅（`E0120`），**绝不静默发布不可读主题** |
| 3 | hex 字体非等宽 / 字体文件损坏 | 实测 200 字形（`16 §6`） | 拒绝加载并回退内置 `Sarasa Mono SC`（`E0130`）；`cell_w` 用内置度量重算，**不半用** |
| 4 | 字形图集创建失败 / 设备丢失 | wgpu error scope（`05 §6 #9`） | 退"纯矩形块"模式（每字节 2 个 8×12 色块）；**图标不受影响**（矢量不依赖图集），这是 D6 的额外收益 |
| 5 | 取整后行高裁切字形 | `ceil_to_device` 后比对 `fm.row_height + 2*row_pad` | `row_h += 1`（`05 §6 #5`），阈值见 §4.10 |
| 6 | 宽度不足（`bpr` 过大或窗口窄） | 布局期 `hex_x0+hex_w+UNIT*4+ascii_w > avail_w` | **先折 ASCII 列**（<560dp，`25 §4.2` 线框 2）并加 `ic.ascii_col` 划除标记；仍不足再运行时下调绘制 `bpr`（`05 §6 #4`），偏移列加 `⌁` 并提示，**不写回配置** |
| 7 | 图标段数超预算 / `IconId` 未注册 | `xtask lint` 静态检查 | 渲染 `ic.error` 占位 + `log::warn`；不 panic、不留空白命中区（空白按钮会被当成坏） |
| 8 | high-contrast 下装饰通道被压平 | 主题加载按 `appearance=hc` 分支 | 强制 1px 实线边框、双环焦点、禁渐变与阴影（`16 §4.8`） |
| 9 | ppp 抖动风暴（跨屏拖动） | 图集重建 >1 次/秒 | 只保留最后一次 ppp 的重建，中间值丢弃（`16 §5`）；期间沿用旧图集（略糊但不闪） |

总原则同 `00 §9.3`：最坏是"退回默认主题或某面板关掉"，不是白屏或 panic（`16 §6`）。
## 7 与其它子系统的契约

| 子系统 | 它读本文 | 本文读它 |
|---|---|---|
| `05-render-and-hexview` | `GridMetrics`、`CaretPaint`、四种选区像素规格、装饰通道尺寸 | `Layout` 字段、`HexViewConfig` 取值、降级表 §6、合成规则 §4.7 |
| `16-ui-shell-theme-i18n` | 4 套 token 表、字阶、图标集、四态、焦点环、DPI 重建 | `Theme`/`DomainTokens`/`FontSet`/`FontMetrics`、布局像素高、`CommandId` 注册表 |
| `25-ux-flows-and-copy` | 四类状态画面、镜像文本组织、空态版式 | `DocState` 八态、三段式话术、术语（游标/选区/补丁/结构/字段）、24dp 目标 |
| `13-visualizers` / `12-yara` / `09-diff` / `08-search` | 画布沿用 4 栅格与字阶；`layer.yara`/`layer.diff`/`layer.search` 的色与装饰通道 | 缩略图/全尺寸尺寸需求；命中上限（`05 §6 #7` 的 `+N` 角标） |
| `17-project-and-persistence` | §8 的 `visual.*` schema | 配置版本迁移（`visual.cfg = 1`） |
| `18-engineering-plan` / CI | §9 的门禁与 12 张基线 | lint 通道（`xtask lint`/`deps`） |
| `21-benchmark-suite` | §5 的数值与不变量 | 基准机与回归判定口径（`21 §4.3`） |

**硬约定**：① 任何视图**不得自带色值**（hex 查 `DomainTokens`、控件查 `SemanticTokens`）；② **不得自带字号或间距**（从 `TypeStep` 与 `UNIT` 取）；③ 本文只决定"长什么样"，**不决定"画哪些像素"**——`05` 是唯一绘制实现方，本文改动必然触发 12 张快照更新，且 PR 必须附"为什么变"；④ 不新增诊断码段，`E0120`/`E0130` 语义归 `16 §6`。
## 8 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `visual.unit` | `4` | 栅格基数（dp）；**不开放给用户改**，仅供 `Dense` 构建微调 |
| `visual.type_scale` | `[11,13,15,18,24]` | 受控字阶；只允许整体换成另一组 5 档（无障碍预设 `[13,15,18,20,28]`） |
| `visual.icon.size` / `.stroke` | `16` / `1.5` | 图标网格与描边（dp）；stroke 只允许 1.0/1.5/2.0 |
| `visual.focus.ring_w` / `.offset` | `2` / `1` | 焦点环宽与外扩间隙（dp） |
| `visual.disabled.text_alpha` / `.icon_alpha` | `0.45` / `0.35` | 仅对 fg 与图标生效（§4.5） |
| `visual.pressed.dy` | `1.0` | pressed 态下移量（dp） |
| `visual.caret.idle_alpha` | `0.35` | 失焦态细线透明度（`blink_ms` 与 `05 §8` 是**同一个键**，以 05 为准） |
| `visual.grid.intra_gap` | `0.0` | 组内字节间隙（§4.3 新增量） |
| `visual.zebra` | **不存在** | 斑马纹已否决（D3）；保留开关等于保留两条渲染路径 |
| `visual.mirror.max_chars` | `120` | 镜像文本单条上限（§4.11） |
| `visual.snapshot.tolerance` | `0.0`（同平台） | 逐像素容差；跨平台改行内色彩直方图 ≤0.1%（`05 §9`） |
| `visual.snapshot.viewport` | `[1280,800]` | 12 张基线的视图内容区（dp） |
| `visual.palette.check_contrast` | `true` | 启动时全 token 对对比度校验（§6 #2） |
| `visual.cfg`（版本） | `1` | 缺失取默认，未知键忽略但 `log::warn`（同 `05 §8` 策略） |

与 `05` 的重叠键（`caret_blink_ms`、`min_contrast`、`colorblind_safe`）**只有一个家**：住在 `05 §8` 的 `hexview.cfg`，本文只引用，避免同一语义两个真相。
## 9 测试策略

### 9.1 像素回归基线（05 §9 的 12 场景，参数写死）

全部场景固定：`theme=dark-default`、`font=builtin:sarasa-mono-sc`、`ppp=1.0`、`lang=zh-CN`、`reduce_motion=true`（关闪烁消除时序抖动）、全部 layer `enabled=true`、样本 `testdata/samples/pe_tiny.bin`。

| # | 场景 | `bpr` | `font_size` | `grouping` | 其它固定参数 | 判据要点 |
|---|---|---|---|---|---|---|
| 1 | Default | 16 | 13.0 | 1 | 游标 0x0、无选区 | 列基线、6dp 字节间隙、`off_w` 6 位 |
| 2 | bpr=1 | 1 | 13.0 | 1 | 视口 [1280,800] | 单字节行的 `hex_w` 与 ASCII 起点 |
| 3 | bpr=64 | 64 | 13.0 | 8 | **视口 [1600,800]**（否则触发 §6 #6 降级） | 8 组 ×8 字节、7 处组间隙 |
| 4 | HexII | 16 | 13.0 | 1 | `display=HexII` | `..`（0x00）与双空格（0x20）字形 |
| 5 | Binary | 16 | 13.0 | 1 | `display=Binary` | 列宽 ×4、`hex_w` 溢出行为 |
| 6 | Decimal | 16 | 13.0 | 1 | `display=Decimal` | 3 字符宽、右对齐基准 |
| 7 | UTF-16LE | 16 | 13.0 | 1 | `ascii.encoding=Utf16Le` | `ascii_glyphs=8`，ASCII 列宽减半 |
| 8 | EBCDIC | 16 | 13.0 | 1 | `ascii.encoding=Ebcdic037` | 缺字 → `invalid_glyph` 固定宽度 |
| 9 | 深嵌套 12 层 | 16 | 13.0 | 1 | `overlay.degrade_depth=6`，14 层结构数据 | 每行 ≤8 色带 / ≤4 括号线 |
| 10 | 100 万字节选区 | 16 | 13.0 | 1 | `selection=0x0..0xF4240`，游标在选区中 | 只脏可见行、纵向无白缝 |
| 11 | 插入模式 | 16 | 13.0 | 1 | `mode=Insert`，游标在行首字节 High | 2dp 空心框占半格 |
| 12 | 块选区 | 16 | 13.0 | 1 | `SelShape::Block{cols:(4,7)}` | 矩形无缝 + 1dp 勾边 |

**判据**：同平台逐像素容差 **0**；跨平台改"行内色彩直方图差异 <0.1%"（`05 §9`）以规避字体光栅化差异。主题×DPI×布局维度由 `16 §9` 的 45 张外壳快照承担（5 主题×3 DPI×3 布局），**两套基线互不替代**：本文 12 张只锁几何，`ppp` 一变就失去意义；16 的 45 张只锁主题与缩放，不逐像素比对网格（DPI 变化时单元格必然重排）。

### 9.2 门禁

| 层 | 手段 | 门禁 |
|---|---|---|
| 对比度 | 对 4 套主题逐 token 对算 WCAG 比值（`05 §4.7` 的 L 公式） | 4.5/3.0/3.0/7.0，任一不足即 fail 并打印 token 名与实测值 |
| 栅格 lint | `xtask lint` 扫视觉模块外的间距/字号字面量、`Color32::from_rgb` | 0 命中 |
| 几何不变量 | 属性测试：随机 `bpr∈1..=64`、`grouping∈{1,2,4,8}`、`ppp∈{1,1.5,2,3}`、随机 `len` | §5 四条恒成立；`x(n)` 与逐项累加在 960 格内误差 <0.5 设备像素 |
| 图标与主题完整性 | 每个 `toolbar:true` 的 Command 必须有 `IconId`（段数 ≤8、无孤儿）；4 套主题的 `layer` 10 项齐全且顺序 = `00 §5.3` | 0 无图标工具栏项；缺项 fail（同时覆盖 `E0120` 路径） |
| 快照 | 12 张（§9.1）+ 45 张（`16 §9`） | 同平台 0 容差；基线更新须在 PR 说明"哪个像素为什么变" |
| 键盘可达 | 遍历 Tab 顺序，断言每个面板与每条可见命令可达、焦点环必现 | 0 不可达控件；焦点环在 4 套主题下 ≥3:1 |
| 无障碍 | accesskit 语义树快照（含 §4.11 三种镜像文本） | 快照比对；镜像单条 ≤120 字符 |
| 降级 | 注入缺 CJK 的假字体、缺 token 主题、坏 TOML、`bpr=64 + 窄窗` | 全走 §6 对应分支，不 panic、不白屏 |
## 10 开放问题

| # | 问题 | 备选 | 倾向 |
|---|---|---|---|
| Q1 | **斑马纹是否影响高亮可读性**：本文以"抢 Value 层通道"否决，但 Value 层关闭时斑马纹对"横着读一行"确有帮助 | 永久否决（本文）/ 仅 `layer.value.enabled=false` 时启用 | 先按否决实现；用 5 人可用性测试（`25 §9.3`）测"横读 16 字节的准确率"，差异 >10% 再重开 |
| Q2 | **16px 图标集够不够表达 YARA 命中**：`ic.yara_hit` 是盾形，无法区分规则命中与规则族命中，也不带命中数 | 单图标 + `+N` 角标（现状）/ 增加 4 个按 severity 变体的 YARA 图标 | 倾向角标——图标膨胀会立刻稀释"≤64"约束，而数字角标信息量更高 |
| Q3 | **面板标题是否值得引入 Bold**：15dp Regular 与正文 13dp 的层级差只有 2dp + 颜色 | 无 Bold（本文，省 220KB）/ 打包 Inter SemiBold 子集 | 倾向无 Bold，但需一次 A/B 截图让 5 人判断"能否一眼分出标题与正文" |
| Q4 | **deuteranopia 是第四套主题还是开关变换**：`16 §4.8/§8` 是布尔 `ui.theme.deuteranopia`，本文给成完整命名主题 | 布尔开关（16）/ 命名主题（本文）/ 开关 → 派生命名主题 | 倾向后者：布尔便于用户理解，命名主题便于基线可比 |
| Q5 | **双缩放模型是否应合并**：`05 §4.2` 网格内 `Ctrl+滚轮` 改 `font.size`，`16 §4.4` 的 `Ctrl+=` 改全局 `zoom` | 两套并存（现状）/ 合并为单一 zoom | 并存更实用（只想放大十六进制时不该连菜单一起放大），但必须补"先乘后 clamp"规则（§4.10），否则 3.0 zoom × 28 size 越界 |
| Q6 | **自绘网格的焦点如何表达**：hex 网格不是 widget，Tab 焦点进入时没有可套焦点环的矩形 | 整个网格画 1dp 外框 / 不画（靠游标行）/ 只画游标行左缘竖条 | 倾向"游标行左缘 3dp `border.focus` 竖条"：既是焦点指示，又不与 2px 结构色带抢通道 |
| Q7 | **`min_contrast` 修正会改掉用户认的语义色**（`05 Q7`）：`layer.search` 的黄在黑底上压暗后不再像"黄" | 修正 / 不修正 / 只修文字不修底纹 | 修正，但底纹不动（只压前景），并在装饰通道上补语义；需一次色觉测试验证"压暗后的黄仍被认作黄" |
| Q8 | **`cell_w` 量化到 `1/ppp` 在 3.0 zoom 下产生 ±0.33dp 抖动**，长行末端累积可见 | 量化 `1/ppp`（`05 §4.3`）/ 量化 0.5dp | 倾向 0.5dp：`bpr=64` 行宽误差从 ~10dp 降到 ~2dp，代价是字形可能落半像素（只在 3.0 zoom 发生，可接受） |
