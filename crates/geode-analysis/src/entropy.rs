//! 熵：`shannon`、滑窗熵曲线与 ≤4096 桶的摘要。
//!
//! DESIGN: 10 §4.3。全文件单遍扫描时维护 256 个计数与 `n`，滑窗"减出、加进"
//! 每字节 O(1)；`summary` 只保留每桶的 min/max/mean，桶数硬上限 4096，
//! 因此渲染内存与文件大小无关。

use geode_core::Offset;
use geode_provider::Provider;

use crate::cancel::CancelToken;
use crate::error::AnalysisError;

/// 默认滑窗大小（字节）。任务 T2.7 指定 512；文档 §4.3 的 1 KiB 见报告说明。
pub const DEFAULT_WINDOW: u32 = 512;
/// 默认步长（字节）。
pub const DEFAULT_STEP: u32 = 256;
/// 熵摘要桶数硬上限。
pub const MAX_SUMMARY_BUCKETS: usize = 4096;
/// 滑窗大小防御上限（16 MiB）：再大对分析没有意义，且环形缓冲会挤占 UI 内存。
pub const MAX_WINDOW: u32 = 16 * 1024 * 1024;

/// `c·log2(c)` 预计算表的覆盖范围；更大的计数回退到 `log2`。
const LOG_TABLE_MAX: usize = 4096;

/// 单次读盘块大小：1 MiB。
const STREAM_CHUNK: usize = 1 << 20;

/// 一个滑窗熵采样点。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntropyPoint {
    /// 窗口起点（字节偏移）。
    pub offset: u64,
    /// 窗口内 256 bin 的 Shannon 熵，范围 `[0, 8]`。
    pub entropy: f64,
}

/// 熵摘要中的一个桶。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EntropyBucket {
    /// 桶覆盖的起始偏移。
    pub start: u64,
    /// 桶覆盖的字节长度（最后一个桶可能不足一个 span）。
    pub len: u64,
    /// 桶内采样点的最小熵。
    pub min: f32,
    /// 桶内采样点的平均熵。
    pub mean: f32,
    /// 桶内采样点的最大熵。
    pub max: f32,
}

/// 滑窗熵配置。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntropyCfg {
    /// 窗口大小（字节），最小 1。
    pub window: u32,
    /// 步长（字节），会被夹到 `[1, window]`。
    pub step: u32,
}

impl Default for EntropyCfg {
    fn default() -> Self {
        Self {
            window: DEFAULT_WINDOW,
            step: DEFAULT_STEP,
        }
    }
}

impl EntropyCfg {
    /// 构造并规范化配置。
    pub fn new(window: u32, step: u32) -> Self {
        Self { window, step }.normalized()
    }

    /// 把窗口/步长夹到合法范围。
    pub fn normalized(self) -> Self {
        let window = self.window.clamp(1, MAX_WINDOW);
        let step = self.step.max(1).min(window);
        Self { window, step }
    }

    /// 针对已知文件长度调整：窗口不超过文件长度，避免小文件没有任何采样点。
    pub fn for_size(self, size: u64) -> Self {
        let mut cfg = self.normalized();
        if size > 0 {
            let size_u32 = u32::try_from(size).unwrap_or(u32::MAX);
            cfg.window = cfg.window.min(size_u32).max(1);
            cfg.step = cfg.step.min(cfg.window).max(1);
        }
        cfg
    }
}

/// 计算字节切片的 Shannon 熵（256 bin，范围 `[0, 8]`）。
///
/// 空输入返回 `0.0`，且结果保证 `is_finite()`。
pub fn shannon(bytes: &[u8]) -> f64 {
    let mut counts = [0u64; 256];
    for &byte in bytes {
        counts[usize::from(byte)] += 1;
    }
    entropy_from_counts(&counts)
}

/// 由 256 bin 计数计算熵；供直方图复用。
pub(crate) fn entropy_from_counts(counts: &[u64; 256]) -> f64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let sum: f64 = counts
        .iter()
        .filter(|&&count| count > 0)
        .map(|&count| {
            let c = count as f64;
            c * c.log2()
        })
        .sum();
    let entropy = (n.log2() - sum / n).clamp(0.0, 8.0);
    if entropy.is_finite() { entropy } else { 0.0 }
}

/// 滑窗熵曲线。
#[derive(Clone, Debug, PartialEq)]
pub struct EntropyProfile {
    /// 实际使用的窗口大小（可能因文件较短而缩小）。
    pub window: u32,
    /// 实际使用的步长。
    pub step: u32,
    /// 按偏移升序的采样点。
    pub points: Vec<EntropyPoint>,
}

impl EntropyProfile {
    /// 单遍扫描 provider，返回滑窗熵曲线。
    ///
    /// `window`/`step` 会被规范化；文件短于窗口时窗口缩到文件长度，
    /// 因此非空文件至少有一个采样点。短读按 `ReadReport.filled` 截断。
    pub fn scan<P: Provider + ?Sized>(
        provider: &P,
        window: u32,
        step: u32,
        cancel: &CancelToken,
    ) -> Result<Self, AnalysisError> {
        if cancel.is_cancelled() {
            return Err(AnalysisError::Cancelled);
        }
        let size_info = provider.size();
        let size_known = size_info.is_known();
        let size = size_info.lower_bound();
        let cfg = if size_known {
            EntropyCfg::new(window, step).for_size(size)
        } else {
            EntropyCfg::new(window, step)
        };
        if size_known && size == 0 {
            return Ok(Self {
                window: cfg.window,
                step: cfg.step,
                points: Vec::new(),
            });
        }

        let mut scanner = EntropyScanner::new(cfg);
        let mut points = Vec::new();
        let mut buffer = vec![0u8; STREAM_CHUNK];
        let mut offset = 0u64;
        // 与 hash_stream 相同：未知长度用探测式读取，已知长度只读声明范围。
        while !size_known || offset < size {
            if cancel.is_cancelled() {
                return Err(AnalysisError::Cancelled);
            }
            let want = if size_known {
                usize::try_from((size - offset).min(STREAM_CHUNK as u64)).unwrap_or(STREAM_CHUNK)
            } else {
                STREAM_CHUNK
            };
            let report = provider.read_range(Offset::new(offset), &mut buffer[..want])?;
            let filled = usize::try_from(report.filled)
                .unwrap_or(usize::MAX)
                .min(want);
            if filled == 0 {
                break;
            }
            scanner.push(&buffer[..filled], |point| points.push(point));
            offset += filled as u64;
            if report.eof_hit {
                break;
            }
        }
        Ok(Self {
            window: cfg.window,
            step: cfg.step,
            points,
        })
    }

    /// 把采样点压成至多 `buckets` 个桶（硬上限 [`MAX_SUMMARY_BUCKETS`]）。
    ///
    /// 每桶输出 min/max/mean；请求 0 桶或没有采样点时返回空集。
    pub fn summary(&self, buckets: usize) -> Vec<EntropyBucket> {
        let buckets = buckets.min(MAX_SUMMARY_BUCKETS);
        if buckets == 0 || self.points.is_empty() {
            return Vec::new();
        }
        let count = self.points.len();
        let buckets = buckets.min(count);
        let mut out = Vec::with_capacity(buckets);
        for index in 0..buckets {
            let start_index = ((index as u128 * count as u128) / buckets as u128) as usize;
            let end_index = (((index + 1) as u128 * count as u128) / buckets as u128) as usize;
            if start_index >= end_index {
                continue;
            }
            let slice = &self.points[start_index..end_index];
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            let mut sum = 0.0f64;
            for point in slice {
                min = min.min(point.entropy);
                max = max.max(point.entropy);
                sum += point.entropy;
            }
            let mean = sum / slice.len() as f64;
            let first = slice[0].offset;
            let last = slice[slice.len() - 1].offset;
            out.push(EntropyBucket {
                start: first,
                len: last
                    .saturating_add(u64::from(self.window))
                    .saturating_sub(first),
                min: min as f32,
                mean: mean as f32,
                max: max as f32,
            });
        }
        out
    }
}

/// 单遍多分析器使用的熵摘要（全局 + 至多 4096 桶）。
#[derive(Clone, Debug, PartialEq)]
pub struct EntropySummary {
    /// 实际使用的窗口大小。
    pub window: u32,
    /// 实际使用的步长。
    pub step: u32,
    /// 全文件字节直方图算出的全局熵。
    pub global: f64,
    /// 至多 [`MAX_SUMMARY_BUCKETS`] 个桶。
    pub buckets: Vec<EntropyBucket>,
}

/// 滑窗熵扫描器：环形缓冲 + 256 计数 + 预计算 `c·log2(c)` 表。
///
/// 每字节只做计数增减与环形下标推进；每 `step` 字节才遍历一次 256 bin 求和，
/// 因而不在热路径上调用 `log2`。默认 512/256 配置下约每字节 1 次表查询。
pub(crate) struct EntropyScanner {
    window: usize,
    step: u64,
    ring: Vec<u8>,
    counts: [u32; 256],
    log_table: Vec<f32>,
    seen: u64,
    pos: usize,
}

impl EntropyScanner {
    pub(crate) fn new(cfg: EntropyCfg) -> Self {
        let cfg = cfg.normalized();
        let window = cfg.window as usize;
        let log_table = (0..=window.min(LOG_TABLE_MAX))
            .map(|count| {
                if count == 0 {
                    0.0
                } else {
                    let c = count as f32;
                    c * c.log2()
                }
            })
            .collect();
        Self {
            window,
            step: u64::from(cfg.step),
            ring: vec![0u8; window],
            counts: [0u32; 256],
            log_table,
            seen: 0,
            pos: 0,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8], mut emit: impl FnMut(EntropyPoint)) {
        let window = self.window as u64;
        for &byte in data {
            if self.seen >= window {
                let old = self.ring[self.pos];
                self.counts[usize::from(old)] -= 1;
            }
            self.ring[self.pos] = byte;
            self.counts[usize::from(byte)] += 1;
            self.pos += 1;
            if self.pos == self.window {
                self.pos = 0;
            }
            self.seen += 1;

            if self.seen >= window {
                let start = self.seen - window;
                if start % self.step == 0 {
                    emit(EntropyPoint {
                        offset: start,
                        entropy: self.entropy(),
                    });
                }
            }
        }
    }

    fn entropy(&self) -> f64 {
        if self.window <= 1 {
            return 0.0;
        }
        let mut sum = 0.0f32;
        for &count in &self.counts {
            let index = count as usize;
            sum += match self.log_table.get(index) {
                Some(&value) => value,
                None => {
                    let c = count as f32;
                    c * c.log2()
                }
            };
        }
        let n = self.window as f32;
        let entropy = (n.log2() - sum / n).clamp(0.0, 8.0);
        if entropy.is_finite() {
            f64::from(entropy)
        } else {
            0.0
        }
    }
}

/// 在线把采样点装进固定 4096 个等宽桶，避免为 100GB 文件保存数亿个点。
pub(crate) struct EntropyAccumulator {
    span: u64,
    size: u64,
    buckets: Vec<BucketAcc>,
}

#[derive(Clone, Copy, Default)]
struct BucketAcc {
    min: f32,
    max: f32,
    sum: f64,
    count: u64,
}

impl EntropyAccumulator {
    pub(crate) fn new(size: u64, window: u32) -> Self {
        let span = if size > 0 {
            size.div_ceil(MAX_SUMMARY_BUCKETS as u64)
                .max(u64::from(window.max(1)))
        } else {
            u64::from(window.max(1))
        };
        let mut buckets = Vec::with_capacity(MAX_SUMMARY_BUCKETS);
        buckets.resize(
            MAX_SUMMARY_BUCKETS,
            BucketAcc {
                min: f32::INFINITY,
                max: f32::NEG_INFINITY,
                sum: 0.0,
                count: 0,
            },
        );
        Self {
            span,
            size,
            buckets,
        }
    }

    pub(crate) fn push(&mut self, point: EntropyPoint) {
        let index = usize::try_from(point.offset / self.span)
            .unwrap_or(usize::MAX)
            .min(MAX_SUMMARY_BUCKETS - 1);
        let bucket = &mut self.buckets[index];
        bucket.min = bucket.min.min(point.entropy as f32);
        bucket.max = bucket.max.max(point.entropy as f32);
        bucket.sum += point.entropy;
        bucket.count += 1;
    }

    pub(crate) fn finish(&self) -> Vec<EntropyBucket> {
        let mut out = Vec::new();
        for (index, bucket) in self.buckets.iter().enumerate() {
            if bucket.count == 0 {
                continue;
            }
            let start = index as u64 * self.span;
            let len = if self.size > 0 {
                self.span.min(self.size.saturating_sub(start))
            } else {
                self.span
            };
            out.push(EntropyBucket {
                start,
                len,
                min: bucket.min,
                mean: (bucket.sum / bucket.count as f64) as f32,
                max: bucket.max,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_zero_finite_entropy() {
        let entropy = shannon(b"");
        assert_eq!(entropy, 0.0);
        assert!(entropy.is_finite());
    }

    #[test]
    fn constant_input_has_zero_entropy() {
        assert_eq!(shannon(&[0u8; 4096]), 0.0);
        assert_eq!(shannon(&[0xAA; 1024]), 0.0);
    }

    #[test]
    fn all_256_bytes_have_entropy_eight() {
        let data: Vec<u8> = (0..=255u8).collect();
        assert!((shannon(&data) - 8.0).abs() < 1e-12);
    }

    #[test]
    fn uniform_megabyte_is_near_eight() {
        let data: Vec<u8> = (0..1 << 20).map(|i| (i % 256) as u8).collect();
        let entropy = shannon(&data);
        assert!((7.996..=8.0).contains(&entropy), "{entropy}");
    }

    #[test]
    fn summary_caps_bucket_count_and_keeps_bounds() {
        let points: Vec<EntropyPoint> = (0..10_000u64)
            .map(|i| EntropyPoint {
                offset: i * 256,
                entropy: (i % 9) as f64,
            })
            .collect();
        let profile = EntropyProfile {
            window: 512,
            step: 256,
            points: points.clone(),
        };
        let summary = profile.summary(99_999);
        assert!(summary.len() <= MAX_SUMMARY_BUCKETS);
        assert_eq!(summary.first().unwrap().start, 0);
        assert!(summary.iter().all(|bucket| bucket.min <= bucket.mean));
        assert!(summary.iter().all(|bucket| bucket.mean <= bucket.max));
        let total: u64 = summary.iter().map(|bucket| bucket.len).sum();
        assert!(total >= 10_000 * 256);
    }

    #[test]
    fn summary_handles_empty_and_zero_buckets() {
        let profile = EntropyProfile {
            window: 512,
            step: 256,
            points: Vec::new(),
        };
        assert!(profile.summary(16).is_empty());
        assert!(profile.summary(0).is_empty());
    }
}
