//! Geode 分析工具箱（M2 T2.7）。
//!
//! DESIGN: `docs/10-analysis-toolkit.md` §3–§5。所有分析都围绕
//! [`geode_provider::Provider`] 工作，不要求把整个文件读入内存：
//!
//! - [`hash`]：自实现 CRC-32/IEEE、BLAKE3、FNV-1a 32/64；
//! - [`entropy`]：Shannon 熵、滑窗熵曲线与 ≤4096 桶摘要；
//! - [`histogram`]：256 bin 字节分布、top-N、χ²；
//! - [`strings`]：可打印 ASCII 与 UTF-16LE 字符串；
//! - [`analyzer`]：单遍多分析器，一次 1 MiB 分块读盘喂全部子分析器。
//!
//! 取消使用本 crate 的 [`CancelToken`]（`geode-core` 尚未提供跨 crate 版本）。
//!
//! # 示例
//!
//! ```
//! use geode_analysis::{Analyzer, CancelToken};
//! use geode_provider::MemoryProvider;
//!
//! let provider = MemoryProvider::new("demo", b"hello, geode".to_vec());
//! let report = Analyzer::default().run(&provider, &CancelToken::new());
//! assert_eq!(report.bytes_read, 12);
//! assert_eq!(report.digests.len(), 2);
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod analyzer;
pub mod cancel;
pub mod entropy;
pub mod error;
pub mod hash;
pub mod histogram;
pub mod strings;

pub use analyzer::{AnalysisReport, Analyzer, CHUNK_SIZE, DigestResult, ENTROPY_BUCKET_CAP};
pub use cancel::CancelToken;
pub use entropy::{
    DEFAULT_STEP, DEFAULT_WINDOW, EntropyBucket, EntropyCfg, EntropyPoint, EntropyProfile,
    EntropySummary, MAX_SUMMARY_BUCKETS, MAX_WINDOW, shannon,
};
pub use error::AnalysisError;
pub use hash::{
    AnyHasher, Blake3Hasher, Crc32Hasher, Fnv1a32Hasher, Fnv1a64Hasher, HashAlgo, HashOutcome,
    Hasher, hash_bytes, hash_hex, hash_stream, hex_encode, new_hasher,
};
pub use histogram::ByteHistogram;
pub use strings::{
    DEFAULT_MAX_ENTRIES, DEFAULT_MIN_LEN, FoundString, MAX_STRING_CHARS, StringEncoding,
    StringsCfg, StringsResult, StringsScanner, extract_ascii, extract_strings, extract_utf16le,
    is_printable,
};
