//! `.gmagic` 规则集解析与头部字节匹配。
//!
//! DESIGN: `docs/23-file-formats.md` §3.7、`docs/10-analysis-toolkit.md` §4.5。
//!
//! 本模块只做两件事：
//!
//! 1. [`MagicSet::parse`] / [`MagicSet::load`] 把 TOML 规则编译成静态合法的
//!    [`MagicRule`]，编译期完成全部结构与范围校验（不读被分析文件）；
//! 2. [`MagicSet::match_head`] / [`MagicSet::best`] 只在
//!    `data[..min(len, 4096)]` 上做定长匹配，规则里没有正则、跳转或脚本，
//!    因此单条规则的成本有硬上限（10 §4.5 的“杜绝病态规则”）。
//!
//! 规则集是只读值：解析成功后不可变，可在线程间共享。`template` 字段按
//! “原样字符串”保存，本模块不解析 BinSpec（模板编译属于 `geode-bspec`）。

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use geode_core::diag::Code;
use geode_core::{ByteRange, Diag, Offset, error_codes};
use serde::Deserialize;

/// `.gmagic` 文件大小上限：4 MiB（23 §3.7）。
pub const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// `match_head` 最多读取的头部字节数：4 KiB。
pub const MAX_HEAD_BYTES: usize = 4096;

/// 单个规则集最多规则数：10,000（23 §3.7、15 §3.1）。
pub const MAX_RULES: usize = 10_000;

/// 单条规则最多 pattern 指令数：64（10 §4.5）。
pub const MAX_PATTERNS_PER_RULE: usize = 64;

/// 单条规则最多 `test[]` 条件数：16（23 §3.7）。
pub const MAX_TESTS_PER_RULE: usize = 16;

/// u16/u32 pattern 的字节序。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endian {
    /// 小端：低字节在前。
    Little,
    /// 大端：高字节在前。
    Big,
}

/// 单条匹配 pattern。
///
/// 所有偏移都相对于文件头起点，且编译期已保证 `offset + len <= read <= 4096`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MagicPattern {
    /// 定长字节串；可选 mask 先与后比。
    Bytes {
        /// 相对文件头的字节偏移。
        offset: u64,
        /// 期望字节。
        bytes: Vec<u8>,
        /// 可选掩码；长度必须与 [`Self::Bytes::bytes`] 相等。
        mask: Option<Vec<u8>>,
    },
    /// 定长 ASCII 文本。
    Ascii {
        /// 相对文件头的字节偏移。
        offset: u64,
        /// 期望文本；只允许 ASCII。
        text: String,
    },
    /// 16 位无符号整数。
    U16 {
        /// 相对文件头的字节偏移。
        offset: u64,
        /// 期望值。
        value: u16,
        /// 字节序。
        endian: Endian,
    },
    /// 32 位无符号整数。
    U32 {
        /// 相对文件头的字节偏移。
        offset: u64,
        /// 期望值。
        value: u32,
        /// 字节序。
        endian: Endian,
    },
}

impl MagicPattern {
    /// pattern 覆盖的字节数（即匹配证据长度）。
    pub fn len(&self) -> u64 {
        match self {
            Self::Bytes { bytes, .. } => bytes.len() as u64,
            Self::Ascii { text, .. } => text.len() as u64,
            Self::U16 { .. } => 2,
            Self::U32 { .. } => 4,
        }
    }

    /// pattern 是否覆盖 0 字节。
    ///
    /// 解析器拒绝空 pattern，因此正常路径恒为 `false`；保留该方法是
    /// clippy 对“有 `len` 就应有 `is_empty`”的约定。
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// pattern 的起始偏移。
    pub fn offset(&self) -> u64 {
        match self {
            Self::Bytes { offset, .. }
            | Self::Ascii { offset, .. }
            | Self::U16 { offset, .. }
            | Self::U32 { offset, .. } => *offset,
        }
    }

    /// 该 pattern 命中时的置信度（10 §4.5 的固定档位）。
    pub fn confidence(&self) -> f32 {
        match self {
            Self::Bytes { mask: None, .. } => 0.95,
            Self::Bytes { mask: Some(_), .. } => 0.85,
            Self::Ascii { .. } => 0.80,
            Self::U16 { .. } | Self::U32 { .. } => 0.90,
        }
    }
}

/// 一条已编译的 magic 规则。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MagicRule {
    /// 规则 id；同一规则集内唯一，匹配 `^[a-z0-9][a-z0-9._-]{0,63}$`。
    pub id: String,
    /// 展示名；TOML 未给时回退为 id。
    pub name: String,
    /// MIME 类型，例如 `image/png`。
    pub mime: Option<String>,
    /// 扩展名（不含点），最多 16 项。
    pub ext: Vec<String>,
    /// 排序优先级；越大越靠前，范围 -100..=100（对应 23 §3.7 的 `weight`）。
    pub priority: i32,
    /// 匹配 pattern；typed 形式下任一命中即算规则命中。
    pub patterns: Vec<MagicPattern>,
    /// 模板 id 或路径，**原样保存**，不解析 BinSpec。
    pub template: Option<String>,
    /// 规则要求的最小文件长度；数据更短时直接不匹配。
    pub min_size: u64,
}

/// 一次成功命中。
#[derive(Clone, Debug, PartialEq)]
pub struct MagicHit {
    /// 命中的规则 id。
    pub rule_id: String,
    /// 规则展示名。
    pub name: String,
    /// 规则声明的 MIME。
    pub mime: Option<String>,
    /// 置信度：同一规则多条 pattern 命中时取最高档。
    pub confidence: f32,
    /// 命中证据覆盖到的最远字节偏移（半开区间的 `end`）。
    pub matched_len: u64,
}

/// 已编译的 `.gmagic` 规则集。
///
/// 只能由 [`Self::parse`] / [`Self::load`] 构造；内部保存每条规则的
/// 读取窗口与匹配模式，保证 [`Self::match_head`] 不会越界。
#[derive(Clone, Debug, Default)]
pub struct MagicSet {
    rules: Vec<MagicRule>,
    runtime: Vec<RuleRuntime>,
}

/// 单条规则的运行期元数据。
#[derive(Clone, Debug)]
struct RuleRuntime {
    /// 该规则允许读取的头部窗口（1..=4096）。
    read: u64,
    /// typed `patterns` 是“任一命中”，文档 `test[]` 是“全部命中”。
    mode: MatchMode,
    /// 与 `patterns` 等长；`true` 表示可选证据（仅 `test[].required=false`）。
    optional: Vec<bool>,
}

/// 规则内部匹配模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatchMode {
    /// 任一 pattern 命中即可（typed `patterns`）。
    Any,
    /// 所有必需 pattern 都命中（文档 `test[]`）。
    All,
}

impl MagicSet {
    /// 解析 `.gmagic` 文本。
    ///
    /// 返回全部**可继续收集**的编译期诊断：未知 pattern 类型、重复 id、
    /// 偏移越界、mask 长度不符、配额超限等。TOML 本身语法错时只能返回一条
    /// `EB003`（解析器无法恢复）。
    pub fn parse(text: &str) -> Result<Self, Vec<Diag>> {
        if text.len() as u64 > MAX_FILE_BYTES {
            return Err(vec![diag(
                error_codes::EB020,
                "/",
                format!(
                    "`.gmagic` 大小 {} 字节超过上限 {MAX_FILE_BYTES}",
                    text.len()
                ),
            )]);
        }
        if text.starts_with('\u{feff}') {
            return Err(vec![diag(
                error_codes::EB003,
                "/",
                "`.gmagic` 不允许 UTF-8 BOM（23 §3.1）",
            )]);
        }
        if text.contains('\0') {
            return Err(vec![diag(
                error_codes::EB003,
                "/",
                "`.gmagic` 含 NUL 控制字符（23 §3.1）",
            )]);
        }

        let doc: RawDoc = match toml::from_str(text) {
            Ok(doc) => doc,
            Err(err) => return Err(vec![toml_diag(&err)]),
        };

        let block = match doc.magic {
            Some(block) => block,
            None => {
                return Err(vec![diag(
                    error_codes::EB002,
                    "/magic",
                    "至少需要一个 `[magic.<id>]` 规则表（23 §3.7）",
                )]);
            }
        };
        let entries = block.into_entries();
        if entries.is_empty() {
            return Err(vec![diag(
                error_codes::EB002,
                "/magic",
                "至少需要一个 `[magic.<id>]` 规则表（23 §3.7）",
            )]);
        }
        if entries.len() > MAX_RULES {
            return Err(vec![diag(
                error_codes::EB020,
                "/magic",
                format!(
                    "规则数 {} 超过上限 {MAX_RULES}（23 §3.7、EB020）",
                    entries.len()
                ),
            )]);
        }

        let version = match doc.schema_version {
            Some(version) => version,
            None => {
                return Err(vec![diag(
                    error_codes::EB004,
                    "/schema_version",
                    "缺少必填字段 `schema_version`（23 §3.7）",
                )]);
            }
        };
        if version > 1 {
            return Err(vec![diag(
                error_codes::EB001,
                "/schema_version",
                format!("schema_version={version} 过新，本版本只支持 1"),
            )]);
        }
        if version != 1 {
            return Err(vec![diag(
                error_codes::EB005,
                "/schema_version",
                format!("schema_version 必须为 1，实际为 {version}"),
            )]);
        }
        if let Some(kind) = doc.kind.as_deref() {
            if kind != "gmagic" {
                return Err(vec![diag(
                    error_codes::EB002,
                    "/kind",
                    format!("kind 必须为 `gmagic`，实际为 `{kind}`"),
                )]);
            }
        }

        let mut rules = Vec::with_capacity(entries.len());
        let mut runtime = Vec::with_capacity(entries.len());
        let mut seen: HashMap<String, usize> = HashMap::new();
        let mut diags = Vec::new();

        for (index, (key, raw)) in entries.into_iter().enumerate() {
            let id = match key {
                Some(key) => {
                    if let Some(field_id) = raw.id.as_deref() {
                        if field_id != key {
                            diags.push(diag(
                                error_codes::EB005,
                                &format!("/magic/{key}"),
                                format!("规则 id 字段 `{field_id}` 与表键 `{key}` 不一致"),
                            ));
                        }
                    }
                    key
                }
                None => match raw.id.clone() {
                    Some(id) => id,
                    None => {
                        diags.push(diag(
                            error_codes::EB004,
                            &format!("/magic[{index}]"),
                            "数组形式的规则缺少必填字段 `id`",
                        ));
                        continue;
                    }
                },
            };

            if seen.insert(id.clone(), index).is_some() {
                diags.push(diag(
                    error_codes::EB010,
                    &format!("/magic/{id}"),
                    format!("重复的规则 id `{id}`（23 §3.7、EB010）"),
                ));
            }

            match compile_rule(id, raw) {
                Ok((rule, meta)) => {
                    rules.push(rule);
                    runtime.push(meta);
                }
                Err(mut rule_diags) => diags.append(&mut rule_diags),
            }
        }

        if diags.is_empty() {
            Ok(Self { rules, runtime })
        } else {
            Err(diags)
        }
    }

    /// 读取 `.gmagic` 文件并解析。
    ///
    /// 先看元数据拒绝超过 4 MiB 的文件，避免为病态输入分配大缓冲；
    /// 读取后再校验一次实际长度（元数据可能过期）。I/O 失败报 `EB501`。
    pub fn load(path: &Path) -> Result<Self, Vec<Diag>> {
        let metadata = fs::metadata(path).map_err(|err| {
            vec![diag(
                error_codes::EB501,
                "/",
                format!("读取 `{}` 失败：{err}", path.display()),
            )]
        })?;
        if metadata.len() > MAX_FILE_BYTES {
            return Err(vec![diag(
                error_codes::EB020,
                "/",
                format!(
                    "`{}` 大小 {} 字节超过 4 MiB 上限",
                    path.display(),
                    metadata.len()
                ),
            )]);
        }
        let bytes = fs::read(path).map_err(|err| {
            vec![diag(
                error_codes::EB501,
                "/",
                format!("读取 `{}` 失败：{err}", path.display()),
            )]
        })?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return Err(vec![diag(
                error_codes::EB020,
                "/",
                format!(
                    "`{}` 实际大小 {} 字节超过 4 MiB 上限",
                    path.display(),
                    bytes.len()
                ),
            )]);
        }
        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
            return Err(vec![diag(
                error_codes::EB003,
                "/",
                "`.gmagic` 不允许 UTF-8 BOM（23 §3.1）",
            )]);
        }
        let text = std::str::from_utf8(&bytes).map_err(|err| {
            vec![diag(
                error_codes::EB003,
                "/",
                format!("`.gmagic` 不是合法 UTF-8：{err}"),
            )]
        })?;
        Self::parse(text)
    }

    /// 在文件头部匹配全部规则。
    ///
    /// 只读取 `data[..min(len, 4096)]`；结果按 `priority` 降序、同分按
    /// `rule_id` 升序排列。单次调用对 1,000 条规则的预算见 00 §7（<5 ms）。
    pub fn match_head(&self, data: &[u8]) -> Vec<MagicHit> {
        let mut hits: Vec<(i32, MagicHit)> = Vec::new();
        for index in 0..self.rules.len() {
            let Some(hit) = self.evaluate(index, data) else {
                continue;
            };
            let priority = self.rules.get(index).map_or(0, |rule| rule.priority);
            hits.push((priority, hit));
        }
        hits.sort_by(|left, right| {
            right
                .0
                .cmp(&left.0)
                .then_with(|| left.1.rule_id.cmp(&right.1.rule_id))
        });
        hits.into_iter().map(|(_, hit)| hit).collect()
    }

    /// 返回优先级最高的一次命中；同优先级取 `rule_id` 最小者。
    ///
    /// 内部不分配结果 `Vec`，是“打开即识别”路径的常用入口。
    pub fn best(&self, data: &[u8]) -> Option<MagicHit> {
        let mut best: Option<(i32, MagicHit)> = None;
        for index in 0..self.rules.len() {
            let Some(hit) = self.evaluate(index, data) else {
                continue;
            };
            let priority = self.rules.get(index).map_or(0, |rule| rule.priority);
            let replace = match &best {
                None => true,
                Some((best_priority, best_hit)) => {
                    priority > *best_priority
                        || (priority == *best_priority && hit.rule_id < best_hit.rule_id)
                }
            };
            if replace {
                best = Some((priority, hit));
            }
        }
        best.map(|(_, hit)| hit)
    }

    /// 规则数量。
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// 是否为空规则集。
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// 按解析顺序（表形式按 id 升序、数组形式按文件顺序）访问规则。
    pub fn rules(&self) -> &[MagicRule] {
        &self.rules
    }

    /// 评估单条规则；数据不足 `min_size` 或任一必需 pattern 不命中时返回 `None`。
    fn evaluate(&self, index: usize, data: &[u8]) -> Option<MagicHit> {
        let rule = self.rules.get(index)?;
        let meta = self.runtime.get(index)?;
        if (data.len() as u64) < rule.min_size {
            return None;
        }
        let read = usize::try_from(meta.read).unwrap_or(MAX_HEAD_BYTES);
        let limit = data.len().min(MAX_HEAD_BYTES).min(read);

        let mut confidence = 0.0_f32;
        let mut matched_len = 0_u64;
        let mut any_match = false;

        for (pattern_index, pattern) in rule.patterns.iter().enumerate() {
            let optional = meta.optional.get(pattern_index).copied().unwrap_or(false);
            match match_pattern(pattern, data, limit) {
                Some(end) => {
                    any_match = true;
                    let pattern_confidence = pattern.confidence();
                    if pattern_confidence > confidence {
                        confidence = pattern_confidence;
                    }
                    if end > matched_len {
                        matched_len = end;
                    }
                }
                None if meta.mode == MatchMode::All && !optional => return None,
                None => {}
            }
        }

        if !any_match {
            return None;
        }
        Some(MagicHit {
            rule_id: rule.id.clone(),
            name: rule.name.clone(),
            mime: rule.mime.clone(),
            confidence,
            matched_len,
        })
    }
}

/// 在 `limit` 字节窗口内检查一个 pattern；命中时返回证据区间的 `end`。
fn match_pattern(pattern: &MagicPattern, data: &[u8], limit: usize) -> Option<u64> {
    let start = usize::try_from(pattern.offset()).ok()?;
    let len = usize::try_from(pattern.len()).ok()?;
    let end = start.checked_add(len)?;
    if end > limit {
        return None;
    }
    let slice = data.get(start..end)?;

    let matched = match pattern {
        MagicPattern::Bytes { bytes, mask, .. } => match mask {
            Some(mask) => slice
                .iter()
                .zip(bytes.iter())
                .zip(mask.iter())
                .all(|((actual, expected), mask)| (actual ^ expected) & mask == 0),
            None => slice == bytes.as_slice(),
        },
        MagicPattern::Ascii { text, .. } => slice == text.as_bytes(),
        MagicPattern::U16 { value, endian, .. } => {
            let array: [u8; 2] = slice.try_into().ok()?;
            let actual = match endian {
                Endian::Little => u16::from_le_bytes(array),
                Endian::Big => u16::from_be_bytes(array),
            };
            actual == *value
        }
        MagicPattern::U32 { value, endian, .. } => {
            let array: [u8; 4] = slice.try_into().ok()?;
            let actual = match endian {
                Endian::Little => u32::from_le_bytes(array),
                Endian::Big => u32::from_be_bytes(array),
            };
            actual == *value
        }
    };

    matched.then_some(end as u64)
}

/// 编译单条规则。
fn compile_rule(id: String, raw: RawRule) -> Result<(MagicRule, RuleRuntime), Vec<Diag>> {
    let base = format!("/magic/{id}");
    let mut diags = Vec::new();

    if !valid_rule_id(&id) {
        diags.push(diag(
            error_codes::EB005,
            &base,
            format!("规则 id `{id}` 不匹配 ^[a-z0-9][a-z0-9._-]{{0,63}}$"),
        ));
    }

    let RawRule {
        id: _field_id,
        name,
        mime,
        ext,
        priority,
        weight,
        template,
        min_size,
        read,
        kind: _kind,
        patterns,
        test,
    } = raw;

    let name = name.unwrap_or_else(|| id.clone());
    if name.trim().is_empty() {
        diags.push(diag(
            error_codes::EB005,
            &format!("{base}/name"),
            "name 不能为空",
        ));
    }

    if mime.is_none() {
        diags.push(diag(
            error_codes::EB004,
            &format!("{base}/mime"),
            "缺少必填字段 `mime`（23 §3.7）",
        ));
    }
    if let Some(mime) = mime.as_deref() {
        if !valid_mime(mime) {
            diags.push(diag(
                error_codes::EB005,
                &format!("{base}/mime"),
                format!("mime `{mime}` 不是合法的 type/subtype 或超过 128 字符"),
            ));
        }
    }

    if ext.len() > 16 {
        diags.push(diag(
            error_codes::EB005,
            &format!("{base}/ext"),
            format!("ext 最多 16 项，实际 {} 项", ext.len()),
        ));
    }
    for (index, value) in ext.iter().enumerate() {
        if !valid_ext(value) {
            diags.push(diag(
                error_codes::EB005,
                &format!("{base}/ext[{index}]"),
                format!("扩展名 `{value}` 必须匹配 ^[a-z0-9]{{1,16}}$ 且不含点"),
            ));
        }
    }

    let priority = priority.or(weight).unwrap_or(0);
    if !(-100..=100).contains(&priority) {
        diags.push(diag(
            error_codes::EB005,
            &format!("{base}/priority"),
            format!("priority/weight 必须在 -100..=100，实际为 {priority}"),
        ));
    }

    let min_size = match min_size {
        Some(value) if value >= 0 => value as u64,
        Some(value) => {
            diags.push(diag(
                error_codes::EB005,
                &format!("{base}/min_size"),
                format!("min_size 不能为负，实际为 {value}"),
            ));
            0
        }
        None => 0,
    };

    let read = match read {
        None => 64,
        Some(value) if (1..=MAX_HEAD_BYTES as i64).contains(&value) => value as u64,
        Some(value) => {
            diags.push(diag(
                error_codes::EB005,
                &format!("{base}/read"),
                format!("read 必须在 1..={MAX_HEAD_BYTES}，实际为 {value}（23 §3.7）"),
            ));
            MAX_HEAD_BYTES as u64
        }
    };

    match compile_patterns(&patterns, &test, read, &base) {
        Ok((compiled, meta)) => {
            if diags.is_empty() {
                Ok((
                    MagicRule {
                        id,
                        name,
                        mime,
                        ext,
                        priority,
                        patterns: compiled,
                        template,
                        min_size,
                    },
                    meta,
                ))
            } else {
                Err(diags)
            }
        }
        Err(mut pattern_diags) => {
            diags.append(&mut pattern_diags);
            Err(diags)
        }
    }
}

/// 编译规则里的 pattern 列表；typed `patterns` 与文档 `test[]` 互斥。
fn compile_patterns(
    patterns: &[RawPattern],
    tests: &[RawTest],
    read: u64,
    base: &str,
) -> Result<(Vec<MagicPattern>, RuleRuntime), Vec<Diag>> {
    if !patterns.is_empty() && !tests.is_empty() {
        return Err(vec![diag(
            error_codes::EB005,
            base,
            "同一规则不能同时使用 `patterns` 与 `test[]`",
        )]);
    }

    let mut diags = Vec::new();

    if !patterns.is_empty() {
        if patterns.len() > MAX_PATTERNS_PER_RULE {
            return Err(vec![diag(
                error_codes::EB020,
                base,
                format!(
                    "单规则 pattern 数 {} 超过上限 {MAX_PATTERNS_PER_RULE}（10 §4.5）",
                    patterns.len()
                ),
            )]);
        }
        let mut compiled = Vec::with_capacity(patterns.len());
        for (index, pattern) in patterns.iter().enumerate() {
            let path = format!("{base}/patterns[{index}]");
            match compile_pattern(pattern, read, &path) {
                Ok(pattern) => compiled.push(pattern),
                Err(err) => diags.push(err),
            }
        }
        if !diags.is_empty() {
            return Err(diags);
        }
        let optional = vec![false; compiled.len()];
        return Ok((
            compiled,
            RuleRuntime {
                read,
                mode: MatchMode::Any,
                optional,
            },
        ));
    }

    if tests.is_empty() {
        return Err(vec![diag(
            error_codes::EB004,
            base,
            "规则至少需要一个 `patterns` 或 `test[]` 条件（23 §3.7）",
        )]);
    }
    if tests.len() > MAX_TESTS_PER_RULE {
        return Err(vec![diag(
            error_codes::EB020,
            base,
            format!(
                "单规则 test 数 {} 超过上限 {MAX_TESTS_PER_RULE}（23 §3.7）",
                tests.len()
            ),
        )]);
    }

    let mut compiled = Vec::with_capacity(tests.len());
    let mut optional = Vec::with_capacity(tests.len());
    for (index, test) in tests.iter().enumerate() {
        let path = format!("{base}/test[{index}]");
        match compile_test(test, read, &path) {
            Ok((pattern, is_optional)) => {
                compiled.push(pattern);
                optional.push(is_optional);
            }
            Err(err) => diags.push(err),
        }
    }
    if !diags.is_empty() {
        return Err(diags);
    }
    Ok((
        compiled,
        RuleRuntime {
            read,
            mode: MatchMode::All,
            optional,
        },
    ))
}

/// 编译一个 typed `[[magic.<id>.patterns]]`。
fn compile_pattern(raw: &RawPattern, read: u64, path: &str) -> Result<MagicPattern, Diag> {
    let offset = parse_offset(raw.offset, raw.off, path)?;
    match raw.kind.to_ascii_lowercase().as_str() {
        "bytes" => {
            let spec = match (&raw.bytes, &raw.value) {
                (Some(spec), _) => spec,
                (None, Some(RawValue::Str(text))) => &ByteSpec::Hex(text.clone()),
                (None, Some(RawValue::Int(_))) => {
                    return Err(diag(
                        error_codes::EB005,
                        path,
                        "bytes pattern 的 `bytes` 必须是 hex 字符串或字节数组",
                    ));
                }
                (None, None) => {
                    return Err(diag(
                        error_codes::EB004,
                        path,
                        "bytes pattern 缺少必填字段 `bytes`",
                    ));
                }
            };
            let bytes = decode_byte_spec(spec, path, "bytes")?;
            if bytes.is_empty() {
                return Err(diag(error_codes::EB005, path, "bytes pattern 不能为空"));
            }
            let mask = match &raw.mask {
                Some(spec) => {
                    let mask = decode_byte_spec(spec, path, "mask")?;
                    if mask.len() != bytes.len() {
                        return Err(diag(
                            error_codes::EB005,
                            path,
                            format!(
                                "mask 长度 {} 与 bytes 长度 {} 不等",
                                mask.len(),
                                bytes.len()
                            ),
                        ));
                    }
                    Some(mask)
                }
                None => None,
            };
            check_end(offset, bytes.len() as u64, read, path)?;
            Ok(MagicPattern::Bytes {
                offset,
                bytes,
                mask,
            })
        }
        "ascii" => {
            if raw.mask.is_some() {
                return Err(diag(
                    error_codes::EB005,
                    path,
                    "ascii pattern 不支持 `mask`",
                ));
            }
            let text = raw.text.clone().ok_or_else(|| {
                diag(
                    error_codes::EB004,
                    path,
                    "ascii pattern 缺少必填字段 `text`",
                )
            })?;
            if text.is_empty() {
                return Err(diag(error_codes::EB005, path, "ascii pattern 不能为空"));
            }
            if !text.is_ascii() {
                return Err(diag(
                    error_codes::EB005,
                    path,
                    "ascii pattern 的 `text` 只能是 ASCII",
                ));
            }
            check_end(offset, text.len() as u64, read, path)?;
            Ok(MagicPattern::Ascii { offset, text })
        }
        "u16" => compile_uint(raw, offset, 2, read, path),
        "u32" => compile_uint(raw, offset, 4, read, path),
        other => Err(diag(
            error_codes::EB031,
            path,
            format!("未知 pattern 类型 `{other}`；只支持 bytes/ascii/u16/u32（23 §3.7）"),
        )),
    }
}

/// 编译 u16/u32 pattern。
fn compile_uint(
    raw: &RawPattern,
    offset: u64,
    width: u64,
    read: u64,
    path: &str,
) -> Result<MagicPattern, Diag> {
    if raw.mask.is_some() || raw.bytes.is_some() {
        return Err(diag(
            error_codes::EB005,
            path,
            "u16/u32 pattern 不支持 `mask`/`bytes`，请用 `value`",
        ));
    }
    let raw_value = match &raw.value {
        Some(RawValue::Int(value)) => *value,
        Some(RawValue::Str(_)) => {
            return Err(diag(
                error_codes::EB005,
                path,
                "u16/u32 pattern 的 `value` 必须是整数",
            ));
        }
        None => {
            return Err(diag(
                error_codes::EB004,
                path,
                "u16/u32 pattern 缺少必填字段 `value`",
            ));
        }
    };
    let endian = parse_endian(raw.endian.as_deref(), path)?;
    check_end(offset, width, read, path)?;

    if width == 2 {
        let value = u16::try_from(raw_value).map_err(|_| {
            diag(
                error_codes::EB005,
                path,
                format!("u16 value {raw_value} 超出 0..=65535"),
            )
        })?;
        Ok(MagicPattern::U16 {
            offset,
            value,
            endian,
        })
    } else {
        let value = u32::try_from(raw_value).map_err(|_| {
            diag(
                error_codes::EB005,
                path,
                format!("u32 value {raw_value} 超出 0..=4294967295"),
            )
        })?;
        Ok(MagicPattern::U32 {
            offset,
            value,
            endian,
        })
    }
}

/// 编译文档形式的 `[[magic.<id>.test]]`。
fn compile_test(raw: &RawTest, read: u64, path: &str) -> Result<(MagicPattern, bool), Diag> {
    if raw.jump.is_some() {
        return Err(diag(
            error_codes::EB005,
            path,
            "`test.jump` 暂不支持（10 §4.5 的 JUMP 指令不在本引擎子集内）",
        ));
    }
    let len = match raw.len {
        Some(value) if (1..=256).contains(&value) => value as u64,
        Some(value) => {
            return Err(diag(
                error_codes::EB005,
                path,
                format!("test.len 必须在 1..=256，实际为 {value}（23 §3.7）"),
            ));
        }
        None => {
            return Err(diag(error_codes::EB004, path, "test 缺少必填字段 `len`"));
        }
    };
    let text = raw.value.as_deref().ok_or_else(|| {
        diag(
            error_codes::EB004,
            path,
            "test 缺少必填字段 `value`（大写偶数长度 hex）",
        )
    })?;
    let bytes = decode_hex(text).ok_or_else(|| {
        diag(
            error_codes::EB005,
            path,
            format!("test.value `{text}` 不是偶数长度 hex"),
        )
    })?;
    if bytes.len() as u64 != len {
        return Err(diag(
            error_codes::EB005,
            path,
            format!("test.len={len} 与 value 解出的 {} 字节不等", bytes.len()),
        ));
    }
    let mask = match raw.mask.as_deref() {
        Some(text) => {
            let mask = decode_hex(text).ok_or_else(|| {
                diag(
                    error_codes::EB005,
                    path,
                    format!("test.mask `{text}` 不是偶数长度 hex"),
                )
            })?;
            if mask.len() != bytes.len() {
                return Err(diag(
                    error_codes::EB005,
                    path,
                    format!(
                        "test.mask 长度 {} 与 value 长度 {} 不等",
                        mask.len(),
                        bytes.len()
                    ),
                ));
            }
            Some(mask)
        }
        None => None,
    };
    let operation = raw.op.as_deref().unwrap_or("eq").to_ascii_lowercase();
    if operation != "eq" {
        return Err(diag(
            error_codes::EB005,
            path,
            format!("test.op `{operation}` 暂不支持；本引擎只做 eq"),
        ));
    }
    let offset = match raw.off {
        Some(value) if value >= 0 => value as u64,
        Some(value) => {
            return Err(diag(
                error_codes::EB005,
                path,
                format!("test.off 不能为负，实际为 {value}"),
            ));
        }
        None => 0,
    };
    check_end(offset, bytes.len() as u64, read, path)?;
    Ok((
        MagicPattern::Bytes {
            offset,
            bytes,
            mask,
        },
        !raw.required,
    ))
}

/// 解析 `offset` / `off` 别名。
fn parse_offset(offset: Option<i64>, off: Option<i64>, path: &str) -> Result<u64, Diag> {
    match (offset, off) {
        (Some(left), Some(right)) if left != right => Err(diag(
            error_codes::EB005,
            path,
            format!("`offset`({left}) 与 `off`({right}) 同时出现且不一致"),
        )),
        (Some(value), _) | (_, Some(value)) => {
            if value < 0 {
                Err(diag(
                    error_codes::EB005,
                    path,
                    format!("offset 不能为负，实际为 {value}"),
                ))
            } else {
                Ok(value as u64)
            }
        }
        (None, None) => Ok(0),
    }
}

/// 校验 `offset + len` 落在规则声明的读取窗口内。
fn check_end(offset: u64, len: u64, read: u64, path: &str) -> Result<(), Diag> {
    let end = offset
        .checked_add(len)
        .ok_or_else(|| diag(error_codes::EB005, path, "offset + len 溢出 u64"))?;
    if end > read {
        return Err(diag(
            error_codes::EB005,
            path,
            format!("offset {offset} + len {len} = {end} 超出读取窗口 {read}（23 §3.7）"),
        ));
    }
    Ok(())
}

/// 解析字节序；缺省为大端（与网络序一致）。
fn parse_endian(value: Option<&str>, path: &str) -> Result<Endian, Diag> {
    match value.map(str::to_ascii_lowercase).as_deref() {
        None | Some("big" | "be") => Ok(Endian::Big),
        Some("little" | "le") => Ok(Endian::Little),
        Some(other) => Err(diag(
            error_codes::EB005,
            path,
            format!("未知 endian `{other}`；只支持 big/little/be/le"),
        )),
    }
}

/// 校验规则 id 的字符集与长度。
fn valid_rule_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first_ok = bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit();
    first_ok
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
}

/// 校验 `type/subtype` 形式与 128 字符上限。
fn valid_mime(mime: &str) -> bool {
    if mime.is_empty() || mime.len() > 128 {
        return false;
    }
    let Some((kind, subtype)) = mime.split_once('/') else {
        return false;
    };
    !kind.is_empty()
        && !subtype.is_empty()
        && !subtype.contains('/')
        && mime.chars().all(|ch| {
            ch == '/'
                || (!ch.is_whitespace()
                    && (ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.' | '_')))
        })
}

/// 校验扩展名：`^[a-z0-9]{1,16}$`，不含点。
fn valid_ext(ext: &str) -> bool {
    !ext.is_empty()
        && ext.len() <= 16
        && ext
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// 十六进制解码；容忍 `0x` 前缀、大小写与空白分隔。
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    let trimmed = text.trim();
    let without_prefix = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let compact: String = without_prefix
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    if compact.is_empty() || compact.len() % 2 != 0 {
        return None;
    }
    let bytes = compact.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_value(bytes[index])?;
        let low = hex_value(bytes[index + 1])?;
        out.push((high << 4) | low);
        index += 2;
    }
    Some(out)
}

/// 单个 hex 字符转 0..=15。
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// 解码 `bytes` / `mask` 的两种写法。
fn decode_byte_spec(spec: &ByteSpec, path: &str, field: &str) -> Result<Vec<u8>, Diag> {
    match spec {
        ByteSpec::Hex(text) => decode_hex(text).ok_or_else(|| {
            diag(
                error_codes::EB005,
                path,
                format!("{field} `{text}` 不是合法的偶数长度 hex"),
            )
        }),
        ByteSpec::Bytes(values) => {
            let mut out = Vec::with_capacity(values.len());
            for (index, value) in values.iter().enumerate() {
                let byte = u8::try_from(*value).map_err(|_| {
                    diag(
                        error_codes::EB005,
                        &format!("{path}/{field}[{index}]"),
                        format!("字节值 {value} 超出 0..=255"),
                    )
                })?;
                out.push(byte);
            }
            Ok(out)
        }
    }
}

/// 由 toml 解析错误构造 `EB003`；有 span 时附 `ByteRange`。
fn toml_diag(err: &toml::de::Error) -> Diag {
    let mut diag = Diag::error(error_codes::EB003, err.message());
    if let Some(span) = err.span() {
        let start = span.start as u64;
        let len = span.end.saturating_sub(span.start) as u64;
        if let Some(range) = ByteRange::from_len(Offset::new(start), len) {
            diag = diag.with_range(range);
        }
    }
    diag
}

/// 构造带结构路径的诊断。
fn diag(code: Code, path: &str, detail: impl Into<String>) -> Diag {
    Diag::error(code, detail).with_path(path)
}

/// 顶层文档。
#[derive(Deserialize)]
struct RawDoc {
    schema_version: Option<i64>,
    kind: Option<String>,
    magic: Option<MagicBlock>,
}

/// `magic` 既可以是 `[[magic]]` 数组，也可以是 `[magic.<id>]` 表。
#[derive(Deserialize)]
#[serde(untagged)]
enum MagicBlock {
    /// `[[magic]]` 数组形式；每项必须自带 `id`。
    List(Vec<RawRule>),
    /// `[magic.<id>]` 表形式；表键即 id。
    Map(BTreeMap<String, RawRule>),
}

impl MagicBlock {
    /// 统一成 `(表键, 规则)`；数组形式表键为 `None`。
    fn into_entries(self) -> Vec<(Option<String>, RawRule)> {
        match self {
            Self::List(rules) => rules.into_iter().map(|rule| (None, rule)).collect(),
            Self::Map(rules) => rules
                .into_iter()
                .map(|(id, rule)| (Some(id), rule))
                .collect(),
        }
    }
}

/// 未编译的规则。
#[derive(Deserialize)]
struct RawRule {
    id: Option<String>,
    name: Option<String>,
    mime: Option<String>,
    #[serde(default)]
    ext: Vec<String>,
    priority: Option<i32>,
    weight: Option<i32>,
    template: Option<String>,
    min_size: Option<i64>,
    read: Option<i64>,
    #[allow(dead_code)]
    kind: Option<String>,
    #[serde(default)]
    patterns: Vec<RawPattern>,
    #[serde(default)]
    test: Vec<RawTest>,
}

/// 未编译的 typed pattern。
#[derive(Deserialize)]
struct RawPattern {
    #[serde(rename = "type")]
    kind: String,
    offset: Option<i64>,
    off: Option<i64>,
    bytes: Option<ByteSpec>,
    mask: Option<ByteSpec>,
    text: Option<String>,
    value: Option<RawValue>,
    endian: Option<String>,
}

/// 未编译的文档形式 test。
#[derive(Deserialize)]
struct RawTest {
    off: Option<i64>,
    jump: Option<i64>,
    len: Option<i64>,
    value: Option<String>,
    mask: Option<String>,
    op: Option<String>,
    #[serde(default = "default_true")]
    required: bool,
}

/// `required` 的缺省值。
fn default_true() -> bool {
    true
}

/// 字节串的两种 TOML 写法：hex 字符串或 `u8` 数组。
#[derive(Deserialize)]
#[serde(untagged)]
enum ByteSpec {
    /// 连续 hex，可带 `0x` 前缀与空白分隔。
    Hex(String),
    /// 0..=255 的整数数组。
    Bytes(Vec<i64>),
}

/// `value` 的两种 TOML 类型；bytes 用字符串、u16/u32 用整数。
#[derive(Deserialize)]
#[serde(untagged)]
enum RawValue {
    /// TOML 整数。
    Int(i64),
    /// TOML 字符串。
    Str(String),
}
