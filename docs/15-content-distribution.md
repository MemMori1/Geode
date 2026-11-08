# 15 — 内容分发（模板库 / 主题库 / magic 库 / 更新器）

> 本文规范 `geode-content` crate：内容包格式、托管方案、签名信任链、本地内容目录、安装事务、内容安全、自身更新器、许可证边界。
> 主题**文件格式与 token 语义**由 `见 16-ui-shell-theme-i18n §3.4/§4.5` 拥有；本文只管主题**包的打包、分发与校验**。项目/工作区格式见 `见 17-project-and-persistence`。

---

## 1. 目标与非目标

| # | 目标 | 可验证判据 |
|---|---|---|
| G1 | 应用内发现并一键安装六类内容（模板 / magic / 主题 / YARA / 插件 / 可视化器预设） | 冷启动 → 装好模板 → 打开 zip 自动匹配，全程 ≤ 30s |
| G2 | 内容来源可验证、可追溯、可撤回 | 每个已装包详情页有 `trust + 签名者指纹 + 发布时间` |
| G3 | 安装/更新/回滚是**事务**，失败不留半装状态 | 随机断电注入 100 次，状态只有"旧"或"新"两态 |
| G4 | 阶段 1 零后端成本 | 只有静态文件 + 一个 CDN，月度运维 ≈ 0 |
| G5 | 内容注入绝不越过内核沙箱 | 恶意包只能降级自身，不能拖垮进程（对齐 00 §9） |

**非目标（明确否决）**：不做账号/评分/评论/付费内容（< 200 包时评分信噪比为负，付费与"离线可用"冲突）；不做自动热替换式应用更新（§4.7：Windows 文件占用 + macOS 公证封印）；阶段 1 不对 `index.json` 本身签名（§10.1）；不做内容的运行时托管——模板执行归 `见 02-dsl-spec §7.2`，插件加载归 `见 14-plugin-system`。

---

## 2. 依赖与边界

```
app ──► geode-ui ──► geode-content ──► geode-format（manifest 解析/版本判定）──► geode-core
                      │  ├──── geode-bspec（仅编译前端：模板语法门禁）
                      │  └──── geode-plugin-host（仅读 ABI 元数据，不 dlopen）
```

- `geode-content` **不依赖 GUI**（00 §2 硬约束）：只暴露状态机 + 进度回调 + 诊断，对话框由 `geode-ui` 画。
- 网络用 `ureq`（阻塞、无 tokio，符合 00 §2：无大量异步 I/O），全部跑在 job 线程（00 §6 规则 1）。
- 密码学 `ed25519-dalek` + `sha2`；归档 `zip`（deflate only）+ 自研路径/配额层（§4.9）。**不引入** Sigstore/cosign（体积 + 强依赖 OIDC）。

| 事项 | 拥有者 | 本文态度 |
|---|---|---|
| BinSpec 语法 / 编译 / 执行预算 | `02-dsl-spec` §7 | 只做门禁 |
| AI 模板 provenance 字段 | `03-ai-dsl-generation` §10 | 复用为 `manifest.origin` |
| 主题 token 结构 | `16-ui-shell-theme-i18n` §3.4 / §4.5 | 只校验 schema + token 完备性 |
| magic 匹配 / YARA 引擎 / 插件 ABI | `10` / `12` / `14` | 只定义包格式与配额 |
| 布局持久化、最近文件 | `17-project-and-persistence` | 不涉及 |
| 安装包制作、代码签名 | `18-engineering-plan` | 只定义更新器协议 |

---

## 3. 数据结构与接口

### 3.1 六类内容包

统一格式 `.geopkg`：**单层 zip，deflate，无加密、无符号链接、无嵌套 zip**，必含 `manifest.toml`（UTF-8，≤ 64KB）。

| 类型 | id 前缀 | 包内结构 | 落点 | 特殊限制 |
|---|---|---|---|---|
| BinSpec 模板 | `tpl.` | `manifest.toml`、`spec/*.bspec`、`samples/*`、`preview.png`、`LICENSE`、`README.md` | `content/templates/<id>/<ver>/` | 源码 ≤ 512KB；`import` 深度 ≤ 8 |
| magic 库 | `magic.` | `manifest.toml`、`magic.toml`、`preview.png` | `content/magic/<id>/<ver>/` | 条目 ≤ 10 000；单 pattern ≤ 256B |
| 主题包 | `theme.` | `manifest.toml`、`theme.toml`、`preview.png` | `content/themes/<id>/<ver>/` | ≤ 128KB；**禁止内嵌字体**（体积 + 授权） |
| YARA 规则包 | `yara.` | `manifest.toml`、`rules/*.yar` | `content/yara/<id>/<ver>/` | 规则 ≤ 5 000；单规则编译 ≤ 2s |
| 插件二进制 | `plugin.` | `manifest.toml`、`lib/<platform>/<n>.geoplug`、`abi.toml` | `content/plugins/<id>/<ver>/` | 只解压当前平台那一个；≤ 128MB |
| 可视化器预设 | `vis.` | `manifest.toml`、`preset.toml` | `content/visualizers/<id>/<ver>/` | **纯数据**，只能参数化内置可视化器 |

**决策：可视化器必须是数据而不是代码。** 需要新渲染器就写插件；否则"预设"会变成第二条代码分发通道，绕过 §4.9 的插件警告流程。

### 3.2 最小元数据（`manifest.toml`）

```toml
schema = 1                                     # manifest 格式版本
id = "tpl.zip"                                 # ^[a-z][a-z0-9-]{1,46}\.[a-z0-9-]{1,32}$
type = "template"                              # template|magic|theme|yara|plugin|visualizer
version = "1.4.0"                              # semver，禁 build metadata
name = { "en-US" = "ZIP Archive", "zh-CN" = "ZIP 归档" }   # 至少 en-US
description = { "en-US" = "Local headers, central directory, EOCD" }
authors = [{ name = "…", url = "…", key_id = "ed25519:9f2c3a11" }]
license = "MIT"                                # SPDX 表达式，必填
applies_to = { magic = ["zip"], mime = ["application/zip"], ext = ["zip"] }
samples = [{ path = "samples/min.zip", sha256 = "b1e4…", size = 118 }]   # 模板类必填 ≥1
preview = { path = "preview.png", sha256 = "77aa…" }   # 512×256 PNG ≤ 256KB
deps = [{ id = "magic.common", req = "^2.1" }]          # 深度 ≤ 2
min_app = "0.7.0"                              # 低于此版本拒装（明确提示）
content_api = 1                                # 宿主不认识即拒装
origin = { kind = "human" }                    # human|ai|imported（imported 追加 from/ref/upstream_license）
files = [{ path = "spec/zip.bspec", sha256 = "0c19…", size = 4210 }]   # 除 manifest 外逐文件
```

**容器约束（`23 §3.4`）**：`.geopkg` 是**单层 zip**，第 1 个 entry 必须是 `manifest.toml` 且方法 0（STORED），其余 entry 只允许 deflate；ZIP64、加密位、symlink/hardlink、嵌套 zip、data descriptor（bit 3）一律拒装（`EB002`）。**`schema` 与 `content_api` 不得合并**：前者管 manifest 的字段格式，后者管宿主运行时能力面，合并会让"加一个只影响字段的补丁"变成"所有旧宿主拒装"。

- `files[]` 逐文件哈希是必须的：让"部分损坏"能定位到文件，也让"只解压当前平台"不必放弃完整性。
- `license` 一律 SPDX 表达式；无法表达时 `LicenseRef-*` + 包内 `LICENSE` 全文。
- **manifest 里不存在任何可执行钩子字段**（`post_install`/`hook`/`script`），出现即拒装 `EA102`。

### 3.3 索引 `index.json`

```json
{ "schema": 1, "channel": "stable",
  "generated_at": "2026-11-08T04:00:00Z", "expires_at": "2026-11-09T04:00:00Z",
  "app": { "latest": "0.9.2", "releases": [
    { "version": "0.9.2", "platform": "macos-aarch64", "url": "https://…/macos-aarch64.pkg",
      "sha256": "3f9c…", "size": 48213120, "min_os": "13.0", "notes_url": "https://…" } ] },
  "packages": [
    { "id": "tpl.zip", "type": "template", "latest": "1.4.0", "license": "MIT", "tags": ["archive"],
      "applies_to": { "magic": ["zip"], "mime": ["application/zip"] },
      "versions": [
        { "version": "1.4.0", "url": "https://…/tpl.zip/1.4.0.geopkg", "sha256": "0c19…", "size": 18234,
          "min_app": "0.7.0", "content_api": 1, "yanked": false, "published_at": "2026-10-02T09:11:00Z" } ] } ] }
```

限值：原始 ≤ 2MB（gzip ≤ 300KB）、`packages` ≤ 5 000——超出即触发 §4.2 阶段 3 升级判据。客户端**不信任** `latest`，自行按 semver 从 `versions` 选最高兼容版本。

### 3.4 本地内容目录

```
~/.local/share/geode/                      # macOS: ~/Library/Application Support/geode
│                                        # Windows: %LOCALAPPDATA%\hex
├── state/content.toml                   # 唯一事实来源（已装内容 + 激活指针）
├── state/content.lock                   # 进程间互斥
├── content/
│   ├── templates/tpl.zip/{1.3.2,1.4.0}/{manifest.toml,spec/,samples/,preview.png}
│   ├── magic/magic.common/2.1.0/        themes/theme.nord/1.0.0/
│   ├── yara/yara.malware/0.9.0/         visualizers/vis.coords/1.1.0/
│   └── plugins/plugin.diffview/0.3.0/lib/macos-aarch64/
├── cache/registry/{index.json,index.meta}    # index.meta 存 etag / fetched_at
├── cache/downloads/<sha256>.part
└── logs/content.log                     # 只记事件与 id，不记带 token 的 URL
```

`GEODE_CONTENT_DIR` 可整体重定向（测试 / 便携模式）。

```toml
# state/content.toml —— 激活 = 改这里一行指针，不移动任何文件
schema = 1
[[installed]]
id = "tpl.zip"                      type = "template"
active = "1.4.0"                    # null = 已装但被禁用
versions = ["1.3.2", "1.4.0"]       # 磁盘上实际存在
history = ["1.3.2", "1.4.0"]        # 激活顺序，回滚依据，最多 8 条
source = "registry"                 # registry | file | builtin
trust = "author_signed"             # unsigned | author_signed | maintainer_signed
verified = { sha256 = "0c19…", key_id = "ed25519:9f2c3a11", at = "2026-10-03T10:22:00Z" }
masked_builtin = true               # 是否遮蔽同名内置版本
pinned = false                      # true → GC 永不删除其 inactive 版本
```

### 3.5 接口草案

```rust
pub struct PackageId(pub String);            // "tpl.zip"
pub struct ContentVersion(pub semver::Version);
pub enum ContentKind { Template, Magic, Theme, Yara, Plugin, Visualizer }
pub enum TrustLevel { Unsigned, AuthorSigned, MaintainerSigned }
pub struct Installed { pub id: PackageId, pub kind: ContentKind, pub active: Option<ContentVersion>,
    pub versions: Vec<ContentVersion>, pub trust: TrustLevel, pub source: Source }
pub struct Resolved { pub dir: PathBuf, pub manifest: Arc<Manifest>, pub builtin: bool }

pub trait ContentStore {
    fn list(&self, kind: Option<ContentKind>) -> Vec<Installed>;
    fn resolve(&self, kind: ContentKind, id: &PackageId) -> Option<Resolved>;   // 用户 > 内置
    fn resolve_for(&self, hint: &FormatHint) -> Vec<Resolved>;                  // magic/mime/ext
    fn install(&mut self, pkg: StagedPackage, opts: &InstallOpts) -> Result<InstallOutcome, ContentError>;
    fn activate(&mut self, id: &PackageId, v: &ContentVersion) -> Result<(), ContentError>;
    fn rollback(&mut self, id: &PackageId) -> Result<ContentVersion, ContentError>;
    fn uninstall(&mut self, id: &PackageId, keep: usize) -> Result<(), ContentError>;
    fn gc(&mut self, policy: &GcPolicy) -> GcReport;
}

pub trait Registry {                         // 全部在 job 线程，可取消
    fn index(&self, force: bool) -> Result<Arc<Index>, FetchError>;
    fn download(&self, id: &PackageId, v: &ContentVersion, cancel: &CancelToken,
                progress: &mut dyn FnMut(u64, u64)) -> Result<StagedPackage, FetchError>;
}
```

`InstallOpts{ allow_missing_deps: false, allow_unsigned: false, activate: true }`；`VerifyReport{ digest, signature, schema, file_count, unpacked_bytes }` 直接喂给详情页。

---

## 4. 核心算法 / 流程

### 4.1 读侧加载语义

| 类型 | 何时加载 | 失败降级 |
|---|---|---|
| 模板 | 打开文件时按 `applies_to` 匹配 → 提示"用 tpl.zip 解析？"（**默认不自动跑**） | 编译失败 → 标 `broken`，其它候选不受影响 |
| magic | 启动时构建只读 trie（≤ 10 万条） | 单库失败 → 跳过并告警 |
| 主题 / 可视化器预设 | 启动 + 热重载（见 16 §4.5）/ 结构树出现 `#[visualize]` 时查表 | 保持上一主题 / 用内置默认参数 |
| YARA | 用户显式加载 / 按 profile | 单规则编译失败 → 仅禁用该规则 |
| 插件 | **默认不加载**，逐个启用（§4.9） | 失败 → 打标记，下次启动跳过 |

### 4.2 托管方案：三阶段，"先别造服务端"

**阶段 1（M5，静态索引）**：`index.json` 由 CI 从各内容仓库 `manifest.toml` 反向聚合，作为 GitHub Release asset 或任意 CDN 同一路径发布；客户端 `GET` + `If-None-Match`/`If-Modified-Since`，本地缓存 6h（`expires_at` 优先），离线可用过期缓存但 UI 标"索引已过期 N 天"；下载走 `cache/downloads/<sha256>.part` + `Range` 断点续传，校验通过才 rename。**主动放弃**：搜索排序、下载统计、灰度渠道（只有 `stable`/`beta` 两个静态文件）、按区域路由（交给 CDN）。

**阶段 2（M5 末期，签名 + 撤回）**：每个 `.geopkg` 附 `.sig`（§4.3），`index.json` 附 `index.json.sig`；`revocation.json`（≤ 64KB）**TTL 15 分钟、客户端每天至少拉一次**，与索引 6h TTL 解耦——撤回是安全事件通道，不能被长缓存拖住。撤回强度：`hard`（安全/兼容危害 → 已装版本立即禁用 + 提示条要求卸载）、`soft`（不再推荐 → 详情页标"已撤回"）。

**阶段 3：什么信号才上服务端**

| 信号 | 阈值 | 静态方案的痛点 |
|---|---|---|
| 索引体积 | `index.json` > 2MB（≈ > 5 000 包） | 每次冷启动解析全部版本，§5 预算破 |
| 发布频率 | 每周新增 > 30 包，索引每日全量重写 | CDN 缓存全失效，流量 ×N |
| 搜索质量 | Top 50 搜索词"无结果"占比 > 25% | 无排序、无同义词/拼音 |
| 权限 / 撤回时效 | 出现付费或企业私有内容；需 < 15 分钟下架 | 无鉴权；轮询粒度不够 |

到阶段 3 也只做**索引 + 搜索**服务端，**包体永远走静态 CDN**——包体走服务端带宽成本直线上升、收益为零，这条不让步。

### 4.3 信任链

```
根密钥 root-2026（ed25519，离线，双人分片保管）
   │ 签发 publishers.json（客户端内置，可远程更新）
   ├──► maint-a / maint-b（CI 用，签 index / revocation）
   └──► author-XXXX（社区作者，人工核验后入册）
                   │ 签
                   └──► <pkg>-<ver>.sig := 签名(SHA256(.geopkg 字节))
```

`.sig` = `{ "schema":1, "alg":"ed25519", "key_id":"ed25519:9f2c3a11", "sha256":"0c19…", "sig":"base64(64B)", "signed_at":"…" }`。**签名对象永远是摘要，不是文件本身**：签名"规范字节"要求严格的 JSON/TOML 规范化（空白、键序、float 表示处处是坑），签摘要把规范化问题彻底消灭，且不丢语义。

**验证顺序（严格执行，任一步失败即中止并删 tmp）**：

```
① 下载 .part
② 哈希   SHA-256 == index.versions[].sha256             失败 → EA001 摘要不符
③ 签名   验签(摘要, .sig) 且 key_id ∈ 受信集            失败 → EA002 签名不符 / EA003 密钥未知
④ 解包   路径穿越 + 配额检查 → 解到 staging              失败 → EA004 穿越 / EA005 炸弹
⑤ schema manifest + files[] 逐文件哈希 + 类型专属校验     失败 → EA101~EA106
         （模板 compile_check / 主题 token 完备 / YARA 编译 / 插件 ABI）
⑥ 安装   fsync → rename 到 content/ → 原子更新 state.toml → 激活
```

顺序不可调换：先哈希后签名，保证签名永远作用在已知摘要上，绝不把未验证的大文件喂给解包器；先解包后 schema，因为校验需读内容，但解包已在受限配额下完成。

**轮换**：`publishers.json` 带 `not_before`/`not_after`；新密钥提前 90 天公布，旧密钥退役后再宽容 90 天，期间双密钥受信，**不需要**发新客户端。**吊销**：`revoked_keys = [{key_id, reason: compromise|retired|abuse, effective_at}]`；被吊销密钥签发的包降级为 `unsigned` 并明示"签名者密钥已吊销"，`content.allow_unsigned = false`（默认）时直接禁用。

**离线签名（维护者本地签）**：离线机器跑 `geode-content-sign`（单一静态二进制 ≤ 6MB）→ 挂载私钥介质（age 加密文件 + 口令，或硬件令牌）→ `geode-content-sign sign --pkg tpl.zip-1.4.0.geopkg --key ~/keys/author-9f2c.age` 产出 `.sig` + 一行可粘贴的 sha256 → 联网机器上传两个文件。**私钥永不进 CI**；CI 只持 `maint-*`，其泄露最坏是"索引被篡改"，仍被包级 `.sig` 兜住（改索引不能让签名不符的包变合法）。

### 4.4 安装事务、原子切换、回滚、GC

```
解包到 tmp/.staging-<uuid>/ → 校验全过
rename → content/<kind>/<id>/<ver>.partial      # 同文件系统，单次 rename
fsync(dir) → 写 state.toml（tmp + rename）= 激活   # 文件先落地，指针后翻
rename …/<ver>.partial → …/<ver>                # 收尾；未激活即被 GC 收走
```

**决策：激活 = 改 `state.toml` 里一行指针，不移动任何文件。** 三个好处：原子性只需保证一个 ≤ 8KB 文件的 rename；回滚 = 指针翻回（0 拷贝）；"已安装未激活"成为有意义的状态（禁用某版本而非删除）。

- **回滚**：按 `history` 取上一次激活版本；若已被 GC → `EA010 无可回滚版本`。
- **GC**（后台 job，可取消）：保留 `active` + `history[0]` + 所有 `pinned`；其余 inactive 且距今 > 30 天者删除；某 id 只剩 1 个版本时永不删除。删除前必须持 `content.lock`，且目标 `canonicalize` 后仍须在 `content/` 之下（防符号链接逃逸）。

### 4.5 优先级：用户内容 vs 内置内容

内置内容在只读目录（`app/resources/content/`，Windows 在 `Program Files`、macOS 在 `.app/Contents/Resources`）。**首启不复制到用户目录**——否则应用升级后会出现"内置已更新、用户副本还是旧的"双份地狱。解析顺序：`content/<kind>/<id>/<active>/` → `<bundle>/content/<kind>/<id>/` → `None`。

- 用户装了同 id ⇒ 用户版本生效，`masked_builtin = true`，详情页写"覆盖了内置 v1.2.0"。
- 用户版本 `content_api` 不兼容 ⇒ 回退内置并提示 `EA012`。**不静默**：静默回退会让用户以为改动无效。
- 卸载 ⇒ 自动回退内置，提示"已恢复内置版本 v1.2.0"；内置内容**不可卸载/修改**，只能被遮蔽，"卸载"按钮对内置版本置灰。

### 4.6 依赖解析

图深度 ≤ 2（模板 → magic 库），不做通用 SAT 求解。贪心：按 id 取满足全部约束的最高版本；不可同时满足 → `EA020 版本冲突`，逐条列出约束来源（"`tpl.zip 1.4.0` 需要 `magic.common ^2.1`，而 `yara.x 0.3` 需要 `~2.0.9`"）；DFS 检测环 → `EA021 循环依赖`。依赖缺失时默认拒装（`InstallOpts.allow_missing_deps = false`），用户在错误框勾选后可"仅装主包"并标 `degraded`。

### 4.7 自身更新器（只做检查与提示）

**决策：不做热替换。** Windows 运行中的 `.exe` 被独占锁定，自替换要走"重命名 + 重启"的 hack，在杀软眼里与恶意行为同构（项目本就有误报问题，见 `01-competitor-feature-research §3.9`）；macOS 替换自身会破坏代码签名与公证封印（bundle 内任一字节变化即 `codesign --verify` 失败）。统一交给平台安装器，比三套 hack 可靠得多。

| `updater.mode` | 行为 |
|---|---|
| `notify`（**默认**） | 启动后 30s + 每 24h 检查一次；有新版 → 状态栏小圆点 + 一次性 toast，点击打开 `notes_url` |
| `download` | 静默下载到 `cache/updates/`，校验 sha256，提示"重启以完成安装"→ 退出并拉起安装器 |
| `off` | 完全不联网 |

| 平台 | 交付物 | 应用方式 |
|---|---|---|
| Windows | 签名 MSI（Major Upgrade） | `msiexec /passive`，应用先退出；MSI 自带回滚 |
| macOS | 公证 `.pkg`（`productsign` + `notarytool`） | `open -W <pkg>`，安装器整体替换 bundle |
| Linux | `tar.zst` + `current` 软链接 | 解到 `versions/<ver>/` → symlink rename 切换 → 提示重启 |

**失败回滚**：下载/校验失败 → 删临时文件、保留旧版、toast 一次后 24h 不再提示；安装失败 → Windows/MSI 与 macOS 安装器各自回滚，Linux 因"软链接切换是最后一步"天然保证；**启动失败检测** → 新版本首次启动写 `state/upgrade.toml = {from,to,attempts}`，进入主窗口 10s 后清除，连续 2 次未清除则提示"上一版本仍可用"并给旧安装包入口（**不自动降级**：崩溃循环里来回切换会掩盖真实 bug）。

### 4.8 许可证与出处审核（ImHex GPL 红线）

内容进索引前必须过 `geode-content-lint`：① `license` 存在且是合法 SPDX 表达式；② 允许清单 `MIT`、`Apache-2.0`、`BSD-2/3-Clause`、`CC0-1.0`、`Unlicense`、`MIT OR Apache-2.0`；③ 出现 `GPL-*`/`AGPL-*`/`LGPL-*` → **硬拒绝** `EA301`；④ `origin.kind = "imported"` 必须带 `from`/`ref`/`upstream_license` 且上游许可证也在允许清单内；⑤ 与已知 ImHex-Patterns 语料做 5-gram Jaccard 相似度（阈值 0.6），命中转人工复核（防"洗稿"）。

**为什么 ImHex 的模板与插件绝对不可搬运**（最容易犯的错，写清楚）：

- **模板是受版权保护的表达，不是"格式事实"**。ImHex-Patterns 整体以 GPL-2.0 发布，一个 `.hexpat` 里的结构命名、字段拆分、注释、断言、组织方式都是作者的创作表达；翻译成 `.bspec` 属于**改编**，仍受 GPL-2.0 约束。
- **分发会传染**。GPL-2.0 §2(b) 要求"基于该作品的整体"按 GPL 授权。把 GPL 模板打进 MIT/Apache 双许可发布物，或由官方索引分发，等于声称整个分发物可按 GPL 条款使用，与我们的许可证声明直接冲突；"聚合（mere aggregation）"抗辩在官方模板库这种**集成式分发**里站不住。
- **插件更严重**：`.geoplug` 是动态库，通过固定 ABI 与宿主构成单一作品，GPL-2.0 下分发插件几乎必然要求宿主也 GPL。
- **唯一安全做法**：格式知识（magic 字节、字段语义）可独立重写——字节签名是事实性数据，不受版权保护；但**不许照搬其文件结构、注释文字、字段命名清单**。来源为 ImHex 的提交一律拒绝。

**Kaitai**：`.ksy` 是 Apache-2.0，可商用可再分发；但 `ksy → bspec` 转换产物是改编作品，必须写 `origin = { kind = "imported", from = "kaitai", ref = "<name>@<ver>", upstream_license = "Apache-2.0" }`、把上游 `NOTICE`/版权声明保留进包内 `LICENSE`，若 `.ksy` 自带 `license:` 覆盖则以上游为准。**010 Editor 模板是商业许可，同样禁止导入。**

**UI 义务**：详情页显示 `license`、作者、签名者指纹、`origin`；列表页至少显示 `license` 徽章，不得隐藏。

**`origin.distribution` 与迁移产物**：来自 24 的转换器产物必须带 `distribution = local-only | forbidden`。`local-only` 允许本地安装但**永不进索引**（索引构建器直接过滤，`EB402`）；`forbidden` 的转换器拒绝产出字节（`EB403`）。`manifest.origin` 还必须有 `converter` 与 `confidence` 两个字段，否则 `geode-content-lint` 判不合格。

### 4.9 内容安全（安装期防御）

| 攻击 | 防御 | 限值 |
|---|---|---|
| zip slip（`../`、绝对路径、盘符、符号链接） | 每个 entry：拒绝 `/`、`\`、`X:` 开头；拒绝含 `..` 段与 `\0`；段数 ≤ 8、总长 ≤ 1024；拒 symlink/hardlink 类型；`canonicalize(目标)` 必须仍在 staging 根下 | 违规整包拒装 `EA004` |
| zip 炸弹 | **流式解压边解边计数**（不信 header 的 `uncompressed_size`，可伪造） | 模板/主题/YARA：总量 ≤ 32MB、单文件 ≤ 8MB；插件 ≤ 128MB、单文件 ≤ 64MB；条目 ≤ 256；单条目压缩比 ≤ 100:1、总比 ≤ 200:1 |
| 磁盘满 | 解包前 `statvfs` 预检 `need = size*1.2 + 4MB` | 不足 `EA006` 并给出所需空间 |
| 病态 YARA 正则 | 安装时逐规则编译 + 超时 | 单规则 ≤ 2s、整包 ≤ 10s、规则 ≤ 5 000 |
| 模板炸弹（嵌套/递归） | 静态检查 struct 嵌套 ≤ 512、`import` 深度 ≤ 8、源码 ≤ 512KB；运行期由 `02-dsl-spec §7.2` Budget 兜底 | 超限 `EA103` |
| 插件 = 任意代码 | **装 ≠ 启用**：安装时红框显示签名者指纹、请求能力（provider/processor/panel/view）、哈希，勾选"我了解这是本机任意代码"才继续；启用是第二个独立开关；签名者变更则重新警告 | 见 `14-plugin-system` |
| magic 库拖慢匹配 | 条目 ≤ 10 000、pattern ≤ 256B、编译期建 trie | 超限 `EA105` |

---

## 5. 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 拉取 + 解析 `index.json` | < 300ms p50 / 900ms p95 | 2MB 原始 / 300KB gzip，流式反序列化 |
| 索引缓存冷读 / 常驻内存 | < 20ms / ≤ 8MB | 5 000 包 / 20 000 版本 |
| 列表搜索（按键即筛） | < 8ms | 5 000 条，name/description/tags 小写包含 |
| 单包安装（1MB） | < 1.5s | 不含网络；哈希+验签+解包+schema+rename |
| 解包吞吐 / SHA-256 / 验签 | > 60MB/s / > 800MB/s / < 0.3ms | deflate 单线程；`sha2` + SHA-NI；每包一次 |
| 启动内容扫描 | < 30ms | 200 包；**只读 state.toml + stat，不重算哈希** |
| magic 库构建 / GC / 撤回检查 | < 40ms / < 100ms / < 50ms | 10 000 条 / 1 000 文件 / 64KB |

进 `bench/content/`。两条硬门禁：5 000 包索引解析；"1GB 解压炸弹必须被拒且判定 < 200ms"（防防御逻辑本身变慢导致 DoS）。

---

## 6. 失败模式与降级

| 失败 | 检测 | 用户可见 | 数据后果 |
|---|---|---|---|
| 网络不可达 / 索引过期 > 7 天 | 连接 5s、读 15s 超时；`expires_at` | 状态栏"内容库离线" / 黄条"索引已过期 N 天"，**不弹框** | 无，已装内容照用 |
| 索引解析失败 | serde | 回退上一份缓存副本，`EA007` | 无 |
| 摘要不符 / 签名不符 | ②③ | 中止、删 `.part`；红框"包已被篡改或签名不正确" | 无 |
| 密钥未知（签名有效） | ③ | "签名者不受信任"，可开 `allow_unsigned` 重试 | 无 |
| 密钥已吊销 | revocation | 已装包立即禁用；`hard` 时提示条要求卸载 | 内容保留不加载 |
| 版本被撤回 | 索引 `yanked` | 新装禁止；已装不动，详情页标"已撤回" | 无 |
| zip slip / 炸弹 | ④ | 整包拒装，附具体 entry 名 | staging 全删 |
| schema 失败 | ⑤ | 指出文件与原因（"第 3 行 struct 未闭合"） | staging 全删 |
| 依赖缺失 / 版本冲突 | 解析 | 列出缺失项 + "仍然安装（功能受限）" / `EA020` 逐条冲突来源 | 装但标 `degraded` / 不装 |
| 磁盘满 | 预检 / 写错误 | `EA006` + 所需空间 | staging 删 |
| `min_app` 不满足 | install 前 | "该内容需要 v0.9+，当前 v0.8.1" | 不装 |
| 插件崩进程 | 见 `14-plugin-system` | 插件禁用，下次启动不加载 | 其它内容不受影响 |
| `state.toml` 损坏 | 解析失败 | 备份为 `.bad`，按磁盘扫描重建 | 已装文件不删 |
| 两实例同时安装 | `content.lock` | 第二实例进只读，"另一实例正在安装内容" | 无 |
| 更新下载失败 / 新版本连崩 2 次 | 校验 / `upgrade.toml` | 保留旧版，24h 不再提示 / "上一版本仍可用" + 旧包入口 | 无（不自动降级） |

总原则（对齐 00 §9）：**内容层任何失败都不得阻止应用启动与打开文件**。最坏是"没有可用模板"，不是"打不开软件"。

---

## 7. 与其它子系统的契约

| 子系统 | 它读本文 | 本文读它 |
|---|---|---|
| `geode-ui`（见 16） | 列表模型、进度事件、`VerifyReport`、`ContentError` | 无；UI 负责对话框与主题热重载触发 |
| `geode-bspec`（见 02） | 模板包 `spec/*.bspec` 路径、`content_api` | `compile_check(src) -> DiagList`（预算 20ms） |
| `geode-ai`（见 03） | 模板库作为 RAG 语料（§4.3 正循环） | `Manifest.origin` 复用其 provenance；只读 `resolve_for`，不写内容目录 |
| `geode-analysis`（见 10） | magic 包 → 只读索引 | 条目 schema 由本文定义 |
| `geode-yara`（见 12） | 规则包 | `compile_check(rules)` 安装前门禁 |
| `geode-plugin-host`（见 14） | 插件落点 + `abi.toml` | `abi_version` 兼容矩阵；启用状态在 `state.toml.plugins_enabled` |
| `geode-visualizers`（见 13） | 预设包 `preset.toml` schema | 内置可视化器 id 清单 |
| `geode-project`（见 17） | 工作区 `workspace.lock` 可引用 id+version | 只提供 `resolve(id, ver)`，不写 project 文件 |

**共享状态**：只有 `state/content.toml` 一个可写文件。其它子系统一律走 `ContentStore` 只读查询，**禁止**直接 `std::fs` 遍历 `content/`。

**诊断码**：本文用 `EAxxx`（内容分发，含更新器 `EA3xx`）。已登记于 `00-architecture-overview §12`。

---

## 8. 配置项与默认值

| 键 | 默认 | 说明 |
|---|---|---|
| `content.registry_url` | `https://content.geode.invalid/stable/index.json` | 索引 URL |
| `content.mirror` / `content.proxy` | `null` / `null` | 覆盖包下载前缀（企业内网）/ 覆盖系统代理 |
| `content.channel` | `"stable"` | `stable` \| `beta` |
| `content.index_ttl_hours` | `6` | 索引 TTL（`expires_at` 优先） |
| `content.revocation_refresh_hours` | `24` | 撤回列表刷新（≥ 1） |
| `content.auto_apply_template` | `false` | 打开文件时自动跑匹配模板（默认只提示） |
| `content.auto_check_updates` | `true` | 检查内容更新（只检查不装） |
| `content.auto_install_updates` | `false` | 自动安装内容更新 |
| `content.allow_unsigned` | `false` | 允许无签名 / 未知签名者内容 |
| `content.max_index_bytes` | `2097152` | 索引硬上限，超出拒解析 |
| `content.max_unpacked_bytes` | template 32MB / theme 4MB / yara 16MB / plugin 128MB | §4.9 |
| `content.gc.keep_versions` / `max_age_days` | `2` / `30` | active + 上一版；inactive 版本过期删除 |
| `content.dir` | 平台默认 | 覆盖 `GEODE_CONTENT_DIR` |
| `updater.mode` | `"notify"` | `notify` \| `download` \| `off` |
| `updater.check_interval_hours` / `startup_delay_seconds` | `24` / `30` | 自身更新检查；避开启动路径 |

全局配置文件位置沿用 `见 17-project-and-persistence` 的约定（`~/.config/geode/config.toml`）。

---

## 9. 测试策略

| 层 | 内容 | 门禁 |
|---|---|---|
| 单元 | manifest/索引 serde 往返、semver 约束求解、路径检查函数（表驱动 60+ 恶意用例） | 路径检查函数 100% 分支覆盖 |
| 集成（本地 fake CDN） | `file://` 与本地 `tiny_http` 双后端：正常装、断点续传、ETag 304、索引过期、撤回下发 | 每 PR 全跑 |
| 篡改 | 翻转包内 1 字节（`EA001`）、改 manifest 版本（`EA001`）、换 `.sig`（`EA002`）、未受信密钥重签（`EA003`） | 必须全部拒绝 |
| 恶意样本 | zip slip 五种（`../x`、`/etc/x`、`C:\x`、`a/../../x`、symlink）、炸弹三种（1GB 零字节、10 000 条目、200:1）、8KB 文件名、`\0` 注入 | 全部拒装且判定 < 200ms |
| fuzz | manifest / index / magic.toml / theme.toml / preset.toml 解析器（10min/PR，1h/夜） | 不 panic、不 OOM |
| 事务性 | 在 rename 与 state 写入前后 kill 进程，重启后必为"旧"或"新" | 100 次随机注入 0 次半装 |
| 性能 / 许可证 / 确定性 | §5 全部；`geode-content-lint` 全量跑 §4.8 五条；同输入两次打包字节一致 | 退化 > 20% 红灯；GPL 命中 fail；打包必须可复现 |

---

## 10. 开放问题

1. **索引是否值得签名**：索引只是"指针"，最坏后果是引导到旧版本包（包级签名仍拦得住）。倾向 v1 只做包级签名 + revocation，索引签名延后到阶段 3。
2. **密钥保管硬件**：现方案是 `age` 加密私钥 + 口令。是否必须采购支持 ed25519 的硬件令牌（Nitrokey 3 / YubiKey FIDO2）？成本与运维复杂度待评估。
3. **是否改用 Sigstore/cosign**：透明的 Fulcio 身份 + Rekor 可审计日志 vs 强依赖 OIDC 与在线服务，与"离线可用"部分冲突。
4. **`magic.*` 拆不拆包**：一个 10 万条的 `magic.common`，还是 `magic.images`/`magic.exec` 细分？细分利于按需加载，但依赖解析与更新提示更复杂。
5. **模板的 semver 语义**：什么算 breaking？字段重命名对 AI RAG / 用户断言是 breaking，对"看一眼结构"不是。需要一页约定，否则版本号退化成序号。
6. **可视化器预设的表达力边界**：一旦要"把多个字段拼成调色板"，就需要小型表达式语言——等于又造一门 DSL。倾向限制为"参数白名单 + 字段路径引用"。
7. **离线导入信任策略**：拖入无签名 `.geopkg` 时是否允许"仅本机使用"（不入受信集、每次启动标黄）？倾向允许 + 持续标黄。
8. ~~`E6xxx/E7xxx` 需登记~~ **已解决**：统一为 `EAxxx`，登记于 `00-architecture-overview §12`。
9. **卸载是否连带删除用户数据**：删模板时用户基于它写的补丁/备注要不要删？倾向不删，但对话框必须说清楚。
