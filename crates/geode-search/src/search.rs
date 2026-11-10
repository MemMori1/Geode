//! 统一搜索入口与流式扫描框架。
//!
//! 四种查询共享同一条“按块读取 → 重叠拼接 → 只报告起始于本块新字节的
//! 命中”的流水线。它不建立索引，也不把整个数据源读进内存；读多少由
//! [`SearchOptions::chunk_size`] 决定。跨块匹配由起始块负责报告，因此
//! 重叠区只用于验证，不会产生重复结果。
//!
//! 空洞语义：优先使用 [`Provider::readable_ranges`] 把扫描限制在可读区间；
//! 若 provider 没有提供区间而 `read_range` 上报了 `holes`，则保守地跳过
//! 整块（可能漏掉同块内的有效命中，但绝不会把补零当真实数据）。

use geode_core::{ByteRange, Offset};
use geode_provider::Provider;
use regex::bytes::Regex;

use crate::SearchError;
use crate::error::Warning;
use crate::numeric::{CompiledNumeric, NumericQuery};
use crate::pattern::{HexPattern, MaskByte};
use crate::regex_query::RegexQuery;
use crate::text::TextQuery;

/// 默认结果上限。
const DEFAULT_MAX_RESULTS: usize = 100_000;
/// 默认块大小：1 MiB。
const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;
/// 正则无上界时的默认重叠窗口：64 KiB。
const DEFAULT_REGEX_OVERLAP: usize = 64 * 1024;
/// 扫描循环中检查取消的字节间隔。
const CANCEL_CHECK_INTERVAL: usize = 64 * 1024;

/// 一次搜索的查询类型。
#[derive(Clone, Debug, PartialEq)]
pub enum SearchQuery {
    /// hex 通配模式。
    Hex(HexPattern),
    /// 多编码文本。
    Text(TextQuery),
    /// 字节正则。
    Regex(RegexQuery),
    /// 数值范围。
    Numeric(NumericQuery),
}

/// 搜索选项。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchOptions {
    /// 最多返回多少条结果；达到后立即停止并置 `truncated = true`。
    pub max_results: usize,
    /// 每次从 provider 读取的新字节数（有效值至少为 1）。
    pub chunk_size: usize,
    /// 正则查询的固定跨块重叠窗口（默认 64 KiB）。
    ///
    /// 由于 `regex` crate 不暴露最大匹配长度，无法像固定模式那样使用
    /// `len - 1`；这里采用设计文档的方案 C，并在结果中附带 E7104 警告。
    pub regex_fallback_overlap: usize,
}

impl Default for SearchOptions {
    fn default() -> Self {
        Self {
            max_results: DEFAULT_MAX_RESULTS,
            chunk_size: DEFAULT_CHUNK_SIZE,
            regex_fallback_overlap: DEFAULT_REGEX_OVERLAP,
        }
    }
}

impl SearchOptions {
    /// 使用默认值构造。
    pub fn new() -> Self {
        Self::default()
    }

    /// 设置结果上限。
    pub fn with_max_results(mut self, max_results: usize) -> Self {
        self.max_results = max_results;
        self
    }

    /// 设置块大小（0 会在扫描时按 1 处理）。
    pub fn with_chunk_size(mut self, chunk_size: usize) -> Self {
        self.chunk_size = chunk_size;
        self
    }

    /// 设置正则兜底重叠窗口。
    pub fn with_regex_fallback_overlap(mut self, overlap: usize) -> Self {
        self.regex_fallback_overlap = overlap;
        self
    }
}

/// 搜索的最终结果。
///
/// 取消或截断时 `hits` 仍然包含此前已经找到的结果（设计文档 F14/F15）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchOutcome {
    /// 命中区间，按起始偏移升序。
    pub hits: Vec<ByteRange>,
    /// 是否因为达到 [`SearchOptions::max_results`] 而提前停止。
    pub truncated: bool,
    /// 是否因为取消回调返回 `true` 而提前停止。
    pub cancelled: bool,
    /// 实际扫描过的新字节数（不含重叠区与空洞）。
    pub bytes_scanned: u64,
    /// 非致命警告（当前仅正则 E7104）。
    pub warning: Option<Warning>,
}

/// 在 provider 上执行一次流式搜索。
///
/// - 按 [`SearchOptions::chunk_size`] 分块读取；固定模式使用
///   `pattern_len - 1` 的重叠，正则使用 `regex_fallback_overlap`。
/// - 每块开始及每 64 KiB 调用一次 `cancel`；返回 `true` 时保留已有结果并
///   置 [`SearchOutcome::cancelled`]。
/// - 达到 `max_results` 时立即停止并置 [`SearchOutcome::truncated`]。
/// - 短读、EOF、越界和空洞都不会 panic；空洞按“不可搜索”处理。
///
/// # Errors
///
/// 查询非法（E7102/E7103/E7105）或 provider 读取失败（E6xxx）时返回错误。
pub fn search(
    provider: &dyn Provider,
    query: &SearchQuery,
    opts: &SearchOptions,
    cancel: &dyn Fn() -> bool,
) -> Result<SearchOutcome, SearchError> {
    let plan = Plan::compile(query, opts)?;
    let mut outcome = SearchOutcome {
        warning: plan.warning.clone(),
        ..SearchOutcome::default()
    };

    if opts.max_results == 0 {
        outcome.truncated = true;
        return Ok(outcome);
    }
    if cancel() {
        outcome.cancelled = true;
        return Ok(outcome);
    }

    let limit = provider.size().lower_bound();
    let regions = readable_regions(provider, limit);
    let mut collector = HitCollector::new(opts.max_results, cancel);
    let mut bytes_scanned = 0u64;
    for region in regions {
        if collector.is_done() {
            break;
        }
        scan_region(
            provider,
            region,
            &plan,
            opts,
            &mut collector,
            &mut bytes_scanned,
        )?;
    }

    outcome.hits = collector.hits;
    outcome.truncated = collector.truncated;
    outcome.cancelled = collector.cancelled;
    outcome.bytes_scanned = bytes_scanned;
    Ok(outcome)
}

/// 归一化后的扫描计划。
struct Plan {
    matcher: Matcher,
    overlap: usize,
    warning: Option<Warning>,
}

enum Matcher {
    Fixed(FixedMatcher),
    Regex(RegexMatcher),
    Numeric(CompiledNumeric),
}

struct FixedMatcher {
    bytes: Vec<MaskByte>,
    /// 用于快速跳过的固定位（偏移、值、掩码）。
    anchor: Anchor,
}

#[derive(Clone, Copy)]
struct Anchor {
    offset: usize,
    value: u8,
    mask: u8,
}

struct RegexMatcher {
    regex: Regex,
    overlapping: bool,
}

impl Plan {
    fn compile(query: &SearchQuery, opts: &SearchOptions) -> Result<Self, SearchError> {
        match query {
            SearchQuery::Hex(pattern) => {
                if pattern.is_empty() {
                    return Err(SearchError::bad_pattern("hex 模式为空"));
                }
                let matcher = FixedMatcher::new(pattern.bytes.clone())?;
                Ok(Self {
                    overlap: pattern.len().saturating_sub(1),
                    matcher: Matcher::Fixed(matcher),
                    warning: None,
                })
            }
            SearchQuery::Text(text) => {
                let needle = text.needle_bytes()?;
                if needle.is_empty() {
                    return Err(SearchError::bad_pattern("文本模式为空"));
                }
                let matcher = FixedMatcher::from_bytes(&needle)?;
                Ok(Self {
                    overlap: needle.len().saturating_sub(1),
                    matcher: Matcher::Fixed(matcher),
                    warning: None,
                })
            }
            SearchQuery::Regex(query) => {
                let regex = query.compile()?;
                Ok(Self {
                    overlap: opts.regex_fallback_overlap,
                    matcher: Matcher::Regex(RegexMatcher {
                        regex,
                        overlapping: query.overlapping,
                    }),
                    warning: Some(Warning::regex_overlap()),
                })
            }
            SearchQuery::Numeric(numeric) => {
                let compiled = numeric.compile()?;
                Ok(Self {
                    overlap: compiled.width().saturating_sub(1),
                    matcher: Matcher::Numeric(compiled),
                    warning: None,
                })
            }
        }
    }
}

impl FixedMatcher {
    fn new(bytes: Vec<MaskByte>) -> Result<Self, SearchError> {
        if bytes.is_empty() {
            return Err(SearchError::bad_pattern("hex 模式为空"));
        }
        let anchor = best_anchor(&bytes)
            .ok_or_else(|| SearchError::bad_pattern("模式没有任何固定字节（E7102）"))?;
        Ok(Self { bytes, anchor })
    }

    fn from_bytes(needle: &[u8]) -> Result<Self, SearchError> {
        Self::new(needle.iter().copied().map(MaskByte::exact).collect())
    }
}

/// 选出信息量最高的固定位；相同信息量时选更靠前的字节。
fn best_anchor(bytes: &[MaskByte]) -> Option<Anchor> {
    let mut best: Option<Anchor> = None;
    for (offset, byte) in bytes.iter().enumerate() {
        if byte.mask == 0 {
            continue;
        }
        let score = byte.mask.count_ones();
        let better = best.is_none_or(|current| score > current.mask.count_ones());
        if better {
            best = Some(Anchor {
                offset,
                value: byte.value,
                mask: byte.mask,
            });
        }
    }
    best
}

/// 收集命中并维护取消/截断状态。
struct HitCollector<'a> {
    hits: Vec<ByteRange>,
    max_results: usize,
    truncated: bool,
    cancelled: bool,
    cancel: &'a dyn Fn() -> bool,
}

impl<'a> HitCollector<'a> {
    fn new(max_results: usize, cancel: &'a dyn Fn() -> bool) -> Self {
        Self {
            hits: Vec::new(),
            max_results,
            truncated: false,
            cancelled: false,
            cancel,
        }
    }

    fn is_done(&self) -> bool {
        self.truncated || self.cancelled || self.hits.len() >= self.max_results
    }

    fn check_cancel(&mut self) -> bool {
        if (self.cancel)() {
            self.cancelled = true;
        }
        self.cancelled
    }

    /// 周期性检查取消，避免每个字节都调用闭包。
    fn periodic_cancel(&mut self, position: usize, last_check: &mut usize) -> bool {
        if position.saturating_sub(*last_check) >= CANCEL_CHECK_INTERVAL {
            *last_check = position;
            self.check_cancel()
        } else {
            false
        }
    }

    /// 追加一条命中；达到上限返回 `false` 以通知调用方停止。
    fn push(&mut self, start: u64, len: usize) -> bool {
        if self.is_done() {
            return false;
        }
        let Some(range) = ByteRange::from_len(Offset::new(start), len as u64) else {
            // start + len 溢出 u64：数据源不可能有这么大，直接忽略。
            return true;
        };
        self.hits.push(range);
        if self.hits.len() >= self.max_results {
            self.truncated = true;
            false
        } else {
            true
        }
    }
}

/// 计算可扫描区间：排序、裁剪到 size、合并重叠/相邻区间。
fn readable_regions(provider: &dyn Provider, limit: u64) -> Vec<ByteRange> {
    if limit == 0 {
        return Vec::new();
    }
    match provider.readable_ranges() {
        None => ByteRange::from_len(Offset::ZERO, limit)
            .into_iter()
            .collect(),
        Some(mut ranges) => {
            ranges.retain(|range| !range.is_empty());
            ranges.sort_unstable_by_key(|range| range.start().get());
            let mut merged: Vec<ByteRange> = Vec::with_capacity(ranges.len());
            for range in ranges {
                let range = range.clip(limit);
                if range.is_empty() {
                    continue;
                }
                if let Some(last) = merged.last_mut() {
                    if range.start().get() <= last.end().get() {
                        let envelope = last.envelope(range);
                        *last = envelope;
                        continue;
                    }
                }
                merged.push(range);
            }
            merged
        }
    }
}

/// 扫描一个可读区间。
fn scan_region(
    provider: &dyn Provider,
    region: ByteRange,
    plan: &Plan,
    opts: &SearchOptions,
    collector: &mut HitCollector<'_>,
    bytes_scanned: &mut u64,
) -> Result<(), SearchError> {
    let region_start = region.start().get();
    let region_end = region.end().get();
    let chunk_size = opts.chunk_size.max(1) as u64;
    let mut start = region_start;
    let mut buffer: Vec<u8> = Vec::new();
    // 正则 non-overlapping 模式下，上一条命中（可能跨块）的绝对结束位置。
    let mut last_regex_end: Option<u64> = None;

    while start < region_end {
        if collector.check_cancel() {
            break;
        }
        let remaining = region_end - start;
        let block_len = chunk_size.min(remaining);
        let block_len_usize = usize::try_from(block_len).unwrap_or(usize::MAX);
        let want_u64 = remaining.min(block_len.saturating_add(plan.overlap as u64));
        let want = usize::try_from(want_u64).unwrap_or(usize::MAX);
        buffer.clear();
        buffer.resize(want, 0);

        match read_chunk(provider, start, &mut buffer)? {
            ChunkRead::Holes => {
                // 无法定位空洞时保守跳过整块，绝不把补零当数据。
                start += block_len;
                last_regex_end = None;
            }
            ChunkRead::Data(filled) => {
                let scan_len = block_len_usize.min(filled);
                if scan_len > 0 {
                    match &plan.matcher {
                        Matcher::Fixed(matcher) => {
                            scan_fixed(matcher, &buffer[..filled], scan_len, start, collector)
                        }
                        Matcher::Regex(matcher) => scan_regex(
                            &matcher.regex,
                            matcher.overlapping,
                            &buffer[..filled],
                            scan_len,
                            start,
                            collector,
                            &mut last_regex_end,
                        ),
                        Matcher::Numeric(matcher) => {
                            scan_numeric(*matcher, &buffer[..filled], scan_len, start, collector)
                        }
                    }
                    *bytes_scanned = bytes_scanned.saturating_add(scan_len as u64);
                }
                if collector.is_done() {
                    break;
                }
                // 短读时按实际读到的字节推进，避免跳过 provider 尚未给出的数据；
                // 完全读不到时按块推进，防止零进展死循环。
                if filled == 0 {
                    start += block_len;
                    last_regex_end = None;
                } else {
                    start += scan_len as u64;
                }
            }
        }
    }
    Ok(())
}

enum ChunkRead {
    Data(usize),
    Holes,
}

/// 读取从 `start` 开始的一块数据，循环处理短读。
///
/// 只要任一次 `read_range` 上报 `holes > 0`，整块视为不可靠并返回
/// [`ChunkRead::Holes`]（`ReadReport` 不提供空洞位置）。
fn read_chunk(
    provider: &dyn Provider,
    start: u64,
    buf: &mut [u8],
) -> Result<ChunkRead, SearchError> {
    let mut filled = 0usize;
    while filled < buf.len() {
        let at = start.saturating_add(filled as u64);
        let report = provider.read_range(Offset::new(at), &mut buf[filled..])?;
        if report.holes > 0 {
            return Ok(ChunkRead::Holes);
        }
        let count = usize::try_from(report.filled)
            .unwrap_or(usize::MAX)
            .min(buf.len() - filled);
        if count == 0 {
            break;
        }
        filled += count;
        if report.eof_hit {
            break;
        }
    }
    Ok(ChunkRead::Data(filled))
}

/// 固定字节/掩码模式扫描。
fn scan_fixed(
    matcher: &FixedMatcher,
    buf: &[u8],
    block_len: usize,
    base: u64,
    collector: &mut HitCollector<'_>,
) {
    let pattern_len = matcher.bytes.len();
    if pattern_len == 0 || buf.len() < pattern_len {
        return;
    }
    let max_start = block_len.min(buf.len() - pattern_len + 1);
    let mut position = 0usize;
    let mut last_cancel_check = 0usize;
    while position < max_start {
        if collector.periodic_cancel(position, &mut last_cancel_check) {
            return;
        }
        let anchor_byte = buf[position + matcher.anchor.offset];
        if anchor_byte & matcher.anchor.mask != matcher.anchor.value & matcher.anchor.mask {
            position += 1;
            continue;
        }
        let matched = matcher
            .bytes
            .iter()
            .enumerate()
            .all(|(index, pattern)| pattern.matches(buf[position + index]));
        if matched {
            let Some(absolute) = base.checked_add(position as u64) else {
                return;
            };
            if !collector.push(absolute, pattern_len) {
                return;
            }
        }
        position += 1;
    }
}

/// 正则扫描。
#[allow(clippy::too_many_arguments)]
fn scan_regex(
    regex: &Regex,
    overlapping: bool,
    buf: &[u8],
    block_len: usize,
    base: u64,
    collector: &mut HitCollector<'_>,
    last_end: &mut Option<u64>,
) {
    let mut at = 0usize;
    let mut last_cancel_check = 0usize;
    while at <= buf.len() {
        if collector.periodic_cancel(at, &mut last_cancel_check) {
            return;
        }
        let Some(found) = regex.find_at(buf, at) else {
            break;
        };
        if found.start() == found.end() {
            at = found.end().saturating_add(1);
            continue;
        }
        if found.start() < block_len {
            let absolute_start = base.saturating_add(found.start() as u64);
            let absolute_end = base.saturating_add(found.end() as u64);
            let already_covered = !overlapping && last_end.is_some_and(|end| absolute_start < end);
            if !already_covered {
                if !collector.push(absolute_start, found.end() - found.start()) {
                    return;
                }
                if !overlapping {
                    *last_end = Some(absolute_end);
                }
            }
        }
        at = if overlapping {
            found.start().saturating_add(1)
        } else {
            found.end().max(found.start() + 1)
        };
    }
}

/// 数值扫描。
fn scan_numeric(
    matcher: CompiledNumeric,
    buf: &[u8],
    block_len: usize,
    base: u64,
    collector: &mut HitCollector<'_>,
) {
    let width = matcher.width();
    if width == 0 || buf.len() < width {
        return;
    }
    let max_start = block_len.min(buf.len() - width + 1);
    let mut position = 0usize;
    let mut last_cancel_check = 0usize;
    while position < max_start {
        if collector.periodic_cancel(position, &mut last_cancel_check) {
            return;
        }
        let Some(absolute) = base.checked_add(position as u64) else {
            return;
        };
        if matcher.matches(absolute, &buf[position..position + width])
            && !collector.push(absolute, width)
        {
            return;
        }
        position += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextEncoding;
    use geode_provider::MemoryProvider;

    fn memory(bytes: &[u8]) -> MemoryProvider {
        MemoryProvider::new("test", bytes.to_vec())
    }

    fn ranges(outcome: &SearchOutcome) -> Vec<(u64, u64)> {
        outcome
            .hits
            .iter()
            .map(|range| (range.start().get(), range.end().get()))
            .collect()
    }

    #[test]
    fn fixed_search_finds_all_occurrences() {
        let provider = memory(b"xxABxxABx");
        let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
        let outcome =
            search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
        assert_eq!(ranges(&outcome), vec![(2, 4), (6, 8)]);
        assert_eq!(outcome.bytes_scanned, 9);
        assert!(!outcome.truncated);
        assert!(!outcome.cancelled);
        assert!(outcome.warning.is_none());
    }

    #[test]
    fn cross_chunk_boundary_hits_are_reported_once() {
        let provider = memory(b"xxxxNEEDLExxxx");
        let query = SearchQuery::Hex(HexPattern::from_bytes(b"NEEDLE"));
        for chunk in 1..=12 {
            let opts = SearchOptions::default().with_chunk_size(chunk);
            let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
            assert_eq!(ranges(&outcome), vec![(4, 10)], "chunk={chunk}");
        }
    }

    #[test]
    fn truncates_at_max_results() {
        let provider = memory(b"ABABABAB");
        let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
        let opts = SearchOptions::default().with_max_results(3);
        let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
        assert_eq!(ranges(&outcome), vec![(0, 2), (2, 4), (4, 6)]);
        assert!(outcome.truncated);
    }

    #[test]
    fn cancellation_returns_partial_results() {
        let data: Vec<u8> = std::iter::repeat_n(*b"AB", 32 * 1024).flatten().collect();
        let provider = memory(&data);
        let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
        let opts = SearchOptions::default().with_chunk_size(16 * 1024);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let cancel = || calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 1;
        let outcome = search(&provider, &query, &opts, &cancel).expect("搜索");
        assert!(outcome.cancelled);
        assert!(!outcome.truncated);
        assert!(!outcome.hits.is_empty());
        assert!(outcome.bytes_scanned > 0);
    }

    #[test]
    fn regex_search_reports_warning_and_matches() {
        let provider = memory(b"abc 123 def 456");
        let query = SearchQuery::Regex(RegexQuery::new(r"\d{3}"));
        let outcome =
            search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
        assert_eq!(ranges(&outcome), vec![(4, 7), (12, 15)]);
        let warning = outcome.warning.expect("应有 E7104 警告");
        assert_eq!(warning.code, "E7104");
    }

    #[test]
    fn regex_non_overlapping_and_overlapping() {
        let provider = memory(b"aaa");
        let non_overlap = SearchQuery::Regex(RegexQuery::new("aa"));
        let outcome = search(&provider, &non_overlap, &SearchOptions::default(), &|| {
            false
        })
        .expect("搜索");
        assert_eq!(ranges(&outcome), vec![(0, 2)]);

        let overlap = SearchQuery::Regex(RegexQuery::new("aa").with_overlapping(true));
        let outcome =
            search(&provider, &overlap, &SearchOptions::default(), &|| false).expect("搜索");
        assert_eq!(ranges(&outcome), vec![(0, 2), (1, 3)]);
    }

    #[test]
    fn regex_match_crosses_chunk_boundary() {
        let provider = memory(b"xxABCyy");
        let query = SearchQuery::Regex(RegexQuery::new("ABC"));
        for chunk in 1..=8 {
            let opts = SearchOptions::default().with_chunk_size(chunk);
            let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
            assert_eq!(ranges(&outcome), vec![(2, 5)], "chunk={chunk}");
            assert_eq!(outcome.warning.expect("警告").code, "E7104");
        }
    }

    #[test]
    fn numeric_search_across_chunks() {
        let mut data = vec![0u8; 5];
        data.extend_from_slice(&0x1234u16.to_le_bytes());
        data.extend_from_slice(&[0xFF, 0xFF]);
        let provider = memory(&data);
        let query = SearchQuery::Numeric(
            NumericQuery::unsigned(
                crate::numeric::NumericKind::U16,
                crate::numeric::Endian::Little,
                0x1234,
                0x1234,
            )
            .expect("合法"),
        );
        for chunk in [1usize, 2, 3, 4] {
            let opts = SearchOptions::default().with_chunk_size(chunk);
            let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
            assert_eq!(ranges(&outcome), vec![(5, 7)], "chunk={chunk}");
        }
    }

    #[test]
    fn empty_provider_and_zero_limit_are_clean() {
        let provider = memory(b"");
        let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
        let outcome =
            search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
        assert!(outcome.hits.is_empty());
        assert_eq!(outcome.bytes_scanned, 0);

        let provider = memory(b"AB");
        let opts = SearchOptions::default().with_max_results(0);
        let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
        assert!(outcome.truncated);
        assert!(outcome.hits.is_empty());
    }

    #[test]
    fn text_utf16_search_finds_encoded_needle() {
        let provider = memory(&[0xFF, 0xFE, b'A', 0, b'B', 0, b'C', 0]);
        let query = SearchQuery::Text(TextQuery::new("AB", TextEncoding::Utf16Le).with_bom(true));
        let outcome =
            search(&provider, &query, &SearchOptions::default(), &|| false).expect("搜索");
        assert_eq!(ranges(&outcome), vec![(0, 6)]);
    }

    #[test]
    fn zero_chunk_size_is_treated_as_one() {
        let provider = memory(b"ABAB");
        let query = SearchQuery::Hex(HexPattern::from_bytes(b"AB"));
        let opts = SearchOptions::default().with_chunk_size(0);
        let outcome = search(&provider, &query, &opts, &|| false).expect("搜索");
        assert_eq!(ranges(&outcome), vec![(0, 2), (2, 4)]);
    }
}
