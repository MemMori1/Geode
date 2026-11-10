//! 64 KiB 页缓存与 `CachedProvider` 装饰器（T1.3 lazy loading）。
//!
//! DESIGN: 06-data-providers.md §3.4、§4.1、§4.3；04 §4.1.1
//!
//! 三个硬约束：
//!
//! 1. **页大小 64 KiB、预算 64 MiB**——1024 页，打开 4 GiB 文件不读一个字节；
//! 2. **CLOCK（二次机会）淘汰**，不是严格 LRU——命中只把引用位翻真，滚动时
//!    命中路径不搬链表，1024 页规模下命中率与 LRU 的差距 < 2%（06 §3.4）；
//! 3. **volatile 源 TTL=0**——进程内存、UDP 等源直接绕过缓存；空洞页做
//!    2 s 负缓存，避免对同一片未映射页反复发起 syscall。
//!
//! 缓存键是 `(ProviderId, page_index)`，因此同一 [`PageCache`] 可以安全地服务
//! 多个 provider。文件尾的短页按实际长度缓存；**短读不会被补零**（补零只
//! 发生在 provider 明确上报 `holes` 时，且零字节由源语义保证）；没有碰到
//! EOF 的非满页不缓存，避免把临时短读固定成永久短页。

use crate::{Provider, ProviderError, ReadReport};
use geode_core::{ByteRange, Offset, ProviderId};
use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// 缓存页大小：64 KiB。
pub const PAGE_SIZE: usize = 64 * 1024;

/// 页大小（`u64` 版本，便于和 [`Offset`] 运算）。
pub const PAGE_SIZE_U64: u64 = PAGE_SIZE as u64;

/// 默认缓存预算：64 MiB（1024 页）。
pub const DEFAULT_CACHE_BYTES: usize = 64 * 1024 * 1024;

/// 默认空洞负缓存 TTL：2 s。
pub const DEFAULT_HOLE_TTL: Duration = Duration::from_secs(2);

type PageKey = (ProviderId, u64);

/// 页缓存配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageCacheConfig {
    /// 缓存预算（字节）。会被向下取整到整页；0 表示禁用缓存。
    pub budget_bytes: usize,
    /// 空洞负缓存 TTL。设为 0 表示不缓存空洞页。
    pub hole_ttl: Duration,
}

impl Default for PageCacheConfig {
    fn default() -> Self {
        Self {
            budget_bytes: DEFAULT_CACHE_BYTES,
            hole_ttl: DEFAULT_HOLE_TTL,
        }
    }
}

/// 缓存统计快照。
///
/// `reads` 是**底层** `Provider::read_range` 的调用次数（一个页可能需要多次
/// 短读才能填满）；`hits + misses` 是逻辑页请求数。负缓存命中算 `hits`，
/// 且不增加 `reads`。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// 底层读调用次数。
    pub reads: u64,
    /// 缓存命中次数（含空洞负缓存命中）。
    pub hits: u64,
    /// 未命中次数（volatile 源每次请求都算未命中）。
    pub misses: u64,
    /// 当前缓存的**数据字节数**（不含空洞负缓存条目）。
    pub bytes_cached: u64,
}

impl CacheStats {
    /// 逻辑页请求数（`hits + misses`）。
    #[must_use]
    pub fn requests(&self) -> u64 {
        self.hits + self.misses
    }

    /// 命中率（`hits / requests`）；没有请求时返回 0。
    #[must_use]
    pub fn hit_rate(&self) -> f64 {
        let requests = self.requests();
        if requests == 0 {
            0.0
        } else {
            self.hits as f64 / requests as f64
        }
    }
}

/// 一次页填充的结果。
///
/// [`PageCache::get_or_fill`] 只返回 `data`；需要知道是否读到源末尾的调用方
/// 用 [`PageCache::get_page`]。
#[derive(Clone, Debug)]
pub struct PageFill {
    /// 页内有效字节。文件尾的页可能短于 [`PAGE_SIZE`]（甚至为空）。
    pub data: Arc<[u8]>,
    /// 这次填充是否碰到了数据源末尾。
    pub eof: bool,
}

#[derive(Debug)]
enum EntryKind {
    Data(Arc<[u8]>),
    /// 整页都是空洞：不占数据预算，命中时返回同样长度的零页。
    Hole {
        len: usize,
    },
}

impl EntryKind {
    fn data_len(&self) -> usize {
        match self {
            Self::Data(data) => data.len(),
            Self::Hole { .. } => 0,
        }
    }
}

#[derive(Debug)]
struct Entry {
    key: PageKey,
    kind: EntryKind,
    eof: bool,
    /// `Some` 表示空洞负缓存条目，到点即失效。
    expires: Option<Instant>,
    /// CLOCK 引用位。
    referenced: bool,
}

impl Entry {
    fn data_len(&self) -> usize {
        match &self.kind {
            EntryKind::Data(data) => data.len(),
            EntryKind::Hole { .. } => 0,
        }
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.expires.is_some_and(|deadline| now >= deadline)
    }

    fn to_fill(&self) -> PageFill {
        let data = match &self.kind {
            EntryKind::Data(data) => data.clone(),
            EntryKind::Hole { len } => Arc::from(vec![0u8; *len]),
        };
        PageFill {
            data,
            eof: self.eof,
        }
    }
}

#[derive(Debug)]
struct Inner {
    /// CLOCK 环；空槽也在这里，避免淘汰时搬移。
    slots: Vec<Option<Entry>>,
    index: HashMap<PageKey, usize>,
    hand: usize,
    max_pages: usize,
    hole_ttl: Duration,
    reads: u64,
    hits: u64,
    misses: u64,
    bytes_cached: u64,
}

impl Inner {
    /// 找一个可写的槽：优先空槽；否则按 CLOCK 跳过引用位为真的页。
    fn next_slot(&mut self) -> usize {
        if self.slots.len() < self.max_pages {
            self.slots.push(None);
            return self.slots.len() - 1;
        }
        debug_assert!(!self.slots.is_empty());
        loop {
            let i = self.hand;
            self.hand = (self.hand + 1) % self.slots.len();
            match &mut self.slots[i] {
                None => return i,
                Some(entry) if !entry.referenced => return i,
                Some(entry) => entry.referenced = false,
            }
        }
    }

    fn remove_at(&mut self, idx: usize) {
        if let Some(old) = self.slots[idx].take() {
            self.index.remove(&old.key);
            self.bytes_cached = self.bytes_cached.saturating_sub(old.data_len() as u64);
        }
    }
}

/// 线程安全的 64 KiB 页缓存。
///
/// 内部只有一把 [`Mutex`]（06 §3.4 允许的实现；1024 页规模下锁竞争不是瓶颈），
/// 不使用任何 `unsafe`。**持锁期间不做 I/O**：未命中时先放锁读源，再拿锁插入。
#[derive(Debug)]
pub struct PageCache {
    inner: Mutex<Inner>,
}

impl Default for PageCache {
    fn default() -> Self {
        Self::new()
    }
}

impl PageCache {
    /// 64 MiB 预算、2 s 空洞 TTL。
    #[must_use]
    pub fn new() -> Self {
        Self::with_config(PageCacheConfig::default())
    }

    /// 自定义预算与空洞 TTL（测试与大文件模式用）。
    #[must_use]
    pub fn with_config(config: PageCacheConfig) -> Self {
        let max_pages = config.budget_bytes / PAGE_SIZE;
        Self {
            inner: Mutex::new(Inner {
                slots: Vec::with_capacity(max_pages.min(1024)),
                index: HashMap::new(),
                hand: 0,
                max_pages,
                hole_ttl: config.hole_ttl,
                reads: 0,
                hits: 0,
                misses: 0,
                bytes_cached: 0,
            }),
        }
    }

    /// 取一页；未命中时从 provider 填充。
    ///
    /// 返回的 `Arc<[u8]>` 只包含有效字节（文件尾可能短于 [`PAGE_SIZE`]）。
    /// volatile 源（`caps().volatile == true`）每次都会真正读源，不缓存。
    pub fn get_or_fill(
        &self,
        provider: &dyn Provider,
        page_index: u64,
    ) -> Result<Arc<[u8]>, ProviderError> {
        Ok(self.get_page(provider, page_index)?.data)
    }

    /// 与 [`Self::get_or_fill`] 相同，但同时返回是否读到源末尾。
    pub fn get_page(
        &self,
        provider: &dyn Provider,
        page_index: u64,
    ) -> Result<PageFill, ProviderError> {
        let key = (provider.id(), page_index);
        let volatile = provider.caps().volatile;

        if !volatile {
            if let Some(page) = self.lookup(key) {
                return Ok(page);
            }
            self.lock().misses += 1;
        }

        let page_start = page_index.checked_mul(PAGE_SIZE_U64).ok_or_else(|| {
            ProviderError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("页号 {page_index} 溢出 u64"),
            ))
        })?;
        let filled = fill_page(provider, page_start)?;

        {
            let mut inner = self.lock();
            inner.reads += filled.reads;
            if volatile {
                inner.misses += 1;
            }
        }

        if !volatile && filled.cacheable {
            self.store(key, filled.data.clone(), filled.eof, filled.all_holes);
        }

        Ok(PageFill {
            data: filled.data,
            eof: filled.eof,
        })
    }

    /// 当前统计快照。
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        let inner = self.lock();
        CacheStats {
            reads: inner.reads,
            hits: inner.hits,
            misses: inner.misses,
            bytes_cached: inner.bytes_cached,
        }
    }

    /// 清空全部缓存页。统计计数保持不变（累计值用于诊断）。
    pub fn clear(&self) {
        let mut inner = self.lock();
        for slot in &mut inner.slots {
            *slot = None;
        }
        inner.index.clear();
        inner.bytes_cached = 0;
        inner.hand = 0;
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // 缓存是纯派生数据：即使别的线程 panic 污染了锁，也没有一致性损失。
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lookup(&self, key: PageKey) -> Option<PageFill> {
        let mut inner = self.lock();
        let idx = *inner.index.get(&key)?;
        let now = Instant::now();
        if inner.slots[idx]
            .as_ref()
            .is_some_and(|entry| entry.is_expired(now))
        {
            inner.remove_at(idx);
            return None;
        }
        let page = {
            let entry = inner.slots[idx].as_mut()?;
            entry.referenced = true;
            entry.to_fill()
        };
        inner.hits += 1;
        Some(page)
    }

    fn store(&self, key: PageKey, data: Arc<[u8]>, eof: bool, all_holes: bool) {
        let mut inner = self.lock();
        if inner.max_pages == 0 || inner.index.contains_key(&key) {
            return;
        }
        let expires = if all_holes {
            if inner.hole_ttl.is_zero() {
                return;
            }
            Some(Instant::now() + inner.hole_ttl)
        } else {
            None
        };

        let kind = if all_holes {
            EntryKind::Hole { len: data.len() }
        } else {
            EntryKind::Data(data)
        };
        let idx = inner.next_slot();
        if let Some(old) = inner.slots[idx].take() {
            inner.index.remove(&old.key);
            inner.bytes_cached = inner.bytes_cached.saturating_sub(old.data_len() as u64);
        }
        inner.bytes_cached += kind.data_len() as u64;
        inner.index.insert(key, idx);
        inner.slots[idx] = Some(Entry {
            key,
            kind,
            eof,
            expires,
            referenced: false,
        });
    }
}

struct FilledPage {
    data: Arc<[u8]>,
    eof: bool,
    all_holes: bool,
    reads: u64,
    cacheable: bool,
}

/// 把一整页读进内存。provider 的短读会被循环补齐；EOF 或零进展即停。
fn fill_page(provider: &dyn Provider, page_start: u64) -> Result<FilledPage, ProviderError> {
    let mut buf = vec![0u8; PAGE_SIZE];
    let mut filled = 0usize;
    let mut holes = 0u64;
    let mut eof = false;
    let mut reads = 0u64;

    while filled < PAGE_SIZE {
        let at = page_start.checked_add(filled as u64).ok_or_else(|| {
            ProviderError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "填充页时偏移溢出 u64",
            ))
        })?;
        let report = provider.read_range(Offset::new(at), &mut buf[filled..])?;
        reads += 1;

        let want = PAGE_SIZE - filled;
        let n = usize::try_from(report.filled).unwrap_or(want).min(want);
        // `ReadReport` 约定 holes ⊆ filled（空洞字节已填 0）。也有 provider 把
        // 整页空洞报成 `filled=0, holes=want`，两种都接受。
        let reported_holes = usize::try_from(report.holes).unwrap_or(want).min(want);
        let (advance, hole_bytes) = if n == 0 && reported_holes > 0 {
            (reported_holes, reported_holes)
        } else {
            (n, reported_holes.min(n))
        };
        holes = holes.saturating_add(hole_bytes as u64);
        filled += advance;

        if report.eof_hit {
            eof = true;
            break;
        }
        if advance == 0 {
            // 零进展但源没有报 EOF：停止本轮，不伪造 `eof_hit`。
            break;
        }
    }

    buf.truncate(filled);
    let all_holes = filled > 0 && holes >= filled as u64;
    let cacheable = filled > 0 && (filled == PAGE_SIZE || eof);
    Ok(FilledPage {
        data: Arc::from(buf),
        eof,
        all_holes,
        reads,
        cacheable,
    })
}

/// `CachedProvider` 的统计与清理入口。
///
/// 内层 provider 若返回 `readable_ranges() == None`，装饰器无法在页缓存之外
/// 还原页内空洞的位置；为保证 [`ReadReport::holes`] 的语义，这种源会直接
/// 透传底层 `read_range`，不经过页缓存。`FileProvider` / `MemoryProvider`
/// 都实现了 `readable_ranges`。volatile 源同样直接透传，避免为一次短读
/// 预取整页。
#[derive(Debug)]
pub struct CachedProvider<P> {
    inner: P,
    cache: PageCache,
}

impl<P: Provider> CachedProvider<P> {
    /// 用默认 64 MiB 缓存包住一个 provider。
    pub fn new(inner: P) -> Self {
        Self {
            inner,
            cache: PageCache::new(),
        }
    }

    /// 共享一个已有缓存（多个 provider 可以共用一个 [`PageCache`]）。
    pub fn with_cache(inner: P, cache: PageCache) -> Self {
        Self { inner, cache }
    }

    /// 缓存统计。
    #[must_use]
    pub fn stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// 清空缓存。**通过 [`Self::inner_mut`] 写源之后必须调用**，否则读到旧页。
    pub fn clear_cache(&self) {
        self.cache.clear();
    }

    /// 被包装的 provider。
    #[must_use]
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// 可变访问被包装的 provider（写入后记得 [`Self::clear_cache`]）。
    pub fn inner_mut(&mut self) -> &mut P {
        &mut self.inner
    }

    /// 共享缓存的引用。
    #[must_use]
    pub fn cache(&self) -> &PageCache {
        &self.cache
    }
}

impl<P: Provider> Provider for CachedProvider<P> {
    fn id(&self) -> ProviderId {
        self.inner.id()
    }

    fn display_name(&self) -> String {
        self.inner.display_name()
    }

    fn caps(&self) -> crate::Caps {
        self.inner.caps()
    }

    fn size(&self) -> crate::SizeInfo {
        self.inner.size()
    }

    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
        if buf.is_empty() {
            return Ok(ReadReport::default());
        }
        // volatile 源不做整页预取；直接读请求区间，语义最接近底层。
        if self.inner.caps().volatile {
            return self.inner.read_range(at, buf);
        }
        // 未知可读区间时无法精确还原 holes，直接透传以保持 short-read/空洞语义。
        let Some(readable) = self.inner.readable_ranges() else {
            return self.inner.read_range(at, buf);
        };
        let mut done = 0usize;
        let mut eof_hit = false;

        while done < buf.len() {
            let pos = match at.get().checked_add(done as u64) {
                Some(pos) => pos,
                None => {
                    eof_hit = true;
                    break;
                }
            };
            let page_index = pos / PAGE_SIZE_U64;
            let page_off = (pos % PAGE_SIZE_U64) as usize;
            let page = self.cache.get_page(&self.inner, page_index)?;

            if page_off >= page.data.len() {
                eof_hit = page.eof;
                break;
            }
            let n = (page.data.len() - page_off).min(buf.len() - done);
            if n == 0 {
                eof_hit = page.eof;
                break;
            }
            buf[done..done + n].copy_from_slice(&page.data[page_off..page_off + n]);
            done += n;

            // 短页且已到源末尾：后面不会再有数据。
            if page.eof && page_off + n == page.data.len() && done < buf.len() {
                eof_hit = true;
                break;
            }
        }

        let holes = count_holes(&readable, at.get(), done as u64);
        Ok(ReadReport {
            filled: done as u64,
            holes,
            eof_hit,
        })
    }

    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        self.inner.readable_ranges()
    }
}

/// 统计 `[at, at+len)` 中不被任何可读区间覆盖的字节数。
fn count_holes(ranges: &[ByteRange], at: u64, len: u64) -> u64 {
    let end = at.saturating_add(len);
    let mut covered = 0u64;
    for r in ranges {
        let start = r.start().get().max(at);
        let stop = r.end().get().min(end);
        if start < stop {
            covered = covered.saturating_add(stop - start);
        }
    }
    len.saturating_sub(covered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Caps, SizeInfo};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn pattern_page(page: u64) -> Vec<u8> {
        (0..PAGE_SIZE)
            .map(|i| (page as usize * 31 + i) as u8)
            .collect()
    }

    #[test]
    fn default_budget_is_64mib() {
        let cache = PageCache::new();
        let inner = cache.lock();
        assert_eq!(inner.max_pages, 1024);
        assert_eq!(DEFAULT_CACHE_BYTES, 64 * 1024 * 1024);
        assert_eq!(PAGE_SIZE, 65536);
    }

    #[test]
    fn fills_on_miss_and_hits_afterwards() {
        let data = pattern_page(0);
        let p = crate::MemoryProvider::new("cache-hit", data.clone());
        let cache = PageCache::new();

        let first = cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(&first[..], &data[..]);
        assert_eq!(
            cache.stats(),
            CacheStats {
                reads: 1,
                hits: 0,
                misses: 1,
                bytes_cached: PAGE_SIZE as u64,
            }
        );

        let second = cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(&second[..], &data[..]);
        let stats = cache.stats();
        assert_eq!((stats.reads, stats.hits, stats.misses), (1, 1, 1));
        assert!((stats.hit_rate() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn clock_eviction_keeps_referenced_pages() {
        let pages: Vec<Vec<u8>> = (0..3).map(pattern_page).collect();
        let mut all = Vec::new();
        for page in &pages {
            all.extend_from_slice(page);
        }
        let p = crate::MemoryProvider::new("cache-clock", all);
        let cache = PageCache::with_config(PageCacheConfig {
            budget_bytes: 2 * PAGE_SIZE,
            hole_ttl: DEFAULT_HOLE_TTL,
        });

        cache.get_or_fill(&p, 0).unwrap();
        cache.get_or_fill(&p, 1).unwrap();
        // 命中 0，给它第二次机会；接下来淘汰的应是 1。
        cache.get_or_fill(&p, 0).unwrap();
        cache.get_or_fill(&p, 2).unwrap();

        let stats = cache.stats();
        assert_eq!(stats.bytes_cached, 2 * PAGE_SIZE as u64);
        let reads_before = stats.reads;
        let a = cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(&a[..], &pages[0][..], "被引用的页不应被淘汰");
        assert_eq!(cache.stats().reads, reads_before, "0 号页应仍在缓存中");
        let c = cache.get_or_fill(&p, 2).unwrap();
        assert_eq!(&c[..], &pages[2][..]);
    }

    #[test]
    fn evicted_page_is_re_read_correctly() {
        let pages: Vec<Vec<u8>> = (0..4).map(pattern_page).collect();
        let mut all = Vec::new();
        for page in &pages {
            all.extend_from_slice(page);
        }
        let p = crate::MemoryProvider::new("cache-evict", all);
        let cache = PageCache::with_config(PageCacheConfig {
            budget_bytes: 2 * PAGE_SIZE,
            hole_ttl: DEFAULT_HOLE_TTL,
        });

        for i in 0..3 {
            let got = cache.get_or_fill(&p, i).unwrap();
            assert_eq!(&got[..], &pages[i as usize][..]);
        }
        // 0 号页已被淘汰，重新填充后字节必须仍然正确。
        let got = cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(&got[..], &pages[0][..]);
        assert!(cache.stats().reads >= 4);
        assert_eq!(cache.stats().bytes_cached, 2 * PAGE_SIZE as u64);
    }

    #[test]
    fn volatile_source_is_never_cached() {
        let data = pattern_page(0);
        let p = crate::MemoryProvider::new("cache-volatile", data).volatile();
        let cache = PageCache::new();

        cache.get_or_fill(&p, 0).unwrap();
        cache.get_or_fill(&p, 0).unwrap();
        let stats = cache.stats();
        assert_eq!((stats.reads, stats.hits, stats.misses), (2, 0, 2));
        assert_eq!(stats.bytes_cached, 0);
    }

    #[derive(Debug)]
    struct HoleProvider {
        reads: AtomicU64,
    }

    impl HoleProvider {
        fn new() -> Self {
            Self {
                reads: AtomicU64::new(0),
            }
        }
    }

    impl Provider for HoleProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new(0x484F_4C45)
        }

        fn display_name(&self) -> String {
            "hole".into()
        }

        fn caps(&self) -> Caps {
            Caps::READ_ONLY_STABLE
        }

        fn size(&self) -> SizeInfo {
            SizeInfo::Known(4 * PAGE_SIZE_U64)
        }

        fn read_range(&self, _at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            // 整页空洞：字节已是 0，holes 显式上报。这里故意用
            // `filled=0, holes=len` 形态，覆盖不把空洞算进 filled 的后端。
            Ok(ReadReport {
                filled: 0,
                holes: buf.len() as u64,
                eof_hit: false,
            })
        }

        fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
            Some(Vec::new())
        }
    }

    #[test]
    fn hole_negative_cache_hit_does_not_read_again() {
        let p = HoleProvider::new();
        let cache = PageCache::new();

        let first = cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(first.len(), PAGE_SIZE);
        assert!(first.iter().all(|b| *b == 0));
        assert_eq!(p.reads.load(Ordering::Relaxed), 1);

        let second = cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(second.len(), PAGE_SIZE);
        let stats = cache.stats();
        assert_eq!(stats.reads, 1, "负缓存命中不应再读源");
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.bytes_cached, 0);
        assert_eq!(p.reads.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn hole_negative_cache_expires() {
        let p = HoleProvider::new();
        let cache = PageCache::with_config(PageCacheConfig {
            budget_bytes: DEFAULT_CACHE_BYTES,
            hole_ttl: Duration::from_millis(10),
        });

        cache.get_or_fill(&p, 0).unwrap();
        std::thread::sleep(Duration::from_millis(25));
        cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(p.reads.load(Ordering::Relaxed), 2, "TTL 过期后应重新读");
    }

    #[test]
    fn cached_provider_reports_holes() {
        let cached = CachedProvider::new(HoleProvider::new());
        let mut buf = [0u8; PAGE_SIZE];
        let report = cached.read_range(Offset::ZERO, &mut buf).unwrap();
        assert_eq!(report.filled, PAGE_SIZE as u64);
        assert_eq!(report.holes, PAGE_SIZE as u64, "装饰器不得吞掉空洞计数");
        assert!(!report.eof_hit);
        assert!(buf.iter().all(|b| *b == 0));
    }

    #[derive(Debug)]
    struct OpaqueHoleProvider {
        reads: AtomicU64,
    }

    impl Provider for OpaqueHoleProvider {
        fn id(&self) -> ProviderId {
            ProviderId::new(0x4F50_4151)
        }

        fn display_name(&self) -> String {
            "opaque-hole".into()
        }

        fn caps(&self) -> Caps {
            Caps::READ_ONLY_STABLE
        }

        fn size(&self) -> SizeInfo {
            SizeInfo::Known(8)
        }

        fn read_range(&self, _at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            let hole_len = 4.min(buf.len());
            buf[..hole_len].fill(0);
            buf[hole_len..].fill(0xAB);
            Ok(ReadReport {
                filled: buf.len() as u64,
                holes: hole_len as u64,
                eof_hit: true,
            })
        }
    }

    #[test]
    fn cached_provider_passes_through_unknown_ranges() {
        let p = OpaqueHoleProvider {
            reads: AtomicU64::new(0),
        };
        let cached = CachedProvider::new(p);
        let mut buf = [0u8; 8];
        let report = cached.read_range(Offset::ZERO, &mut buf).unwrap();
        assert_eq!((report.filled, report.holes, report.eof_hit), (8, 4, true));
        assert_eq!(cached.stats().reads, 0, "未知区间应直接透传，不进入页缓存");
    }

    #[test]
    fn clear_drops_pages_but_keeps_counters() {
        let data = pattern_page(0);
        let p = crate::MemoryProvider::new("cache-clear", data);
        let cache = PageCache::new();
        cache.get_or_fill(&p, 0).unwrap();
        assert_eq!(cache.stats().bytes_cached, PAGE_SIZE as u64);

        cache.clear();
        let stats = cache.stats();
        assert_eq!(stats.bytes_cached, 0);
        assert_eq!(stats.reads, 1);
    }

    #[test]
    fn cached_provider_preserves_short_read() {
        let p = crate::MemoryProvider::new("cache-short", vec![1, 2, 3, 4, 5]);
        let cached = CachedProvider::new(p);
        let mut buf = [0u8; 8];
        let report = cached.read_range(Offset::new(2), &mut buf).unwrap();
        assert_eq!((report.filled, report.holes, report.eof_hit), (3, 0, true));
        assert_eq!(&buf[..3], &[3, 4, 5]);
        assert_eq!(cached.stats().reads, 1);
    }

    #[test]
    fn cached_provider_reads_one_page_per_page() {
        let data = pattern_page(0);
        let p = crate::MemoryProvider::new("cache-page-once", data.clone());
        let cached = CachedProvider::new(p);
        let mut one = [0u8; 1];
        cached.read_range(Offset::new(0), &mut one).unwrap();
        cached.read_range(Offset::new(1), &mut one).unwrap();
        assert_eq!(cached.stats().reads, 1);
        assert_eq!(cached.stats().hits, 1);
    }
}
