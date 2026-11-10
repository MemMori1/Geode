//! 分析过程中的错误。
//!
//! 只区分"用户取消"和"数据源读失败"两类：算法本身对任何输入都不失败
//! （空输入、短读、空洞都有确定语义）。不新增诊断码，UI 层如需 `Diag`
//! 由调用方把 [`ProviderError`] 转成已有的 `E6xxx`。

use core::fmt;

use geode_provider::ProviderError;

/// 分析入口可能返回的错误。
#[derive(Debug)]
pub enum AnalysisError {
    /// 用户请求取消；已产生的部分结果由调用方决定是否保留。
    Cancelled,
    /// 数据源读取失败（权限、断连、I/O 错误等）。
    Provider(ProviderError),
}

impl fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("分析已取消"),
            Self::Provider(err) => write!(f, "数据源读取失败：{err}"),
        }
    }
}

impl std::error::Error for AnalysisError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cancelled => None,
            Self::Provider(err) => Some(err),
        }
    }
}

impl From<ProviderError> for AnalysisError {
    fn from(value: ProviderError) -> Self {
        Self::Provider(value)
    }
}
