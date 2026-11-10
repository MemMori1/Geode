//! 项目层错误。
//!
//! DESIGN: 23-file-formats.md §3.6、docs/errors.toml 的 EB0xx 区段。

use std::fmt;

/// 项目层操作失败。
///
/// 每个变体都映射到稳定的错误码；`code()` 返回的错误码可用于日志与 UI
/// 诊断（不随 Display 文案变化）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectError {
    /// 记录序号不是从 1 开始严格 +1。
    SeqMismatch {
        /// 按位置推导出的期望序号（1-based）。
        expected: u64,
        /// 盘上实际序号。
        actual: u64,
    },
    /// `FinalLen` 给出的最终长度与累计计算不符。
    FinalLenMismatch {
        /// 按 base_size 与记录累计得到的长度。
        expected: u64,
        /// `FinalLen` 记录声明的长度。
        actual: u64,
    },
    /// `FinalLen` 不是最后一条记录。
    FinalLenNotLast {
        /// 该 `FinalLen` 的 0-based 位置。
        index: usize,
        /// 总记录数。
        total: usize,
    },
    /// 记录的目标区间越过当前有效长度。
    OutOfBounds {
        /// 目标偏移。
        at: u64,
        /// 记录声称读写的长度。
        len: u64,
        /// 当前有效长度（base_size 加前序记录长度变化）。
        limit: u64,
    },
    /// 应用前目标长度与文件头的 `base_size` 不一致。
    BaseSizeMismatch {
        /// 文件头声明的应用前长度。
        expected: u64,
        /// 实际编辑器的长度。
        actual: u64,
    },
    /// 已通过桥接校验，但 `geode-buffer` 拒绝了具体编辑。
    EditFailed {
        /// 出错的目标偏移。
        at: u64,
        /// 底层错误的文本。
        detail: String,
    },
}

impl ProjectError {
    /// 稳定错误码。
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::SeqMismatch { .. } => "EB012",
            Self::FinalLenMismatch { .. } | Self::FinalLenNotLast { .. } => "EB014",
            Self::OutOfBounds { .. } | Self::BaseSizeMismatch { .. } | Self::EditFailed { .. } => {
                "EB013"
            }
        }
    }
}

impl fmt::Display for ProjectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SeqMismatch { expected, actual } => {
                write!(
                    f,
                    "{}: 记录序号不连续：期望 {expected}，实际 {actual}",
                    self.code()
                )
            }
            Self::FinalLenMismatch { expected, actual } => {
                write!(
                    f,
                    "{}: FinalLen 不符：按记录累计应为 {expected}，实际声明 {actual}",
                    self.code()
                )
            }
            Self::FinalLenNotLast { index, total } => write!(
                f,
                "{}: FinalLen 必须是最后一条记录（位置 {index}，共 {total} 条）",
                self.code()
            ),
            Self::OutOfBounds { at, len, limit } => write!(
                f,
                "{}: 记录越界：at={at}、len={len}，当前有效长度 {limit}",
                self.code()
            ),
            Self::BaseSizeMismatch { expected, actual } => write!(
                f,
                "{}: base_size 不符：文件声明 {expected}，编辑器实际 {actual}",
                self.code()
            ),
            Self::EditFailed { at, detail } => {
                write!(f, "{}: 应用 at={at} 的编辑失败：{detail}", self.code())
            }
        }
    }
}

impl std::error::Error for ProjectError {}
