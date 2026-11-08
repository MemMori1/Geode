# 23 — 自有文件格式规范（`.geows` / `.geoproj` / `.geopkg` / `.geopatch` / `.gmagic` / `.geojournal` / `.geolock`）

> **状态：正式规范（normative），不是设计说明。** 验收判据只有一条：**第三方工程师只读本文 + `tests/vectors/`，就能写出可读可写的实现。**
> 本文**拥有**：容器布局、字段表、编码与端序、版本语义、迁移与弃用规则、校验和/签名位置、schema 工件与向量约定。**不拥有**：字段业务语义与写入时机（各拥有者）、算法实现、UI 呈现。
> 上游硬契约 `见 00 §3`、`见 00 §9`、`见 00 §12`；字段来源 `见 17 §3.2–§4.9`、`见 15 §3.1–§4.3`、`见 04 §4.5`、`见 10 §4.5`。**既有文档给过的字段一律沿原名、原义、原单位**；本文只做规范化、补全、明确约束。冲突处以本文为准并必须回填（§10 末段）。

## 0 决策速览

| # | 决策 | 理由 |
|---|---|---|
| D1+D2 | 落盘 `schema_version` 只承载**主版本**（次/补丁记工件 `rev`）；文本 TOML 1.0.0 + UTF-8(无 BOM) + LF，二进制一律**大端** | 现存文件里它是整数，改类型等于废掉所有旧文件；端序不写死，第三方实现必然在首例翻车 |
| D3 | 未知字段：`config` **保留**、项目文件**忽略+告警**（EB007）；`TooNew` **拒绝且不写盘**（EB001） | 静默忽略后写回是最常见的毁数据路径（`见 17 §4.4`） |
| D4 | 二进制记录帧统一 `kind u8 │ seq u64 │ len u32 │ payload │ crc32 u32` | 两种二进制格式共用一套帧，校验器只需一份实现 |
| D5 | `.geopatch` 载荷 = **定长 BE + 裸字节**；04 §4.5 的"自有 CBOR"裁决为**取消 CBOR** | CBOR 变长整数与 Map 键序无规范序，与"字节级可复现"冲突 |
| D6 | `.geojournal` 头由 `见 17 §4.6` 标称的 64B 修正为 **80B**（字段累加 73B，补 `reserved[7]`） | 标称值与字段表自相矛盾，第三方无法取舍 |
| D7 | `.geopkg` 的 `manifest.toml` **必须是第 1 个 entry 且 STORED** | 探测/流式校验/无 seek 解析全靠它；≤64KB 不值得压 |
| D8 | **校验收紧 = 次版本**：只对新写入生效，读侧 Warning（EB030）不拒绝 | 否则一次安全收紧让所有历史文件打不开 |
| D9 | 弃用 = 标记 → 告警**两个大版本** → 移除；入口 `geode migrate` / `geode fmt-project` | 给生态迁移窗口；迁移必须幂等可回滚 |
| D10 | 每格式每版有工件 `schemas/<format>/v<N>.json` 随发布打 tag；**测试向量是规范的一部分**，冲突以向量为准 | 生态要写模板、CI 要校验 schema，靠人读 Markdown 不行 |

## 1 目标与非目标

| # | 目标 | 可验证判据 |
|---|---|---|
| G1 | **每字节有归属**：要么在字段表里，要么是显式 `reserved`（写 0、读忽略） | 覆盖率检查，`reserved` 之外无未命名字节（§9.1） |
| G2+G3 | **一份规范两种实现**（Rust 与第三方对同一向量给出同一 verdict 与错误码）；**版本判定无歧义**（`Ok/Migrated/TooNew/Corrupt` 唯一） | conformance 全绿（§9.2）；每格式 ≥3 个损坏向量 |
| G4+G5 | **迁移可逆**（迁移前必有 `.bak`，`migrate(migrate(x)) == migrate(x)`）；**格式不漂移**（字段增删同步改本文+工件+向量） | property test + 回滚测试（§9.3）；CI 门禁：改 schema 不改本文即红灯 |

**为什么这份文档是生态基础设施**：① 别人要写 BinSpec 模板、在 CI 里校验 `.geoproj`、写 `.geopatch` 解析器（取证脚本、批量打补丁流水线），必须有一份可引用的规范；② 没有规范，字段会随实现漂移，而**漂移是静默的**——不报错，只是让别人的解析器少读一个字段；③ 签名、CRC、迁移这些安全机制各写各的，就是漏洞。
**非目标**：BinSpec 语法（`见 02`）；主题 token 语义（`见 16 §3.4`）；插件 ABI（`见 14 §3.1`）；为 IPS/IPS32/zip 背书——本文只规定它们**作为导出目标**时的约束（`见 04 §4.5`）。

## 2 依赖与边界

| 格式 | 本文节 | 字段语义拥有者 | 只读消费方 |
|---|---|---|---|
| `.geows` / `.geoproj` | §3.3 / §3.4 | `geode-project`（`见 17 §3.3/§3.4`） | `geode-ui`、`geode-content`、`geode-ai` |
| `.geopkg` / `.geopatch` | §3.5 / §3.6 | `geode-content`（`见 15 §3.1`）/ `geode-buffer`（`见 04 §4.5`） | `geode-plugin-host`/`geode-bspec`/`geode-yara`；`geode-diff`、`geode-project`（导出） |
| `.gmagic` | §3.7 | `geode-analysis`（`见 10 §4.5`） | `app`（编排）、`geode-ui` |
| `.geojournal` / `.geolock` | §3.8 / §3.9 | `geode-project`（`见 17 §4.6/§4.9`） | `app`（恢复编排） |

**实现落点**：新增 `geode-format`（仅依赖 `geode-core` + `serde`/`toml`/`crc32fast`/`blake3`/`sha2`/`zip`(deflate only)，**无业务逻辑**），供 project/content/buffer/analysis 复用；回填前先落 `geode-core::fmt`。**格式层不得依赖任何业务 crate**，否则第三方无法复用；**不引入**自动生成字段表的框架（字段表唯一来源是人写的本文与手写工件）。

## 3 数据结构与接口

### 3.1 通用约定（七种格式适用）

1. **文本**：UTF-8 无 BOM；写侧 LF、读侧接受 CRLF 并规范化；TOML 1.0.0；键名 `snake_case`；禁 `\0` 与除 `\t\n\r` 外控制字符（EB003）。
2. **二进制**：多字节整数**大端**；CRC-32 = IEEE 802.3（reflected poly `0xEDB88320`、init/xorout `0xFFFFFFFF`，即 zlib/zip 所用）。
3. **时间**：RFC 3339 UTC 带 `Z`、无小数秒、无偏移；本地时间禁止落盘。
4. **标识**：`doc_uuid`/`workspace_id`/`project_id`/`instance_uuid` 一律 ULID（26 字符 Crockford base32 大写）；二进制里落其 **16 字节**形式。
5. **摘要与区间**：字段名自带算法时用**裸小写 hex**（`head_blake3`/`sha256`），通用摘要字段用 `<alg>:<hex>` 不截断；`ByteRange` 为 `{ start = <u64>, len = <u64> }`，`start+len` 不得溢出，偏移/长度上限 `2^63-1`（EB005）。
6. **十六进制字节串**：**大写、无前缀、偶数长度**（`"89504E47"`），否则 EB005。
7. **上限**：`.geows`/`.geoproj` 8 MiB、`.gmagic` 4 MiB、`.geolock` 8 KiB、`manifest.toml` 64 KiB（超限 EB020）；**`reserved` 非零 ⇒ 读侧忽略不报错**（前向兼容），写侧恒 0。

### 3.2 版本登记与 schema 工件

落盘字段名固定：文本 `schema_version`（整数）、二进制 `fmt_ver`（u16）、manifest `schema`（历史命名，`见 15 §3.2`）。**三者语义等价，只是载体不同。**

| 格式 | 落盘字段 | 当前值 | 工件 |
|---|---|---|---|
| `.geows` / `.geoproj` / `.gmagic` / `.geolock` | `schema_version` | `1`（geolock 可缺省） | `schemas/<f>/v1.json` |
| `.geopkg` manifest | `schema` | `1` | `schemas/geopkg-manifest/v1.json` |
| `.geopatch` / `.geojournal`（另登记 *config / snapshot*：非本文格式，防撞号） | `fmt_ver` / `schema_version` | `1` / `2`、`1` | `schemas/<f>/v1.json` + `v1.layout.md`；config/snapshot `见 17 §4.4` |

工件：`schemas/<format>/v<N>.json`（JSON Schema）、二进制另加 `v<N>.layout.md`（字节布局表）、`schemas/<format>/CHANGELOG.md`（版本与 `rev` 变更记录），随发布打 tag `schema-v<format>-<N>`。工件限 **JSON Schema 2020-12 子集**（`type/properties/required/enum/pattern/minimum/maximum/items/additionalProperties/const/oneOf`），**禁**跨文件 `$ref` 与 `format` 断言；`additionalProperties` 必须显式 `true`（反映 D3）；每字段 `description` 必须含本文中文说明与节号。

### 3.3 `.geows` 工作区

`.geows`｜`application/vnd.geode.workspace+toml`（未注册前 `text/plain` 兜底）｜**magic：无**｜UTF-8/LF｜≤8 MiB。判定序：TOML 可解析（否则 EB003）→ `kind=="geows"`（否则 EB002）→ 版本可判定（EB001/EB004）→ `docs` 存在（EB004）。语义与写入时机 `见 17 §3.3`。

| 字段 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `schema_version` / `kind` / `workspace_id` | int / str / str | 是 | — | `==1`，`>1` ⇒ EB001；`kind` 恒 `"geows"`；`workspace_id` 为 ULID | 版本见 §3.2；id 用于锁与跨实例识别 |
| `project` / `window` / `layout` | str / table / table | 否 | 无 / 无 / 单叶 tab | `project` 相对 `.geows` 目录，绝对路径 ⇒ EB009 告警；`window`：`w`/`h` u32 1..=65535、`dpi_scale` f32 0.5..=4.0；`layout`：`type∈{split,tab}`，split 需 `axis∈{horizontal,vertical}`+`ratio∈(0,1)`+`children[]`，tab 需 `views[]`，树深 ≤16、节点 ≤256（EB005） | 工作区可独立于项目；`window` 机器本地**可安全丢弃**；布局树叶子引用 `views[].view_id` |
| `docs[]` | array\<table\> | 是 | — | 1..=512；`doc_uuid`（ULID）唯一否则 EB010；`source.kind∈{file,untitled,provider}`，`file` 必须带 `path`、`provider` 必须带 `provider` 串；`dirty_at_exit` bool 默认 `false`（`true` ⇒ 启动走恢复）；`project_doc` 须匹配 `.geoproj` 的 `documents[].id` 否则 EB008 | 会话中打开的文档；`doc_uuid` 是与 journal 交叉引用的唯一键 |
| `views[]` | array\<table\> | 否 | `[]` | ≤256；`view_id` u32 1..=2^31-1 唯一（EB010）；`doc_uuid` 必须存在于 `docs[]`（EB008）；`kind∈{hex,structure,inspector,diff,disasm,search,yara,processor,vis}`（未知 ⇒ EB006 视为 `hex`）；`cursor` 0..=2^63-1；`selection.ranges[]` ≤1024 每项 `{start,len}`+`rect` bool；`scroll_first_row` ≥0 且**是行号不是像素**；`columns`：`bytes_per_row∈{1,2,4,8,16,32,64}`、`group∈{1,2,4,8}` 且整除前者、`radix∈{hex,dec,oct}`、`width_adaptive` bool；`text_encoding` 取 `EncodingId`（`见 08 §5`，未知 ⇒ EB006 退回 utf-8）；`extras` ≤256 KiB 且**原样写回** | 视图私有 |

```toml
schema_version = 1; kind = "geows"; workspace_id = "01J9Z8QK7R3M2X0V5B4N6T1C8D"   # ← 最小可用
docs = [{ doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1", source = { kind = "file", path = "samples/a.bin" } }]
```
```toml
schema_version = 1; kind = "geows"                   # ← 完整
workspace_id = "01J9Z8QK7R3M2X0V5B4N6T1C8D"; project = "acme-router.geoproj"; window = { w = 1680, h = 1020, dpi_scale = 2.0 }; layout = { type = "split", axis = "horizontal", ratio = 0.62, children = [{ type = "tab", views = [1] }] }
docs = [{ doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1", project_doc = "fw", dirty_at_exit = false, source = { kind = "file", path = "samples/router_v1.2.bin" } }, { doc_uuid = "01J9Z9B2S4P6N8K0H2G4F6D8E1", dirty_at_exit = true, source = { kind = "untitled" } }]
views = [{ view_id = 1, doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1", kind = "hex", cursor = 0x1040A, scroll_first_row = 4096, selection = { ranges = [{ start = 0x10400, len = 32 }], rect = false }, text_encoding = "windows-1252", columns = { bytes_per_row = 16, group = 4, radix = "hex", width_adaptive = true } }]
```

**迁移规则**：v1 初版无历史；`>1` ⇒ EB001 拒绝加载且**不写盘**（允许只读原文视图）。工作区是本地状态，**不承诺跨主版本互读**（`见 17 §4.4` 第 5 条）：迁移失败允许降级为"丢弃 `layout`/`views`，只恢复 `docs`"并提示（EB025 Warning）。

### 3.4 `.geoproj` 项目

`.geoproj`｜`application/vnd.geode.project+toml`｜**magic：无**｜判定序同 §3.3 且 `kind=="geoproj"`｜≤8 MiB。**项目是可提交的团队资产，约束最严**：禁绝对路径、禁密钥值、禁内嵌样本（`见 17 §3.4`）。

| 字段 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `schema_version` / `kind` / `project_id` / `name` | int / str / str / str | 是 | — | `==1`；`kind` 恒 `"geoproj"`；`project_id` ULID；`name` 1..=200 字符无换行 | 版本见 §3.2 |
| `created_at` / `modified_at` / `app_version` | str | 是 / 否 / 否 | — / `created_at` / 无 | RFC 3339 UTC；时间**不参与**任何判定与排序；`app_version` semver 仅诊断 | — |
| `documents[]` | array\<table\> | 是 | — | 1..=256；`doc_uuid`（ULID）与 `id`（`^[a-z][a-z0-9_-]{0,31}$`）各自唯一（EB010）；`path` 相对项目根、禁绝对、禁 `..` 逃出根（EB009）、≤1024 字符；`optional` bool 默认 `false`（决定不可达时的降级，`见 17 §4.3`）；`note` ≤2000；`fingerprint`：首次写入时必须存在（缺失 ⇒ EB004 并标"未验证"），`size` u64、`head_len` 4096..=1048576 默认 `65536`、`head_blake3` 64 hex 小写必填、`full_blake3` 仅 `size≤512MB` 或 `files.fingerprint="full"` 时存在（关卡 4 依据，`见 17 §3.4.1`） | 文档引用 |
| `documents[].templates[]` | array\<table\> | 否 | `[]` | ≤32；`template` 为内容包 id（`^[a-z][a-z0-9-]{1,46}\.[a-z0-9-]{1,32}$`）、`version` semver req 无 build metadata、`source∈{library,project_file,generated}`（未知 ⇒ EB006 视为 library）、`sha256` 在 `project_file` 时必填、`entry_point` 包内相对路径默认 `"main"` | **引用不拷贝** |
| `processors` / `yara` / `assertions[]` | table / table / array | 否 | 无 / 无 / `[]` | `processors`：`nodes[].id` u32 唯一、`kind` 必须在 `geode-proc` 注册表（EB008）、`params` 中密钥必须 `SecretRef`（EB005）、`edges[].from/to` 必须指向已存 id（EB008）、`bypass` 默认 `false`；`yara`：`ruleset`、`rule_dirs[]`、`namespaces[]`、`min_hits` u32 ≥1；`assertions`：≤1000、`id` 唯一、`doc` 有效、`expr` 非空、`severity∈{error,warn,info}` | `见 07` / `见 12` / `见 02` |
| `bookmarks[]` / `search_history` / `ai.provenance[]` | array / table / array | 否 | `[]` / 无 / `[]` | `bookmarks` ≤5000：`id` 唯一、`doc` 匹配 `documents[].id`、`label` ≤256、`color` 为主题 token（未知 ⇒ EB006）、`tags[]` ≤16 每项 ≤32；`search_history` ≤20 条每 ≤4 KiB，**只存查询与参数不存结果**；`provenance` 字段 `at/provider/model/task/input_digest/output_sha256/accepted/human_edits`，**禁 prompt、禁样本字节**（EB005） | `见 03 §10` |

```toml
schema_version = 1; kind = "geoproj"; project_id = "01J9Z8P2M4K6H8G0F2D4S6A8B1"    # ← 最小可用
name = "样例项目"; created_at = "2026-11-08T12:00:00Z"
[[documents]] doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1"; id = "fw"; path = "samples/a.bin"; fingerprint = { size = 4096, head_len = 65536, head_blake3 = "9f2a1c7b5e07…" }
```
```toml
schema_version = 1; kind = "geoproj"; app_version = "0.9.2"                       # ← 完整
project_id = "01J9Z8P2M4K6H8G0F2D4S6A8B1"; name = "ACME 路由器固件逆向"; created_at = "2026-11-08T12:00:00Z"; modified_at = "2026-11-08T13:10:44Z"
[[documents]] doc_uuid = "01J9Z9A0Q1M4R7T2V5X8B3N6C1"; id = "fw"; path = "samples/router_v1.2.bin"; note = "SPI flash 完整镜像"; fingerprint = { size = 8388608, head_len = 65536, head_blake3 = "9f2a1c7b5e07…", full_blake3 = "1b7d0f3a…" }
[[documents.templates]] template = "tpl.mips-firmware"; version = "^1.2"; source = "library"; sha256 = "3c9f…"; entry_point = "firmware.bin"
bookmarks = [{ id = "bm1", doc = "fw", range = { start = 0x12, len = 4 }, label = "magic", tags = ["hdr"] }]; assertions = [{ id = "a1", doc = "fw", expr = "$size % 16 == 0", severity = "error" }]
processors = { nodes = [{ id = 3, kind = "xor", params = { key = { secret_ref = "keyring:geode/proc/acme-xor" } } }], edges = [{ from = 1, to = 3 }], bypass = false }; yara = { ruleset = "rules/acme.yar", rule_dirs = ["rules/"], namespaces = ["acme"], min_hits = 1 }; search_history = { queries = [{ q = "4D 5A", mode = "hex" }] }
ai = { provenance = [{ at = "2026-11-08T13:10:44Z", provider = "anthropic", model = "claude-sonnet-4", task = "generate", input_digest = "blake3:5c1e…", output_sha256 = "a71f…", accepted = true, human_edits = 34 }] }
```

**迁移规则**：v1 起，**承诺跨主版本前向迁移**（团队资产）。把 `templates[].version` 的解析结果内联为 `resolved` 属**只增可选字段 ⇒ 次版本不 bump**；`geode-proc` 改图模型才 bump v2。迁移必须保留未知字段——**迁移不得成为"顺手删掉新版字段"的通道**。

### 3.5 `.geopkg` 内容包

`.geopkg`｜`application/vnd.geode.content-package+zip`（兜底 `application/zip`）｜**magic `50 4B 03 04`**（zip local header）。结构约束（D7）：**第 1 个 entry 必须是 `manifest.toml` 且方法 0（STORED）**，其余只允许 deflate（8）；ZIP64、加密位、symlink/hardlink entry、嵌套 zip、data descriptor（bit 3）⇒ EB002。配额、zip slip 防御、签名与验证序的**语义**归 `见 15 §3.1/§4.3/§4.9`。

| 字段（`manifest.toml`） | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `schema` / `content_api` | int | 是 | — | 均 `==1`；超前 ⇒ EB001 拒装 | **不得合并**：前者管字段格式，后者管运行时能力 |
| `id` / `type` / `version` / `min_app` | str | 是 | — | `id`：`^[a-z][a-z0-9-]{1,46}\.[a-z0-9-]{1,32}$` 且前缀匹配 `type∈{template, magic, theme, yara, plugin, visualizer}`（未知 ⇒ EB006 拒装）；`version`/`min_app` semver，**禁 build metadata** | 与目录名、索引三处一致 |
| `name` / `description` | table | 是 / 否 | — / 无 | 键 BCP-47 且**至少 `en-US`**；`name` 1..=80、`description` ≤400 字符 | 多语言展示 |
| `authors[]` / `license` / `origin` | array / str / table | 是 | — / — / `{kind="human"}` | `authors` ≥1，`name` 必填，`key_id` 形如 `ed25519:<8hex>`；`license` 为 SPDX 表达式，GPL/AGPL/LGPL 家族拒装（`见 15 §4.8`），无法表达用 `LicenseRef-*`；`origin.kind∈{human,ai,imported}`，`imported` 追加 `from`/`ref`/`upstream_license` | 归属与出处审计 |
| `applies_to` / `samples[]` / `preview` | table / array / table | 否 / 条件 / 否 | 无 / `[]` / 无 | `applies_to`：`magic[]`/`mime[]`/`ext[]`；`samples` 在 `type=="template"` 时 ≥1，每项 `path`/`sha256`/`size`；`preview.path` 必须为 `preview.png`，512×256 PNG ≤256 KiB，带 `sha256` | 匹配提示 / 回归基准 / 预览 |
| `deps[]` / `files[]` | array\<table\> | 否 / 是 | `[]` / — | `deps`：`id`+`req`(semver req)，深度 ≤2，禁环（`见 15 §4.6`）；`files`：除 manifest 外**逐文件** `path`/`sha256`/`size`，集合与实际 entry **完全相等**（多/少均 EB004） | 让损坏可定位 |

旁路 `<pkg>-<ver>.sig`（JSON ≤4 KiB）：`schema`(1)/`alg`(`"ed25519"`)/`key_id`/`sha256`(被签摘要)/`sig`(base64 64B)/`signed_at`。**签的是摘要不是文件体**（`见 15 §4.3`）。

```toml
schema = 1; id = "theme.nord"; type = "theme"; version = "1.0.0"; min_app = "0.7.0"      # ← 最小可用
name = { "en-US" = "Nord" }; authors = [{ name = "Ana" }]; license = "MIT"; content_api = 1; origin = { kind = "human" }; files = [{ path = "theme.toml", sha256 = "0c19…", size = 4210 }]
```
```toml
schema = 1; id = "tpl.zip"; type = "template"; version = "1.4.0"; min_app = "0.7.0"       # ← 完整
name = { "en-US" = "ZIP Archive", "zh-CN" = "ZIP 归档" }; description = { "en-US" = "Local headers, central directory, EOCD" }; license = "MIT"; content_api = 1; origin = { kind = "human" }; deps = [{ id = "magic.common", req = "^2.1" }]
authors = [{ name = "Ana", url = "https://example.invalid/ana", key_id = "ed25519:9f2c3a11" }]; applies_to = { magic = ["zip"], mime = ["application/zip"], ext = ["zip"] }
samples = [{ path = "samples/min.zip", sha256 = "b1e4…", size = 118 }]; preview = { path = "preview.png", sha256 = "77aa…" }; files = [{ path = "spec/zip.bspec", sha256 = "0c19…", size = 4210 }]
```

**迁移规则**：`schema > 1` ⇒ EB001 拒装（**不落盘、不 staging**）。允许"降级读取"——读出未知字段用于展示"此包需要更新的 Geode"，但**绝不安装**。`content_api` 与 `schema` 不得合并：前者是能力契约，后者是字段格式。

### 3.6 `.geopatch` 补丁（二进制）

`.geopatch`｜`application/vnd.geode.patch`｜**magic `47 45 4F 50 41 54 43 48` = `"GEOPATCH"`（8B）**｜大端｜头定长 32 B + 记录帧序列｜尾部可有 1..=4 个 `0x00` 填充（读侧忽略）。支持长度变化与 u64 偏移；IPS/IPS32 因 16MB/4GB 上限且不支持长度变化，只能作**导出**目标（`见 04 §4.5`）。

| 偏移 | 长度 | 头字段（共 32 B） | 类型 | 约束 / 说明 |
|---|---|---|---|---|
| 0 | 8 | `magic` | bytes | 恒 `"GEOPATCH"`，否则 EB002 |
| 8 | 2 | `fmt_ver` | u16 | `==1`；`>1` ⇒ EB001（不写盘，仍可只读列记录摘要） |
| 10 | 2 | `flags` | u16 | bit0 `HAS_FILE_META`（置位则第 1 条记录必须是 `0x10`）；bit1..15 必须 0（否则 EB005） |
| 12 | 4 | `header_crc32` | u32 | CRC-32/IEEE，覆盖字节 `0..12` |
| 16 | 8 | `base_size` | u64 | 应用前目标字节数，用于预检 |
| 24 / 28 | 4 / 4 | `record_count` / `reserved` | u32 / 4×u8 | `record_count` 必须等于可解析记录数，否则 EB014（截断的第一道检出）；`reserved` 写 0，读忽略 |

**记录帧**：`kind u8 │ seq u64 │ payload_len u32 │ payload │ crc32 u32`（偏移相对记录首字节）。`kind` 见下表，未知 ⇒ EB006 **跳过该帧继续**（`payload_len` 自描述）；`seq` 从 1 起严格 +1，乱序/重复/跳号 ⇒ EB012；`payload_len` ≤ `formats.geopatch.max_record_bytes`（默认 64 MiB），`13+payload_len+4 >` 剩余 ⇒ EB013；`crc32` 位于偏移 `13+payload_len`，覆盖 `0..13+payload_len`（含 `kind`/`seq`/`len`/`payload`，不含自身）⇒ 不符 EB011。

| `kind` | 名称 | 载荷（从 `payload` 首字节起，全部大端） | 语义 |
|---|---|---|---|
| `0x01` `0x02` `0x03` | `Overwrite` / `Insert` / `Remove` | `at u64 │ len u64 │ data[len]`（`0x03` 无 `data`） | 等长替换（`len` 必须等于被替换长度）/ 在 `at` 前插入 / 删除 `[at, at+len)` |
| `0x04` / `0x12` | `Checkpoint` / `Comment` | `label_len u16 │ label[label_len]` / `text_len u16 │ text[text_len]` | 撤销分组边界 / UTF-8 注释 ≤4 KiB，无副作用 |
| `0x10` | `FileMeta` | `head_len u32 │ base_head_blake3[32] │ path_len u16 │ path[path_len]` | 建议性目标标识；`path` **不得**用于自动定位 |
| `0x11` | `FinalLen` | `final_size u64` | 应用后长度必须等于此值（否则 EB014）；**必须是最后一条** |

```text
# 最小可用：1 条 Overwrite（偏移 0 处 1 字节改为 0x42）
0000  47 45 4F 50 41 54 43 48  00 01  00 00  <hcrc:4> | 0010  <base_size:8 = 1>  <record_count:4 = 1>  00 00 00 00
0020  01 <seq:8 = 1> 00 00 00 09  <at:8 = 0> <len:8 = 1> 42  <crc:4>
```
```text
# 完整：FileMeta + Overwrite + Insert + FinalLen（flags = 0x0001）
0000  47 45 4F 50 41 54 43 48  00 01  00 01  <hcrc:4> | 0010  <base_size:8 = 4096>  <record_count:4 = 4>  00 00 00 00
0020  10 <seq:1> 00 00 00 2E <head_len:4> <head_blake3:32> <path_len:2 = 5> "a.bin" <crc:4> | 00C0  11 <seq:4> 00 00 00 08 <final_size:8 = 4100> <crc:4>
0042  01 <seq:2> 00 00 00 19 <at:8 = 0x10> <len:8 = 0x10> <16B data> <crc:4> | 007B  02 <seq:3> 00 00 00 14 <at:8 = 0x20> <len:8 = 4> <4B data> <crc:4>
```

（`<...:N>` 按 §3.1 第 2 条计算，真值见 `tests/vectors/geopatch/v1/0001-overwrite-1b/`。）
**应用规则**：① **先只扫不应用**——校验头 CRC、全部记录 CRC、`seq`、`record_count`、`FinalLen` 位置；② 全过才按序应用（全量预扫，**不流式边读边改**）；③ 应用期任何失败 ⇒ 回滚到应用前 checkpoint（整体是**一个**可撤销事务，`见 04 §4.5`）；④ 应用后 `len() == final_size` 否则 EB014。
**迁移规则**：`fmt_ver` 只向前加记录类型（未知 `kind` 跳过 ⇒ EB006 告警），**次版本无需 bump**；一旦改字段布局/端序 ⇒ bump 主版本并提供 `geode migrate --format geopatch`。旧版读新文件 ⇒ EB001，但仍允许只读导出为 IPS/hex 摘要——别让用户彻底失去数据。

### 3.7 `.gmagic` magic 规则集

`.gmagic`｜`application/vnd.geode.magic+toml`｜**magic：无**｜判定序：TOML 可解析 → 至少一个 `[magic.<id>]` 表存在（否则 EB002）→ 版本可判定｜≤4 MiB。匹配算法与预算 `见 10 §4.5`。**`schema_version` 为本规范补全的必填字段**（10 §4.5 原文无）。

| 字段 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `schema_version` / `kind` | int / str | 是 / 否 | — / 按扩展名推断 | `==1`；`kind` 恒 `"gmagic"` | 补全字段 |
| `[magic.<id>]` | table | 是 | — | 键 `^[a-z0-9][a-z0-9._-]{0,63}$`；单库 ≤10000 表（EB020） | 每规则一表 |
| `.mime` / `.ext` / `.kind` / `.weight` / `.read` | str / array / str / int / int | 是 / 否 / 否 / 否 / 否 | — / `[]` / `"data"` / `0` / `64` | `mime` 为 `type/subtype` ≤128 字符；`ext[]` ≤16 项、`^[a-z0-9]{1,16}$`、不含 `.`；`kind∈{image,audio,video,archive,exec,doc,data,db,font,diskimage,capture,rom,other}`（供 UI 分组）；`weight∈-100..=100` 命中加分；`read 1..=4096` 字节（**硬上限 4 KiB**，EB005）可读窗口 | 结果写入 `FormatGuess` |
| `[[magic.<id>.test]]` | array\<table\> | 是 | — | 1..=16 条/规则；全库每规则 ≤64 指令（EB020） | 判定条件 |
| `.test[].off` / `.jump` / `.len` / `.value` | int / int / int / str | 条件 / 否 / 是 / 是 | `0` / 无 / — / — | `off` 与 `jump` 互斥且 `0..=4095`（`jump` 指向已读偏移）；`len 1..=256` 且 `off+len ≤ read`；`value` **大写偶数长度 hex**，字符数 `==2*len` | 取值与比较值 |
| `.test[].mask` / `.op` / `.required` | str / str / bool | 否 | 全 `F` / `"eq"` / `true` | `mask` 与 `value` 等长，先与后比；`op∈{eq,ne,lt,gt,in_range,contains}`，未知 ⇒ EB006 视为 `eq`；`required=false` 为可选证据（加分不限流） | — |

```toml
schema_version = 1                                   # ← 最小可用
[magic.png]
mime = "image/png"
[[magic.png.test]] off = 0; len = 8; value = "89504E470D0A1A0A"
```
```toml
schema_version = 1; kind = "gmagic"                  # ← 完整
[magic.png]
mime = "image/png"; ext = ["png"]; kind = "image"; weight = 10; read = 64
[[magic.png.test]] off = 0;  len = 8; mask = "ffffffffffffffff"; value = "89504E470D0A1A0A"
[[magic.png.test]] off = 12; len = 4; value = "49484452"; required = false
[magic.elf]
mime = "application/x-elf"; ext = ["elf", "so", "o"]; kind = "exec"; weight = 20; read = 32
[[magic.elf.test]] off = 0; len = 4; value = "7F454C46"    # 同规则多条件：全部命中才判定
[[magic.elf.test]] off = 4; len = 1; op = "in_range"; value = "0102"
```

**迁移规则**：v1 起（`kind` 缺省的推断属 v1，不 bump）。市场下发更高 `schema_version` 的库 ⇒ **该库整体跳过并告警**（EB001），其它库照常加载（同 `见 15 §4.1` 的单库降级原则）。

### 3.8 `.geojournal` 编辑日志（二进制）

`.geojournal`｜`application/vnd.geode.journal`｜**magic `47 45 4F 4A 52 4E 30 31` = `"GEOJRN01"`**｜大端｜头 **80 B**（`见 17 §4.6` 标称 64 B 系笔误，见 D6）｜**只记 `EditOp`，绝不记文件副本**。回放四道关卡 `见 17 §4.6`。

| 偏移 | 长度 | 头字段（共 80 B） | 类型 | 约束 / 说明 |
|---|---|---|---|---|
| 0 / 8 | 8 / 2 | `magic` / `fmt_ver` | bytes / u16 | 恒 `"GEOJRN01"`（否则 EB002）；`fmt_ver ==1`，否则关卡 1 失败 ⇒ EB001 |
| 10 / 12 | 2 / 16 | `flags` / `doc_uuid` | u16 / ULID 二进制 | 保留写 0；必须等于 `docs[].doc_uuid` 的 16B 形式 |
| 28 / 29 / 37 | 1 / 8 / 4 | `base_kind` / `base_size` / `head_len` | u8 / u64 / u32 | `base_kind∈{0x00 file,0x01 untitled,0x02 provider}`；`base_size` base 字节数；`head_len` 头部采样长度 |
| 41 / 73 | 32 / 7 | `base_head_blake3` / `reserved` | bytes | 关卡 4 的唯一依据；`reserved` 写 0，读忽略 |

**头无 CRC 是刻意的**：头只写一次，损坏由 magic/`fmt_ver`（关卡 1）与 base 指纹（关卡 4）检出；加 CRC 会把"头一字节翻转"从"可识别"退化为"整份只剩 Error"。**记录帧**同 §3.6（CRC 覆盖 `0..13+payload_len`）：`seq` 从 1 起严格 +1（乱序/重复 ⇒ 关卡 3 = EB012，按首个异常前截断）；`13+len+4 >` 剩余 ⇒ 截断（EB013）；CRC 不符 ⇒ 关卡 2（EB011）**停止回放、标 `Partial`，已回放前缀仍用于恢复**。

| `kind` | 名称 | 载荷 | 语义 |
|---|---|---|---|
| `0x01` `0x02` `0x03` | `Overwrite` / `Insert` / `Remove` | 同 §3.6 的 `0x01`/`0x02`/`0x03` | 等长替换 / 插入 / 删除 |
| `0x04` | `Checkpoint` | `label_len u16 │ label[label_len]` | 撤销分组边界 |
| `0x05` / `0x06` | `SaveMark` / `FinalLen` | `new_base_size u64 │ new_head_blake3[32]` / `len u64` | 保存成功后**截断重写**并更新头 `base_*`（旧记录全丢）/ 回放后 `len()` 必须等于末条 `FinalLen`（关卡 4） |

```text
# 最小可用：无编辑（头 + 1 条 FinalLen，表示 base 未被改动）
0000  47 45 4F 4A 52 4E 30 31 00 01 00 00 <doc_uuid:16> ｜ 0028  00 <base_size:8> <head_len:4> <base_head_blake3:32> <reserved:7>
0050  06 <seq:8 = 1> 00 00 00 08 <len:8> <crc:4>
```
```text
# 完整：3 条编辑 + 末条 FinalLen（头为最近一次 SaveMark 之后的新 base）
0000  47 45 4F 4A 52 4E 30 31 00 01 00 00 <doc_uuid:16> ｜ 0028  00 <base_size:8 = 4194304> <head_len:4 = 65536> <base_head_blake3:32> 00×7
0050  01 <seq:1> 00 00 00 19 <at:8 = 0x10400> <len:8 = 0x10> <16B> <crc:4> ｜ 0071  04 <seq:2> 00 00 00 06 <label_len:2 = 4> "paste" <crc:4>
0087  02 <seq:3> 00 00 00 14 <at:8 = 0x20000> <len:8 = 4> <4B> <crc:4> ｜ 00A8  06 <seq:4> 00 00 00 08 <len:8 = 4194308> <crc:4>
```

**迁移规则**：**不提供跨版本迁移器**——journal 是 14 天内的临时状态（`见 17 §8`），维护迁移链是负收益。`fmt_ver` 不匹配 ⇒ 关卡 1 失败 ⇒ **保留文件绝不删**（`见 17 §4.7` 铁律 ①）+ 提供 `geode journal export <uuid> -o out.geopatch`（把可解析前缀转 patch）。**这句必须出现在错误提示里**，否则用户会以为数据没了。

### 3.9 `.geolock` 锁文件

`.geolock`（数据文件锁 `.<name>.geolock`、工作区锁 `.<name>.geows.lock`；内容层 `state/content.lock` **不属本格式**，`见 15 §3.4`）｜`application/vnd.geode.lock+toml`｜**magic：无**｜判定序：TOML 可解析 → 存在 `instance_uuid`+`pid`+`host` 三键（否则视为**未知锁文件**：不接管、报 EB022）｜≤8 KiB。锁语义（`flock`/`LockFileEx`、冲突三选一、只读降级）`见 17 §4.9`。

| 字段 | 类型 | 必填 | 默认 | 约束 | 说明 |
|---|---|---|---|---|---|
| `schema_version` / `kind` / `instance_uuid` | int / str / str | 否 / 否 / 是 | `1` / 推断 / — | `==1`（缺失视为 1）；`kind` 恒 `"geolock"`；`instance_uuid` ULID 且与 `state/instances/<uuid>.json` 同名 | 瞬时物，不做迁移；id 是实例身份 |
| `pid` / `host` / `boot_id` / `started_at` / `acquired_at` / `app_version` / `target` | int / str / str / str / str / str / str | 是 / 是 / 是 / 是 / 否 / 是 / 否 | — / — / — / — / 无 / — / 无 | `pid` 1..=2^31-1；`host` ≤255 且**跨 host 的锁不可信**；`boot_id` Linux 取 `/proc/sys/kernel/random/boot_id`，Windows/macOS 用启动时间戳；三个时间/版本字段 RFC 3339 UTC + semver，`acquired_at` 每次重入刷新；`target` 绝对路径 ≤1024 | **`pid`+`started_at`+`host` 三者合起来才唯一标识持有者**；`boot_id` 区分 pid 复用；`target` 仅诊断**不参与**判定 |

```toml
instance_uuid = "01J9ZA1B2C3D4E5F6G7H8J9K0M"; pid = 48213                     # ← 最小可用
host = "ana-mbp"; boot_id = "9e2b7c4a-1f30-4d11-8f9a-77c2e5b1d004"; started_at = "2026-11-08T13:02:11Z"; app_version = "0.9.2"
```
```toml
schema_version = 1; kind = "geolock"                                          # ← 完整
instance_uuid = "01J9ZA1B2C3D4E5F6G7H8J9K0M"; pid = 48213; host = "ana-mbp"; boot_id = "9e2b7c4a-1f30-4d11-8f9a-77c2e5b1d004"
started_at = "2026-11-08T13:02:11Z"; acquired_at = "2026-11-08T13:02:12Z"; app_version = "0.9.2"; target = "/home/ana/samples/router_v1.2.bin"
```

**残留判定（先拿锁再看文件）**：能拿到 `flock` ⇒ 文件是崩溃残留 ⇒ **静默接管并覆盖写**（EB022 记 Info 不记 Warning）；拿不到 ⇒ 他人持有 ⇒ 按 `host` 比对，同 host 读 `pid`+`started_at` 确认存活，跨 host 一律**只读降级**并标"无锁保护"。**迁移规则**：无；解析失败即未知锁文件 ⇒ 不接管（EB022）。
## 4 核心算法流程

### 4.1 兼容性总则

**`schema_version` 只表达主版本**：**补丁**（措辞/错误消息/新增向量/`reserved` 注释）不改字段；**次**（新增可选字段、新增枚举值、放宽约束、**校验收紧**）不改落盘值、只在工件记 `rev`；**主**（删字段、改语义/单位/编码/默认值、可选改必填、改字节布局或端序）**落盘值 +1**。读侧：未知字段 ⇒ EB007（config 保留 / 项目忽略+告警）、未知枚举 ⇒ EB006、收紧 ⇒ EB030 Warning **不拒绝**；`TooNew` ⇒ EB001。

**递增判据的唯一形式是反例**：能举出**一份**真实旧文件在新版下"按原语义读不出"（或反之），就必须 bump 主版本；举不出 ⇒ 只是次/补丁——这条写死是为了终结"想 bump 又不想写迁移器"的讨价还价。**兼容窗口**：① **后向**——同一 `master` 上任意两提交，**新读者必须读通旧文件**（唯一例外是按 §4.6 移除的字段，且移除已 bump 主版本）；② **前向**——旧读者见新文件一律 `TooNew`（拒绝加载 + 拒绝写盘 + 只读原文），**绝不忽略未知字段后写回**（`见 17 §4.4` 第 3 条）；③ **迁移器保留 ≥3 个大版本**（`formats.migration.window_majors`），超出 ⇒ EB027；④ **只读降级优先于拒绝打开**：除 `magic` 不符与 `TooNew` 外，其余失败都必须产出"能用的部分结果 + 精确诊断"。

### 4.2 统一读路径

```
open_format(path, expect) -> OpenVerdict:                      # 探测只读头部；永不整读文件（超限 EB020）
  bytes = read_bounded(path, limit_of(detect(path))); fmt = detect(bytes, path)
  if fmt is None or (fmt.magic and not bytes.starts_with(fmt.magic)) -> Corrupt(EB002)
  ver = read_version(bytes, fmt)                               # schema_version / fmt_ver / schema / content_api
  if ver is None -> Corrupt(EB004); if ver > supported(fmt) -> TooNew{ver, supported, fmt}   # 不加载、不写盘
  doc = parse(bytes, fmt); diags = validate(doc, fmt)          # 未知字段按 §4.1；引用/约束/范围/路径
  if fatal(diags) -> Corrupt(diags) | OkPartial(doc, diags)
  return Ok{doc, diags} if ver == supported(fmt) else Migrated{from: ver, doc, diags}
```
`OpenVerdict = Ok | Migrated{from} | TooNew{found,supported,fmt} | Corrupt(Diag)`——沿用 `见 17 §4.4` 的枚举并新增 `fmt` 字段。

### 4.3 统一写路径

`① canonicalize`（键序按本文表序、LF、hex 大写、空表清理、`reserved` 归零）→ `② serialize + 写前自校验`（失败即 bug，不是用户错误）→ `③ tmp = <dir>/.<name>.geode-<pid>-<rnd>.tmp → fsync(file)` → `④ rename(tmp, target) → fsync(parent_dir)`。原子写与三平台差异 `见 17 §4.10`。**写侧恒写当前最高支持版本**（不提供"另存为旧版"，确有需要走 `geode migrate --to v<N>`）。

### 4.4 校验和与签名位置（唯一权威表）

| 格式 | 保护手段 | 位置 | 保护对象 | 失败码 |
|---|---|---|---|---|
| `.geopkg` | SHA-256 摘要 + ed25519 签名（**签摘要不签文件体**）+ 逐文件 SHA-256 + 路径配额 | 旁路 `<pkg>-<ver>.sig`；索引 `versions[].sha256`；`manifest.files[]`；解包期逐 entry | 整个 zip 字节 / 每个文件 / 文件系统 | EB015/EB016/EB017/EB019/EB020 |
| `.geopatch` / `.geojournal` | CRC-32/IEEE | patch：头 `[12..16]`（覆盖 `0..12`）+ 每帧尾（覆盖 `0..13+len`）；journal：仅每帧尾，**头无 CRC**（§3.8） | 头 / 每记录 | EB011 |
| `.geolock` / `.geows` / `.geoproj` / `.gmagic` | **无内建校验和** | — | 可靠性靠 `flock` + `pid`/`started_at`/`host` 判定持有者；文本格式由 TOML 解析做结构校验，完整性交指纹/`git` | —（冲突走 EB022） |

**geopkg 验证序（严格执行，任一步失败即中止并删 tmp）**：`① 下载 .part → ② SHA-256 == 索引 → ③ 验签(摘要) 且 key_id ∈ 受信集 → ④ 解包（路径 + 配额）→ ⑤ manifest schema + files[] 逐文件哈希 + 类型专属校验 → ⑥ fsync → rename → 原子更新 state → 激活`。顺序不可调换：先哈希后签名（签名永远作用在已知摘要上）；先解包后 schema（校验需读内容，但解包已在配额下完成）。信任链 `见 15 §4.3`。
**journal 四道关卡**（`见 17 §4.6`）：① 格式（magic + `fmt_ver`）；② CRC（逐条，遇坏即 `Partial`，前缀仍用于恢复）；③ `seq` 严格递增；④ 语义（`base_*` 与磁盘指纹一致 + 末条 `FinalLen`）；三分支 `Clean/Partial/Mismatch` 必须如实呈现。

### 4.5 迁移引擎

```rust
pub trait Migration { fn from(&self)->u32; fn to(&self)->u32;                // 纯函数，无 I/O
    fn apply(&self, doc: &mut toml::Value) -> Result<(), MigrateError>;      // 就地变换
    fn postcondition(&self) -> fn(&toml::Value) -> Result<(), MigrateError>; }  // 必填（幂等断言）
pub fn migrate_chain(doc, from: u32, to: u32) -> Result<toml::Value, MigrateError>;  // 缺环 ⇒ EB023
```

① **链式** v1→v2→v3，静态表按 `from` 排序，缺环 = 编译期测试失败（`见 17 §9.1`）；② **幂等** `migrate(migrate(x)) == migrate(x)`，由 `postcondition()` 每跳后断言，违反 ⇒ EB026（**发布门禁**，不是运行时容错）；③ **可回滚**：迁移前写 `<file>.v<old>.bak`（仅留最近一份，`formats.migration.keep_bak_days=14`），`geode migrate --rollback` 恢复原字节，`.bak` 缺失/损坏 ⇒ EB028；④ **无副作用**：纯文本变换，唯一例外是 config 的"明文密钥 → 钥匙串"（`见 17 §4.4`），必须额外保证幂等与失败可续；⑤ **失败不写盘**：原文件不动，只产诊断。

### 4.6 弃用政策与两条命令

**状态机**：`Active → Deprecated（写入即告警 EB029 Warning）→ 满两个大版本 → Removed（读侧 EB004/EB006 报错，写侧禁止）`。移除**必须**已 bump 主版本，且必须提供迁移器把旧值搬走或**显式**丢弃（丢弃要写进 CHANGELOG，不许"悄悄没了"）。

```
geode migrate [--check] [--in-place] [--to <ver>] [--rollback] [--format <fmt>] [--json] <path>...
    --check 只报告"v1→v2 需 3 步：重命名 1 字段、删除 1 字段"不写盘；--in-place 写回原路径（先自动 .bak，被占用则拒绝）；--rollback 用 .bak 恢复原字节（幂等）
geode fmt-project [--check] [--json] <path>...
    规范化写回：键序、LF、hex 大写、空表/空数组清理、reserved 归零；不改语义、不触发迁移；幂等
```

两条命令都必须支持 `--json` 机器可读报告，CI 才能卡住 schema 漂移。

## 5 性能预算与基准

| 场景 | 预算 | 条件 |
|---|---|---|
| 格式探测（magic/扩展名/TOML 首表） | < 1ms p99 | 任意格式任意大小（只读头部 ≤4 KiB） |
| `.geows` / `.geoproj` 解析 + 校验 | < 8ms / < 15ms | 2 MiB / 512 文档 / 256 视图；500 行 TOML、8 文档引用（对齐 `见 17 §5` 的 80ms 恢复预算） |
| `.gmagic` 编译 / `.geolock` 读+判定 | < 40ms / < 0.2ms | 10 000 条（`见 15 §5`）；`geolock` 需在 UI 线程同步调用 |
| `.geopatch` 全量预扫 / 应用 1 万条 | > 300MB/s / < 50ms | 单线程，1 GiB patch < 3.5s |
| `.geojournal` 追加一帧 / 全量回放 / `.geopkg` manifest 校验+逐文件哈希 | < 0.5ms p99 / < 800ms / < 1.5s | payload ≤4 KiB / 10 万条含 CRC（`见 17 §5`）/ 单包 1 MiB 哈希预算 `见 15 §5` |
| **恶意输入拒绝** | < 200ms | 1 GiB 解压炸弹 / 16 MiB 病态 `.gmagic` / 伪造 `payload_len` |

**硬门禁**：① 探测路径**永不整读文件**；② 防御逻辑不得成为 DoS 面（否决一条输入须 200ms 内完成且内存 O(1)）；③ 基准进 `bench/format/`，退化 > 20% 红灯。

## 6 失败模式与降级

| 场景 | 检出手段 | 用户可见结果 | 是否可恢复 |
|---|---|---|---|
| **版本过新** | 读版本字段 | "文件 v4 / 本应用支持 v3" + [以原文查看] + [升级]；**不加载、不写盘、不迁移** | 是（升级；`.geopkg` 则拒绝安装） |
| **CRC 不符 / 截断** | 逐帧 CRC-32；`13+len+4 >` 剩余；`record_count` 不符 | journal：`Partial` +"第 1841 条损坏，之前 1840 条可用"；patch：整份拒用并报"第 N 条·第 M 字节截断，缺约 K 字节" | journal 是（前缀恢复）；patch 需重新生成 |
| **zip slip** | 逐 entry 路径检查 + `canonicalize` 必须仍在 staging 下 | 整包拒装，"entry `a/../../x` 非法路径" | 否（需重打包） |
| **manifest 缺字段** | schema 校验（EB004）+ `files[]` 集合比对 | "包 `tpl.zip` 1.4.0 缺少 `license`" / "`files[]` 少列 2 个文件" | 否（需作者修） |
| **签名过期或密钥被吊销** | `not_before`/`not_after`；`revoked_keys` | 已装内容**立即禁用** + 常驻提示条要求卸载；`hard` 撤回显示"已被撤回" | 是（换版本/卸载）；`soft` 仅标记 |
| **锁文件残留** | 能拿到 `flock` 但文件存在 | 无感（静默接管 + 日志 Info）；跨 host 时状态栏"无锁保护" | 是（自动） |
| **journal base 指纹不符** | 关卡 4（`base_size`+`base_head_blake3`） | `Mismatch` + "文件已变化" + 字节级差异摘要 + **强制恢复**（二次确认） | 是（需用户确认） |
| 未知枚举 / 悬空引用 / 校验收紧 | serde + 引用校验 / 新规则命中旧文件 | 该条目降级或忽略，其余照常（EB006/EB008）；收紧时 Warning 条 +"该字段将在一版后成为必填" | 是（部分可用 / `fmt-project` 补齐） |
| 迁移失败 / 缺环 / 写盘失败 | `MigrateError`、EB023；写 tmp 出错（ENOSPC/EROFS/EBUSY） | 不写盘 + 保留 `.bak` + 报字段路径；写盘失败时原文件**从不被触碰**，提示另存或导出 patch（`见 04 §4.6`） | 是（修文件/回滚/另存） |
**总原则**（`见 00 §9.3`）：格式层任何失败都不得阻止应用启动与打开其它文件；提示条可关闭但**不可静默**。

### 诊断码登记（`EBxxx`：自有文件格式校验与迁移）

**已登记于 `见 00 §12`；码值一旦发布不得改义，只能新增。** 同格内多个码共享该行的检出手段与处置，严重度按列出顺序对应。

| 码 | 格式 | 触发条件 | 严重度 | 建议动作 |
|---|---|---|---|---|
| EB001 / EB002 | 全部 | 版本过新（`TooNew`）/ magic·容器不符、zip 结构违规（ZIP64/加密/symlink/嵌套）、`kind` 不匹配 | Error | 拒绝加载/安装、不写盘；提示升级或"不是 Geode 文件" |
| EB003 / EB004 | 全部（EB003 仅文本） | TOML·UTF-8 语法错、BOM、非法控制字符 / 必填字段缺失、`files[]` 与 entry 集合不等 | Error | 报行列号或字段名；只读原文，不加载该对象 |
| EB005 / EB009 | 全部 / 文本 | 类型·取值·范围·hex 大小写与长度违规、`reserved` 非零、`params` 明文密钥 / 路径非法（绝对、`..` 逃根、超长、含 `\0`） | Error（`project` 键告警） | 报期望值与实际值；`fmt-project` 可修；拒绝该路径 |
| EB006 / EB007 / EB030 | 全部 | 未知枚举或未知记录 `kind` / 未知字段（项目忽略、config 保留）/ 校验收紧致旧文件不达标 | Warning / Info / Warning | 忽略·跳过·保留并继续，**不删除**；收紧只告警不拒绝；`--json` 列出 |
| EB008 / EB010 | `.geows`/`.geoproj` | 悬空引用（`doc_uuid`/`id`/`from`/`to`/`doc`/`kind`）/ 重复主键（`doc_uuid`/`view_id`/`bookmark.id`/`documents[].id`） | Warning / Error | 降级该条目其余可用；重复时保留首个、拒绝重复项 |
| EB011 / EB012 / EB013 / EB014 | `.geopatch`/`.geojournal` | 记录 CRC 不符 / `seq` 乱序·重复·跳号 / 截断（帧跨文件尾、头长不足 32·80 B）/ 长度不符（`record_count`、`FinalLen`、`Overwrite` 长度） | Error | journal 前缀恢复 `Partial`；patch 整份拒用并报第 N 条、缺失字节数 |
| EB015 / EB016 / EB017 / EB018 | `.geopkg` | 摘要不符 / 验签失败 / 密钥未知·不在受信集 / 签名过期或密钥已吊销 | Error（EB017 默认 Warning→Error） | 中止并删 `.part`；**绝不**降级为 unsigned 安装；已装内容立即禁用并要求卸载 |
| EB019 / EB020 | `.geopkg` / 全部 | zip slip（穿越·绝对路径·symlink entry）/ 配额超限（大小、条目数、解压总量、压缩比、规则数、指令数） | Error | 整包拒装并附 entry 名；拒解析并报所需与实测 |
| EB021 / EB022 | `.geojournal` / `.geolock` | base 指纹不符（关卡 4）/ 锁被占用或锁文件不可解析 | Warning / Info·Warning | 进强制恢复（二次确认）；残留静默接管、占用只读降级 |
| EB023 / EB024 / EB025 / EB026 / EB027 / EB028 | 全部（迁移） | 缺环 / 字段类型不符 / 后置条件失败 / 非幂等 / 超保留窗口（>3 大版本）/ 回滚失败（`.bak` 缺失或损坏） | Error | 一律**不写盘**并保留 `.bak`；缺环提示用对应版本打开；非幂等红灯（实现 bug）；回滚失败从 VCS 恢复 |
| EB029 | 全部 | 使用已弃用字段（写侧仍写入） | Warning | 写侧自动改用新字段；读侧映射后继续 |

## 7 与其它子系统的契约

| 子系统 | 读本文 | 本文读它 | 共享状态 |
|---|---|---|---|
| `geode-project`(17) | §3.3/§3.4/§3.8/§3.9 字段与版本语义 | `EditOp` 形态、`ViewState`、锁语义 | 是 `*.geows`/`*.geoproj`/journal/lock 的**唯一写入者** |
| `geode-content`(15) | §3.5 容器与 manifest 形状 | 签名算法、信任链、安装事务 | 只有 `state/content.toml` 可写；包是只读输入 |
| `geode-buffer`(04) | §3.6 字节布局 | `PatchSet`/`Patch`/`OldBytes` 语义 | `.geopatch` 导出/导入 |
| `geode-analysis`(10) | §3.7 字段与指令约束 | 匹配算法与预算 | `.gmagic` 只读 |
| `geode-diff`(09) / `geode-core`(00) | §3.6（导出 patch 到 diff 视图）/ `ByteRange`·`Diag`·`DocId` 与 §12 的 `EBxxx` 段 | 缓存格式**独立**不属本文管辖 / 诊断码命名空间 | 无 / 共享诊断码命名空间 |
| `geode-ui`(16) / `18` | verdict 与诊断的展示要求 / `schemas/` 打 tag、向量进 CI | 无 | 只读 / 发布流水线 |

**两条硬契约**：① **只有拥有者 crate 写该格式**，其它 crate 只走解析 API，禁止 `std::fs` 直改（对齐 `见 15 §7`）；② 本文是**字段形状的唯一权威**——新增字段必须先改本文 + 工件 + 向量，否则 CI 红灯。

## 8 配置项与默认值

| 键 | 默认 | 范围 / 影响 |
|---|---|---|
| `formats.unknown_fields` | `"warn"` | `warn`/`ignore`/`error`；`error` 主要给 CI（等价 `GEODE_SCHEMA_STRICT=1`） |
| `formats.strict` / `formats.read_only_fallback` / `formats.vector_strict` | `false` / `true` / `true` | `strict` ⇒ 任何 Warning 即失败（`见 17 §8`）；关掉降级 ⇒ 非致命诊断也拒绝打开（不建议）；`vector_strict` 为 conformance 下"码不符即红灯" |
| `formats.max_text_bytes` / `formats.max_manifest_bytes` | `8388608` / `65536` | 文本格式上限 1 MiB~64 MiB；manifest 上限 |
| `formats.migration.window_majors` / `.keep_bak_days` | `3` / `14` | 超出 ⇒ EB027；`keep_bak_days` 0~365（对齐 `见 17 §4.4`） |
| `formats.migration.require_postcondition` / `formats.deprecation.warn_majors` | `true` / `2` | 前者不可关（不是可选优化）；后者 1~5，弃用告警窗口 |
| `formats.geopatch.max_record_bytes` / `.verify_before_apply` | `67108864` / `true` | 64 MiB~1 GiB；关掉预扫会破坏事务语义 |
| `formats.gmagic.max_entries` / `formats.journal.max_replay_ops` | `10000` / `10000000` | 单库条目上限（对齐 `见 15 §3.1`）；超限 ⇒ 拒绝回放并提示导出 patch |

环境变量沿用 `见 17 §8`：`GEODE_SCHEMA_STRICT=1`、`GEODE_CONFIG_DIR`/`GEODE_STATE_DIR`/`GEODE_CONTENT_DIR`（重定向后向量测试可在只读目录跑）。

## 9 测试策略

| 层 | 数量目标 | 工具 | 覆盖 |
|---|---|---|---|
| 单元（编解码） | ≥ 220 | `#[test]` | 每格式 round-trip、边界值（0/1/2^63-1）、hex 大小写、时间格式、`reserved` 非零 |
| 布局（二进制） | ≥ 40 | 逐字节断言 | §3.6/§3.8 每个偏移都有断言；端序错误必须被抓到 |
| 版本判定 / 迁移 | ≥ 60 / ≥ 40 | 表驱动 / 快照 + property | 四态 × 7 格式 × 缺字段/类型错/损坏；每条链 + 幂等 + `.bak` + 回滚（`见 17 §9.1`） |
| fuzz / conformance / 基准 | 7 目标 / §9.2 / ≥ 10 | `cargo-fuzz`、向量驱动、`criterion` | 不 panic/不 OOM/单次 < 200ms；第三方复用同一套向量；§5 全表 |

### 9.1 向量目录与格式（**规范的一部分**）

```
tests/vectors/<format>/v<N>/<NNNN>-<slug>/{input.<ext>, expect.json, golden.<ext>?, base.bin?}
例：geows/v1/0001-minimal · geoproj/v1/0004-no-fingerprint · geopkg/v1/0002-zip-slip · geopkg/v1/0003-bomb · geopatch/v1/0001-overwrite-1b
    geopatch/v1/0003-crc-flip · geopatch/v1/0005-seq-gap · gmagic/v1/0003-unknown-op · geojournal/v1/0002-partial-crc · geojournal/v1/0003-base-mismatch
```

**规则**：① 每格式**至少 3 个故意损坏向量**，合计覆盖 版本过新 / CRC / 截断 / zip slip / manifest 缺字段 / base 指纹不符 六类；② `input.*` 不可被测试修改（只读挂载，断言字节与 mtime 不变）；③ 二进制向量的期望结果全在 `expect.json`，不依赖 `golden.*` 存在；④ `golden.*` 只用于**规范化往返**（`fmt` 幂等），不用于语义判定；⑤ 每个字段至少 1 个"缺它/写错它"的向量——**字段未被向量覆盖即视为未规范化**。

```json
{ "vector": "geopatch/v1/0003-crc-flip", "format": "geopatch", "schema_version": 1, "verdict": "reject", "codes": ["EB011"],
  "failed_record": 12, "recovered_bytes": 4096, "recovered_sha256": "…", "diags": [{ "code": "EB011", "severity": "error", "offset": 644 }] }
```

`verdict∈{ok,migrated,too_new,corrupt,partial,mismatch}`；`failed_record` 为 1-based，非记录格式省略；`recovered_*` 仅 `partial` 需要。**判定**：`verdict`、`codes`（集合相等）、`failed_record`、`recovered_sha256` 任一不符即失败。

### 9.2 conformance 套件与写侧门禁

`cargo test -p geode-format --features vectors` 跑全部向量；同一目录必须能被**语言无关的 runner** 消费（`tests/vectors/run.py` 是一份 ~150 行参考实现，CI 用它做"第二实现"对拍）。**禁止**在向量里放 Rust 特有期望值（如 `Debug` 输出）——否则第三方实现永远对不上。门禁：① 每条链迁完比快照、再迁一次断言幂等；② 对 `>current` 样本断言 `TooNew` 且**文件字节未被修改**（hash + mtime）；③ `geode fmt-project` 对全部 golden 跑两次断言字节一致；④ CI 开 `GEODE_SCHEMA_STRICT=1`，Warning 即红灯（否则 Warning 会无声积累成事故，`见 17 §9.1`）。

## 10 开放问题

| # | 问题 | 现状 / 倾向 | 时限 |
|---|---|---|---|
| Q1 | **是否承诺"5 年内可打开"** | `geoproj` **是**（团队资产：迁移器保留 ≥3 个大版本且永久不删）；`geows`/`geolock`/journal **否**（本地状态/瞬时物）；`geopkg` 是（签名与 schema 长期有效）。待产品层确认"永久迁移器"的维护成本 | M6 |
| Q2 | **`.gmagic` 是否改二进制** | 文本可手改、可 diff、可进 git（`见 10 §10 Q5`）；二进制快但抬高生态门槛。当前选文本；若实测 10 万条 > 100ms，用预编译缓存解决而不改格式 | M5 后按实测 |
| Q3 | **`.geopkg` 是否支持 delta 更新** | 现为整包替换（原子性最简单）。delta 会让"签名对摘要"复杂化（需对 base+delta 二元组签），回滚语义也变难。倾向 v2 再议 | v2 |
| Q4 | **schema 是否对外发布 JSON 版本** | 倾向**是**：`schemas/` 随 release tag 公开发布 + SemVer 标签，允许第三方 vendored 校验。风险：一经公开即成契约 | M6 |
| Q5 | **是否允许第三方扩展字段命名空间** | 倾向允许 `x-<vendor>-*`（TOML 键 `x_acme_note`），承诺永不占用该前缀，迁移/`fmt` 一律原样保留。它与"未知字段忽略+告警"冲突，需要一条显式**白名单告警抑制**规则 | M4 |
| Q6 / Q7 | **是否去 IANA 注册** `application/vnd.geode.*` / **`.geopatch` 是否要压缩容器** | 未注册前用 `octet-stream`/`text/plain` 兜底，注册是行政工作但能改善"双击打开"与邮件网关；大 patch（>100MB）传输成本真实，但压缩要重定义 CRC 覆盖范围，倾向"外层 `.geopatch.zst` 仅传输壳、不改内部格式" | M7 / M4 |
| Q8 | **未知枚举值的写回策略** | 现为"忽略+告警"，但用户随后保存 ⇒ **该值丢失**。是否改为"保留原位并强制只读直到用户处理"？与 D3 的 config 策略对齐更安全，但会让"打开就改不了"变常见 | M3 |

**本文发现的不一致（必须回填，否则规范与设计文档互相矛盾）**：① `00 §12` 登记 `EBxxx` 归本文；② `00 §3` 若采纳 `geode-format` 需在分层图补该 crate（回填前落 `geode-core::fmt`）；③ `17 §4.4` 的"五个 schema_version"扩为 §3.2 的八行，并补"落盘只承载主版本"；④ `17 §4.6` journal 头 64B → **80B**，并写明 CRC 覆盖范围与大端；⑤ `17` 正文残留的工作区/项目/锁文件旧名统一为 `geows`/`geoproj`/`geolock`；⑥ `04 §4.5` 的"自有 CBOR"改为"定长 BE + 裸字节（见 23 §3.6）"并补"应用前全量预扫"；⑦ `15 §3.2` 补"manifest 必须是第 1 个 entry 且 STORED"与 `schema`≠`content_api`；⑧ `10 §4.5` 补必填 `schema_version` 与 `.test[].op` 枚举全集；⑨ `18` 把 `schemas/` 打 tag、向量进 CI、conformance runner 列入里程碑。
