//! 单遍多分析器：一次读盘同时喂哈希、熵、直方图与字符串。
//!
//! DESIGN: 10 §3。分析面板的四类结果（摘要、熵曲线、字节分布、字符串）
//! 如果各扫一遍，1 GB 文件就要读四遍；这里固定 1 MiB 块，每块只读一次、
//! 分发给所有已启用的子分析器。熵的全局值复用直方图计数，不额外扫描。

use geode_core::Offset;
use geode_provider::Provider;

use crate::cancel::CancelToken;
use crate::entropy::{
    EntropyAccumulator, EntropyCfg, EntropyScanner, EntropySummary, MAX_SUMMARY_BUCKETS,
};
use crate::hash::{AnyHasher, HashAlgo, Hasher, new_hasher};
use crate::histogram::ByteHistogram;
use crate::strings::{StringsCfg, StringsResult, StringsScanner};

/// 单遍读盘的块大小：1 MiB（与 10 §5 的性能测量口径一致）。
pub const CHUNK_SIZE: usize = 1 << 20;

/// 单个哈希算法的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestResult {
    /// 使用的算法。
    pub algo: HashAlgo,
    /// 小写十六进制摘要。
    pub hex: String,
    /// 实际喂入该哈希器的字节数。
    pub bytes: u64,
}

/// 一次单遍分析的完整结果。
///
/// `cancelled` 或 `error` 非空时，各字段是**部分结果**：哈希只覆盖前
/// `bytes_read` 字节，熵桶与字符串也截止到取消/出错位置。
#[derive(Clone, Debug)]
pub struct AnalysisReport {
    /// 各哈希算法摘要，顺序与 [`Analyzer::hashers`] 一致。
    pub digests: Vec<DigestResult>,
    /// 熵摘要；未启用时为 `None`。
    pub entropy: Option<EntropySummary>,
    /// 字节直方图；未启用时为 `None`。
    pub histogram: Option<ByteHistogram>,
    /// 字符串提取结果；未启用时为 `None`。
    pub strings: Option<StringsResult>,
    /// 实际读入并喂给子分析器的字节数（含空洞补零）。
    pub bytes_read: u64,
    /// 其中被数据源标记为空洞的字节数。
    pub holes: u64,
    /// 是否因取消而提前结束。
    pub cancelled: bool,
    /// 数据源读取错误（若有）；发生后停止读取并保留部分结果。
    pub error: Option<String>,
}

impl AnalysisReport {
    /// 是否完整走完数据源且无错误、无取消。
    pub fn is_complete(&self) -> bool {
        !self.cancelled && self.error.is_none()
    }
}

/// 单遍多分析器配置。
///
/// ```
/// use geode_analysis::{Analyzer, HashAlgo};
/// let analyzer = Analyzer {
///     hashers: vec![HashAlgo::Crc32Ieee, HashAlgo::Blake3],
///     ..Analyzer::default()
/// };
/// assert_eq!(analyzer.hashers.len(), 2);
/// ```
#[derive(Clone, Debug)]
pub struct Analyzer {
    /// 要计算的哈希算法；空表示不哈希。
    pub hashers: Vec<HashAlgo>,
    /// 熵摘要配置；`None` 表示不算熵。
    pub entropy: Option<EntropyCfg>,
    /// 是否输出字节直方图。
    pub histogram: bool,
    /// 字符串提取配置；`None` 表示不提取。
    pub strings: Option<StringsCfg>,
}

impl Default for Analyzer {
    /// 文档默认组：CRC-32 + BLAKE3、默认熵窗、直方图开、字符串开。
    fn default() -> Self {
        Self {
            hashers: vec![HashAlgo::Crc32Ieee, HashAlgo::Blake3],
            entropy: Some(EntropyCfg::default()),
            histogram: true,
            strings: Some(StringsCfg::default()),
        }
    }
}

impl Analyzer {
    /// 与 [`Default`] 相同的构造函数。
    pub fn new() -> Self {
        Self::default()
    }

    /// 执行单遍分析。
    ///
    /// 读循环每块检查一次取消；短读按 [`geode_provider::ReadReport::filled`] 截断；
    /// 读取失败不 panic，记录到 [`AnalysisReport::error`] 后返回部分结果。
    pub fn run<P: Provider + ?Sized>(&self, provider: &P, cancel: &CancelToken) -> AnalysisReport {
        let size_info = provider.size();
        let size_known = size_info.is_known();
        let size = size_info.lower_bound();
        let entropy_cfg = self.entropy.map(|cfg| {
            if size_known {
                cfg.for_size(size)
            } else {
                cfg.normalized()
            }
        });

        let mut hashers: Vec<(HashAlgo, AnyHasher)> = self
            .hashers
            .iter()
            .copied()
            .map(|algo| (algo, new_hasher(algo)))
            .collect();
        let mut histogram = self.histogram.then(ByteHistogram::new);
        // 熵的全局值必须覆盖全文件，因此即使不输出直方图也单独计数。
        let mut entropy_histogram = entropy_cfg.map(|_| ByteHistogram::new());
        let mut entropy_scanner = entropy_cfg.map(EntropyScanner::new);
        let mut entropy_accumulator =
            entropy_cfg.map(|cfg| EntropyAccumulator::new(size, cfg.window));
        let mut strings = self.strings.map(StringsScanner::new);

        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut bytes_read = 0u64;
        let mut holes = 0u64;
        let mut cancelled = cancel.is_cancelled();
        let mut error = None;
        let mut offset = 0u64;

        // 未知长度时探测到 EOF；已知长度只读声明范围，短读用 filled 推进。
        while !size_known || offset < size {
            if cancel.is_cancelled() {
                cancelled = true;
                break;
            }
            let want = if size_known {
                usize::try_from((size - offset).min(CHUNK_SIZE as u64)).unwrap_or(CHUNK_SIZE)
            } else {
                CHUNK_SIZE
            };
            let report = match provider.read_range(Offset::new(offset), &mut buffer[..want]) {
                Ok(report) => report,
                Err(err) => {
                    error = Some(err.to_string());
                    break;
                }
            };
            let filled = usize::try_from(report.filled)
                .unwrap_or(usize::MAX)
                .min(want);
            if filled == 0 {
                break;
            }
            let chunk = &buffer[..filled];

            for (_, hasher) in &mut hashers {
                hasher.update(chunk);
            }
            if let Some(histogram) = histogram.as_mut() {
                histogram.observe_slice(chunk);
            }
            if let Some(histogram) = entropy_histogram.as_mut() {
                histogram.observe_slice(chunk);
            }
            if let (Some(scanner), Some(accumulator)) =
                (entropy_scanner.as_mut(), entropy_accumulator.as_mut())
            {
                scanner.push(chunk, |point| accumulator.push(point));
            }
            if let Some(scanner) = strings.as_mut() {
                scanner.push(chunk, offset);
            }

            bytes_read = bytes_read.saturating_add(filled as u64);
            holes = holes.saturating_add(report.holes);
            offset = offset.saturating_add(filled as u64);
            if report.eof_hit {
                break;
            }
        }

        let digests = hashers
            .into_iter()
            .map(|(algo, hasher)| DigestResult {
                algo,
                hex: crate::hash::hex_encode(&hasher.finish()),
                bytes: bytes_read,
            })
            .collect();

        let entropy = match (entropy_cfg, entropy_histogram, entropy_accumulator) {
            (Some(cfg), Some(histogram), Some(accumulator)) => Some(EntropySummary {
                window: cfg.window,
                step: cfg.step,
                global: histogram.entropy(),
                buckets: accumulator.finish(),
            }),
            _ => None,
        };

        AnalysisReport {
            digests,
            entropy,
            histogram,
            strings: strings.map(StringsScanner::finish),
            bytes_read,
            holes,
            cancelled,
            error,
        }
    }
}

/// 熵摘要的桶数上限（转导出，方便 UI 预分配）。
pub const ENTROPY_BUCKET_CAP: usize = MAX_SUMMARY_BUCKETS;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::hash_hex;
    use geode_provider::MemoryProvider;

    #[test]
    fn single_pass_feeds_every_analyzer() {
        let mut data = Vec::new();
        data.extend_from_slice(b"\x00\x01hello world\x00");
        data.extend((0..=255u8).cycle().take(4096));
        let provider = MemoryProvider::new("single-pass", data.clone());
        let analyzer = Analyzer::default();
        let report = analyzer.run(&provider, &CancelToken::new());

        assert!(report.is_complete());
        assert_eq!(report.bytes_read, data.len() as u64);
        assert_eq!(report.holes, 0);
        assert_eq!(report.digests.len(), 2);
        assert_eq!(report.digests[0].hex, hash_hex(HashAlgo::Crc32Ieee, &data));
        assert_eq!(report.digests[1].hex, hash_hex(HashAlgo::Blake3, &data));
        assert_eq!(
            report.histogram.as_ref().unwrap().total(),
            data.len() as u64
        );
        let entropy = report.entropy.as_ref().unwrap();
        assert!((entropy.global - crate::entropy::shannon(&data)).abs() < 1e-9);
        let strings = report.strings.as_ref().unwrap();
        assert!(
            strings
                .entries
                .iter()
                .any(|entry| entry.text == "hello world")
        );
    }

    #[test]
    fn cancellation_before_first_read_yields_empty_report() {
        let provider = MemoryProvider::new("cancel", vec![7u8; 32]);
        let cancel = CancelToken::new();
        cancel.cancel();
        let report = Analyzer::default().run(&provider, &cancel);
        assert!(report.cancelled);
        assert!(!report.is_complete());
        assert_eq!(report.bytes_read, 0);
        assert_eq!(report.digests[0].hex, hash_hex(HashAlgo::Crc32Ieee, b""));
    }

    #[test]
    fn empty_provider_is_well_defined() {
        let provider = MemoryProvider::new("empty", Vec::new());
        let report = Analyzer::default().run(&provider, &CancelToken::new());
        assert!(report.is_complete());
        assert_eq!(report.bytes_read, 0);
        assert_eq!(report.histogram.as_ref().unwrap().total(), 0);
        assert_eq!(report.entropy.as_ref().unwrap().global, 0.0);
        assert!(report.entropy.as_ref().unwrap().buckets.is_empty());
        assert!(report.strings.as_ref().unwrap().entries.is_empty());
    }
}
