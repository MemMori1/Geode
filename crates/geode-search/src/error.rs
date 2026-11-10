//! 搜索错误与警告。
//!
//! 所有错误码都引用 `docs/errors.toml` 中已登记的 E7xxx / E6xxx，本模块不新增码位。

use core::fmt;

use geode_provider::ProviderError;

/// 搜索子系统错误。
///
/// 变体只携带人类可读的细节；错误码通过 [`SearchError::code`] 取得，
/// 保证 UI 可以按 `docs/errors.toml` 稳定翻译。
#[derive(Debug)]
pub enum SearchError {
    /// E7102：hex/文本模式非法、为空或没有任何固定字节。
    BadPattern {
        /// 具体原因。
        detail: String,
    },
    /// E7103：正则使用了反向引用或环视等流式扫描无法支持的特性。
    UnsupportedRegex {
        /// 具体原因。
        detail: String,
    },
    /// E7103：正则语法本身非法。
    ///
    /// `docs/errors.toml` 的 E7 段没有单独的“正则语法错误”码，沿用
    /// E7103（正则不支持/不可用）而不新增码位。
    InvalidRegex {
        /// 具体原因。
        detail: String,
    },
    /// E7105：数值查询的宽度、边界或对齐参数非法。
    InvalidNumeric {
        /// 具体原因。
        detail: String,
    },
    /// E6xxx：数据源读取失败，码位由 [`ProviderError`] 决定。
    Provider(ProviderError),
}

impl SearchError {
    /// 构造 [`SearchError::BadPattern`]。
    pub fn bad_pattern(detail: impl Into<String>) -> Self {
        Self::BadPattern {
            detail: detail.into(),
        }
    }

    /// 构造 [`SearchError::UnsupportedRegex`]。
    pub fn unsupported_regex(detail: impl Into<String>) -> Self {
        Self::UnsupportedRegex {
            detail: detail.into(),
        }
    }

    /// 构造 [`SearchError::InvalidRegex`]。
    pub fn invalid_regex(detail: impl Into<String>) -> Self {
        Self::InvalidRegex {
            detail: detail.into(),
        }
    }

    /// 构造 [`SearchError::InvalidNumeric`]。
    pub fn invalid_numeric(detail: impl Into<String>) -> Self {
        Self::InvalidNumeric {
            detail: detail.into(),
        }
    }

    /// 返回 `docs/errors.toml` 中登记的错误码。
    pub fn code(&self) -> &'static str {
        match self {
            Self::BadPattern { .. } => "E7102",
            Self::UnsupportedRegex { .. } | Self::InvalidRegex { .. } => "E7103",
            Self::InvalidNumeric { .. } => "E7105",
            Self::Provider(err) => err.to_diag().code.0,
        }
    }
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadPattern { detail } => write!(f, "E7102 搜索模式非法：{detail}"),
            Self::UnsupportedRegex { detail } => {
                write!(f, "E7103 正则不支持该特性：{detail}")
            }
            Self::InvalidRegex { detail } => write!(f, "E7103 正则语法错误：{detail}"),
            Self::InvalidNumeric { detail } => write!(f, "E7105 数值查询非法：{detail}"),
            Self::Provider(err) => write!(f, "数据源读取失败：{err}"),
        }
    }
}

impl std::error::Error for SearchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Provider(err) => Some(err),
            _ => None,
        }
    }
}

impl From<ProviderError> for SearchError {
    fn from(err: ProviderError) -> Self {
        Self::Provider(err)
    }
}

/// 搜索期间产生的非致命警告。
///
/// 只用于“结果仍可用，但某些命中可能被漏掉”的场景。当前只有正则跨块
/// 兜底窗口一种，码位固定为 E7104。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// `docs/errors.toml` 中登记的警告码。
    pub code: &'static str,
    /// 面向日志/UI 的说明（UI 最终仍按码查表）。
    pub message: String,
}

impl Warning {
    /// 正则使用固定 64 KiB 重叠窗口时的警告（E7104）。
    pub fn regex_overlap() -> Self {
        Self {
            code: "E7104",
            message: "正则匹配长度无上界，已使用 64 KiB 重叠窗口；跨越窗口的超长匹配可能被漏掉。"
                .to_string(),
        }
    }
}
