# 13 — 字段可视化器体系

> 状态：Draft v0.1（未实现）。上位契约：`00-architecture-overview`（§2 分层、§5.3 `AnnotationIndex`、§6 线程模型、§7 预算）。
> 归属 crate：`geode-viz`（纯 CPU 解码 + 度量，**不得依赖 egui/wgpu**）+ `geode-ui` 中的绘制适配层。
> 职责边界：本文拥有"字段字节 → 视觉产物"的注册、解码、度量、降级与反向联动语义；**不拥有** hex 网格渲染（见 05-render-and-hexview）、不拥有属性语法定义（见 02-dsl-spec §6）、不拥有具体 GPU 管线实现（见 05）。

**结论先行（5 条，后文全部围绕它们裁决）**

1. **可视化器被切成两半**：解码（`geode-viz`，CPU、可后台、可 fuzz）与绘制（`geode-ui`）。这条线是 00 §2「除 geode-ui 外任何 crate 不得依赖 GUI」的直接推论，也是插件可视化器唯一可行的落地方式（见 14-plugin-system §1）。
2. **产物只有三种**：RGBA8 位图、几何（点/三角）、二维条带图元（波形/时间轴/热图）。不存在第四种，也不给可视化器一条"随便画"的后门。
3. **一切来自文件 header 的尺寸都必须预检**。不预检 = 一张 100KB 的 PNG 解出 20GB。这是本子系统唯一的安全红线。
4. **缩略图优先**：任何可视化面板必须在首帧内给出 64×64 缩略图（≤8ms），全尺寸在 150ms 静止后异步补上。
5. **反向映射（像素 → 字节）是双向联动的硬要求**，但只有"无容器原始像素"格式能做到 O(1) 精确；容器格式 v1 只承诺字段级粗粒度高亮，理由与成本见 §3.4。

---

## 1. 目标与非目标

### 1.1 目标

| # | 目标 | 可验收的判据 |
|---|---|---|
| G1 | BinSpec 字段可一键可视化 | 结构树右键/双击字段 → 右侧面板出图，路径 ≤ 2 次点击 |
| G2 | 覆盖桌面逆向最常用的 5 类 | 原始像素图、容器图、3D 点云/网格、时间轴、音频波形；其余（色板、热图、hex 矩阵）为低成本补充 |
| G3 | 解码任意不可信数据不崩、不 OOM、不卡 UI | 解码器全部 fuzz；超过硬上限一律拒绝并给出可读原因 |
| G4 | 与 hex 视图双向联动 | hover 图上像素 → hex 高亮对应字节，延迟 ≤ 1 帧；点 hex 字节 → 图上定位（仅 raw 格式） |
| G5 | 可导出 | PNG / OBJ / CSV，导出必须走"宿主代写文件"，可视化器本身无文件系统权限 |
| G6 | 插件可注册可视化器 | 走 14 的 ABI，插件侧只产出 CPU 侧产物，不接触 egui |

### 1.2 非目标（明确否决）

| 否决项 | 理由 |
|---|---|
| 可视化器直接改字节 | 违背 00 §6「编辑只在 UI 线程发生」。可视化器是**只读视图**，永远不持有 `&mut ByteBuffer` |
| 通用着色器编辑器 / 用户写 GLSL | 攻击面 + 不可 fuzz + 无法保证预算。否决 |
| 视频/动画解码（MP4、WebM） | 解码器体量与专利风险远超收益；用户可导出后再用外部工具 |
| 3D 模型编辑、材质、光照参数面板 | 只做"看懂字段"，不做建模器。OBJ 导出即止 |
| 自相关 / digram 图 | 远期（本版仅登记需求，见 §10） |
| 可视化器触网 | 恒假，不可配置（§8） |
| 让可视化器拿到 `egui::Ui` | 破坏 00 §2 分层，见 14-plugin-system §1 的决策表 |

---

## 2. 依赖与边界

### 2.1 上下游

```
geode-bspec(Node/attrs)  geode-buffer(read)  geode-annot(§5.3)  geode-core
        └──────────────────┴──────────────────┴─────────────┘
                                 ▼
        geode-viz（本子系统，纯 CPU）: registry / decode / measure / OffsetMap
                                 │ RenderCtx —— 只下沉"产物"，不含任何绘制 API
                                 ▼
        geode-ui（egui + wgpu）: 纹理池 / 3D viewport / 面板与文本标签
```

- **不得依赖**：`egui`、`wgpu`、`winit`、任何窗口/图形 API。`geode-viz` 的 CI 里有一条"禁止依赖图形库"的检查（与 00 §2 同一条）。
- **可依赖的第三方（白名单）**：`image`（解码 BMP/PNG/JPEG/GIF/QOI/TGA/WebP，配 `image` 的 `limits` 做尺寸上限）、`png`/`gif`（需要逐行字节溯源时改用底层 crate）、`flate2`（PNG 溯源）、`rubato`/`dasp`（重采样，可选）。新增依赖需在 18-engineering-plan 登记许可证。
- **3D 不做数学库依赖**：只需要 `egui::emath` 级别的向量运算（由 geode-ui 侧提供），`geode-viz` 只输出 `[f32;3]` 数组。

### 2.2 与相邻子系统的边界

| 子系统 | 本子系统要它什么 | 本子系统绝不做什么 |
|---|---|---|
| `geode-bspec` | `Node.offset/size/attrs`、`#[visualize]` 原始字符串（见 02-dsl-spec §6/§7.3） | 不解析 DSL、不做类型检查、不改字节消费语义 |
| `geode-buffer` | `read(range, out)` + `version()` | 不写、不 patch、不持锁超时 |
| `geode-annot` | `insert/remove`、`Layer::Cursor`（hover 高亮用） | 不自建高亮机制（00 §5.3 是唯一高亮数据源） |
| `geode-provider` | 间接，经 Buffer | 不直接 `read_at`（避免绕过 piece table） |
| job 系统（00 §5.4） | `Job` + `CancelToken` + `UiEvent` | 不在 UI 线程解码任何 >64KB 的数据 |
| `geode-plugin-host` (14) | 外部 `Visualizer` 注册项 | 不实现 ABI、不碰 dlopen |

---

## 3. 数据结构与接口

### 3.1 注册表

```rust
pub enum VizSource { Builtin, Plugin { plugin_id: String } }

pub struct VisualizerRegistry { entries: Vec<Entry> }   // 按 (priority, id) 有序，结果确定

impl VisualizerRegistry {
    pub fn register(&mut self, v: Arc<dyn Visualizer>, src: VizSource) -> Result<(), RegError>;
    pub fn unregister(&mut self, id: &str) -> bool;        // 14 卸载插件时调用
    /// 唯一解析入口：属性串 → 可视化器 + 配置
    pub fn resolve(&self, field: &FieldDesc) -> Result<Resolved, VizError>;
    /// "以…查看"菜单用：列出所有能接受该字段的可视化器
    pub fn candidates(&self, field: &FieldDesc) -> Vec<&'static str>;
}
```

**解析顺序（确定性规则，不允许"看运气"）**

1. `#[visualize: "id/params"]` 中 `id` 精确命中 → 用它，命中失败即报错（`E8002`），**不回退**到猜测。
2. 无 `id`（简写形式，如 `#[visualize("image/png")]`）→ 按 `fmt=` 反查 `image.container`。
3. 无 `#[visualize]` 属性，但用户在 UI 里手动"以…查看" → 用 `candidates()` 的顺序。
4. 内置条目恒排在插件条目之前；同一 id 冲突时，插件必须用带命名空间的 id（`acme.waveform`），否则注册被拒（`RegError::IdTaken`）。

### 3.2 `Visualizer` trait 草案

```rust
pub struct FieldDesc<'a> {
    pub path:   &'a [usize],      // Node.path，双向联动的键（02-dsl-spec §7.3）
    pub type_name: &'a str,
    pub range:  ByteRange,        // 已 clamp 到 [0, buffer.len())
    pub attr:   Option<&'a str>,  // "[visualize]" 的原始参数串，未解析
}

pub struct VizConfig {
    pub visualizer: &'static str, // "image.raw" | "image.container" | "mesh3d" | ...
    pub params: ParamMap,         // key 全部小写；值只有 Int / Str / Bool
}

pub enum OutputKind { Bitmap, Geometry, Bands }

pub struct SizeEstimate {
    pub kind: OutputKind,
    pub pixels: u64,              // 位图：w*h
    pub primitives: u64,          // 几何：点数 + 三角数
    pub bytes_hint: u64,          // 峰值内存上界估计（含解码中间态）
    pub confidence: EstConf,      // Exact（算出来的）| UpperBound（header 声明，不可信）
}

pub trait Visualizer: Send + Sync + 'static {
    fn id(&self) -> &'static str;
    fn matches(&self, field: &FieldDesc) -> Option<VizConfig>;      // 只看 type_name/attr，不读数据
    fn measure(&self, cfg: &VizConfig, data: &[u8]) -> Result<SizeEstimate, VizError>;
    fn render(&self, cfg: &VizConfig, data: &[u8], out: &mut dyn RenderCtx) -> Result<(), VizError>;
    /// 反向映射：图像坐标 → 源字节区间。不支持时返回 None（UI 退化为字段级高亮）
    fn reverse(&self, cfg: &VizConfig, px: (u32, u32)) -> Option<ByteRange> { None }
}
```

### 3.3 `RenderCtx`：绘制下沉的唯一通道

`RenderCtx` 定义在 `geode-viz`，**实现**在 `geode-ui`。可视化器只"提交产物"，永远看不到 `Ui`、`Painter`、`Device`。

```rust
pub trait RenderCtx {
    fn push_bitmap(&mut self, tex: BitmapDesc);      // RGBA8, w, h, stride, origin
    fn push_mesh(&mut self, mesh: MeshDesc);          // positions:[f32;3], indices:u32, normals?:[f32;3]
    fn push_polyline(&mut self, line: PolylineDesc);  // 波形包络、列级条带
    fn push_band(&mut self, band: BandDesc);          // 时间轴/热图的 2D 矩形 + 颜色
    fn budget_left(&self) -> PrimBudget;              // 剩余图元配额，超了必须自行降采样
}
```

这条设计同时解决了三件事：(a) 分层不被破坏；(b) 插件可视化器只需实现 CPU 侧 trait（14）；(c) 导出（PNG/OBJ/CSV）可以在**不启动 GPU** 的情况下跑（用 `NullRenderCtx` 收集产物），因此导出可后台化、可测试。

### 3.4 反向映射（双向联动的关键）

```rust
pub struct OffsetMap {
    pub base: u64,        // 字段起始偏移
    pub header_len: u32,  // header=1 时跳过的文件头字节数
    pub w: u32, pub h: u32, pub stride: u32, pub bpp_bits: u16,  // 每像素位数
    pub row_order: RowOrder,   // TopDown | BottomUp
    pub precise: bool,         // false = 只能给字段级区间
}
impl OffsetMap {
    /// O(1)，无查表，无分配
    pub fn pixel_to_range(&self, x: u32, y: u32) -> Option<ByteRange>;
    /// hex 视图点击 → 图上像素（精确模式可用；否则返回 None）
    pub fn range_to_pixel(&self, off: u64) -> Option<(u32, u32)>;
}
```

| 情形 | 映射精度 | 成本 |
|---|---|---|
| 原始像素（`fmt=rgba8` 等） | **精确，O(1) 闭式公式**：`off = base + header_len + y_eff*stride + (x*bpp_bits)/8`；`bpp<8` 时再按位偏移取子字节。调色板索引同式，另记调色板条目区间 | 0 |
| 容器图（PNG/JPEG/GIF…） | v1 **只给字段级**：hover 高亮整张图的 `ByteRange` | 0 |
| 容器图 + PNG 逐行溯源（v1.1 可选） | 行级精确 | inflate 时每行记 `(out_row, in_pos)` ≈ 8B/行；4096 行 = 32KB，占比 <1% |

**决策**：v1 不做逐行溯源。理由：JPEG/GIF/WebP 的熵编码流根本无法建立"输出像素 → 输入字节"的有效映射，只为 PNG 开一条精准通道收益不足以抵消"用户以为四种格式都能用"的期望落差。UI 上对容器格式**显式**显示"该格式可定位到字段，不可定位到像素"。

### 3.5 内置可视化器清单

v1 内置 9 个，全部静态编译进宿主（不依赖插件），按 §3.1 规则 4 恒优先于插件条目。

| id | 输入 / 参数 | 硬上限 | 失败降级 |
|---|---|---|---|
| `image.raw` | 裸像素。`fmt` ∈ `rgba8 bgra8 rgb565 rgb555 argb1555 gray8 gray16 bin1 pal8 rgb332 rgbe5551`；`w,h` 必填，`stride` 默认 `w*bpp`（须 ≥ `w*bpp` 且 ≤65535），`order=td\|bu`，`header=N` 跳过 N 字节文件头，`pal_off/pal_n/pal_bpp` 供 `pal8` | 边 ≤4096、像素 ≤16M、输入 ≤64MiB | 缺 `w/h` → 占位网格 + 提示；`stride` 非法 → 回退 `w*bpp` + warning；数据不足 → 画已读部分并画斜纹补齐 |
| `image.container` | 容器解码：**BMP / PNG / JPEG / GIF / QOI / TGA / WebP**。`fmt` 可留空（嗅探 magic）| 输出 ≤16M 像素、≤256MiB 中间态、≤256 帧（只解首帧）、输入 ≤64MiB | 嗅探失败 → 回退 `image.raw` 提示"未识别容器"；声明尺寸超限 → 拒绝分配（`E8004`）；解码中途失败 → 部分图像 + 横幅 |
| `mesh3d` | 点云：`float32 xyz` 连续三元组；网格：`vtx` 顶点表（`[f32;3]`）+ `idx` 索引表（`u16/u32`），`fmt=points\|tris`。相机轨道（默认）/自由（WASD），坐标轴 + 网格地面（可关），`point_size`、`wireframe`、`normals` | 点 + 三角合计 ≤500k | 超限 → 确定性 stride 抽样（保留包围盒极值）+ 面板标注比例；顶点数不是 3 的倍数 → 丢弃尾部 + warning |
| `timeline` | 结构数组，每元素含 `ts`（时间戳字段名/偏移/单位）+ 若干值字段；`agg=1s\|10s\|1m\|1h`、`color=<字段名>` | ≤100 万条记录，聚合后 ≤4096 桶 | 无 `ts` 字段 → 退化为索引轴（"第 N 条"）；记录数超限 → 先聚合再采样 |
| `waveform` | PCM：`bits=8\|16\|24\|32`、`ch=1\|2`、`rate`、`signed`、`endian`。输出峰值包络（`min/max` 对）+ RMS 线 | 采样数 ≤50M，包络点 ≤4×画布宽 | 未给 `rate` → 横轴显示"样本号"；位深不支持 → 提示并给 hex 预览 |
| `palette` | 索引色表：`n`、`bpp_pal=24\|32\|16`、`order=rgb\|bgr`、`alpha`。附带"按索引排列的渐变条" | n ≤ 4096 | 参数缺失 → **不猜**，直接显示参数表单（§10 第 6 条） |
| `heatmap` | 把字节流当 2D 矩阵：`w`、`cell=1\|2\|4`、`map=hue\|gray\|viridis`（字节值 → 色相） | 边 ≤2048 格、总格 ≤4M | `w` 未给 → 按 256 宽推断并 warning |
| `hexmatrix` | 每字节一格，值 → 灰度；不自动缩放（保持 1:1 才能"看纹理"） | 边 ≤8192 格、总格 ≤16M | 超限 → 提示改用 `heatmap` 并给一键切换按钮 |
| `autocorr` / `digram` | **v1 不实现**，仅登记 id 与占位面板 | — | 面板显示"计划中（见 13 §10）" |

三点补充：

- **`image.raw` 的格式覆盖是硬需求**，因为固件/游戏资源里 `RGB565`、`RGBA5551`（"RGBE5551"）、`RGB332`、二值位图、行尾 padding 遍地都是；缺一个，用户就得先外部转换，体验断崖。
- **`mesh3d` 走独立的小 wgpu viewport**（egui `PaintCallback` 挂一块 `wgpu::RenderPass`），与 hex 网格共用同一个 `Device`，但独立 `Camera` 与深度缓冲。这是本子系统唯一允许碰 wgpu 的地方，且代码位置在 `geode-ui`，不在 `geode-viz`。
- **时间轴/波形/热图都只产出 `Bands`/`Polyline`**，不含任何字符串布局；坐标轴刻度与标签由 geode-ui 用 egui 文本画（保证 i18n 与字体一致，见 16）。

---

## 4. 核心算法 / 流程

### 4.1 触发路径（端到端）

```
① BinSpec 解析产出 Node，携带 attrs["visualize"] = "image/w=64,h=64,fmt=rgba8"
② geode-ui 渲染结构树那一行时，发现该节点有 visualize 属性
   → registry.resolve(FieldDesc) → (visualizer, VizConfig)      ← 主线程，必须 <10µs
③ 用户点开字段 → 提交 VizJob（Idle 优先级），入参=(DocId, ByteRange, buffer.version, cfg)
④ VizJob: 读字节(≤64MB，超出即 E8004) → measure() → 预算校验
⑤ 先跑"缩略图通道"（64×64）→ UiEvent::VizReady(thumb)；面板首帧只画缩略图 + "生成中"
⑥ 150ms 静止后提交全尺寸 Job → render(RenderCtx) → geode-ui 上传纹理/顶点缓冲 → 绘制
⑦ hover 像素 → map.pixel_to_range → geode-ui 对 Layer::Cursor 插入一条 Ann
```

四条硬规则：

- 步骤 ② 在 UI 线程内跑，因此 `matches()` **禁止读字节**，只允许看 `type_name` / `attr` / `range`。
- 步骤 ④ 的 `measure()` 允许读**至多 4KB 头部**，用于解析 header 声明尺寸。它读到的任何数字都是不可信的（`EstConf::UpperBound`），必须再过预算。
- 步骤 ⑥ 的 `render()` 在后台线程跑，不许碰 `AnnotationIndex`，改动通过 `UiEvent` 回主线程。
- 任何产物回主线程时必须校验 `buffer.version()`（00 §6 规则 3），不匹配直接丢弃。

### 4.2 参数文法

```
attr      := "[" "visualize" ":" "\"" visual "\"" "]"          // 02-dsl-spec §6 的属性壳
visual    := id [ "/" param ("," param)* ]
param     := key "=" value
key       := [a-z][a-z0-9_]*
value     := integer | quoted-string | bareword
```

- 未知 key → 忽略 + `E8001` warning（不阻断渲染，因为模板作者手写参数很常见）。
- 未知 `id` → **阻断**，`E8002`，面板显示"未知可视化器 `xxx`，可选：image.raw, mesh3d, …"。
- 数值越界（如 `w=0`、`w=99999`）→ `E8004`，附实际值与上限。
- 文法总量 <60 行 BNF，实现控制在 200 行内，须与属性解析器共用词法（不引入第二套 parser）。

### 4.3 缩略图优先（两阶段渲染）

```
fn viz_pipeline(data, cfg):
    est = measure(cfg, data)
    check(est)                                   # 上限校验，见 §4.4
    thumb_cfg = downscale(cfg, target=64)
    emit(thumbnail(render(thumb_cfg, data)))     # 目标 ≤8ms，超时则退化为"首列像素条"
    if idle_for(150ms) and panel_visible:
        emit(full(render(cfg, data)))            # 目标 ≤250ms @ 4096²
```

`downscale(cfg, 64)` 对原始像素是"隔行隔列抽样"（不重采样，保持零成本）；对容器图是**解码器级别缩放**（`image::imageops::thumbnail` 仍然要全解码，因此容器图只有一阶段 + 更严的尺寸上限，见 §6）。

### 4.4 硬上限校验（唯一安全闸门）

```
MAX_EDGE      = 4096
MAX_PIXELS    = 16_000_000      # 4096×4096 略余
MAX_PRIM      = 500_000         # 点+三角合计
MAX_DECODE    = 64 MiB          # 单次解码输入字节上限
MAX_PEAK_MEM  = 256 MiB         # bytes_hint 上限
DECODE_TIMEOUT= 2000 ms
```

校验顺序固定为 **声明 → 上限 → 分配**。绝不允许"先 `Vec::with_capacity(width*height*4)` 再说"——那是解码炸弹的唯一入口。

### 4.5 3D 降采样

- 点云：`n = len / 12`（3×f32）。`n > 500k` 时按 `stride = ceil(n/500k)` 抽样（确定性，不用随机，保证同一字段两次渲染完全一致）；抽样时必须**保留包围盒极值点**（否则模型会"缩水"）。
- 网格：顶点表 + 索引表。`n_tri > 500k` 时按索引三元组抽样（会破洞，仅用于"看个大概"），并在面板上标注"已降采样 1/N"。
- 坐标归一化在 CPU 侧完成（输出 `[-1,1]³` + 一个 `scale_hint`），GPU 侧不做矩阵推导，避免浮点差异导致黄金图像测试不稳。

---

## 5. 性能预算与基准

| 场景 | 预算 | 条件 | 退化信号 |
|---|---|---|---|
| `resolve()` 单次 | < 10µs | 属性串 ≤128B | 结构树每帧对可见 ~200 行调用 |
| 缩略图生成 | < 8ms | 输入 ≤1MB，raw | 首帧出现"空白面板" |
| 全尺寸解码（raw） | < 60ms | 2048×2048 RGBA8 = 16MB | 面板卡在"生成中" |
| 全尺寸解码（PNG） | < 250ms | 输出 4096×4096 | 同上 |
| 点云构建 | < 120ms | 500k 点（6MB 输入） | 3D 视口空转 |
| 时间轴聚合 | < 60ms | 100 万条记录 → 2000 桶 | 拖动缩放时掉帧 |
| hover 反查 + 高亮 | < 1ms/帧 | 任意 | hex 视图跟随鼠标掉帧 |
| 视口不可见时的额外开销 | **0** | 面板折叠 / 切标签 | — |
| 全系统纹理显存 | < 256MB | 同时打开 8 个可视化面板 | GPU 分配失败 |
| 可视化面板对帧时间的影响 | ≤ 2ms | 已就绪状态，静态 | 滚动掉到 30fps |
| 导出 PNG（4096²） | < 400ms | 后台线程 | — |

三条约束：

1. **静态面板不得每帧重绘**。产物缓存在 `(visualizer_id, cfg_hash, ByteRange, buffer.version)` 上，四者不变即复用。
2. **纹理复用池**：同尺寸位图复用同一 `wgpu::Texture`（按 2 的幂分级：64/128/256/512/1024/2048/4096），避免反复分配触发 wgpu 内部碎片。
3. `bytes_hint` 与实际峰值内存的偏差超过 2× 视为 bug（说明 `measure` 在骗人），CI 里对每个内置可视化器跑一次 RSS 采样断言。

---

## 6. 失败模式与降级

| 失败 | 检测点 | 降级行为 | 诊断码 |
|---|---|---|---|
| 属性文法错误 | §4.2 parse | 面板显示"参数错误"原文 + 正确示例；不渲染 | `E8001` |
| 未知可视化器 id | `resolve()` | 面板列出候选；不渲染 | `E8002` |
| 格式不支持（如 `fmt=xyz`） | `matches()` | 回退 hex 预览（复用 05 的只读小窗） | `E8002` |
| header 声明尺寸超限 | `measure()` | **不分配**，直接报错并显示"声明 60000×60000，上限 4096" | `E8004` |
| 解码途中失败（截断/损坏） | `render()` | 显示**已解出的部分**（前 N 行）+ 错误横幅；这是逆向场景的常态，全屏报错不可接受 | `E8003` |
| 解码超时 2s | Job watchdog | 保留缩略图作静态截图，面板标注"解码超时" | `E8005` |
| 图元超预算且无法降采样 | `budget_left()` | 输出"降采样后的形态"并标注比例 | `E8006` |
| GPU 纹理分配失败 / 超上限 | geode-ui 侧 | 降级为 CPU 缩放后画到 2048² 以内 | — |
| 插件可视化器 panic | `catch_unwind` | 该可视化器**本会话内禁用**，其它字段不受影响 | `E8007` |
| 数据版本已过期 | `buffer.version()` | 静默丢弃产物，重提交一次；仍失败则停止自动重试 | — |
| 导出写文件失败 | 宿主侧 | 提示条，保留产物在内存中供重试 | — |

**解码炸弹的专门防线（必须有独立测试）**

1. 容器格式一律走 `image` crate 的 `Limits`（`max_alloc`、`width/height` 上限），且在上限之外**再加一道** `w*h*4 ≤ 256MiB` 的自检。
2. 绝不信 header：`measure()` 得到的 `UpperBound` 只用于"拒绝"，不用于"分配"。
3. 递归/嵌套结构（GIF 多帧、APNG、动画 WebP）有帧数上限（默认 256 帧，只解首帧 + 第一帧之外的跳过）。
4. 单次解码输入 ≤64MiB：字段声明 100MB 的"图片"是明显的错用，直接拒绝。

---

## 7. 与其它子系统的契约

### 7.1 读

| 来源 | 读什么 | 时机 |
|---|---|---|
| `geode-bspec` | `Node.offset/size/type_name/attrs/path` | 结构树渲染、`resolve()` |
| `geode-buffer` | `read(range, out)`、`version()` | Job 内 |
| `geode-ui` 布局 | 面板可见性、DPI、主题色 | 每帧（不触发解码） |

### 7.2 写

| 目标 | 写什么 | 约束 |
|---|---|---|
| `geode-annot`（00 §5.3） | hover 像素 → `Layer::Cursor` 一条 `Ann`；导出后清理 | **不新增 Layer**（`Layer` 枚举是 00 拥有的契约）。若要"可视化器自己的常驻高亮"，需先改 00 §5.3，见 §10 |
| job 系统 | 提交/取消 `VizJob` | 必须带 `CancelToken`，100ms 内响应（00 §6 规则 4） |
| `geode-ui` 纹理池 | 产物 | 按 LRU 回收，上限 256MB |

### 7.3 需要上游配合的三处改动

1. `02-dsl-spec §6` 目前写作 `#[visualize("image/png")]`，建议统一为 `#[visualize("image.container/fmt=png")]`，并把旧形式定义为语法糖（一行改动）。
2. **错误码段已登记**：`00-architecture-overview §12` 现为 `E8xxx = 字段可视化器`（复用同一 `Diag` 结构，UI 才能统一渲染）。原"借用 E6xxx"的临时方案作废，码值已是正式分配。
3. `00 §5.3` 若未来要 `Layer::Viz`，须由 00 提案，本子系统不擅自扩展。

### 7.4 与 14 的接口

插件可视化器在宿主侧被包装成 `Arc<dyn Visualizer>`：`matches`/`measure`/`render` 三个调用各自是一次 ABI 跨界（见 14 §3.3 的 `VisualizerVTable`），`data` 由宿主以 `(ptr,len)` 借出，**回调期间有效**。

---

## 8. 配置项与默认值

| 键 | 默认 | 范围 | 可配置 | 说明 |
|---|---|---|---|---|
| `viz.enabled` | true | — | ✅ | 总开关 |
| `viz.thumbnail_px` | 64 | 16~256 | ✅ | 缩略图边长 |
| `viz.full_scale_delay_ms` | 150 | 0~2000 | ✅ | 静止多久后出全尺寸 |
| `viz.max_edge` | 4096 | 256~8192 | ⚠️ 仅下调 | **上调需重启并二次确认**，因涉及显存预算 |
| `viz.max_pixels` | 16_000_000 | 65_536~33_554_432 | ⚠️ 仅下调 | 同上 |
| `viz.max_primitives` | 500_000 | 10_000~2_000_000 | ✅ | 点/三角上限 |
| `viz.decode_timeout_ms` | 2000 | 200~30_000 | ✅ | 单次解码墙钟上限 |
| `viz.max_decode_input` | 64MiB | 1~512MiB | ✅ | 输入字节上限 |
| `viz.texture_budget_mb` | 256 | 64~2048 | ✅ | 全局显存池 |
| `viz.container.enabled` | true | — | ✅ | 是否允许容器解码（可被企业策略关死） |
| `viz.container.max_frames` | 256 | 1~4096 | ✅ | 动图帧上限 |
| `viz.audio.playback` | false | — | ✅ | 是否加载音频后端（cpal）；不影响波形绘制 |
| `viz.export.dir` | 空（每次询问） | — | ✅ | 导出目录 |
| `viz.network` | **false** | **不可改** | ❌ | 硬编码；可视化器无权访问网络 |
| `viz.write_files` | **false** | **不可改** | ❌ | 导出由宿主代写 |

---

## 9. 测试策略

| 层次 | 内容 | 门禁 |
|---|---|---|
| 单元 | 参数文法（含 fuzz 解析器）、`OffsetMap` 闭式公式、降采样抽样确定性、预算校验分支 | 行覆盖 ≥90% |
| 黄金图像 | 每可视化器固定输入 → 产物 PNG，**逐像素比对**（raw/热图/hex 矩阵是确定性的），时间轴用结构快照（JSON）而非像素 | 差异 = 0 像素 |
| 3D/GPU | 顶点缓冲快照（JSON，坐标量化到 1e-4）而非截图，避免后端差异 | 差异 = 0 |
| 解码炸弹 | 样本库：100KB PNG 声明 60000×60000、GIF 1000 帧、BMP 声明负数宽度、PNG 超大 IDAT、QOI 声明尺寸与数据不匹配、TGA 行跨度为负 | 全部必须以 `E8004` 拒绝，且峰值 RSS 增量 < 8MB |
| fuzz | 每内置解码器一个 `cargo-fuzz` target（含 `image` 调用面）；`Visualizer` trait 输入 fuzz（data 长度 0/1/奇数字节） | 24h 无 panic、无超时 |
| 集成 | 结构树点击 → 面板出现 ≤2 帧；hover → hex 高亮同帧生效；导出的 PNG 能被外部工具打开 | 全绿 |
| 性能 | §5 全部条目进 `bench/`，接 CI（00 §7 要求"没有基准的功能等于没有功能"） | 退化 >15% 红灯 |
| GPU 后端矩阵 | Vulkan / Metal / DX12 + `lavapipe`（CI 软件渲染）；同一产物在两个后端上渲染结果允许 ≤1 LSB 差异 | 无崩溃 |
| 插件 | 加载一个故意 panic 的可视化器 → `E8007` + 其余功能正常 | 全绿 |

---

## 10. 开放问题

1. **容器格式的逐行字节溯源是否值得**（§3.4）。只为 PNG 开一条 ±8B/行的溯源通道，能让"hover 像素高亮压缩流字节"成立；但用户会对 JPEG/GIF 提出同样要求。**倾向 v1.1 只做 PNG，并在 UI 上明确标注能力差异。**
2. **是否需要 `Layer::Viz`**。目前常驻高亮只能塞进 `Layer::Structure` 或 `Cursor`，语义都不完美。改动点在 00 §5.3，需 00 提案。
3. **时间轴的数据源**：直接吃"结构数组（每条含 timestamp + 值字段）"，还是允许吃 `geode-proc` 节点图的输出（07）？后者表达力强但会让 13 依赖 07，暂不引入。
4. **音频播放**是否进 v1。波形绘制零依赖；但播放要引入 `cpal`（平台音频线程、格式协商、与 Job 系统抢线程），且播放属于"改变状态"的行为。**倾向 v1 只画不播。**
5. **3D 是否进 v1**。它是 ImHex 的展示性卖点，但工作量约为其余 6 个可视化器之和（独立 wgpu viewport、相机、拾取）。**倾向 v1 先做点云（不含网格与法线），网格推到 v1.1。**
6. **色板提取**：调色板可视化需要"从字段字节里猜调色板布局"（16 色? 256 色? RGB888? BGR?），猜测失败率高。是否要求用户显式给参数（`n=256,bpp=24`）？**倾向强制显式，不猜。**
7. **导出的原子性**：BG 导出到一半用户关窗口怎么办？倾向"写临时文件 + rename"（与 04 的保存事务同构），需与 04 对齐细节。
8. ~~`E6xxx` 段的归属~~ **已裁决**：不属 DSL 诊断体系，独立成 `E8xxx` 段，登记于 `00-architecture-overview §12`。
