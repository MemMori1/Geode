//! 数据源抽象层。
//!
//! DESIGN: 06-data-providers.md §3.1、00 §5.1
//!
//! 四条铁律（00 §5.1，全层强制）：
//!
//! 1. **短读合法**——读到文件末尾不算错误，用 [`ReadReport`] 报告实际填充量；
//! 2. **越界不 panic**——任何 `offset + len` 越界都裁剪后返回；
//! 3. **空洞是显式结果**——稀疏文件/GDB 未映射页必须通过 `holes` 表达，绝不静默填 0；
//! 4. **volatile 与 holes 必须明示**——调用方据此决定是否缓存、是否允许保存。

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod file;
pub mod memory;
pub mod page_cache;

use core::fmt::{self, Debug};
use geode_core::{ByteRange, Diag, Offset, ProviderId};

pub use file::FileProvider;
pub use memory::MemoryProvider;
pub use page_cache::{
    CacheStats, CachedProvider, DEFAULT_CACHE_BYTES, DEFAULT_HOLE_TTL, PAGE_SIZE, PAGE_SIZE_U64,
    PageCache, PageCacheConfig, PageFill,
};

/// 数据源能力标志。上层**只看能力、不看具体类型**（06 §3.1）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Caps {
    /// 支持随机访问读（所有实现都必须支持；预留为条件能力）。
    pub random_read: bool,
    /// 支持写回。只读源为 `false`，UI 据此禁掉保存。
    pub writable: bool,
    /// 长度可靠。未知长度走"探测扩展"策略（06 §4.2）。
    pub sized: bool,
    /// 内容随时可能变化（进程内存、UDP、GDB）。此类源不缓存（TTL=0）。
    pub volatile: bool,
}

impl Caps {
    /// 只读、有界、稳定——本地文件与磁盘镜像的默认能力。
    pub const READ_ONLY_STABLE: Self = Self {
        random_read: true,
        writable: false,
        sized: true,
        volatile: false,
    };

    /// 可写、有界、稳定。
    pub const READ_WRITE_STABLE: Self = Self {
        random_read: true,
        writable: true,
        sized: true,
        volatile: false,
    };

    /// 易变且不可写（进程内存只读视图）。
    pub const VOLATILE_READ_ONLY: Self = Self {
        random_read: true,
        writable: false,
        sized: true,
        volatile: true,
    };
}

/// 长度信息。未知长度不能假装是 0——见 06 §4.2 的探测扩展策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeInfo {
    /// 精确长度。
    Known(u64),
    /// 未知，只有下界（探测到的可读字节数）。
    Unknown {
        /// 已确认可读的字节数。
        lower_bound: u64,
    },
}

impl SizeInfo {
    /// 已知长度；未知时返回下界。
    pub fn lower_bound(self) -> u64 {
        match self {
            SizeInfo::Known(n) => n,
            SizeInfo::Unknown { lower_bound } => lower_bound,
        }
    }

    /// 是否精确。
    pub fn is_known(self) -> bool {
        matches!(self, SizeInfo::Known(_))
    }
}

/// 一次读操作的结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadReport {
    /// 实际读到的字节数。
    pub filled: u64,
    /// 其中属于空洞（无后端数据）的字节数，已填 0 但**必须**上报。
    pub holes: u64,
    /// 是否碰到了数据源末尾。
    pub eof_hit: bool,
}

impl ReadReport {
    /// 全部读满、无空洞。
    pub fn full(len: u64) -> Self {
        Self {
            filled: len,
            holes: 0,
            eof_hit: false,
        }
    }

    /// 是否完整读到请求的全部字节且无空洞。
    pub fn is_clean(&self, requested: u64) -> bool {
        self.filled == requested && self.holes == 0
    }
}

/// 数据源变化的种类（供缓存与视图失效判定，06 §3.4）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// 无变化。
    Unchanged,
    /// 长度变了（尾部追加或截断），需重建 tail 相关缓存。
    Resized,
    /// 区间不再可读（GDB 断连、进程退出）。
    Unmapped,
    /// 内容被替换（同长度），只需失效对应块。
    Replaced,
}

/// 数据源错误。码位占 `E6xxx`（00 §12 已登记）。
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    /// E6001 权限不足。
    #[error("E6001 权限不足：{detail}")]
    Permission {
        /// 细节（仅进日志）。
        detail: String,
    },
    /// E6002 目标消失（文件被删、进程退出）。
    #[error("E6002 目标消失：{detail}")]
    Gone {
        /// 细节。
        detail: String,
    },
    /// E6003 连接断开（SSH/GDB）。
    #[error("E6003 连接断开：{detail}")]
    Disconnected {
        /// 细节。
        detail: String,
    },
    /// E6004 协议超时。
    #[error("E6004 协议超时：{detail}")]
    Timeout {
        /// 细节。
        detail: String,
    },
    /// E6005 格式校验失败（Intel HEX / SREC 记录损坏）。
    #[error("E6005 格式校验失败：{detail}")]
    BadFormat {
        /// 细节。
        detail: String,
    },
    /// 其他 I/O 错误（归类为 E6002 之外的兜底）。
    #[error("I/O 错误：{0}")]
    Io(#[from] std::io::Error),
}

impl ProviderError {
    /// 转成跨层诊断（带错误码，供 UI 查表生成文案）。
    pub fn to_diag(&self) -> Diag {
        let code = match self {
            ProviderError::Permission { .. } => "E6001",
            ProviderError::Gone { .. } => "E6002",
            ProviderError::Disconnected { .. } => "E6003",
            ProviderError::Timeout { .. } => "E6004",
            ProviderError::BadFormat { .. } => "E6005",
            ProviderError::Io(_) => "E6002",
        };
        Diag::error(geode_core::diag::Code(code), self.to_string())
    }
}

/// 只读数据源。
///
/// 实现者只需保证：短读合法、越界不 panic、空洞显式上报。
pub trait Provider: Debug + Send + Sync {
    /// 稳定身份，用于日志与"是否同一文件"判定。
    fn id(&self) -> ProviderId;

    /// 人类可读名字（`文件: /path`、`进程 1234 内存`）。
    fn display_name(&self) -> String;

    /// 能力标志。
    fn caps(&self) -> Caps;

    /// 长度信息。
    fn size(&self) -> SizeInfo;

    /// 读 `at` 起、填充 `buf`。
    ///
    /// **不保证填满**：返回的 [`ReadReport`] 说明填了多少、其中多少是空洞。
    fn read_range(&self, at: Offset, buf: &mut [u8]) -> Result<ReadReport, ProviderError>;

    /// 连续可读区间（稀疏文件、进程内存的已映射页）。
    ///
    /// 返回 `None` 表示"未知，请逐块探测"。
    fn readable_ranges(&self) -> Option<Vec<ByteRange>> {
        None
    }

    /// 便捷：读一段并返回 Vec，同时要求报告无空洞。
    fn read_exact_vec(&self, at: Offset, len: u64) -> Result<(Vec<u8>, ReadReport), ProviderError> {
        let mut buf = vec![0u8; usize::try_from(len).unwrap_or(usize::MAX)];
        let report = self.read_range(at, &mut buf)?;
        buf.truncate(usize::try_from(report.filled).unwrap_or(buf.len()));
        Ok((buf, report))
    }
}

impl fmt::Display for Caps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "caps(rw={}, sized={}, volatile={})",
            self.writable, self.sized, self.volatile
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_report_clean_check() {
        assert!(ReadReport::full(16).is_clean(16));
        assert!(
            !ReadReport {
                filled: 16,
                holes: 4,
                eof_hit: false
            }
            .is_clean(16)
        );
        assert!(
            !ReadReport {
                filled: 10,
                holes: 0,
                eof_hit: true
            }
            .is_clean(16)
        );
    }

    #[test]
    fn size_info_lower_bound() {
        assert_eq!(SizeInfo::Known(10).lower_bound(), 10);
        assert_eq!(SizeInfo::Unknown { lower_bound: 3 }.lower_bound(), 3);
        assert!(!SizeInfo::Unknown { lower_bound: 3 }.is_known());
    }

    #[test]
    fn diag_codes_are_registered_segments() {
        let d = ProviderError::Permission { detail: "x".into() }.to_diag();
        assert_eq!(d.code.segment(), "E6");
    }
}
