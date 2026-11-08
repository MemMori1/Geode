//! 诊断：**机器可读的错误码** + 人类可读的细节。
//!
//! DESIGN: 00 §12（错误码登记表）、25 §3.4（文案表）

use crate::range::ByteRange;
use core::fmt;

/// 严重级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// 提示，不影响结果可用性。
    Info,
    /// 已降级但结果可用（例如指针无法解析、字段标为未知）。
    Warning,
    /// 该结构单元失败，其余继续（BinSpec 的软失败，见 02 §9）。
    Error,
    /// 整次操作失败。
    Fatal,
}

/// 诊断码，形如 `E1001` / `EB001` / `EC001`。
///
/// 段位分配由 00 §12 登记，**一码一义、发布后只增不改**。
/// 业务 crate 只产出码与细节，**不上屏任何句子**——句子由 `geode-ui`
/// 按 i18n key 查表生成（25 §3.4：`dsl.err.<CODE>.{short,long,action}`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Code(pub &'static str);

impl Code {
    /// 段落前缀（首个 ASCII 字母段），例如 `E1` 或 `EB`。
    pub fn segment(self) -> &'static str {
        let s = self.0.as_bytes();
        let mut n = 0;
        while n < s.len() && s[n].is_ascii_alphabetic() {
            n += 1;
        }
        // 单字母前缀（E）再吃掉一位数字，得到 E0/E1/…/E9 这样的段位；
        // 双字母前缀（EB/EC）本身即段位。
        if n == 1 && n < s.len() && s[n].is_ascii_digit() {
            n += 1;
        }
        &self.0[..n]
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// 一条诊断。
#[derive(Clone, Debug)]
pub struct Diag {
    /// 严重级别。
    pub severity: Severity,
    /// 登记过的错误码。
    pub code: Code,
    /// 涉及的字节范围；UI 据此高亮，可为空（非字节级错误）。
    pub range: Option<ByteRange>,
    /// 结构路径，例如 `/header/entries[3]/offset`。
    pub path: Option<String>,
    /// 仅进日志的细节文本——**不要直接上屏**。
    pub detail: String,
    /// 是否可恢复（软失败）。
    pub recoverable: bool,
}

impl Diag {
    /// 构造一条错误级诊断。
    pub fn error(code: Code, detail: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code,
            range: None,
            path: None,
            detail: detail.into(),
            recoverable: true,
        }
    }

    /// 构造一条警告级诊断。
    pub fn warning(code: Code, detail: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, detail)
        }
    }

    /// 附带字节范围。
    #[must_use]
    pub fn with_range(mut self, range: ByteRange) -> Self {
        self.range = Some(range);
        self
    }

    /// 附带结构路径。
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// 标记为不可恢复。
    #[must_use]
    pub fn fatal(mut self) -> Self {
        self.severity = Severity::Fatal;
        self.recoverable = false;
        self
    }
}

impl fmt::Display for Diag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?} {}", self.severity, self.code)?;
        if let Some(r) = self.range {
            write!(f, " @{r}")?;
        }
        if let Some(p) = &self.path {
            write!(f, " {p}")?;
        }
        write!(f, ": {}", self.detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segment_splits_letters_from_digits() {
        assert_eq!(Code("E1001").segment(), "E1");
        assert_eq!(Code("EB402").segment(), "EB");
        assert_eq!(Code("EC001").segment(), "EC");
        assert_eq!(Code("E0120").segment(), "E0");
    }

    #[test]
    fn warning_is_recoverable() {
        let d = Diag::warning(Code("E1002"), "长度字段超出文件末尾");
        assert_eq!(d.severity, Severity::Warning);
        assert!(d.recoverable);
    }
}
